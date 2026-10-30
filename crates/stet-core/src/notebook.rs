//! Notebook mode's model: which fenced blocks are cells, what each one last
//! produced, and where that is kept.
//!
//! A cell is a top-level fenced code block. Its info string names the
//! language and, Pandoc style, optional attributes:
//! ```text
//! ```python {#weekly kernel=duckdb .hide}
//! ```
//! Whether a language can run is the shell's business (it knows the
//! kernels); this module only reads the document.
//!
//! What a run produced is kept in a [`Slot`]. Slots are not stored in the
//! document, so they are matched to cells again after every edit:
//! 1. a cell with an `#id` takes the slot with that id;
//! 2. otherwise a slot whose code is the cell's exact code;
//! 3. otherwise the slot at the same position, for the same language,
//!    which is then *stale*: the code changed since it ran.
//!
//! Slots persist beside the document as `.stet/<name>.out.json`, with each
//! output in Jupyter's own JSON form (`stream`, `display_data`,
//! `execute_result`, `error`), so a notebook converts to `.ipynb` as is.
//! Pictures are written to `.stet/<name>.out/` as files and referred to by
//! name, so the JSON stays small and the picture loader reads them like any
//! other image.

use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

/// The parts of a fence's info string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// Lower case: `python`. Empty for a bare fence.
    pub lang: String,
    pub id: Option<String>,
    /// `.name` flags, without the dot.
    pub classes: Vec<String>,
    /// `key=value` pairs, quotes removed.
    pub attrs: Vec<(String, String)>,
}

impl Info {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
    pub fn has_class(&self, class: &str) -> bool {
        self.classes.iter().any(|name| name == class)
    }
}

/// Reads `python {#id .class key=value}`, `{.python #id}` and plain `python`.
pub fn parse_info(info: &str) -> Info {
    let info = info.trim();
    let (head, braces) = match info.find('{') {
        Some(open) => {
            let close = info[open..].rfind('}').map_or(info.len(), |at| open + at);
            (&info[..open], &info[open + 1..close])
        }
        None => (info, ""),
    };
    let mut parsed = Info {
        lang: head.split_whitespace().next().unwrap_or("").to_lowercase(),
        ..Info::default()
    };
    for word in split_attrs(braces) {
        if let Some(id) = word.strip_prefix('#') {
            parsed.id = Some(id.to_string());
        } else if let Some(class) = word.strip_prefix('.') {
            if parsed.lang.is_empty() {
                parsed.lang = class.to_lowercase();
            } else {
                parsed.classes.push(class.to_string());
            }
        } else if let Some((key, value)) = word.split_once('=') {
            parsed
                .attrs
                .push((key.to_string(), value.trim_matches('"').to_string()));
        } else if parsed.lang.is_empty() {
            parsed.lang = word.to_lowercase();
        }
    }
    parsed
}

/// Words of an attribute list; a quoted value may hold spaces.
fn split_attrs(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                word.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// One fenced block of the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Lines from the opening fence to the closing one, both included.
    pub fence: Range<usize>,
    pub info: Info,
    /// The text between the fences.
    pub code: String,
    /// [`code_hash`] of `code`.
    pub hash: String,
}

impl Cell {
    /// The line the cell's output is shown below: its closing fence.
    pub fn last_line(&self) -> usize {
        self.fence.end.saturating_sub(1)
    }

    /// What a fold remembers the cell by: `#id` when it has one, otherwise
    /// its code, so a fold follows the cell when lines move around it.
    pub fn fold_key(&self) -> String {
        match &self.info.id {
            Some(id) => format!("#{id}"),
            None => self.hash.clone(),
        }
    }

    /// The lines a fold hides: the code and the closing fence. The opening
    /// fence stays as the cell's summary, and outputs stay under it.
    pub fn folded_lines(&self) -> Range<usize> {
        (self.fence.start + 1).min(self.fence.end)..self.fence.end
    }
}

