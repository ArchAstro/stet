//! One running kernel: a blocking handle in front, a worker thread behind.
//!
//! The worker runs a single-threaded tokio runtime. It launches the kernel
//! process, connects the shell, control and iopub sockets, and waits for the
//! kernel to answer before saying [`Event::Ready`]. Two small tasks read the
//! shell and iopub sockets into one queue; the worker's loop takes from that
//! queue, from the handle's commands and from the process exiting.
//!
//! A run finishes when both of its halves have arrived: the `execute_reply`
//! on the shell socket and the `idle` status on iopub. They travel on
//! different sockets, so either can come first; outputs are only certain to
//! have arrived once the idle status has. The kernel may already be running
//! the next run while the last one's reply is on its way, so events for a
//! later run are held back until the runs before it have finished: whoever
//! reads the events sees runs one at a time, in the order they were queued.

use crate::output::Output;
use crate::spec::{Backend, Spec};
use jupyter_protocol::{
    ConnectionInfo, ExecuteRequest, ExecutionState, InterruptRequest, JupyterMessage, JupyterMessageContent,
    KernelInfoRequest, ReplyStatus, ShutdownRequest, connection_info::Transport,
};
use jupyter_zmq_client::{ClientControlConnection, DealerSendConnection};
use std::collections::{BTreeMap, HashMap};
#[cfg(not(unix))]
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep, timeout};

/// How long a kernel may take to answer its first `kernel_info_request`.
/// A cold Python with a heavy environment can take several seconds.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a kernel asked to shut down gets before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// How much of the kernel's stderr is kept, to explain why it died.
const STDERR_KEPT: usize = 4096;
/// How many times to launch a kernel that exits while starting. The ports
/// are chosen before the kernel binds them, and another process can take one
/// in between; a fresh launch picks new ones, as Jupyter's own client does.
const START_ATTEMPTS: u32 = 3;

/// Names one call to [`Kernel::execute`]. Unique for the life of the handle,
/// across restarts.
pub type RunId = u64;

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// The code raised; the error came as an [`Output::Error`] before this.
    Error,
    /// The run never ran or was cut short: an earlier run in the queue
    /// failed, the kernel was interrupted, restarted or died.
    Aborted,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The kernel is up and answering. Sent after a start and each restart.
    Ready {
        language: String,
        implementation: String,
        banner: String,
    },
    /// The kernel began running this code.
    Started(RunId),
    Output(RunId, Output),
    /// The run is over and every output it made has arrived. `count` is the
    /// kernel's execution count (the `[3]` beside a cell), if it gave one.
    Finished {
        run: RunId,
        status: Status,
        count: Option<usize>,
    },
    /// The kernel could not start, or exited. Runs it had are finished as
    /// [`Status::Aborted`] first. [`Kernel::restart`] starts it again.
    Died(String),
}

pub(crate) enum Command {
    Execute(RunId, String),
    Interrupt,
    Restart,
    Shutdown,
}

