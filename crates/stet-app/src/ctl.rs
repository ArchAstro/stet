//! `stet ctl`: talking to the running window from another program. One JSON
//! request per line over the local socket, one JSON reply.
//!
//! The client half turns a command line into a request; the server half
//! runs a request against the live editor on the UI thread.

use crate::session::Session as Window;
use crate::{images, platform, retouch};
use serde_json::{Value, json};
use stet_core::Editor;
use stet_core::editor::{RemoteEdit, Session, Target};

pub const USAGE: &str = r#"stet ctl <command>   control the running stet (replies are JSON)

  sessions                       list the open documents
  read [--doc D] [--lines A-B]   the live text (unsaved changes included), cursor, selection, suggestions
  suggest [--doc D] <target> ... propose an edit as a suggestion the writer accepts or rejects
  edit    [--doc D] <target> ... change the text directly
  wait [--name NAME]             connect as an assistant: blocks until the writer sends a message from
                                 stet (Cmd/Ctrl-Shift-A or ga), prints it with their selection, and exits
  say --text TEXT                show one line in stet's status bar
  open <file>...                 open files as tabs
  command <command>              run an editor command, as typed after `:` (theme nord, sidebar, w)
  keys <notation>                press keys, in vim notation (ggVG, <D-/>, <Esc>)
  type <text>                    type text as the keyboard would
  click X Y [right]              click at a point in the window (points from the top left)
  drag X Y TO_X TO_Y             press at one point, move to another, and let go
  shot <file.png>                save a picture of what the window shows
  images [--doc D]               list the document's pictures: where each file is, and its size in pixels
  annotate [--doc D] [--image N] --ops JSON
                                 draw on a picture, crop it, or hide part of it; the result is kept
                                 beside the original and the text refers to it (undo brings it back).
                                 --preview FILE writes the result there and leaves the document alone
  call                           send one JSON request read from stdin

targets (one of):
  --old TEXT --new TEXT          replace TEXT, which must occur exactly once (or add --occurrence N)
  --at cursor|end --text TEXT    insert at the writer's cursor, or append
  --line N --text TEXT           insert before line N
  --range REV:START-END --text TEXT
                                 replace chars START..END as they were at revision REV; typing since is followed