/// Every top-level fenced block, given the document's lines and the fence
/// line ranges from its analysis ([`crate::markdown::Doc::fences`]).
pub fn cells(lines: &[&str], fences: &[Range<usize>]) -> Vec<Cell> {
    fences
        .iter()
        .filter_map(|fence| {
            let opening = lines.get(fence.start)?.trim_start();
            let marker = opening.chars().next().filter(|c| *c == '`' || *c == '~')?;
            let ticks = opening.chars().take_while(|c| *c == marker).count();
            let info = parse_info(&opening[ticks..]);
            let last = fence.end.min(lines.len());
            let closed = last > fence.start + 1
                && lines[last - 1]
                    .trim_start()
                    .starts_with(&marker.to_string().repeat(ticks))
                && lines[last - 1].trim().chars().all(|c| c == marker);
            let body_end = if closed { last - 1 } else { last };
            let code = lines[fence.start + 1..body_end.max(fence.start + 1)].join("\n");
            Some(Cell {
                fence: fence.clone(),
                hash: code_hash(&code),
                info,
                code,
            })
        })
        .collect()
}

/// A stable digest of a cell's code: FNV-1a, in hex. Trailing whitespace on
/// the whole block does not count.
pub fn code_hash(code: &str) -> String {
    let hash = code.trim_end().bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
    Ok,
    Error,
    /// Never ran, or was cut short.
    Aborted,
}

impl Status {
    pub fn is_done(self) -> bool {
        matches!(self, Status::Ok | Status::Error | Status::Aborted)
    }
    fn name(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Ok => "ok",
            Status::Error => "error",
            Status::Aborted => "aborted",
        }
    }
    fn from_name(name: &str) -> Status {
        match name {
            "ok" => Status::Ok,
            "error" => Status::Error,
            // A run that was going when the file was written did not finish.
            _ => Status::Aborted,
        }
    }
}

/// One output, as Jupyter shapes it. `data` maps MIME types to values;
/// a picture's value may be `{"$file": "name.png"}`, a file in the
/// notebook's output folder.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Stream {
        stderr: bool,
        text: String,
    },
    Display {
        data: Map<String, Value>,
    },
    Result {
        data: Map<String, Value>,
    },
    Error {
        name: String,
        value: String,
        traceback: Vec<String>,
    },
}

impl Item {
    pub fn to_json(&self, count: Option<usize>) -> Value {
        match self {
            Item::Stream { stderr, text } => json!({
                "output_type": "stream",
                "name": if *stderr { "stderr" } else { "stdout" },
                "text": text,
            }),
            Item::Display { data } => json!({ "output_type": "display_data", "data": data, "metadata": {} }),
            Item::Result { data } => json!({
                "output_type": "execute_result",
                "execution_count": count,
                "data": data,
                "metadata": {},
            }),
            Item::Error { name, value, traceback } => json!({
                "output_type": "error",
                "ename": name,
                "evalue": value,
                "traceback": traceback,
            }),
        }
    }

    pub fn from_json(value: &Value) -> Option<Item> {
        let text = |value: &Value| match value {
            Value::String(text) => text.clone(),
            // nbformat allows text as a list of lines.
            Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
            _ => String::new(),
        };
        // Text kept as a list of lines, as nbformat may, is joined: other
        // types (JSON) can be lists in their own right.
        let data = |value: &Value| {
            let mut data = value
                .get("data")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (mime, value) in data.iter_mut() {
                if (mime.starts_with("text/") || mime == "image/svg+xml") && value.is_array() {
                    *value = Value::String(text(value));
                }
            }
            data
        };
        Some(match value.get("output_type")?.as_str()? {
            "stream" => Item::Stream {
                stderr: value.get("name").and_then(Value::as_str) == Some("stderr"),
                text: text(value.get("text")?),
            },
            "display_data" | "update_display_data" => Item::Display { data: data(value) },
            "execute_result" => Item::Result { data: data(value) },
            "error" => Item::Error {
                name: value.get("ename").map(text).unwrap_or_default(),
                value: value.get("evalue").map(text).unwrap_or_default(),
                traceback: value
                    .get("traceback")
                    .and_then(Value::as_array)
                    .map(|lines| lines.iter().map(text).collect())
                    .unwrap_or_default(),
            },
            _ => return None,
        })
    }
}