/// A kernel process and the way to talk to it. Every method returns at once.
/// Dropping the handle asks the kernel to shut down and does not wait.
pub struct Kernel {
    spec: Spec,
    commands: UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    next_run: AtomicU64,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Kernel {
    /// Launch the kernel with `cwd` as its working directory. `wake` is
    /// called, from the worker thread, after each event is queued.
    pub fn start(spec: &Spec, cwd: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Kernel {
        let (commands, inbox) = unbounded_channel();
        let (outbox, events) = mpsc::channel();
        let emit = Emit { outbox, wake };
        let worker = {
            let spec = spec.clone();
            let cwd = cwd.to_path_buf();
            std::thread::Builder::new()
                .name(format!("kernel {}", spec.name))
                .spawn(move || match &spec.backend {
                    Backend::Jupyter(_) => work(spec, cwd, inbox, emit),
                    Backend::Runner { .. } => crate::runner::work(spec, cwd, inbox, emit),
                })
                .ok()
        };
        Kernel {
            spec: spec.clone(),
            commands,
            events,
            next_run: AtomicU64::new(1),
            worker,
        }
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Queue code to run. Runs are executed in the order queued; outputs
    /// arrive as events carrying the returned id.
    pub fn execute(&self, code: &str) -> RunId {
        let run = self.next_run.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.send(Command::Execute(run, code.to_string()));
        run
    }

    /// Stop the run in progress, as Ctrl-C would. Runs queued behind it are
    /// aborted by the kernel.
    pub fn interrupt(&self) {
        let _ = self.commands.send(Command::Interrupt);
    }

    /// Kill the kernel and start a fresh one: every variable is gone.
    pub fn restart(&self) {
        let _ = self.commands.send(Command::Restart);
    }

    /// The events that have arrived, without waiting.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// The next event, waiting up to `wait` for it.
    pub fn next_event(&self, wait: Duration) -> Option<Event> {
        self.events.recv_timeout(wait).ok()
    }

    /// Shut the kernel down and wait for it to be gone.
    pub fn shutdown(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

pub(crate) struct Emit {
    outbox: mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Emit {
    pub(crate) fn send(&self, event: Event) {
        if self.outbox.send(event).is_ok() {
            (self.wake)();
        }
    }
}

fn work(spec: Spec, cwd: PathBuf, mut inbox: UnboundedReceiver<Command>, emit: Emit) {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return emit.send(Event::Died(format!("could not start the kernel's runtime: {error}"))),
    };
    runtime.block_on(async move {
        loop {
            let mut attempt = 1;
            let launched = loop {
                match Session::launch(&spec, &cwd, &emit).await {
                    Err(failure) if failure.exited && attempt < START_ATTEMPTS => attempt += 1,
                    launched => break launched,
                }
            };
            let ended = match launched {
                Ok(mut session) => {
                    let ended = session.serve(&mut inbox, &emit).await;
                    session.stop().await;
                    ended
                }
                Err(failure) => {
                    emit.send(Event::Died(failure.why));
                    Ended::Died
                }
            };
            match ended {
                Ended::Restart => continue,
                Ended::Shutdown => return,
                Ended::Died => {
                    // Nothing to run on: abort whatever is asked of it until
                    // the writer restarts the kernel or lets it go.
                    loop {
                        match inbox.recv().await {
                            Some(Command::Restart) => break,
                            Some(Command::Execute(run, _)) => emit.send(aborted(run)),
                            Some(Command::Interrupt) => {}
                            Some(Command::Shutdown) | None => return,
                        }
                    }
                }
            }
        }
    });
}

pub(crate) fn aborted(run: RunId) -> Event {
    Event::Finished {
        run,
        status: Status::Aborted,
        count: None,
    }
}

enum Ended {
    Restart,
    Shutdown,
    Died,
}

/// Why a kernel did not start. `exited`: it quit on its own, which a
/// fresh launch may fix.
struct Failure {
    why: String,
    exited: bool,
}

impl From<String> for Failure {
    fn from(why: String) -> Failure {
        Failure { why, exited: false }
    }
}

enum Incoming {
    Shell(JupyterMessage),
    IoPub(JupyterMessage),
    Lost(String),
}

/// A run the kernel has been sent, waiting for its two halves.
#[derive(Default)]
struct Pending {
    reply: Option<(Status, Option<usize>)>,
    idle: bool,
    /// Events that came before the runs ahead of this one finished.
    held: Vec<Event>,
}

struct Session {
    /// The kernel's socket files, under IPC; a killed kernel leaves them behind.
    sockets: Vec<PathBuf>,
    child: Child,
    shell: DealerSendConnection,
    control: ClientControlConnection,
    incoming: UnboundedReceiver<Incoming>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    connection_file: PathBuf,
    stderr: Arc<Mutex<Vec<u8>>>,
    by_message: bool,
    /// Runs sent and not yet finished, oldest first.
    runs: BTreeMap<RunId, Pending>,
    /// The run each `execute_request` belongs to, by message id.
    requests: HashMap<String, RunId>,
}

impl Session {
    async fn launch(spec: &Spec, cwd: &Path, emit: &Emit) -> Result<Session, Failure> {
        let runtime_dir = jupyter_zmq_client::dirs::runtime_dir();
        tokio::fs::create_dir_all(&runtime_dir)
            .await
            .map_err(|error| format!("could not create {}: {error}", runtime_dir.display()))?;
        let name = format!("stet-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let (info, listeners) = endpoints(&runtime_dir.join(&name)).await?;
        let info = ConnectionInfo {
            signature_scheme: "hmac-sha256".to_string(),
            key: uuid::Uuid::new_v4().to_string(),
            kernel_name: Some(spec.name.clone()),
            ..info
        };
        let connection_file = runtime_dir.join(format!("kernel-{name}.json"));
        let json = serde_json::to_string(&info).map_err(|error| error.to_string())?;
        tokio::fs::write(&connection_file, json)
            .await
            .map_err(|error| format!("could not write {}: {error}", connection_file.display()))?;

        let Backend::Jupyter(dir) = &spec.backend else {
            return Err("not a Jupyter kernel".to_string().into());
        };
        let mut command = (**dir)
            .clone()
            .command(&connection_file, Some(Stdio::piped()), Some(Stdio::null()))
            .map_err(|error| error.to_string())?;
        // ipykernel watches this process and exits when it does, so a kernel
        // never outlives a stet that quit without shutting it down.
        command
            .current_dir(cwd)
            .env("JPY_PARENT_PID", std::process::id().to_string())
            .kill_on_drop(true);
        let spawned = command.spawn();
        drop(listeners);
        let mut child = spawned.map_err(|error| format!("could not run {}: {error}", spec.display_name))?;

        // Keep the end of stderr, and keep reading it so the kernel never
        // blocks on a full pipe.
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        if let Some(mut pipe) = child.stderr.take() {
            let kept = stderr.clone();
            readers.push(tokio::spawn(async move {
                let mut chunk = [0u8; 1024];
                while let Ok(read @ 1..) = pipe.read(&mut chunk).await {
                    let mut kept = kept.lock().unwrap_or_else(|poison| poison.into_inner());
                    kept.extend_from_slice(&chunk[..read]);
                    let over = kept.len().saturating_sub(STDERR_KEPT);
                    kept.drain(..over);
                }
            }));
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        // The kernel binds its ports some time after it starts, and until
        // then a connection is refused; try again until it is listening.
        let deadline = Instant::now() + START_TIMEOUT;
        let (mut iopub, shell, control) = loop {
            let connect = async {
                let iopub = jupyter_zmq_client::create_client_iopub_connection(&info, "", &session_id).await?;
                let identity = jupyter_zmq_client::peer_identity_for_session(&session_id)?;
                let shell =
                    jupyter_zmq_client::create_client_shell_connection_with_identity(&info, &session_id, identity)
                        .await?;
                let control = jupyter_zmq_client::create_client_control_connection(&info, &session_id).await?;
                Ok::<_, jupyter_zmq_client::RuntimeError>((iopub, shell, control))
            };
            let error = match connect.await {
                Ok(sockets) => break sockets,
                Err(error) => error,
            };
            let exited = matches!(child.try_wait(), Ok(Some(_)));
            if exited || Instant::now() >= deadline {
                let _ = child.kill().await;
                let _ = tokio::fs::remove_file(&connection_file).await;
                let why = if exited {
                    "the kernel exited during startup".to_string()
                } else {
                    format!("could not connect to the kernel: {error}")
                };
                sleep(Duration::from_millis(20)).await;
                let kept = stderr.lock().unwrap_or_else(|poison| poison.into_inner());
                let tail = String::from_utf8_lossy(&kept);
                return Err(Failure {
                    why: with_tail(&why, tail.trim()),
                    exited,
                });
            }
            sleep(Duration::from_millis(50)).await;
        };
        let (shell, mut shell_in) = shell.split();

        let (queue, incoming) = unbounded_channel();
        readers.push(tokio::spawn({
            let queue = queue.clone();
            async move {
                loop {
                    match shell_in.read().await {
                        Ok(message) => {
                            if queue.send(Incoming::Shell(message)).is_err() {
                                return;
                            }
                        }
                        Err(error) => return drop(queue.send(Incoming::Lost(format!("shell: {error}")))),
                    }
                }
            }
        }));
        readers.push(tokio::spawn(async move {
            loop {
                match iopub.read().await {
                    Ok(message) => {
                        if queue.send(Incoming::IoPub(message)).is_err() {
                            return;
                        }
                    }
                    Err(error) => return drop(queue.send(Incoming::Lost(format!("iopub: {error}")))),
                }
            }
        }));

        let mut session = Session {
            sockets: socket_files(&info),
            child,
            shell,
            control,
            incoming,
            readers,
            connection_file,
            stderr,
            by_message: spec.interrupts_by_message(),
            runs: BTreeMap::new(),
            requests: HashMap::new(),
        };
        match session.handshake().await {
            Ok(ready) => {
                emit.send(ready);
                Ok(session)
            }
            Err(error) => {
                let exited = matches!(session.child.try_wait(), Ok(Some(_)));
                session.stop().await;
                Err(Failure {
                    why: session.explain(&error),
                    exited,
                })
            }
        }
    }

    /// Ask the kernel who it is until it answers on both sockets. The iopub
    /// subscription takes effect some time after connecting, and anything
    /// published before then is lost; seeing the status for our own request
    /// proves that outputs will be seen too.
    async fn handshake(&mut self) -> Result<Event, String> {
        let deadline = Instant::now() + START_TIMEOUT;
        let mut ready = None;
        while Instant::now() < deadline {
            let request: JupyterMessage = KernelInfoRequest {}.into();
            let asked = request.header.msg_id.clone();
            self.shell
                .send(request)
                .await
                .map_err(|error| format!("could not ask the kernel: {error}"))?;
            let mut heard = false;
            let round = Instant::now() + Duration::from_secs(1);
            while Instant::now() < round {
                tokio::select! {
                    exit = self.child.wait() => {
                        return Err(match exit {
                            Ok(status) => format!("the kernel exited during startup ({status})"),
                            Err(error) => format!("the kernel exited during startup: {error}"),
                        });
                    }
                    message = timeout(round - Instant::now(), self.incoming.recv()) => match message {
                        Err(_) => break,
                        Ok(None) => return Err("the kernel's sockets closed".into()),
                        Ok(Some(Incoming::Lost(error))) => return Err(error),
                        Ok(Some(Incoming::Shell(message))) => {
                            if let JupyterMessageContent::KernelInfoReply(info) = message.content {
                                ready = Some(Event::Ready {
                                    language: info.language_info.name.to_lowercase(),
                                    implementation: info.implementation.clone(),
                                    banner: info.banner.clone(),
                                });
                            }
                        }
                        Ok(Some(Incoming::IoPub(message))) => {
                            heard |= message.parent_header.as_ref().is_some_and(|parent| parent.msg_id == asked);
                        }
                    },
                }
                if heard && ready.is_some() {
                    return Ok(ready.take().expect("checked"));
                }
            }
        }
        Err(format!("the kernel did not answer within {}s", START_TIMEOUT.as_secs()))
    }

    async fn serve(&mut self, inbox: &mut UnboundedReceiver<Command>, emit: &Emit) -> Ended {
        loop {
            tokio::select! {
                command = inbox.recv() => match command {
                    Some(Command::Execute(run, code)) => self.execute(run, code, emit).await,
                    Some(Command::Interrupt) => self.interrupt().await,
                    Some(Command::Restart) => {
                        self.abort_all(emit);
                        return Ended::Restart;
                    }
                    Some(Command::Shutdown) | None => {
                        self.abort_all(emit);
                        return Ended::Shutdown;
                    }
                },
                message = self.incoming.recv() => match message {
                    Some(Incoming::Shell(message)) => self.on_shell(message, emit),
                    Some(Incoming::IoPub(message)) => self.on_iopub(message, emit),
                    Some(Incoming::Lost(error)) => return self.died(&format!("lost the kernel's {error}"), emit),
                    None => return self.died("the kernel's sockets closed", emit),
                },
                exit = self.child.wait() => {
                    let why = match exit {
                        Ok(status) => format!("the kernel exited ({status})"),
                        Err(error) => format!("the kernel exited: {error}"),
                    };
                    return self.died(&why, emit);
                }
            }
        }
    }

    async fn execute(&mut self, run: RunId, code: String, emit: &Emit) {
        let request: JupyterMessage = ExecuteRequest {
            code,
            silent: false,
            store_history: true,
            user_expressions: None,
            // No `input()` prompts yet: a kernel asking for input would wait forever.
            allow_stdin: false,
            stop_on_error: true,
        }
        .into();
        let id = request.header.msg_id.clone();
        match self.shell.send(request).await {
            Ok(()) => {
                self.requests.insert(id, run);
                self.runs.insert(run, Pending::default());
            }
            Err(error) => {
                let failed = Output::Error {
                    name: "KernelError".into(),
                    value: format!("could not send the code: {error}"),
                    traceback: Vec::new(),
                };
                self.runs.insert(
                    run,
                    Pending {
                        reply: Some((Status::Aborted, None)),
                        idle: true,
                        held: vec![Event::Output(run, failed)],
                    },
                );
                self.settle(emit);
            }
        }
    }

    async fn interrupt(&mut self) {
        #[cfg(unix)]
        if !self.by_message
            && let Some(pid) = self.child.id()
        {
            // SAFETY: sends a signal to the kernel process we spawned; no memory is involved.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) };
            return;
        }
        let _ = self.control.send(InterruptRequest {}.into()).await;
    }

    fn on_shell(&mut self, message: JupyterMessage, emit: &Emit) {
        let JupyterMessageContent::ExecuteReply(reply) = &message.content else {
            return;
        };
        let Some(pending) = self.run_of(&message).and_then(|run| self.runs.get_mut(&run)) else {
            return;
        };
        let status = match reply.status {
            ReplyStatus::Ok => Status::Ok,
            ReplyStatus::Error => Status::Error,
            ReplyStatus::Aborted => Status::Aborted,
        };
        // Aborted replies carry no meaningful count.
        let count = (status != Status::Aborted && reply.execution_count.0 > 0).then_some(reply.execution_count.0);
        pending.reply = Some((status, count));
        self.settle(emit);
    }

    fn on_iopub(&mut self, message: JupyterMessage, emit: &Emit) {
        let Some(run) = self.run_of(&message) else { return };
        let event = match &message.content {
            JupyterMessageContent::Status(status) => match status.execution_state {
                ExecutionState::Busy => Event::Started(run),
                ExecutionState::Idle => {
                    if let Some(pending) = self.runs.get_mut(&run) {
                        pending.idle = true;
                    }
                    return self.settle(emit);
                }
                _ => return,
            },
            content => match Output::from_iopub(content) {
                Some(output) => Event::Output(run, output),
                None => return,
            },
        };
        match self.runs.first_key_value() {
            Some((&first, _)) if first == run => emit.send(event),
            _ => {
                if let Some(pending) = self.runs.get_mut(&run) {
                    pending.held.push(event);
                }
            }
        }
    }

    /// The unfinished run a message answers, if any.
    fn run_of(&self, message: &JupyterMessage) -> Option<RunId> {
        let parent = message.parent_header.as_ref()?;
        let run = *self.requests.get(&parent.msg_id)?;
        self.runs.contains_key(&run).then_some(run)
    }

    /// Finish the oldest run while it has both halves, then let the next one
    /// speak: send what it held, and finish it too if it is done.
    fn settle(&mut self, emit: &Emit) {
        while let Some(mut first) = self.runs.first_entry() {
            for event in first.get_mut().held.drain(..) {
                emit.send(event);
            }
            let (true, Some((status, count))) = (first.get().idle, first.get().reply) else {
                return;
            };
            let run = first.remove_entry().0;
            self.requests.retain(|_, waiting| *waiting != run);
            emit.send(Event::Finished { run, status, count });
        }
    }

    /// Finish every run still waiting, as aborted, oldest first.
    fn abort_all(&mut self, emit: &Emit) {
        for (run, pending) in std::mem::take(&mut self.runs) {
            for event in pending.held {
                emit.send(event);
            }
            emit.send(aborted(run));
        }
        self.requests.clear();
    }

    fn died(&mut self, why: &str, emit: &Emit) -> Ended {
        self.abort_all(emit);
        emit.send(Event::Died(self.explain(why)));
        Ended::Died
    }

    /// `why`, followed by the last lines the kernel wrote to stderr.
    fn explain(&self, why: &str) -> String {
        let kept = self.stderr.lock().unwrap_or_else(|poison| poison.into_inner());
        with_tail(why, String::from_utf8_lossy(&kept).trim())
    }

    /// Ask the kernel to exit, kill it if it does not, and clean up.
    async fn stop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.control.send(ShutdownRequest { restart: false }.into()).await;
            if timeout(STOP_TIMEOUT, self.child.wait()).await.is_err() {
                let _ = self.child.kill().await;
            }
        }
        // Let the stderr reader take the kernel's last words before it goes.
        sleep(Duration::from_millis(20)).await;
        for reader in self.readers.drain(..) {
            reader.abort();
        }
        let _ = tokio::fs::remove_file(&self.connection_file).await;
        for socket in &self.sockets {
            let _ = tokio::fs::remove_file(socket).await;
        }
    }
}

fn with_tail(why: &str, tail: &str) -> String {
    if tail.is_empty() {
        why.to_string()
    } else {
        format!("{why}\n{tail}")
    }
}

/// Where the kernel listens. On Unix, socket files named after `base`, so no
/// two kernels can ever want the same address. Elsewhere, five free TCP
/// ports on localhost, held by the returned listeners until the kernel is
/// spawned so nothing else takes them; drop those before the kernel binds.
#[cfg(unix)]
async fn endpoints(base: &Path) -> Result<(ConnectionInfo, Option<Vec<tokio::net::TcpListener>>), String> {
    let info = ConnectionInfo {
        transport: Transport::IPC,
        ip: base.to_string_lossy().into_owned(),
        stdin_port: 1,
        control_port: 2,
        hb_port: 3,
        shell_port: 4,
        iopub_port: 5,
        signature_scheme: String::new(),
        key: String::new(),
        kernel_name: None,
    };
    Ok((info, None))
}

#[cfg(not(unix))]
async fn endpoints(_: &Path) -> Result<(ConnectionInfo, Option<Vec<tokio::net::TcpListener>>), String> {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (ports, listeners) = jupyter_zmq_client::peek_ports_with_listeners(ip, 5)
        .await
        .map_err(|error| format!("no free ports for the kernel: {error}"))?;
    let info = ConnectionInfo {
        transport: Transport::TCP,
        ip: ip.to_string(),
        stdin_port: ports[0],
        control_port: ports[1],
        hb_port: ports[2],
        shell_port: ports[3],
        iopub_port: ports[4],
        signature_scheme: String::new(),
        key: String::new(),
        kernel_name: None,
    };
    Ok((info, Some(listeners)))
}

fn socket_files(info: &ConnectionInfo) -> Vec<PathBuf> {
    match info.transport {
        Transport::IPC => (1..=5)
            .map(|port| PathBuf::from(format!("{}-{port}", info.ip)))
            .collect(),
        Transport::TCP => Vec::new(),
    }
}
