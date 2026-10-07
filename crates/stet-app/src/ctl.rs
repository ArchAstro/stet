//! `stet ctl`: talking to the running window from another program. One JSON
//! request per line over the local socket, one JSON reply.
//!
//! The client half turns a command line into a request; the server half
//! runs a request against the live editor on the UI thread.

use crate::platform;
use serde_json::{Value, json};
use stet_core::Editor;
use stet_core::editor::{RemoteEdit, Session, Target};

pub const USAGE: &str = "stet ctl <command>   control the running stet (replies are JSON)

  sessions                       list the open documents
  read [--doc D] [--lines A-B]   the live text (unsaved changes included), cursor, selection, suggestions
  suggest [--doc D] <target> ... propose an edit as a suggestion the writer accepts or rejects
  edit    [--doc D] <target> ... change the text directly
  wait [--name NAME]             connect as an assistant: blocks until the writer sends a message from
                                 stet (Cmd/Ctrl-Shift-A or ga), prints it with their selection, and exits
  say --text TEXT                show one line in stet's status bar
  open <file>...                 open files as tabs
  call                           send one JSON request read from stdin

targets (one of):
  --old TEXT --new TEXT          replace TEXT, which must occur exactly once (or add --occurrence N)
  --at cursor|end --text TEXT    insert at the writer's cursor, or append
  --line N --text TEXT           insert before line N
  --range REV:START-END --text TEXT
                                 replace chars START..END as they were at revision REV; typing since is followed

  --doc D        tab number, file name or path (default: the document in front)
  --author NAME  whose suggestion it is (default: Claude)
  A value of @FILE is read from FILE, and - from stdin.";

fn session_json(session: &Session) -> Value {
    json!({
        "tab": session.index + 1,
        "path": session.path.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "title": session.title,
        "active": session.active,
        "unsaved": session.dirty,
        "lines": session.lines,
        "words": session.words,
        "revision": session.revision,
        "cursor": { "line": session.cursor.0 + 1, "column": session.cursor.1 + 1 },
        "suggestions": session.suggestions,
    })
}

fn fail(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}

/// Runs one request against the editor. Returns the reply, and whether the
/// window should come forward.
pub fn handle(editor: &mut Editor, request: &str) -> (Value, bool) {
    let Ok(request) = serde_json::from_str::<Value>(request) else {
        return (fail("the request is not JSON"), false);
    };
    let text = |key: &str| request.get(key).and_then(Value::as_str);
    let number = |key: &str| request.get(key).and_then(Value::as_u64).map(|value| value as usize);
    let doc = match request.get("doc") {
        None | Some(Value::Null) => Ok(editor.active),
        Some(Value::Number(tab)) => editor.find_session(&tab.to_string()),
        Some(Value::String(spec)) => editor.find_session(spec),
        Some(_) => Err("`doc` is a tab number, file name or path".to_string()),
    };
    match text("cmd").unwrap_or("") {
        "open" => {
            let files = request
                .get("files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for file in files.iter().filter_map(Value::as_str) {
                if let Err(err) = editor.open_in_tab(std::path::Path::new(file)) {
                    return (fail(err), true);
                }
            }
            (json!({ "ok": true }), true)
        }
        "sessions" => {
            let sessions: Vec<Value> = editor.sessions().iter().map(session_json).collect();
            (json!({ "ok": true, "sessions": sessions }), false)
        }
        "read" => {
            let index = match doc {
                Ok(index) => index,
                Err(err) => return (fail(err), false),
            };
            let snapshot = editor.snapshot(index);
            let lines: Vec<&str> = snapshot.text.split('\n').collect();
            let from = number("from_line").unwrap_or(1).clamp(1, lines.len());
            let to = number("to_line").unwrap_or(lines.len()).clamp(from, lines.len());
            let offset: usize = lines[..from - 1].iter().map(|line| line.chars().count() + 1).sum();
            let cursor_line = snapshot.session.cursor.0 + 1;
            let suggestions = editor.with_session(index, |ed| {
                ed.refresh();
                ed.doc()
                    .suggestions
                    .iter()
                    .map(|suggestion| {
                        json!({
                            "id": suggestion.id,
                            "author": suggestion.author,
                            "kind": format!("{:?}", suggestion.kind).to_lowercase(),
                            "old": suggestion.old_text,
                            "new": suggestion.new_text,
                            "line": ed.buf.line_of(suggestion.span.start) + 1,
                        })
                    })
                    .collect::<Vec<Value>>()
            });
            let reply = json!({
                "ok": true,
                "session": session_json(&snapshot.session),
                "revision": snapshot.session.revision,
                "from_line": from,
                "to_line": to,
                "offset": offset,
                "text": lines[from - 1..to].join("\n"),
                "cursor": { "line": cursor_line, "column": snapshot.session.cursor.1 + 1, "offset": snapshot.cursor },
                "selection": snapshot.selection.map(|(range, text)| json!({ "start": range.start, "end": range.end, "text": text })),
                "suggestions": suggestions,
            });
            (reply, false)
        }
        "edit" => {
            let index = match doc {
                Ok(index) => index,
                Err(err) => return (fail(err), false),
            };
            let target = if let Some(old) = text("old") {
                Target::Text {
                    old: old.to_string(),
                    occurrence: number("occurrence"),
                }
            } else if let (Some(revision), Some(start), Some(end)) = (
                request.get("base").and_then(Value::as_u64),
                number("start"),
                number("end"),
            ) {
                Target::Range {
                    revision,
                    range: start..end.max(start),
                }
            } else if let Some(line) = number("line") {
                Target::Line(line.saturating_sub(1))
            } else {
                match text("at") {
                    Some("cursor") => Target::Cursor,
                    Some("end") => Target::End,
                    _ => {
                        return (
                            fail(
                                "name a target: `old`, `at` (cursor or end), `line`, or `base` with `start` and `end`",
                            ),
                            false,
                        );
                    }
                }
            };
            let Some(new) = text("new").or(text("text")) else {
                return (fail("`new` (or `text`) is missing"), false);
            };
            let edit = RemoteEdit {
                target,
                text: new.to_string(),
                suggest: text("mode") != Some("direct"),
                author: text("author").unwrap_or("Claude").to_string(),
            };
            match editor.remote_edit(index, &edit) {
                Ok(applied) => (
                    json!({ "ok": true, "revision": applied.revision, "line": applied.line + 1, "suggested": edit.suggest }),
                    false,
                ),
                Err(err) => (fail(err), false),
            }
        }
        "say" => match text("text").map(str::trim).filter(|text| !text.is_empty()) {
            Some(said) => {
                let author = text("author").unwrap_or("Claude");
                let line = said.lines().next().unwrap_or("");
                editor.message = Some(stet_core::editor::Message {
                    text: format!("{author}: {line}"),
                    error: false,
                });
                (json!({ "ok": true }), false)
            }
            None => (fail("`text` is missing"), false),
        },
        other => (fail(format!("unknown command `{other}`")), false),
    }
}

