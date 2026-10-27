//! What an assistant asks of a notebook through `stet ctl`: its cells, runs
//! (waited on or not), a cell's outputs, its kernels, and the variables a
//! Python kernel holds. Replies are JSON; outputs are Jupyter's own shapes,
//! with pictures as files the assistant can open.

use super::Kernels;
use crate::ctl::{doc_index, fail};
use crate::session::Session;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use stet_core::Editor;
use stet_core::editor::{Book, Which};
use stet_core::notebook::{self, Item, Slot, Status};

/// The `stet ctl` commands answered here.
pub const COMMANDS: &[&str] = &["cells", "run", "output", "kernel", "vars"];

/// A long reply is cut to this many characters of text per output.
const TEXT_LIMIT: usize = 4000;

/// Answers `request`, now or, for a run waited on and for `vars`, once the
/// kernel is done. `id` is the request's, so a caller who hangs up can be
/// forgotten.
pub fn request(session: &mut Session, id: u64, request: &Value, reply: Sender<String>) {
    let Session { editor, kernels, .. } = session;
    let answer = match answer(editor, kernels, id, request, &reply) {
        Ok(Some(answer)) => answer,
        Ok(None) => return,
        Err(err) => fail(err),
    };
    let _ = reply.send(answer.to_string());
}

