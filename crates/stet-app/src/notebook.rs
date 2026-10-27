//! The kernels behind notebook mode. Each document gets its own kernel per
//! language, started when one of its cells first runs, so two notebooks
//! never share variables. Runs come in as [`Effect::Run`]s; what the
//! kernels say goes back to the editor by document and slot.
//!
//! Finding what is installed means reading kernel directories and asking
//! `jupyter`, which can take a moment, so it happens on a thread at start.
//! Runs asked for before it is done wait for it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use stet_core::Editor;
use stet_core::editor::{CellRun, KernelState};
use stet_core::notebook::{Item, Status};
use stet_kernel::{Event, Kernel, Output, RunId, Spec};

type Wake = Arc<dyn Fn() + Send + Sync>;

/// A kernel started for one document.
struct Started {
    doc: u64,
    kernel: Kernel,
    state: KernelState,
    /// Which slot each of its runs reports to.
    runs: HashMap<RunId, u64>,
}

pub struct Kernels {
    wake: Wake,
    specs: Option<Vec<Spec>>,
    found: Arc<Mutex<Option<Vec<Spec>>>>,
    started: Vec<Started>,
    /// Runs asked for before the kernels were found.
    waiting: Vec<(u64, Option<PathBuf>, CellRun)>,
}

impl Kernels {
    pub fn new(wake: Wake) -> Kernels {
        let found = Arc::new(Mutex::new(None));
        {
            let found = found.clone();
            let wake = wake.clone();
            let _ = std::thread::Builder::new().name("find kernels".into()).spawn(move || {
                let specs = stet_kernel::kernelspecs();
                *found.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(specs);
                wake();
            });
        }
        Kernels {
            wake,
            specs: None,
            found,
            started: Vec::new(),
            waiting: Vec::new(),
        }
    }

    pub fn run(&mut self, ed: &mut Editor, doc: u64, cwd: Option<PathBuf>, runs: Vec<CellRun>) {
        for run in runs {
            match &self.specs {
                None => self.waiting.push((doc, cwd.clone(), run)),
                Some(_) => self.start_run(ed, doc, cwd.clone(), run),
            }
        }
        self.publish_state(ed, doc);
    }

    fn start_run(&mut self, ed: &mut Editor, doc: u64, cwd: Option<PathBuf>, run: CellRun) {
        let specs = self.specs.as_deref().unwrap_or_default();
        let wanted = run.kernel.as_deref().unwrap_or(&run.lang);
        let Some(spec) = stet_kernel::find(specs, wanted) else {
            ed.notebook_output(
                doc,
                run.uid,
                Item::Error {
                    name: "NoKernel".into(),
                    value: format!(
                        "nothing on this machine runs {wanted}: install a Jupyter kernel or put a runner on the PATH"
                    ),
                    traceback: Vec::new(),
                },
            );
            return ed.notebook_finished(doc, run.uid, Status::Error, None, None);
        };
        let at = match self
            .started
            .iter()
            .position(|started| started.doc == doc && started.kernel.spec().name == spec.name)
        {
            Some(at) => at,
            None => {
                let cwd = cwd
                    .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                    .unwrap_or_default();
                self.started.push(Started {
                    doc,
                    kernel: Kernel::start(spec, &cwd, self.wake.clone()),
                    state: KernelState {
                        name: spec.name.clone(),
                        busy: false,
                        starting: spec.keeps_state,
                        dead: None,
                    },
                    runs: HashMap::new(),
                });
                self.started.len() - 1
            }
        };
        let started = &mut self.started[at];
        let id = started.kernel.execute(&run.code);
        started.runs.insert(id, run.uid);
    }

    pub fn interrupt(&mut self, doc: u64) {
        for started in self.started.iter().filter(|started| started.doc == doc) {
            started.kernel.interrupt();
        }
    }

