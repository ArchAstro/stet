//! Control from outside the window: other programs (an assistant, a script)
//! list the open documents, read them as they are right now, and edit them
//! while the user keeps typing.
//!
//! An edit names its target by the text it replaces, or by a range read at
//! an earlier revision, which is carried forward through whatever was typed
//! since. Either way it lands as its own undo step, never moves the user's
//! cursor off their text, and by default arrives as a suggestion to accept
//! or reject rather than a change.

use super::{Editor, Mode};
use crate::buffer::Rebased;
use crate::critic::is_valid_author;
use std::ops::Range;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// Tab position, from 0.
    pub index: usize,
    pub path: Option<PathBuf>,
    pub title: String,
    pub active: bool,
    pub dirty: bool,
    pub lines: usize,
    pub words: usize,
    pub revision: u64,
    /// Zero-based line and column of the cursor.
    pub cursor: (usize, usize),
    pub suggestions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub session: Session,
    pub text: String,
    /// Char offset of the cursor.
    pub cursor: usize,
    /// The selection as a char range, with its text.
    pub selection: Option<(Range<usize>, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Replace this exact text. It must occur once, or `occurrence` (from 1)
    /// picks which.
    Text { old: String, occurrence: Option<usize> },
    /// Replace a char range as it was at `revision`.
    Range { revision: u64, range: Range<usize> },
    /// Insert before this zero-based line.
    Line(usize),
    /// Insert where the user's cursor is.
    Cursor,
    /// Append to the document.
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEdit {
    pub target: Target,
    pub text: String,
    /// Record as a suggestion by `author` instead of changing the text.
    pub suggest: bool,
    pub author: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub revision: u64,
    /// Zero-based line where the edit landed.
    pub line: usize,
}

impl Editor {
    fn session(&mut self, index: usize) -> Session {
        self.refresh();
        let line = self.buf.line_of(self.cursor);
        Session {
            index,
            path: self.path.clone(),
            title: self.file_name(),
            active: false,
            dirty: self.buf.is_dirty(),
            lines: self.buf.line_count(),
            words: self.doc.words,
            revision: self.buf.revision(),
            cursor: (line, self.cursor - self.buf.line_start(line)),
            suggestions: self.doc.suggestions.len(),
        }
    }

    /// Every open document.
    pub fn sessions(&mut self) -> Vec<Session> {
        let active = self.active;
        (0..self.tab_count())
            .map(|index| {
                let mut session = self.with_doc(index, |ed| ed.session(index));
                session.active = index == active;
                session
            })
            .collect()
    }

    /// Runs `work` on document `index` without bringing it to the front.
    pub fn with_session<R>(&mut self, index: usize, work: impl FnOnce(&mut Editor) -> R) -> R {
        self.with_doc(index, work)
    }

    /// Finds a document by `active` (or nothing), tab number from 1, full
    /// path, or file name.
    pub fn find_session(&mut self, spec: &str) -> Result<usize, String> {
        let spec = spec.trim();
        if spec.is_empty() || spec == "active" {
            return Ok(self.active);
        }
        let sessions = self.sessions();
        if let Ok(number) = spec.parse::<usize>() {
            return (1..=sessions.len())
                .contains(&number)
                .then(|| number - 1)
                .ok_or(format!("no tab {number}"));
        }
        let wanted = std::path::absolute(spec).unwrap_or_else(|_| PathBuf::from(spec));
        let by_path = sessions
            .iter()
            .filter(|session| session.path.as_deref() == Some(wanted.as_path()));
        let by_name = sessions.iter().filter(|session| {
            session.title == spec
                || session
                    .path
                    .as_ref()
                    .is_some_and(|path| path.file_stem().is_some_and(|stem| stem.to_string_lossy() == spec))
        });
        let found: Vec<usize> = by_path.chain(by_name).map(|session| session.index).collect();
        match found.as_slice() {
            [] => Err(format!("no open document matches `{spec}`")),
            [first, rest @ ..] if rest.iter().all(|index| index == first) => Ok(*first),
            _ => Err(format!(
                "`{spec}` matches several open documents; use the full path or the tab number"
            )),
        }
    }

    /// The document as it is now, unsaved changes included.
    pub fn snapshot(&mut self, index: usize) -> Snapshot {
        let active = self.active;
        self.with_doc(index, |ed| {
            let mut session = ed.session(index);
            session.active = index == active;
            Snapshot {
                session,
                text: ed.buf.text(),
                cursor: ed.cursor,
                selection: (index == active)
                    .then(|| ed.selection())
                    .flatten()
                    .map(|range| (range.clone(), ed.buf.slice(range))),
            }
        })
    }

    fn locate(&self, target: &Target) -> Result<Range<usize>, String> {
        let len = self.buf.len();
        match target {
            Target::Cursor => Ok(self.cursor.min(len)..self.cursor.min(len)),
            Target::End => Ok(len..len),
            Target::Line(line) => {
                let at = if *line >= self.buf.line_count() {
                    len
                } else {
                    self.buf.line_start(*line)
                };
                Ok(at..at)
            }
            Target::Range { revision, range } => match self.buf.rebase(range.clone(), *revision) {
                Rebased::At(range) if range.end <= len => Ok(range),
                Rebased::At(_) => Err("that range is outside the document".to_string()),
                Rebased::Conflict => Err("the text in that range changed since it was read; read it again".to_string()),
                Rebased::Unknown => Err("that revision is too old to follow; read the document again".to_string()),
            },
            Target::Text { old, occurrence } => {
                if old.is_empty() {
                    return Err("the text to replace is empty".to_string());
                }
                let text = self.buf.text();
                let found: Vec<usize> = text.match_indices(old.as_str()).map(|(at, _)| at).collect();
                let at = match (found.len(), occurrence) {
                    (0, _) => {
                        return Err(
                            "that text is not in the document (it may have just been edited); read it again"
                                .to_string(),
                        );
                    }
                    (1, None) => found[0],
                    (count, None) => {
                        return Err(format!(
                            "that text occurs {count} times; include more of the surrounding text, or pick an occurrence"
                        ));
                    }
                    (count, Some(nth)) => *found
                        .get(nth.wrapping_sub(1))
                        .ok_or(format!("there are only {count} occurrences"))?,
                };
                let start = text[..at].chars().count();
                Ok(start..start + old.chars().count())
            }
        }
    }

    /// Applies an edit from outside to document `index`.
    pub fn remote_edit(&mut self, index: usize, edit: &RemoteEdit) -> Result<Applied, String> {
        let visible = index == self.active;
        let applied = self.with_doc(index, |ed| ed.apply_remote(edit))?;
        let verb = if edit.suggest { "suggested an edit" } else { "edited" };
        let place = if visible {
            String::new()
        } else {
            format!(" in {}", self.tabs()[index].title)
        };
        self.info(format!("{} {verb} on line {}{place}", edit.author, applied.line + 1));
        Ok(applied)
    }

    fn apply_remote(&mut self, edit: &RemoteEdit) -> Result<Applied, String> {
        if edit.suggest && !is_valid_author(&edit.author) {
            return Err(format!(
                "`{}` cannot be written into a suggestion as an author",
                edit.author
            ));
        }
        let range = self.locate(&edit.target)?;
        if range.is_empty() && edit.text.is_empty() {
            return Err("nothing to change".to_string());
        }
        // The user's unfinished undo step ends here and a new one starts
        // after, so `u` takes back this edit alone.
        let typing = self.group_open;
        self.close_group();
        let before = self.buf.revision();
        let (cursor, anchor, mode) = (self.cursor, self.anchor, self.mode);
        let text = edit.text.replace("\r\n", "\n").replace('\r', "\n");
        let landed = if edit.suggest {
            let author = std::mem::replace(&mut self.config.author, edit.author.clone());
            let result = self.try_suggest(range, &text);
            self.config.author = author;
            result.map_err(str::to_string)?.start
        } else {
            self.raw_edit(range, &text).start
        };
        self.close_group();
        // Their cursor stays on the text it was on, before anything inserted there.
        self.cursor = self.buf.rebase_pos(cursor, before, true);
        self.anchor = anchor.map(|anchor| self.buf.rebase_pos(anchor, before, true));
        self.mode = mode;
        if self.anchor.is_none() && self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        self.clamp_cursor();
        if typing {
            self.open_group();
        }
        Ok(Applied {
            revision: self.buf.revision(),
            line: self.buf.line_of(landed.min(self.buf.len())),
        })
    }
}