fn answer(
    ed: &mut Editor,
    kernels: &mut Kernels,
    id: u64,
    request: &Value,
    reply: &Sender<String>,
) -> Result<Option<Value>, String> {
    let index = doc_index(ed, request)?;
    let text = |key: &str| request.get(key).and_then(Value::as_str).map(str::to_string);
    let cmd = text("cmd").unwrap_or_default();
    let in_doc = |ed: &mut Editor, work: &mut dyn FnMut(&mut Editor) -> Result<Value, String>| {
        ed.with_pane(index, false, |ed| {
            ed.refresh();
            work(ed)
        })
        .unwrap_or_else(|| Err("that document is not open".into()))
    };
    match cmd.as_str() {
        "cells" => in_doc(ed, &mut |ed| {
            let book = book(ed)?;
            let cells: Vec<Value> = (0..book.cells.len()).map(|at| cell_json(book, at, false)).collect();
            Ok(json!({ "ok": true, "notebook": book.on, "cells": cells }))
        })
        .map(Some),
        "output" => {
            let cell = text("cell").ok_or("say which cell: --cell ID or N")?;
            in_doc(ed, &mut |ed| {
                let book = book(ed)?;
                let at = book.find_cell(&cell)?;
                Ok(json!({ "ok": true, "cell": cell_json(book, at, true) }))
            })
            .map(Some)
        }
        "run" => {
            let cell = text("cell").unwrap_or_else(|| "all".into());
            let author = text("author").unwrap_or_else(|| "Claude".into());
            let wait = request.get("wait").and_then(Value::as_bool).unwrap_or(false);
            let mut ran = (0, Vec::new());
            let started = in_doc(ed, &mut |ed| {
                if ed.notebook_of(ed.doc_id()).is_none_or(|book| !book.on) {
                    ed.toggle_notebook(Some(true));
                }
                let which = match cell.as_str() {
                    "all" => Which::All,
                    "stale" => Which::Stale,
                    "here" => Which::Here,
                    "above" => Which::Above,
                    "below" => Which::Below,
                    named => Which::At(book(ed)?.find_cell(named)?),
                };
                let uids = ed.run_cells(which, Some(author.clone()))?;
                ran = (ed.doc_id(), uids.clone());
                Ok(json!({ "ok": true, "queued": uids.len() }))
            })?;
            if !wait {
                return Ok(Some(started));
            }
            kernels.watch(ed, id, ran.0, ran.1, reply.clone());
            Ok(None)
        }
        "kernel" => {
            let doc = ed.with_pane(index, false, |ed| ed.doc_id()).unwrap_or_default();
            match text("action").as_deref().unwrap_or("list") {
                "list" => Ok(Some(kernels.list(ed, doc))),
                "start" => {
                    let name = text("name").ok_or("say which kernel: stet ctl kernel start NAME")?;
                    let cwd = in_doc(ed, &mut |ed| {
                        let book = book(ed)?;
                        Ok(json!(book.path.as_deref().and_then(Path::parent)))
                    })?;
                    let cwd = cwd.as_str().map(PathBuf::from);
                    kernels.start(ed, doc, cwd, &name)?;
                    Ok(Some(json!({ "ok": true })))
                }
                "restart" => {
                    kernels.restart(ed, doc);
                    Ok(Some(json!({ "ok": true })))
                }
                "interrupt" => {
                    kernels.interrupt(doc);
                    Ok(Some(json!({ "ok": true })))
                }
                other => Err(format!(
                    "not a kernel action: {other} (list, start, restart, interrupt)"
                )),
            }
        }
        "vars" => {
            let doc = ed.with_pane(index, false, |ed| ed.doc_id()).unwrap_or_default();
            kernels.probe(id, doc, text("kernel").as_deref(), VARS, reply.clone())?;
            Ok(None)
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

fn book(ed: &Editor) -> Result<&Book, String> {
    ed.notebook_of(ed.doc_id())
        .ok_or_else(|| "the margin cannot be a notebook".to_string())
}

/// One cell: where it is, what it runs, and how its last run went. `full`
/// adds every output; otherwise a short summary of them.
pub fn cell_json(book: &Book, at: usize, full: bool) -> Value {
    let cell = &book.cells[at];
    let mut value = json!({
        "n": at + 1,
        "id": cell.info.id,
        "lang": cell.info.lang,
        "kernel": cell.info.attr("kernel"),
        "lines": [cell.fence.start + 1, cell.fence.end],
    });
    let Some((slot, stale)) = book.output_of(at) else {
        value["status"] = json!("never run");
        return value;
    };
    let fields = json!({
        "status": status_name(slot.status),
        "count": slot.count,
        "stale": stale,
        "ran_by": slot.ran_by,
        "took_ms": slot.took_ms,
    });
    for (key, field) in fields.as_object().into_iter().flatten() {
        value[key] = field.clone();
    }
    if full {
        let dir = book
            .files_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("stet-outputs"));
        value["outputs"] = outputs_json(slot, &dir);
    } else {
        value["summary"] = json!(summary(slot));
    }
    value
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Queued => "queued",
        Status::Running => "running",
        Status::Ok => "ok",
        Status::Error => "error",
        Status::Aborted => "aborted",
    }
}

/// A slot's outputs in Jupyter's shapes, with each picture written out and
/// given as a path, and bulky markup (HTML, SVG) left as its size when
/// something plainer says the same.
fn outputs_json(slot: &Slot, dir: &Path) -> Value {
    let items: Vec<Value> = slot
        .outputs
        .iter()
        .map(|item| {
            let mut value = item.to_json(slot.count);
            if let Some(data) = value.get_mut("data").and_then(Value::as_object_mut) {
                files_as_paths(data, dir);
            }
            if let Some(text) = value.get_mut("text") {
                *text = json!(clip(&plain(text.as_str().unwrap_or(""))));
            }
            if let Some(Value::Array(lines)) = value.get_mut("traceback") {
                for line in lines.iter_mut() {
                    *line = json!(plain(line.as_str().unwrap_or("")));
                }
            }
            value
        })
        .collect();
    json!(items)
}

fn files_as_paths(data: &mut Map<String, Value>, dir: &Path) {
    notebook::store_files(data, dir);
    for value in data.values_mut() {
        if let Some(name) = notebook::file_ref(value) {
            *value = json!({ "path": dir.join(name).to_string_lossy() });
        }
    }
    let plainer = data.contains_key("text/plain") || data.contains_key(stet_kernel::TABLE);
    for mime in ["text/html", "image/svg+xml"] {
        if plainer && let Some(Value::String(markup)) = data.get(mime) {
            let size = markup.len();
            data.insert(mime.into(), json!(format!("({size} bytes left out)")));
        }
    }
    if let Some(Value::String(plain)) = data.get_mut("text/plain") {
        *plain = clip(plain);
    }
}

/// Text as a terminal would show it, without its colour codes.
fn plain(text: &str) -> String {
    let lines = crate::view::outputs::plain_lines(text);
    let mut plain = lines.join("\n");
    if text.ends_with('\n') && !plain.ends_with('\n') {
        plain.push('\n');
    }
    plain
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(TEXT_LIMIT) {
        Some((cut, _)) => format!("{}… ({} more characters)", &text[..cut], text[cut..].chars().count()),
        None => text.to_string(),
    }
}

/// A line or so of what a run said, for listings.
fn summary(slot: &Slot) -> String {
    let mut parts = Vec::new();
    for item in &slot.outputs {
        match item {
            Item::Stream { text, .. } => parts.push(text.trim_end().to_string()),
            Item::Display { data } | Item::Result { data } => {
                if data.keys().any(|mime| mime.starts_with("image/")) {
                    parts.push("[picture]".into());
                } else if data.contains_key(stet_kernel::TABLE) {
                    let total = data[stet_kernel::TABLE]["total"].as_u64().unwrap_or(0);
                    parts.push(format!("[table, {total} rows]"));
                } else if let Some(plain) = data.get("text/plain").and_then(Value::as_str) {
                    parts.push(plain.trim_end().to_string());
                }
            }
            Item::Error { name, value, .. } => parts.push(format!("{name}: {value}")),
        }
    }
    let joined = parts.join("\n");
    match joined.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &joined[..cut]),
        None => joined,
    }
}

/// Asked of a Python kernel by `vars`: the writer's variables, as one line
/// of JSON. Names are kept out of the kernel's namespace.
const VARS: &str = r#"def __stet_vars():
    import json
    shell = get_ipython()
    hidden = shell.user_ns_hidden
    skip = ('module', 'function', 'type', 'builtin_function_or_method', 'method')
    found = []
    for name, value in list(shell.user_ns.items()):
        if name.startswith('_') or name in hidden or type(value).__name__ in skip:
            continue
        try:
            shape = getattr(value, 'shape', None)
            size = list(shape) if shape is not None else len(value)
        except Exception:
            size = None
        try:
            text = repr(value)
        except Exception as error:
            text = '<repr failed: %s>' % error
        if len(text) > 160:
            text = text[:157] + '...'
        found.append({'name': name, 'type': type(value).__name__, 'size': size, 'repr': text})
    print(json.dumps(found, default=str))
__stet_vars()
del __stet_vars"#;