marks for --ops, a JSON list; positions are in the picture's own pixels, from its top left:
  {"tool":"arrow","from":[x,y],"to":[x,y]}       {"tool":"rect","x":..,"y":..,"w":..,"h":..}
  {"tool":"ellipse","x":..,"y":..,"w":..,"h":..}  {"tool":"text","at":[x,y],"text":"..."}
  {"tool":"highlight","x":..,"y":..,"w":..,"h":..} {"tool":"pen","points":[[x,y],...]}
  {"tool":"redact","x":..,"y":..,"w":..,"h":..}   {"tool":"crop","x":..,"y":..,"w":..,"h":..}
  each takes "color" (red, yellow, green, blue, black, white, #rrggbb) and "size"
  (stroke width, or the height of text, in pixels); both have defaults that suit the picture

  --doc D        tab number, file name or path (default: the document in front)
  --margin       read or write the document's margin (its scratch pane for research) instead of its text
  --author NAME  whose suggestion it is (default: Claude)
  A value of @FILE is read from FILE, and - from stdin."#;

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
    // `margin` addresses the document's scratch pane instead of its text.
    let margin = request.get("margin").and_then(Value::as_bool).unwrap_or(false);
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
            let Some(snapshot) = editor.snapshot(index, margin) else {
                return (fail("that document has no margin until it is saved"), false);
            };
            let lines: Vec<&str> = snapshot.text.split('\n').collect();
            let from = number("from_line").unwrap_or(1).clamp(1, lines.len());
            let to = number("to_line").unwrap_or(lines.len()).clamp(from, lines.len());
            let offset: usize = lines[..from - 1].iter().map(|line| line.chars().count() + 1).sum();
            let cursor_line = snapshot.session.cursor.0 + 1;
            let suggestions = editor
                .with_pane(index, margin, |ed| {
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
                })
                .unwrap_or_default();
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
            match editor.remote_edit(index, margin, &edit) {
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

/// `images` and `annotate`: the document's pictures, and drawing on one.
/// These need the window's fonts and its cache of downloads, so they run
/// against the session rather than the editor alone.
pub fn pictures(window: &mut Window, request: &Value) -> Value {
    let editor = &mut window.editor;
    let doc = match request.get("doc") {
        None | Some(Value::Null) => Ok(editor.active),
        Some(Value::Number(tab)) => editor.find_session(&tab.to_string()),
        Some(Value::String(spec)) => editor.find_session(spec),
        Some(_) => Err("`doc` is a tab number, file name or path".to_string()),
    };
    let index = match doc {
        Ok(index) => index,
        Err(err) => return fail(err),
    };
    let margin = request.get("margin").and_then(Value::as_bool).unwrap_or(false);
    let listed = editor.with_pane(index, margin, |ed| {
        ed.refresh();
        (ed.doc().images.clone(), ed.path.clone(), ed.config.remote_images)
    });
    let Some((found, path, remote)) = listed else {
        return fail("that document has no margin until it is saved");
    };
    let source = |url: &str| images::resolve(url, path.as_deref(), remote);
    let file = |url: &str| match source(url) {
        Some(images::Source::File(file)) => Some(file),
        _ => None,
    };
    if request["cmd"] == "images" {
        let list: Vec<Value> = found
            .iter()
            .enumerate()
            .map(|(at, picture)| {
                let file = file(&picture.url);
                let video = source(&picture.url).is_some_and(|source| source.video().is_some());
                let size = file
                    .as_ref()
                    .filter(|_| !video)
                    .and_then(|file| image::image_dimensions(file).ok());
                json!({
                    "image": at + 1,
                    "line": picture.line + 1,
                    "url": picture.url,
                    "path": file.map(|file| file.to_string_lossy().into_owned()),
                    "width": size.map(|size| size.0),
                    "height": size.map(|size| size.1),
                    "video": video,
                })
            })
            .collect();
        return json!({ "ok": true, "images": list });
    }

    // Which picture: its number in the list, or how the text refers to it.
    let wanted = match &request["image"] {
        Value::Null if found.len() == 1 => Some(0),
        Value::Number(number) => number.as_u64().and_then(|number| (number as usize).checked_sub(1)),
        Value::String(name) => found
            .iter()
            .position(|picture| picture.url == *name)
            .or_else(|| found.iter().position(|picture| picture.url.ends_with(name.as_str()))),
        _ => None,
    };
    let Some(picture) = wanted.and_then(|at| found.get(at)) else {
        return fail(format!(
            "name the picture with `image`: its number (1 to {}) from `stet ctl images`, or its url",
            found.len()
        ));
    };
    let ops = match &request["ops"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
        other => other.clone(),
    };
    let drawn = source(&picture.url)
        .filter(|source| source.video().is_none())
        .and_then(|source| window.view.images.bytes(&source))
        .ok_or(format!("{} cannot be read as a picture", picture.url))
        .and_then(|bytes| retouch::decode(&bytes))
        .and_then(|base| {
            let marks = retouch::parse(&ops, base.width(), base.height())?;
            if marks == retouch::Marks::default() {
                return Err("`ops` has no marks in it".to_string());
            }
            let result = retouch::render(&base, &marks, true, &mut window.view.fonts);
            Ok((retouch::encode(&result)?, result.width(), result.height()))
        });
    let (png, width, height) = match drawn {
        Ok(drawn) => drawn,
        Err(err) => return fail(err),
    };
    if let Some(preview) = request["preview"].as_str() {
        return match std::fs::write(preview, &png) {
            Ok(()) => json!({ "ok": true, "preview": preview, "width": width, "height": height }),
            Err(err) => fail(format!("{preview}: {err}")),
        };
    }
    let kept = window
        .editor
        .with_pane(index, margin, |ed| ed.replace_image(picture.line, &picture.url, &png));
    match kept {
        Some(Ok(link)) => json!({
            "ok": true,
            "link": link,
            "path": file(&link).map(|file| file.to_string_lossy().into_owned()),
            "was": picture.url,
            "line": picture.line + 1,
            "width": width,
            "height": height,
        }),
        Some(Err(err)) => fail(err),
        None => fail("that document has no margin until it is saved"),
    }
}

/// What an assistant receives when the writer sends it a message: the
/// request, and where they were when they wrote it.
pub fn message(editor: &mut Editor, text: &str, selection: Option<(std::ops::Range<usize>, String)>) -> Value {
    let in_margin = editor.in_margin();
    let snapshot = editor
        .snapshot(editor.active, in_margin)
        .expect("the pane with the keyboard exists");
    let line = snapshot.session.cursor.0;
    json!({
        "ok": true,
        "message": text,
        "session": session_json(&snapshot.session),
        "revision": snapshot.session.revision,
        "cursor": { "line": line + 1, "column": snapshot.session.cursor.1 + 1, "offset": snapshot.cursor },
        "line_text": snapshot.text.split('\n').nth(line).unwrap_or(""),
        "in_margin": in_margin,
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
            "--margin" => {
                request.insert("margin".into(), json!(true));
            }
            "--doc" => {
                let doc = next("--doc")?;
                request.insert("doc".into(), doc.parse::<u64>().map_or(json!(doc), |tab| json!(tab)));
            }
            "--old" | "--new" | "--text" | "--author" | "--at" | "--name" => {
                request.insert(arg[2..].to_string(), json!(value(next(arg)?)?));
            }
            "--ops" => {
                let ops: Value =
                    serde_json::from_str(&value(next(arg)?)?).map_err(|err| format!("--ops is not JSON: {err}"))?;
                request.insert("ops".into(), ops);
            }
            "--image" => {
                let image = next(arg)?;
                request.insert("image".into(), image.parse::<u64>().map_or(json!(image), |n| json!(n)));
            }
            "--preview" => {
                let file = next(arg)?;
                let file = std::path::absolute(file).map_or(file.clone(), |path| path.to_string_lossy().into_owned());
                request.insert("preview".into(), json!(file));
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
            "right" if command == "click" => {
                request.insert("button".into(), json!("right"));
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            file => files.push(file.to_string()),
        }
    }
    let cmd = match command.as_str() {
        "sessions" | "read" | "wait" | "say" | "images" | "annotate" => command.as_str(),
        "command" | "keys" | "type" => {
            let field = if command == "type" { "text" } else { command.as_str() };
            request.insert(field.into(), json!(files.join(" ")));
            command.as_str()
        }
        "click" => {
            let point: Vec<f64> = files.iter().filter_map(|value| value.parse().ok()).collect();
            let [x, y] = point[..] else {
                return Err("click expects X Y".to_string());
            };
            request.insert("x".into(), json!(x));
            request.insert("y".into(), json!(y));
            "click"
        }
        "drag" => {
            let points: Vec<f64> = files.iter().filter_map(|value| value.parse().ok()).collect();
            let [x, y, to_x, to_y] = points[..] else {
                return Err("drag expects X Y TO_X TO_Y".to_string());
            };
            for (key, value) in [("x", x), ("y", y), ("to_x", to_x), ("to_y", to_y)] {
                request.insert(key.into(), json!(value));
            }
            "drag"
        }
        "shot" => {
            let path = files.first().ok_or("shot expects a file to write")?;
            let path = std::path::absolute(path).map_or(path.clone(), |path| path.to_string_lossy().into_owned());
            request.insert("path".into(), json!(path));
            "shot"
        }
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