    pub fn restart(&mut self, ed: &mut Editor, doc: u64) {
        for started in self.started.iter_mut().filter(|started| started.doc == doc) {
            started.kernel.restart();
            started.state.starting = started.kernel.spec().keeps_state;
            started.state.dead = None;
        }
        self.publish_state(ed, doc);
    }

    /// Take in what the kernels said since last time. True if anything did.
    pub fn poll(&mut self, ed: &mut Editor) -> bool {
        let mut changed = false;
        if self.specs.is_none() {
            let found = self.found.lock().unwrap_or_else(|poison| poison.into_inner()).take();
            if let Some(specs) = found {
                let mut tags: Vec<String> = specs.iter().flat_map(Spec::tags).collect();
                tags.extend(specs.iter().map(|spec| spec.name.to_lowercase()));
                ed.set_runnable(Some(tags));
                self.specs = Some(specs);
                for (doc, cwd, run) in std::mem::take(&mut self.waiting) {
                    self.start_run(ed, doc, cwd, run);
                }
                changed = true;
            }
        }
        for closed in ed.closed_notebooks() {
            // Dropping a kernel asks it to shut down.
            self.started.retain(|started| started.doc != closed);
        }
        let mut touched = Vec::new();
        for started in &mut self.started {
            let events = started.kernel.poll();
            if events.is_empty() {
                continue;
            }
            changed = true;
            touched.push(started.doc);
            for event in events {
                apply(ed, started, event);
            }
        }
        touched.dedup();
        for doc in touched {
            self.publish_state(ed, doc);
        }
        changed
    }

    /// Wait, up to `limit`, until every run asked for has finished. For
    /// screenshots, which have no event loop to wait in.
    pub fn settle(&mut self, ed: &mut Editor, limit: std::time::Duration) {
        let deadline = std::time::Instant::now() + limit;
        loop {
            self.poll(ed);
            let busy = ed.doc_ids().into_iter().any(|doc| {
                ed.notebook_of(doc)
                    .is_some_and(|book| book.notebook.busy().next().is_some())
            });
            if (!busy && self.specs.is_some()) || std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn publish_state(&self, ed: &mut Editor, doc: u64) {
        let states = self
            .started
            .iter()
            .filter(|started| started.doc == doc)
            .map(|started| KernelState {
                busy: !started.runs.is_empty(),
                ..started.state.clone()
            })
            .collect();
        ed.set_kernels(doc, states);
    }
}

fn apply(ed: &mut Editor, started: &mut Started, event: Event) {
    let doc = started.doc;
    match event {
        Event::Ready { .. } => {
            started.state.starting = false;
            started.state.dead = None;
        }
        Event::Started(run) => {
            if let Some(&uid) = started.runs.get(&run) {
                ed.notebook_started(doc, uid);
            }
        }
        Event::Output(run, output) => {
            let Some(&uid) = started.runs.get(&run) else { return };
            match output {
                Output::Clear { wait } => ed.notebook_clear(doc, uid, wait),
                output => ed.notebook_output(doc, uid, item(output)),
            }
        }
        Event::Finished {
            run,
            status,
            count,
            took,
        } => {
            if let Some(uid) = started.runs.remove(&run) {
                let status = match status {
                    stet_kernel::Status::Ok => Status::Ok,
                    stet_kernel::Status::Error => Status::Error,
                    stet_kernel::Status::Aborted => Status::Aborted,
                };
                ed.notebook_finished(doc, uid, status, count, took.map(|took| took.as_millis() as u64));
            }
        }
        Event::Died(why) => {
            started.state.starting = false;
            started.state.dead = Some(why.lines().next().unwrap_or("the kernel stopped").to_string());
        }
    }
}

fn item(output: Output) -> Item {
    match output {
        Output::Stream { stderr, text } => Item::Stream { stderr, text },
        Output::Display(data) => Item::Display { data },
        Output::Result(data) => Item::Result { data },
        Output::Error { name, value, traceback } => Item::Error { name, value, traceback },
        Output::Clear { .. } => unreachable!("clears are applied, not kept"),
    }
}