/// What one cell last produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// Names the slot for the life of the notebook in memory: runs report
    /// to it by this, however the document is edited meanwhile.
    pub uid: u64,
    pub id: Option<String>,
    /// The hash of the code that ran.
    pub hash: String,
    /// The cell's position among the document's cells when last matched.
    pub index: usize,
    pub lang: String,
    pub status: Status,
    /// The kernel's execution count: the `[3]` beside a cell.
    pub count: Option<usize>,
    pub outputs: Vec<Item>,
    /// Who asked for the run: `None` for the writer, else the assistant's name.
    pub ran_by: Option<String>,
    /// When the run started and how long it took, in milliseconds.
    pub started_ms: Option<u64>,
    pub took_ms: Option<u64>,
    /// A `clear_output(wait=True)` is waiting for the next output.
    clear_on_next: bool,
}

/// How a cell stands against its slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// Index into [`Notebook::slots`].
    pub slot: usize,
    /// The code changed since the slot's run.
    pub stale: bool,
}

/// One document's runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notebook {
    pub slots: Vec<Slot>,
    /// [`Cell::fold_key`]s of the cells shown folded.
    pub folded: BTreeSet<String>,
    next_uid: u64,
    /// Changed since last written.
    pub dirty: bool,
}

impl Notebook {
    /// Pairs each cell with its slot, if it has one (see the module docs).
    pub fn matching(&self, cells: &[Cell]) -> Vec<Option<Match>> {
        let mut found: Vec<Option<Match>> = vec![None; cells.len()];
        let mut taken: HashSet<usize> = HashSet::new();
        let take = |found: &mut Vec<Option<Match>>, taken: &mut HashSet<usize>, cell: usize, slot: usize| {
            taken.insert(slot);
            found[cell] = Some(Match {
                slot,
                stale: self.slots[slot].hash != cells[cell].hash,
            });
        };
        for (at, cell) in cells.iter().enumerate() {
            let Some(id) = &cell.info.id else { continue };
            if let Some(slot) = self.slots.iter().position(|slot| slot.id.as_ref() == Some(id)) {
                take(&mut found, &mut taken, at, slot);
            }
        }
        for (at, cell) in cells.iter().enumerate() {
            if found[at].is_some() || cell.info.id.is_some() {
                continue;
            }
            let free = |slot: &(usize, &Slot)| !taken.contains(&slot.0) && slot.1.id.is_none();
            let same_code = self
                .slots
                .iter()
                .enumerate()
                .filter(free)
                .filter(|(_, slot)| slot.hash == cell.hash);
            let best = same_code
                .min_by_key(|(_, slot)| slot.index.abs_diff(at))
                .map(|(slot, _)| slot);
            if let Some(slot) = best {
                take(&mut found, &mut taken, at, slot);
            }
        }
        for (at, cell) in cells.iter().enumerate() {
            if found[at].is_some() || cell.info.id.is_some() {
                continue;
            }
            let slot = self.slots.iter().enumerate().position(|(slot, candidate)| {
                !taken.contains(&slot)
                    && candidate.id.is_none()
                    && candidate.index == at
                    && candidate.lang == cell.info.lang
            });
            if let Some(slot) = slot {
                take(&mut found, &mut taken, at, slot);
            }
        }
        found
    }

    /// Keep each matched slot's position current, so position matching
    /// follows cells as others are added or removed above them.
    pub fn settle(&mut self, matches: &[Option<Match>]) {
        for (at, found) in matches.iter().enumerate() {
            if let Some(found) = found {
                self.slots[found.slot].index = at;
            }
        }
    }

