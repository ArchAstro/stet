//! Runners: a program on the `PATH` that is handed each cell on stdin and
//! runs it on its own. Nothing carries from one run to the next, so there is
//! nothing to start: a runner is ready at once, and restarting it only
//! clears its queue.
//!
//! Runs go one at a time, in the order queued, with the same events a
//! Jupyter kernel gives: `Started`, its output as it is printed, then
//! `Finished`. A run that exits non-zero ends with an [`Output::Error`].

use crate::kernel::{Command, Emit, Event, RunId, Status, aborted};
use crate::output::{Output, table};
use crate::spec::{Backend, Shape, Spec};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;

/// How long an interrupted run gets to stop before it is killed.
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
/// How long to keep reading output after the program exits. Something it
/// started in the background can hold its pipes open long after.
const DRAIN: Duration = Duration::from_millis(300);

pub(crate) fn work(spec: Spec, cwd: PathBuf, mut inbox: UnboundedReceiver<Command>, emit: Emit) {
    let Backend::Runner { argv, output } = spec.backend.clone() else {
        return;
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return emit.send(Event::Died(format!("could not start the runner: {error}"))),
    };
    let ready = Event::Ready {
        language: spec.language.clone(),
        implementation: spec.name.clone(),
        banner: String::new(),
    };
    runtime.block_on(async move {
        emit.send(ready.clone());
        let mut queue: VecDeque<(RunId, String)> = VecDeque::new();
        let mut count = 0;
        loop {
            let (run, code) = match queue.pop_front() {
                Some(next) => next,
                None => match inbox.recv().await {
                    Some(Command::Execute(run, code, _)) => (run, code),
                    Some(Command::Interrupt) => continue,
                    Some(Command::Restart) => {
                        emit.send(ready.clone());
                        continue;
                    }
                    Some(Command::Shutdown) | None => return,
                },
            };
            count += 1;
            let ended = execute(&argv, output, &cwd, run, &code, count, &mut inbox, &mut queue, &emit).await;
            match ended {
                Next::Continue => {}
                Next::Abort => {
                    for (run, _) in queue.drain(..) {
                        emit.send(aborted(run));
                    }
                }
                Next::Restart => {
                    for (run, _) in queue.drain(..) {
                        emit.send(aborted(run));
                    }
                    count = 0;
                    emit.send(ready.clone());
                }
                Next::Shutdown => {
                    for (run, _) in queue.drain(..) {
                        emit.send(aborted(run));
                    }
                    return;
                }
            }
        }
    });
}

/// What to do once a run is over.
enum Next {
    Continue,
    /// The run failed or was interrupted: drop the queue behind it, as a
    /// kernel does.
    Abort,
    Restart,
    Shutdown,
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    argv: &[String],
    shape: Shape,
    cwd: &std::path::Path,
    run: RunId,
    code: &str,
    count: usize,
    inbox: &mut UnboundedReceiver<Command>,
    queue: &mut VecDeque<(RunId, String)>,
    emit: &Emit,
) -> Next {
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own process group, so an interrupt reaches everything the cell
    // started (`sleep` under `sh`), as Ctrl-C in a terminal does.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            emit.send(Event::Output(
                run,
                failure("RunnerError", format!("could not run {}: {error}", argv[0])),
            ));
            emit.send(Event::Finished {
                run,
                status: Status::Error,
                count: None,
                took: None,
            });
            return Next::Abort;
        }
    };
    let pid = child.id();
    let began = std::time::Instant::now();
    emit.send(Event::Started(run));

    // The code goes in on its own task, so a program that prints a lot
    // before reading all its input cannot deadlock against us.
    if let Some(mut stdin) = child.stdin.take() {
        let code = code.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(code.as_bytes()).await;
        });
    }
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut rows = String::new();
    let (mut out_buf, mut err_buf) = ([0u8; 4096], [0u8; 4096]);
    let mut deadline = None;
    let mut interrupted = false;
    let mut after = Next::Continue;
    let mut exit = None;

    let exit = loop {
        if let Some(status) = exit.take_if(|_| stdout.is_none() && stderr.is_none()) {
            break status;
        }
        tokio::select! {
            read = read_some(&mut stdout, &mut out_buf) => match read {
                Some(text) if shape == Shape::Rows => rows.push_str(&text),
                Some(text) => emit.send(Event::Output(run, Output::Stream { stderr: false, text })),
                None => stdout = None,
            },
            read = read_some(&mut stderr, &mut err_buf) => match read {
                Some(text) => emit.send(Event::Output(run, Output::Stream { stderr: true, text })),
                None => stderr = None,
            },
            command = inbox.recv(), if matches!(after, Next::Continue) => match command {
                Some(Command::Execute(next, code, _)) => queue.push_back((next, code)),
                Some(Command::Interrupt) => {
                    signal(&mut child, pid, Signal::Interrupt);
                    interrupted = true;
                    deadline = Some(tokio::time::Instant::now() + INTERRUPT_GRACE);
                }
                Some(Command::Restart) => {
                    signal(&mut child, pid, Signal::Kill);
                    after = Next::Restart;
                }
                Some(Command::Shutdown) | None => {
                    signal(&mut child, pid, Signal::Kill);
                    after = Next::Shutdown;
                }
            },
            _ = sleep_until(deadline) => {
                deadline = None;
                match exit {
                    // Exited, and the pipes are still open: stop reading.
                    Some(_) => (stdout, stderr) = (None, None),
                    None => signal(&mut child, pid, Signal::Kill),
                }
            }
            status = child.wait(), if exit.is_none() => {
                exit = Some(status);
                deadline = Some(tokio::time::Instant::now() + DRAIN);
                // Whatever the cell left running goes with it.
                signal(&mut child, pid, Signal::Kill);
            }
        }
    };

    if !rows.is_empty() {
        for output in tables(&rows) {
            emit.send(Event::Output(run, output));
        }
    }
    let status = match (&after, exit) {
        (Next::Restart | Next::Shutdown, _) => Status::Aborted,
        (_, Ok(exit)) if exit.success() => Status::Ok,
        (_, Ok(exit)) => {
            let (name, value) = if interrupted || killed_by_interrupt(&exit) {
                ("KeyboardInterrupt", "interrupted".to_string())
            } else {
                ("ExitStatus", format!("{} exited with {exit}", argv[0]))
            };
            emit.send(Event::Output(run, failure(name, value)));
            Status::Error
        }
        (_, Err(error)) => {
            emit.send(Event::Output(run, failure("RunnerError", error.to_string())));
            Status::Error
        }
    };
    emit.send(Event::Finished {
        run,
        status,
        count: (status != Status::Aborted).then_some(count),
        took: Some(began.elapsed()),
    });
    match (after, status) {
        (Next::Continue, Status::Ok) => Next::Continue,
        (Next::Continue, _) => Next::Abort,
        (after, _) => after,
    }
}

