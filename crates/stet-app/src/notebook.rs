//! The kernels behind notebook mode. Each document gets its own kernel per
//! language, started when one of its cells first runs, so two notebooks
//! never share variables. Runs come in as [`stet_core::editor::Effect::Run`]s; what the
//! kernels say goes back to the editor by document and slot.
//!
//! Finding what is installed means reading kernel directories and asking
//! `jupyter`, which can take a moment, so it happens on a thread at start.
//! Runs asked for before it is done wait for it.

pub mod agent;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use stet_core::Editor;
use stet_core::editor::{CellRun, KernelState, RemoteEdit, Target};
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
    /// Quiet runs an assistant asked for, and what they printed so far.
    probes: HashMap<RunId, Probe>,
    /// The theme a Python kernel last drew in, by name.
    theme: String,
    /// The data refs a Python kernel was last given: name to file.
    refs: Value,
    /// When the refs of a document not in front were last looked at.
    refs_checked: Option<std::time::Instant>,
}

struct Probe {
    request: u64,
    reply: Sender<String>,
    printed: String,
    failed: Option<String>,
}

/// An assistant waiting for runs to finish.
struct Watch {
    request: u64,
    doc: u64,
    uids: Vec<u64>,
    reply: Sender<String>,
}

pub struct Kernels {
    wake: Wake,
    specs: Option<Vec<Spec>>,
    found: Arc<Mutex<Option<Vec<Spec>>>>,
    started: Vec<Started>,
    /// Runs asked for before the kernels were found.
    waiting: Vec<(u64, Option<PathBuf>, CellRun)>,
    watches: Vec<Watch>,
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
            watches: Vec::new(),
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
        let spec = spec.clone();
        let started = self.kernel_for(ed, doc, cwd, &spec);
        let id = started.kernel.execute(&run.code);
        started.runs.insert(id, run.uid);
    }

    /// The document's kernel for `spec`, started if it is not yet.
    fn kernel_for(&mut self, ed: &mut Editor, doc: u64, cwd: Option<PathBuf>, spec: &Spec) -> &mut Started {
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
                    probes: HashMap::new(),
                    theme: String::new(),
                    refs: Value::Null,
                    refs_checked: None,
                });
                let started = self.started.last_mut().expect("just pushed");
                boot(ed, started);
                self.started.len() - 1
            }
        };
        &mut self.started[at]
    }

    /// `stet ctl kernel start NAME`: start a kernel for the document before
    /// any of its cells run.
    pub fn start(&mut self, ed: &mut Editor, doc: u64, cwd: Option<PathBuf>, name: &str) -> Result<(), String> {
        let specs = self
            .specs
            .as_deref()
            .ok_or("still looking for kernels; ask again in a moment")?;
        let spec = stet_kernel::find(specs, name)
            .ok_or_else(|| format!("no kernel or runner is called {name} (stet ctl kernel list)"))?
            .clone();
        self.kernel_for(ed, doc, cwd, &spec);
        self.publish_state(ed, doc);
        Ok(())
    }

    /// `stet ctl kernel list`: what can run here, and what runs for the document.
    pub fn list(&self, ed: &Editor, doc: u64) -> Value {
        let installed: Option<Vec<Value>> = self.specs.as_ref().map(|specs| {
            specs
                .iter()
                .map(|spec| {
                    json!({
                        "name": spec.name,
                        "display_name": spec.display_name,
                        "language": spec.language,
                        "keeps_state": spec.keeps_state,
                    })
                })
                .collect()
        });
        let running: Vec<Value> = ed
            .notebook_of(doc)
            .map(|book| book.kernels.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|state| {
                let status = match (&state.dead, state.starting, state.busy) {
                    (Some(_), ..) => "stopped",
                    (None, true, _) => "starting",
                    (None, false, true) => "busy",
                    (None, false, false) => "idle",
                };
                json!({ "name": state.name, "status": status, "why": state.dead })
            })
            .collect();
        json!({ "ok": true, "installed": installed, "running": running })
    }

    /// Answer `reply` once every run in `uids` has finished, with their cells.
    pub fn watch(&mut self, ed: &Editor, request: u64, doc: u64, uids: Vec<u64>, reply: Sender<String>) {
        self.watches.push(Watch {
            request,
            doc,
            uids,
            reply,
        });
        self.answer_watches(ed);
    }

    fn answer_watches(&mut self, ed: &Editor) {
        self.watches.retain(|watch| {
            let Some(book) = ed.notebook_of(watch.doc) else {
                let _ = watch
                    .reply
                    .send(crate::ctl::fail("the document was closed").to_string());
                return false;
            };
            let done = |uid: &u64| book.notebook.slot(*uid).is_none_or(|slot| slot.status.is_done());
            if !watch.uids.iter().all(done) {
                return true;
            }
            // The cells the runs belong to, as they now stand.
            let cells: Vec<Value> = (0..book.cells.len())
                .filter(|&at| {
                    book.output_of(at)
                        .is_some_and(|(slot, _)| watch.uids.contains(&slot.uid))
                })
                .map(|at| agent::cell_json(book, at, true))
                .collect();
            let failed = cells.iter().filter(|cell| cell["status"] != "ok").count();
            let answer = json!({ "ok": true, "ran": cells.len(), "failed": failed, "cells": cells });
            let _ = watch.reply.send(answer.to_string());
            false
        });
    }

    /// Run `code` quietly on the document's kernel for `language` (or the
    /// one named), and answer `reply` with the JSON it prints last.
    pub fn probe(
        &mut self,
        request: u64,
        doc: u64,
        name: Option<&str>,
        code: &str,
        reply: Sender<String>,
    ) -> Result<(), String> {
        let started = self
            .started
            .iter_mut()
            .filter(|started| started.doc == doc && started.kernel.spec().keeps_state)
            .find(|started| {
                let spec = started.kernel.spec();
                match name {
                    Some(name) => spec.name == name,
                    None => spec.language.eq_ignore_ascii_case("python"),
                }
            })
            .ok_or("no Python kernel is running for this document; run one of its cells first")?;
        let run = started.kernel.execute_quietly(code);
        started.probes.insert(
            run,
            Probe {
                request,
                reply,
                printed: String::new(),
                failed: None,
            },
        );
        Ok(())
    }

    /// The caller of request `id` hung up: stop waiting on its behalf.
    pub fn forget(&mut self, id: u64) {
        self.watches.retain(|watch| watch.request != id);
        for started in &mut self.started {
            started.probes.retain(|_, probe| probe.request != id);
        }
    }

    pub fn interrupt(&mut self, doc: u64) {
        for started in self.started.iter().filter(|started| started.doc == doc) {
            started.kernel.interrupt();
        }
    }

    pub fn restart(&mut self, ed: &mut Editor, doc: u64) {
        for started in self.started.iter_mut().filter(|started| started.doc == doc) {
            started.kernel.restart();
            boot(ed, started);
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
        // A Python kernel draws in the editor's theme; tell it of a new one.
        for started in self
            .started
            .iter_mut()
            .filter(|started| is_python(started.kernel.spec()))
        {
            let mut args = serde_json::Map::new();
            if started.theme != ed.theme.name {
                started.theme = ed.theme.name.clone();
                args.insert("theme".into(), theme_json(ed));
            }
            // And of the data refs its document names, as they change. A
            // document behind another tab is looked at once a second.
            let front = ed.doc_id() == started.doc && !ed.in_margin();
            let due = started
                .refs_checked
                .is_none_or(|checked| checked.elapsed() >= std::time::Duration::from_secs(1));
            if !front && due {
                started.refs_checked = Some(std::time::Instant::now());
            }
            if let Some(refs) = (front || due).then(|| refs_json(ed, started.doc)).flatten()
                && refs != started.refs
            {
                started.refs = refs.clone();
                args.insert("refs".into(), refs);
            }
            if !args.is_empty() {
                let args = Value::Object(args).to_string();
                started.kernel.execute_quietly(&format!(
                    "__import__('stet')._attach(**__import__('json').loads({}))",
                    json!(args)
                ));
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
        if changed {
            self.answer_watches(ed);
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
    if let Some(answer) = probed(started, &event) {
        return answer;
    }
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
            if let Output::Display(data) = &output
                && let Some(published) = data.get(PUBLISH)
            {
                offer_ref(ed, doc, published);
            }
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
            for (_, probe) in started.probes.drain() {
                let _ = probe
                    .reply
                    .send(crate::ctl::fail(format!("the kernel stopped: {why}")).to_string());
            }
            started.state.starting = false;
            started.state.dead = Some(why.lines().next().unwrap_or("the kernel stopped").to_string());
        }
    }
}

/// The module Python kernels get: Stet's formatters, theme and data refs.
const STET_PY: &str = include_str!("../python/stet/__init__.py");

fn is_python(spec: &Spec) -> bool {
    spec.keeps_state && spec.language.eq_ignore_ascii_case("python")
}

/// A chart's look, from the editor's theme: series in the theme's accents.
pub fn chart_theme(ed: &Editor, size: f32) -> stet_chart::Theme {
    let (c, p) = (ed.theme.colors, ed.theme.palette);
    let rgb = |rgb: stet_core::theme::Rgb| [rgb.0, rgb.1, rgb.2];
    stet_chart::Theme {
        background: rgb(c.background),
        text: rgb(c.text),
        muted: rgb(c.muted),
        grid: rgb(c.text.mix(c.background, 0.1)),
        series: [p.blue, p.peach, p.green, p.mauve, p.red, p.teal, p.yellow, p.pink]
            .into_iter()
            .map(rgb)
            .collect(),
        // Named faces: the system UI font (San Francisco) is a variable
        // font that draws nothing in an SVG.
        font: "'Helvetica Neue', Helvetica, 'Segoe UI', Arial, 'DejaVu Sans', sans-serif".into(),
        size: size.round().max(9.0),
    }
}

/// The theme as the kernel's `stet` module takes it: colours as `#rrggbb`.
fn theme_json(ed: &Editor) -> Value {
    let theme = chart_theme(ed, 11.0);
    let hex = |color: stet_chart::Color| format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
    json!({
        "background": hex(theme.background),
        "text": hex(theme.text),
        "muted": hex(theme.muted),
        "grid": hex(theme.grid),
        "series": theme.series.into_iter().map(hex).collect::<Vec<_>>(),
    })
}

/// Give a Python kernel the `stet` module, before anything else runs on it:
/// the one installed if it is Stet's, else the one built in.
fn boot(ed: &mut Editor, started: &mut Started) {
    if !is_python(started.kernel.spec()) {
        return;
    }
    started.theme = ed.theme.name.clone();
    let root = ed.notebook_of(started.doc).and_then(|book| {
        book.path
            .as_deref()?
            .parent()
            .map(|dir| dir.to_string_lossy().into_owned())
    });
    let refs = refs_json(ed, started.doc).unwrap_or(Value::Null);
    started.refs = refs.clone();
    let args = json!({ "theme": theme_json(ed), "root": root, "refs": refs }).to_string();
    // JSON strings are Python strings too.
    let code = format!(
        r#"def __stet_boot(source, args):
    import json, sys, types
    module = sys.modules.get("stet")
    if module is None or not hasattr(module, "_attach"):
        module = types.ModuleType("stet")
        module.__file__ = "<stet>"
        sys.modules["stet"] = module
        exec(compile(source, "<stet>", "exec"), module.__dict__)
    module._attach(**json.loads(args))
__stet_boot({}, {})
del __stet_boot"#,
        json!(STET_PY),
        json!(args)
    );
    started.kernel.execute_quietly(&code);
}

/// What `stet.publish` shows: `{name, path, rows}`.
const PUBLISH: &str = "application/vnd.stet.publish+json";

/// The data refs of document `doc` as its kernel takes them, a name to a
/// file. `None` if the document is not open.
fn refs_json(ed: &mut Editor, doc: u64) -> Option<Value> {
    let read = |ed: &mut Editor| {
        ed.refresh();
        let refs: serde_json::Map<String, Value> = ed
            .doc()
            .data_refs
            .iter()
            .map(|data| {
                let file = crate::data::resolve(&data.path, ed.path.as_deref());
                (data.name.clone(), json!(file))
            })
            .collect();
        Value::Object(refs)
    };
    if ed.doc_id() == doc && !ed.in_margin() {
        return Some(read(ed));
    }
    let index = doc_index_of(ed, doc)?;
    ed.with_pane(index, false, read)
}

/// The tab that holds document `doc`.
fn doc_index_of(ed: &mut Editor, doc: u64) -> Option<usize> {
    (0..ed.tab_count()).find(|&index| ed.with_pane(index, false, |ed| ed.doc_id()) == Some(doc))
}

/// A kernel published a table: offer its document a ref to it, as a
/// suggestion to accept. Nothing if the document already names it there.
fn offer_ref(ed: &mut Editor, doc: u64, published: &Value) {
    let (Some(name), Some(path)) = (published["name"].as_str(), published["path"].as_str()) else {
        return;
    };
    let Some(index) = doc_index_of(ed, doc) else {
        return;
    };
    let placed = ed.with_pane(index, false, |ed| {
        ed.refresh();
        if ed.doc().data_ref(name).is_some_and(|data| data.path == path) {
            return None;
        }
        // Beside the other refs; else at the end, after a blank line, as a
        // definition cannot follow a paragraph.
        Some(match ed.doc().data_refs.last() {
            Some(last) => (Target::Line(last.line + 1), format!("[{name}]: {path}\n")),
            None => {
                let count = ed.buf.line_count();
                let filled = |line: usize| !ed.buf.line_text(line).trim().is_empty();
                let blank = if filled(count - 1) {
                    "\n\n"
                } else if count >= 2 && filled(count - 2) {
                    "\n"
                } else {
                    ""
                };
                (Target::End, format!("{blank}[{name}]: {path}\n"))
            }
        })
    });
    let Some(Some((target, text))) = placed else { return };
    let edit = RemoteEdit {
        target,
        text,
        suggest: true,
        author: "stet.publish".into(),
    };
    if let Err(err) = ed.remote_edit(index, false, &edit) {
        ed.message = Some(stet_core::editor::Message {
            text: format!("publish {name}: {err}"),
            error: true,
        });
    }
}

/// Takes an event that belongs to a probe. `Some` if it did.
fn probed(started: &mut Started, event: &Event) -> Option<()> {
    let run = match event {
        Event::Started(run) | Event::Output(run, _) | Event::Finished { run, .. } => *run,
        _ => return None,
    };
    let probe = started.probes.get_mut(&run)?;
    match event {
        Event::Output(_, Output::Stream { stderr: false, text }) => probe.printed.push_str(text),
        Event::Output(_, Output::Error { name, value, .. }) => probe.failed = Some(format!("{name}: {value}")),
        Event::Finished { .. } => {
            let probe = started.probes.remove(&run)?;
            let last = probe.printed.lines().last().unwrap_or("");
            let answer = match (probe.failed, serde_json::from_str::<Value>(last)) {
                (Some(failed), _) => crate::ctl::fail(failed),
                (None, Ok(vars)) => json!({ "ok": true, "vars": vars }),
                (None, Err(_)) => crate::ctl::fail("the kernel's answer was not JSON"),
            };
            let _ = probe.reply.send(answer.to_string());
        }
        _ => {}
    }
    Some(())
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