    /// Ready the slot for a new run of cell `at`: its old outputs go, and it
    /// is queued. Returns the slot's uid, which the run reports to.
    pub fn queue(&mut self, cells: &[Cell], at: usize, ran_by: Option<String>) -> u64 {
        let cell = &cells[at];
        let slot = match self.matching(cells)[at] {
            Some(found) => found.slot,
            None => {
                self.next_uid += 1;
                self.slots.push(Slot {
                    uid: self.next_uid,
                    id: None,
                    hash: String::new(),
                    index: at,
                    lang: String::new(),
                    status: Status::Queued,
                    count: None,
                    outputs: Vec::new(),
                    ran_by: None,
                    started_ms: None,
                    took_ms: None,
                    clear_on_next: false,
                });
                self.slots.len() - 1
            }
        };
        let slot = &mut self.slots[slot];
        slot.id = cell.info.id.clone();
        slot.hash = cell.hash.clone();
        slot.index = at;
        slot.lang = cell.info.lang.clone();
        slot.status = Status::Queued;
        slot.count = None;
        slot.outputs.clear();
        slot.ran_by = ran_by;
        slot.started_ms = None;
        slot.took_ms = None;
        slot.clear_on_next = false;
        self.dirty = true;
        slot.uid
    }

    pub fn slot(&self, uid: u64) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.uid == uid)
    }
    fn slot_mut(&mut self, uid: u64) -> Option<&mut Slot> {
        self.dirty = true;
        self.slots.iter_mut().find(|slot| slot.uid == uid)
    }

    pub fn started(&mut self, uid: u64, now_ms: u64) {
        if let Some(slot) = self.slot_mut(uid) {
            slot.status = Status::Running;
            slot.started_ms = Some(now_ms);
        }
    }

    /// Add an output. Text printed in pieces joins the stream before it.
    pub fn output(&mut self, uid: u64, item: Item) {
        let Some(slot) = self.slot_mut(uid) else { return };
        if std::mem::take(&mut slot.clear_on_next) {
            slot.outputs.clear();
        }
        if let (
            Item::Stream { stderr, text },
            Some(Item::Stream {
                stderr: last,
                text: joined,
            }),
        ) = (&item, slot.outputs.last_mut())
            && stderr == last
        {
            joined.push_str(text);
            return;
        }
        slot.outputs.push(item);
    }

    /// `clear_output`: now, or with `wait`, when the next output arrives.
    pub fn clear(&mut self, uid: u64, wait: bool) {
        let Some(slot) = self.slot_mut(uid) else { return };
        if wait {
            slot.clear_on_next = true;
        } else {
            slot.outputs.clear();
        }
    }

    pub fn finished(&mut self, uid: u64, status: Status, count: Option<usize>, now_ms: u64) {
        let Some(slot) = self.slot_mut(uid) else { return };
        slot.status = status;
        slot.count = count;
        slot.took_ms = slot.started_ms.map(|started| now_ms.saturating_sub(started));
        slot.clear_on_next = false;
    }

    /// Set how long a run took, as measured where it ran.
    pub fn took(&mut self, uid: u64, ms: u64) {
        if let Some(slot) = self.slot_mut(uid) {
            slot.took_ms = Some(ms);
        }
    }

    /// Runs that have not finished.
    pub fn busy(&self) -> impl Iterator<Item = &Slot> {
        self.slots.iter().filter(|slot| !slot.status.is_done())
    }

    /// The notebook as `.out.json` holds it. Slots no cell matches are left
    /// out: their cells are gone.
    pub fn to_json(&self, cells: &[Cell]) -> Value {
        let kept: HashSet<usize> = self
            .matching(cells)
            .into_iter()
            .flatten()
            .map(|found| found.slot)
            .collect();
        let mut slots: Vec<&Slot> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(at, _)| kept.contains(at))
            .map(|(_, slot)| slot)
            .collect();
        slots.sort_by_key(|slot| slot.index);
        let entries: Vec<Value> = slots
            .into_iter()
            .map(|slot| {
                json!({
                    "id": slot.id,
                    "hash": slot.hash,
                    "index": slot.index,
                    "language": slot.lang,
                    "status": slot.status.name(),
                    "execution_count": slot.count,
                    "ran_by": slot.ran_by,
                    "started_ms": slot.started_ms,
                    "took_ms": slot.took_ms,
                    "outputs": slot.outputs.iter().map(|item| item.to_json(slot.count)).collect::<Vec<_>>(),
                })
            })
            .collect();
        let present: HashSet<String> = cells.iter().map(Cell::fold_key).collect();
        let folded: Vec<&String> = self.folded.iter().filter(|key| present.contains(*key)).collect();
        let mut value = json!({ "stet_notebook": 1, "cells": entries });
        if !folded.is_empty() {
            value["folded"] = json!(folded);
        }
        value
    }

    pub fn from_json(value: &Value) -> Notebook {
        let mut notebook = Notebook::default();
        let cells = value
            .get("cells")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for cell in cells {
            let text = |key: &str| cell.get(key).and_then(Value::as_str).map(str::to_string);
            let number = |key: &str| cell.get(key).and_then(Value::as_u64);
            notebook.next_uid += 1;
            notebook.slots.push(Slot {
                uid: notebook.next_uid,
                id: text("id"),
                hash: text("hash").unwrap_or_default(),
                index: number("index").unwrap_or(0) as usize,
                lang: text("language").unwrap_or_default(),
                status: Status::from_name(&text("status").unwrap_or_default()),
                count: number("execution_count").map(|count| count as usize),
                outputs: cell
                    .get("outputs")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(Item::from_json).collect())
                    .unwrap_or_default(),
                ran_by: text("ran_by"),
                started_ms: number("started_ms"),
                took_ms: number("took_ms"),
                clear_on_next: false,
            });
        }
        notebook.folded = value
            .get("folded")
            .and_then(Value::as_array)
            .map(|keys| keys.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        notebook
    }
}