/// What an assistant receives when the writer sends it a message: the
/// request, and where they were when they wrote it.
pub fn message(editor: &mut Editor, text: &str, selection: Option<(std::ops::Range<usize>, String)>) -> Value {
    let snapshot = editor.snapshot(editor.active);
    let line = snapshot.session.cursor.0;
    json!({
        "ok": true,
        "message": text,
        "session": session_json(&snapshot.session),
        "revision": snapshot.session.revision,
        "cursor": { "line": line + 1, "column": snapshot.session.cursor.1 + 1, "offset": snapshot.cursor },
        "line_text": snapshot.text.split('\n').nth(line).unwrap_or(""),
        "selection": selection.map(|(range, text)| json!({ "start": range.start, "end": range.end, "text": text })),
    })
}

fn value(raw: &str) -> Result<String, String> {
    use std::io::Read;
    if raw == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| err.to_string())?;
        return Ok(text);
    }
    match raw.strip_prefix('@') {
        Some(file) => std::fs::read_to_string(file).map_err(|err| format!("{file}: {err}")),
        None => Ok(raw.to_string()),
    }
}

fn build(args: &[String]) -> Result<Value, String> {
    let (command, rest) = args.split_first().ok_or(String::new())?;
    let mut request = serde_json::Map::new();
    let mut files = Vec::new();
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        let mut next = |name: &str| rest.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--doc" => {
                let doc = next("--doc")?;
                request.insert("doc".into(), doc.parse::<u64>().map_or(json!(doc), |tab| json!(tab)));
            }
            "--old" | "--new" | "--text" | "--author" | "--at" | "--name" => {
                request.insert(arg[2..].to_string(), json!(value(next(arg)?)?));
            }
            "--occurrence" | "--line" => {
                let number: u64 = next(arg)?.parse().map_err(|_| format!("{arg} expects a number"))?;
                request.insert(arg[2..].to_string(), json!(number));
            }
            "--lines" => {
                let lines = next("--lines")?;
                let (from, to) = lines.split_once('-').unwrap_or((lines, lines));
                let parse = |text: &str| text.parse::<u64>().map_err(|_| "--lines expects A-B".to_string());
                request.insert("from_line".into(), json!(parse(from)?));
                request.insert("to_line".into(), json!(parse(to)?));
            }
            "--range" => {
                let range = next("--range")?;
                let parsed = range.split_once(':').and_then(|(base, span)| {
                    let (start, end) = span.split_once('-')?;
                    Some((
                        base.parse::<u64>().ok()?,
                        start.parse::<u64>().ok()?,
                        end.parse::<u64>().ok()?,
                    ))
                });
                let (base, start, end) = parsed.ok_or("--range expects REV:START-END")?;
                request.insert("base".into(), json!(base));
                request.insert("start".into(), json!(start));
                request.insert("end".into(), json!(end));
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            file => files.push(file.to_string()),
        }
    }
    let cmd = match command.as_str() {
        "sessions" | "read" | "wait" | "say" => command.as_str(),
        "suggest" | "edit" => {
            request.insert(
                "mode".into(),
                json!(if command == "edit" { "direct" } else { "suggest" }),
            );
            "edit"
        }
        "open" => {
            let absolute: Vec<String> = files
                .iter()
                .map(|file| std::path::absolute(file).map_or(file.clone(), |path| path.to_string_lossy().into_owned()))
                .collect();
            request.insert("files".into(), json!(absolute));
            "open"
        }
        "call" => return serde_json::from_str(&value("-")?).map_err(|err| format!("stdin is not JSON: {err}")),
        other => return Err(format!("unknown command `{other}`")),
    };
    request.insert("cmd".into(), json!(cmd));
    Ok(Value::Object(request))
}

/// The `stet ctl` command line. Returns the process exit code.
pub fn cli(args: &[String]) -> i32 {
    let request = match build(args) {
        Ok(request) => request,
        Err(err) => {
            if !err.is_empty() {
                eprintln!("stet ctl: {err}\n");
            }
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let Some(reply) = platform::request(&request.to_string(), request["cmd"] == "wait") else {
        println!("{}", fail("stet is not running; start it with `stet <file>`"));
        return 1;
    };
    let parsed: Value = serde_json::from_str(&reply).unwrap_or_else(|_| fail("stet sent a reply that is not JSON"));
    println!("{}", serde_json::to_string_pretty(&parsed).unwrap_or(reply));
    if parsed["ok"] == json!(true) { 0 } else { 1 }
}