/// The next chunk from a pipe, or `None` at its end. A closed pipe waits forever.
async fn read_some<R: AsyncReadExt + Unpin>(pipe: &mut Option<R>, buf: &mut [u8]) -> Option<String> {
    let Some(reader) = pipe else {
        return std::future::pending().await;
    };
    match reader.read(buf).await {
        Ok(0) | Err(_) => None,
        Ok(read) => Some(String::from_utf8_lossy(&buf[..read]).into_owned()),
    }
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Interrupt,
    Kill,
}

/// Signal the run's whole process group: the program and anything it
/// started. `pid` is the program's, kept from the start: once it has been
/// waited for, the child no longer knows it.
fn signal(child: &mut tokio::process::Child, pid: Option<u32>, signal: Signal) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let _ = child;
        let number = match signal {
            Signal::Interrupt => libc::SIGINT,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: signals the process group we created for this run; no memory is involved.
        unsafe { libc::kill(-(pid as libc::pid_t), number) };
    }
    #[cfg(not(unix))]
    if let Signal::Kill = signal {
        let _ = pid;
        let _ = child.start_kill();
    }
}

fn killed_by_interrupt(exit: &std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        exit.signal() == Some(libc::SIGINT) || exit.code() == Some(130)
    }
    #[cfg(not(unix))]
    {
        let _ = exit;
        false
    }
}

fn failure(name: &str, value: String) -> Output {
    Output::Error {
        name: name.to_string(),
        value,
        traceback: Vec::new(),
    }
}

/// A SQL runner's output: one JSON array of row objects per statement that
/// returned rows, with columns in the order the program wrote them.
/// Anything that is not such an array is passed on as text.
fn tables(text: &str) -> Vec<Output> {
    let mut outputs = Vec::new();
    let mut statements = serde_json::Deserializer::from_str(text).into_iter::<Vec<Row>>();
    for rows in statements.by_ref() {
        let Ok(rows) = rows else { break };
        let columns: Vec<String> = rows
            .first()
            .map(|row| row.0.iter().map(|(name, _)| name.clone()).collect())
            .unwrap_or_default();
        let rows = rows
            .into_iter()
            .map(|row| {
                let mut cells: Vec<Value> = vec![Value::Null; columns.len()];
                for (name, value) in row.0 {
                    if let Some(i) = columns.iter().position(|column| *column == name) {
                        cells[i] = value;
                    }
                }
                cells
            })
            .collect();
        outputs.push(Output::Display(table(columns, rows)));
    }
    let rest = text[statements.byte_offset()..].trim_start();
    if !rest.is_empty() {
        outputs.push(Output::Stream {
            stderr: false,
            text: rest.to_string(),
        });
    }
    outputs
}

/// A JSON object, keeping its keys in order.
struct Row(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for Row {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Row, D::Error> {
        struct Fields;
        impl<'de> serde::de::Visitor<'de> for Fields {
            type Value = Row;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a row object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<Row, M::Error> {
                let mut fields = Vec::new();
                while let Some(field) = map.next_entry::<String, Value>()? {
                    fields.push(field);
                }
                Ok(Row(fields))
            }
        }
        deserializer.deserialize_map(Fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rows_become_tables_and_the_rest_stays_text() {
        let outputs = tables("[{\"z\":1,\"a\":\"x\"}]\n[{\"n\":2}]\nError: nope\n");
        assert_eq!(outputs.len(), 3);
        let Output::Display(first) = &outputs[0] else { panic!() };
        assert_eq!(first[crate::output::TABLE]["columns"], serde_json::json!(["z", "a"]));
        assert_eq!(first[crate::output::TABLE]["rows"], serde_json::json!([[1, "x"]]));
        assert_eq!(
            outputs[2],
            Output::Stream {
                stderr: false,
                text: "Error: nope\n".into()
            }
        );
    }
}
