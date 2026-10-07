//! Links between documents: `[[wiki links]]`, relative markdown links and
//! URLs. Following one opens the target note, creating it on first save, so
//! a folder of markdown files works as a linked knowledge base.

use super::{Editor, Effect};
use crate::markdown::Block;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static WIKI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[([^\[\]\n]+)\]\]").unwrap());
static INLINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"!?\[[^\]\n]*\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"\n]*")?\s*\)"#).unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:https?://|mailto:)[^\s<>)\]"]+"#).unwrap());

const NOTE_EXTENSIONS: [&str; 5] = ["md", "markdown", "mdown", "mdx", "txt"];
const SKIPPED_DIRS: [&str; 6] = ["node_modules", "target", "dist", "build", "vendor", "__pycache__"];
const MAX_FILES: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// `[[Note]]`, `[[Note#Heading]]`, `[[Note|shown text]]`.
    Wiki {
        note: String,
        anchor: Option<String>,
    },
    /// A relative or absolute file destination, with an optional `#anchor`.
    File {
        path: String,
        anchor: Option<String>,
    },
    Url(String),
}

fn split_anchor(target: &str) -> (String, Option<String>) {
    match target.split_once('#') {
        Some((path, anchor)) => (
            path.to_string(),
            Some(anchor.to_string()).filter(|anchor| !anchor.is_empty()),
        ),
        None => (target.to_string(), None),
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes.get(at + 1..at + 3).and_then(|hex| std::str::from_utf8(hex).ok());
        match (bytes[at], hex.and_then(|hex| u8::from_str_radix(hex, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The link whose source text covers byte `at` of `line`.
pub fn link_in(line: &str, at: usize) -> Option<Link> {
    let covers = |found: regex::Match| found.start() <= at && at < found.end();
    if let Some(captures) = WIKI
        .captures_iter(line)
        .find(|captures| covers(captures.get(0).unwrap()))
    {
        let target = captures[1].split('|').next().unwrap_or("").trim();
        let (note, anchor) = split_anchor(target);
        return Some(Link::Wiki { note, anchor });
    }
    if let Some(captures) = INLINE
        .captures_iter(line)
        .find(|captures| covers(captures.get(0).unwrap()))
    {
        let dest = &captures[1];
        if URL.find(dest).is_some_and(|found| found.start() == 0) {
            return Some(Link::Url(dest.to_string()));
        }
        let (path, anchor) = split_anchor(dest);
        return Some(Link::File {
            path: percent_decode(&path),
            anchor,
        });
    }
    let bare = URL.find_iter(line).find(|found| covers(*found))?;
    Some(Link::Url(
        bare.as_str()
            .trim_end_matches(['.', ',', ';', ':', '!', '?', '\''])
            .to_string(),
    ))
}

/// GitHub-style heading anchor.
fn slug(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| match c {
            ' ' | '-' => Some('-'),
            c if c.is_alphanumeric() || c == '_' => Some(c.to_lowercase().next().unwrap_or(c)),
            _ => None,
        })
        .collect()
}

fn is_note(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| NOTE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Notes under `root`, breadth-first so nearby files come first.
pub fn notes_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0)]);
    let mut seen = 0;
    while let Some((dir, depth)) = queue.pop_front() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            seen += 1;
            if seen > MAX_FILES {
                return out;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if depth < 12 && !SKIPPED_DIRS.contains(&name.as_ref()) {
                    queue.push_back((path, depth + 1));
                }
            } else if is_note(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// A note that links to another: where, and the line that does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlink {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

impl Editor {
    fn doc_dir(&self) -> PathBuf {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    /// The folder treated as one knowledge base: the nearest ancestor that
    /// looks like a project or vault root, else the document's folder.
    pub fn workspace_root(&self) -> PathBuf {
        let dir = self.doc_dir();
        dir.ancestors()
            .take(8)
            .find(|dir| {
                [".git", ".obsidian", ".md-root"]
                    .iter()
                    .any(|marker| dir.join(marker).exists())
            })
            .map_or(dir.clone(), Path::to_path_buf)
    }

    pub fn workspace_notes(&self) -> Vec<PathBuf> {
        notes_under(&self.workspace_root())
    }

    pub fn link_at(&self, pos: usize) -> Option<Link> {
        let line = self.buf.line_of(pos);
        let text = self.buf.line_text(line);
        let col = pos - self.buf.line_start(line);
        let byte = text.char_indices().nth(col).map_or(text.len(), |(byte, _)| byte);
        link_in(&text, byte)
    }

    /// Where a wiki link leads: an existing note with that name anywhere in
    /// the workspace (nearest first), else a new note beside this one.
    pub fn resolve_note(&self, note: &str) -> PathBuf {
        let dir = self.doc_dir();
        let wanted = Path::new(note);
        let direct = if is_note(wanted) {
            dir.join(wanted)
        } else {
            dir.join(format!("{note}.md"))
        };
        if direct.exists() {
            return direct;
        }
        let stem = wanted.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        let suffix = wanted.with_extension("");
        self.workspace_notes()
            .into_iter()
            .filter(|path| {
                path.file_stem()
                    .is_some_and(|found| found.to_string_lossy().to_lowercase() == stem)
            })
            .find(|path| suffix.components().count() == 1 || path.with_extension("").ends_with(&suffix))
            .unwrap_or(direct)
    }

    fn goto_anchor(&mut self, anchor: &str) {
        self.refresh();
        let wanted = slug(anchor);
        let found = (0..self.buf.line_count()).find(|&line| {
            matches!(self.doc.block(line), Block::Heading(_))
                && slug(self.buf.line_text(line).trim_start_matches(['#', ' '])) == wanted
        });
        match found {
            Some(line) => self.cursor = self.buf.line_start(line),
            None => self.error(format!("no heading `{anchor}`")),
        }
    }

    pub fn follow_link(&mut self) {
        match self.link_at(self.cursor) {
            Some(link) => self.follow(link),
            None => self.error("no link under the cursor"),
        }
    }

    /// Follows the link at `pos`, if there is one (Cmd/Ctrl-click).
    pub fn follow_link_at(&mut self, pos: usize) -> bool {
        match self.link_at(pos) {
            Some(link) => {
                self.cursor = pos.min(self.buf.len());
                self.follow(link);
                true
            }
            None => false,
        }
    }

    fn follow(&mut self, link: Link) {
        let (path, anchor) = match link {
            Link::Url(url) => return self.effects.push(Effect::OpenUrl(url)),
            Link::Wiki { note, anchor } if note.is_empty() => (None, anchor),
            Link::Wiki { note, anchor } => (Some(self.resolve_note(&note)), anchor),
            Link::File { path, anchor } if path.is_empty() => (None, anchor),
            Link::File { path, anchor } => {
                let path = self.doc_dir().join(path);
                if !is_note(&path) && path.extension().is_some() {
                    // Images, PDFs and the like belong to their own apps.
                    return self.effects.push(Effect::OpenUrl(path.to_string_lossy().into_owned()));
                }
                (Some(path), anchor)
            }
        };
        self.push_jump();
        if let Some(path) = path {
            let new = !path.exists();
            if let Err(err) = self.open_path(&path) {
                self.back.pop();
                return self.error(err);
            }
            if new {
                self.info(format!("new note \"{}\" — :w creates it", self.file_name()));
            }
        }
        if let Some(anchor) = anchor {
            self.goto_anchor(&anchor);
        }
        self.clamp_cursor();
    }

    /// Notes in the workspace that link to the active document.
    pub fn backlinks(&self) -> Vec<Backlink> {
        let Some(own) = self.path.clone() else {
            return Vec::new();
        };
        let stem = own.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        let name = own.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        let mut out = Vec::new();
        for path in self.workspace_notes() {
            if path == own || std::fs::metadata(&path).map_or(true, |meta| meta.len() > 4 << 20) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !text.to_lowercase().contains(&stem) {
                continue;
            }
            for (line, source) in text.lines().enumerate() {
                let wiki = WIKI.captures_iter(source).any(|captures| {
                    let target = captures[1].split(['|', '#']).next().unwrap_or("").trim().to_lowercase();
                    Path::new(&target)
                        .with_extension("")
                        .file_name()
                        .is_some_and(|found| found.to_string_lossy() == stem)
                });
                let inline = INLINE.captures_iter(source).any(|captures| {
                    let dest = percent_decode(captures[1].split('#').next().unwrap_or("")).to_lowercase();
                    Path::new(&dest)
                        .file_name()
                        .is_some_and(|found| found.to_string_lossy() == name)
                });
                if wiki || inline {
                    out.push(Backlink {
                        path: path.clone(),
                        line,
                        text: source.trim().to_string(),
                    });
                }
            }
        }
        out
    }
}