const STET_DIR: &str = ".stet";

/// Where the outputs of the document at `path` are kept.
pub fn outputs_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(STET_DIR).join(format!("{name}.out.json"))
}

/// The folder for the document's output pictures.
pub fn files_dir(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(STET_DIR).join(format!("{name}.out"))
}

/// MIME types kept as files instead of inside the JSON, with their extension.
const FILE_TYPES: &[(&str, &str)] = &[("image/png", "png"), ("image/jpeg", "jpg"), ("image/gif", "gif")];

/// A data value that is a file in the output folder: its name.
pub fn file_ref(value: &Value) -> Option<&str> {
    value.get("$file").and_then(Value::as_str)
}

/// Move the pictures in `data` out to files in `dir`, named by their
/// content, leaving `{"$file": name}` in their place. Values that fail to
/// decode or write stay as they are.
pub fn store_files(data: &mut Map<String, Value>, dir: &Path) {
    for (mime, extension) in FILE_TYPES {
        let Some(Value::String(encoded)) = data.get(*mime) else {
            continue;
        };
        let Some(bytes) = base64_decode(encoded) else { continue };
        let name = format!("{}.{extension}", code_hash(encoded));
        let file = dir.join(&name);
        if !file.exists() && (std::fs::create_dir_all(dir).is_err() || std::fs::write(&file, &bytes).is_err()) {
            continue;
        }
        data.insert(mime.to_string(), json!({ "$file": name }));
    }
}

/// The opposite of [`store_files`]: pictures back inline, as base64, for
/// formats that must carry them (`.ipynb`).
pub fn load_files(data: &mut Map<String, Value>, dir: &Path) {
    for (mime, _) in FILE_TYPES {
        let Some(name) = data.get(*mime).and_then(file_ref) else {
            continue;
        };
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            data.insert(mime.to_string(), Value::String(base64_encode(&bytes)));
        }
    }
}

/// Read a notebook's outputs, or an empty notebook if it has none.
pub fn load(path: &Path) -> Notebook {
    std::fs::read_to_string(outputs_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|value| Notebook::from_json(&value))
        .unwrap_or_default()
}

/// Write a notebook's outputs and folds; with neither left, remove the file.
pub fn save(path: &Path, notebook: &Notebook, cells: &[Cell]) -> std::io::Result<()> {
    let file = outputs_path(path);
    let value = notebook.to_json(cells);
    if value["cells"].as_array().is_none_or(|cells| cells.is_empty()) && value.get("folded").is_none() {
        return match std::fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")))?;
    let text = serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?;
    let temp = file.with_extension("tmp");
    std::fs::write(&temp, text + "\n")?;
    std::fs::rename(temp, file)
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes base64, skipping whitespace (kernels wrap long images in lines).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            byte if byte.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = acc << 6 | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_strings() {
        assert_eq!(parse_info("python").lang, "python");
        let info = parse_info("Python {#weekly .hide kernel=duckdb title=\"Q3 churn\"}");
        assert_eq!(info.lang, "python");
        assert_eq!(info.id.as_deref(), Some("weekly"));
        assert!(info.has_class("hide"));
        assert_eq!(info.attr("kernel"), Some("duckdb"));
        assert_eq!(info.attr("title"), Some("Q3 churn"));
        let pandoc = parse_info("{.sql #counts}");
        assert_eq!((pandoc.lang.as_str(), pandoc.id.as_deref()), ("sql", Some("counts")));
        assert_eq!(parse_info("").lang, "");
    }

    fn doc(text: &str) -> Vec<Cell> {
        let lines: Vec<&str> = text.split('\n').collect();
        let doc = crate::markdown::parse(text);
        cells(&lines, &doc.fences)
    }

    #[test]
    fn cells_from_fences() {
        let found =
            doc("# t\n\n```python {#a}\nx = 1\nprint(x)\n```\n\ntext\n\n~~~sh\necho hi\n~~~\n\n```\nplain\n```\n");
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].code, "x = 1\nprint(x)");
        assert_eq!(found[0].info.id.as_deref(), Some("a"));
        assert_eq!(found[0].last_line(), 5);
        assert_eq!((found[1].info.lang.as_str(), found[1].code.as_str()), ("sh", "echo hi"));
        assert_eq!(found[2].info.lang, "");
        // An unclosed fence runs to the end; its last line is code.
        let open = doc("```sh\necho a\necho b");
        assert_eq!(open[0].code, "echo a\necho b");
    }

    fn run(notebook: &mut Notebook, cells: &[Cell], at: usize, text: &str) -> u64 {
        let uid = notebook.queue(cells, at, None);
        notebook.started(uid, 0);
        notebook.output(
            uid,
            Item::Stream {
                stderr: false,
                text: text.into(),
            },
        );
        notebook.finished(uid, Status::Ok, Some(at + 1), 5);
        uid
    }

    #[test]
    fn outputs_follow_their_cells_through_edits() {
        let mut notebook = Notebook::default();
        let before = doc("```sh\necho a\n```\n\n```sh\necho b\n```\n");
        run(&mut notebook, &before, 0, "a\n");
        run(&mut notebook, &before, 1, "b\n");

        // A new cell above: both keep their outputs, by code.
        let after = doc("```sh\necho new\n```\n\n```sh\necho a\n```\n\n```sh\necho b\n```\n");
        let found = notebook.matching(&after);
        assert_eq!(found[0], None);
        assert_eq!(found[1].map(|found| found.stale), Some(false));
        assert_eq!(
            notebook.slots[found[2].unwrap().slot].outputs[0],
            Item::Stream {
                stderr: false,
                text: "b\n".into()
            }
        );
        notebook.settle(&found);

        // Editing a cell keeps its output, marked stale.
        let edited = doc("```sh\necho new\n```\n\n```sh\necho a2\n```\n\n```sh\necho b\n```\n");
        let found = notebook.matching(&edited);
        assert_eq!(found[1].map(|found| found.stale), Some(true));
        assert_eq!(found[2].map(|found| found.stale), Some(false));
    }

    #[test]
    fn ids_win_over_code() {
        let mut notebook = Notebook::default();
        let cells = doc("```python {#load}\nx = 1\n```\n");
        run(&mut notebook, &cells, 0, "one\n");
        let moved = doc("```python\nx = 1\n```\n\n```python {#load}\nx = 2\n```\n");
        let found = notebook.matching(&moved);
        assert_eq!(found[0], None, "the slot belongs to #load, not to the same code");
        assert_eq!(found[1].map(|found| found.stale), Some(true));
    }

    #[test]
    fn streams_join_and_clears_wait() {
        let mut notebook = Notebook::default();
        let cells = doc("```sh\nx\n```\n");
        let uid = notebook.queue(&cells, 0, Some("Claude".into()));
        notebook.output(
            uid,
            Item::Stream {
                stderr: false,
                text: "a".into(),
            },
        );
        notebook.output(
            uid,
            Item::Stream {
                stderr: false,
                text: "b\n".into(),
            },
        );
        notebook.output(
            uid,
            Item::Stream {
                stderr: true,
                text: "warn".into(),
            },
        );
        assert_eq!(notebook.slot(uid).unwrap().outputs.len(), 2);
        notebook.clear(uid, true);
        assert_eq!(notebook.slot(uid).unwrap().outputs.len(), 2);
        notebook.output(
            uid,
            Item::Stream {
                stderr: false,
                text: "50%".into(),
            },
        );
        assert_eq!(
            notebook.slot(uid).unwrap().outputs,
            vec![Item::Stream {
                stderr: false,
                text: "50%".into()
            }]
        );
        assert_eq!(notebook.slot(uid).unwrap().ran_by.as_deref(), Some("Claude"));
    }

    #[test]
    fn round_trips_through_json_and_drops_orphans() {
        let mut notebook = Notebook::default();
        let cells = doc("```python\n6*7\n```\n\n```sh\necho gone\n```\n");
        let uid = notebook.queue(&cells, 0, None);
        let mut data = Map::new();
        data.insert("text/plain".into(), "42".into());
        notebook.output(uid, Item::Result { data });
        notebook.output(
            uid,
            Item::Error {
                name: "E".into(),
                value: "v".into(),
                traceback: vec!["t".into()],
            },
        );
        notebook.finished(uid, Status::Error, Some(3), 0);
        run(&mut notebook, &cells, 1, "gone\n");

        let remaining = doc("```python\n6*7\n```\n");
        let value = notebook.to_json(&remaining);
        assert_eq!(value["cells"].as_array().unwrap().len(), 1);
        assert_eq!(value["cells"][0]["outputs"][0]["execution_count"], 3);
        let back = Notebook::from_json(&value);
        assert_eq!(back.slots[0].outputs, notebook.slots[0].outputs);
        assert_eq!((back.slots[0].status, back.slots[0].count), (Status::Error, Some(3)));
        assert!(back.matching(&remaining)[0].is_some());

        // nbformat's text as lines reads as one string; JSON lists stay lists.
        let listed = json!({"output_type": "execute_result", "data": {
            "text/plain": ["   a\n", "0  1"], "application/json": [1, 2]}});
        let Some(Item::Result { data }) = Item::from_json(&listed) else {
            panic!()
        };
        assert_eq!(
            (data["text/plain"].clone(), data["application/json"].clone()),
            (json!("   a\n0  1"), json!([1, 2]))
        );
    }

    #[test]
    fn unfinished_runs_load_as_aborted() {
        let value = json!({"cells": [{"hash": "x", "status": "running", "outputs": []}]});
        assert_eq!(Notebook::from_json(&value).slots[0].status, Status::Aborted);
    }

    #[test]
    fn pictures_move_to_files_and_back() {
        let dir = std::env::temp_dir().join(format!("stet-out-{}", std::process::id()));
        let png = base64_encode(b"\x89PNG fake bytes");
        let mut data = Map::new();
        data.insert("image/png".into(), Value::String(png.clone()));
        data.insert("text/plain".into(), "<Figure>".into());
        store_files(&mut data, &dir);
        let name = file_ref(&data["image/png"]).unwrap().to_string();
        assert_eq!(std::fs::read(dir.join(&name)).unwrap(), b"\x89PNG fake bytes");
        load_files(&mut data, &dir);
        assert_eq!(data["image/png"], Value::String(png));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn base64_round_trip() {
        for text in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let encoded = base64_encode(text.as_bytes());
            assert_eq!(base64_decode(&encoded).unwrap(), text.as_bytes(), "{encoded}");
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn saving_writes_and_removes_the_file() {
        let dir = std::env::temp_dir().join(format!("stet-nb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("q3.md");
        let cells = doc("```sh\necho a\n```\n");
        let mut notebook = Notebook::default();
        run(&mut notebook, &cells, 0, "a\n");
        save(&path, &notebook, &cells).unwrap();
        assert!(outputs_path(&path).exists());
        assert_eq!(load(&path).slots[0].outputs, notebook.slots[0].outputs);
        save(&path, &notebook, &[]).unwrap();
        assert!(!outputs_path(&path).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
