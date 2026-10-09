//! The margin: a scratch pane beside each document for research notes, raw
//! material and data, kept out of the document itself.
//!
//! A document `notes.md` has its margin in `.stet/notes.md.margin.md` and
//! its attached files in `.stet/notes.md.files/`, both in the document's
//! folder. The margin is an ordinary markdown buffer with one difference:
//! it saves itself.
//!
//! Whichever pane has the keyboard lives in the editor's own fields; the
//! other one waits in a stash, like a background tab.

use super::links::rewrite_destinations;
use super::tabs::Stash;
use super::{Editor, Mode};
use crate::buffer::Buffer;
use std::path::{Path, PathBuf};

const MARGIN_DIR: &str = ".stet";

/// Where the margin of the document at `path` is kept.
pub fn margin_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(MARGIN_DIR).join(format!("{name}.margin.md"))
}

pub(super) fn files_dir(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(MARGIN_DIR).join(format!("{name}.files"))
}

impl Editor {
    /// The pane with the keyboard is the margin.
    pub fn in_margin(&self) -> bool {
        self.margin_active
    }

    /// The path of the document this pane pair belongs to.
    pub(super) fn margin_owner(&self) -> Option<PathBuf> {
        match &self.margin_parent {
            Some(parent) => parent.path.clone(),
            None => self.path.clone(),
        }
    }

    /// The margin pane is on screen.
    pub fn margin_visible(&self) -> bool {
        self.margin_open && self.margin_owner().is_some()
    }

    fn load_margin(&mut self, owner: &Path) -> Stash {
        if let Some(stash) = self.margins.remove(owner) {
            return stash;
        }
        let path = margin_path(owner);
        let text = std::fs::read(&path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();
        self.next_doc_id += 1;
        Stash {
            buf: Buffer::from_text(&text),
            cursor: 0,
            disk_mtime: std::fs::metadata(&path).and_then(|meta| meta.modified()).ok(),
            path: Some(path),
            doc: Default::default(),
            doc_revision: None,
            scroll_line: 0,
            scroll_px: 0.0,
            doc_id: self.next_doc_id,
            snapshot: None,
        }
    }

    /// Margins save themselves; there is nothing to remember to do.
    pub fn autosave_margin(&mut self) {
        if self.margin_active && self.buf.is_dirty() {
            self.close_group();
            let message = self.message.take();
            if let Err(err) = self.save(None, false) {
                return self.error(format!("the margin could not be saved: {err}"));
            }
            self.message = message;
        }
    }

    /// Gives the keyboard to the margin, opening it if needed.
    pub fn focus_margin(&mut self) {
        if self.margin_active {
            return;
        }
        let Some(owner) = self.path.clone() else {
            return self.error("save the document first; its margin is kept beside it");
        };
        self.close_group();
        self.vim.clear_pending();
        self.cmdline = None;
        let mut other = self.load_margin(&owner);
        self.trade(&mut other);
        self.margin_parent = Some(other);
        self.margin_active = true;
        self.margin_open = true;
        self.anchor = None;
        self.group_open = false;
        self.mode = if self.config.vim { Mode::Normal } else { Mode::Insert };
        self.clamp_cursor();
    }

    /// Gives the keyboard back to the document.
    pub fn focus_document(&mut self) {
        let Some(mut other) = self.margin_parent.take() else {
            return;
        };
        self.autosave_margin();
        self.close_group();
        self.vim.clear_pending();
        self.cmdline = None;
        self.trade(&mut other);
        self.margin_active = false;
        if let Some(owner) = self.path.clone() {
            self.margins.insert(owner, other);
        }
        self.anchor = None;
        self.group_open = false;
        self.mode = if self.config.vim { Mode::Normal } else { Mode::Insert };
        self.clamp_cursor();
    }

    /// Hidden → shown with the keyboard → hidden. Shown without the keyboard
    /// takes it.
    pub fn toggle_margin(&mut self) {
        if self.margin_active {
            self.focus_document();
            self.margin_open = false;
        } else {
            self.focus_margin();
        }
    }

    /// Moves the keyboard to the other pane.
    pub fn switch_pane(&mut self) {
        if self.margin_active {
            self.focus_document()
        } else {
            self.focus_margin()
        }
    }

    /// Runs `work` with the other pane's buffer in place, leaving the
    /// keyboard, mode and pending keys where they are. `None` if there is
    /// no other pane (an untitled document has no margin).
    pub fn with_other_pane<R>(&mut self, work: impl FnOnce(&mut Editor) -> R) -> Option<R> {
        let kept = (self.mode, self.anchor, self.group_open, self.goal_col);
        let result;
        // While `work` runs the pane that stepped aside is parked where a
        // real switch would leave it, so `work` may itself look across.
        if self.margin_active {
            let mut margin = self.margin_parent.take()?;
            self.trade(&mut margin);
            self.margin_active = false;
            let owner = self.path.clone()?;
            self.margins.insert(owner.clone(), margin);
            (self.mode, self.anchor, self.group_open, self.goal_col) = (Mode::Normal, None, false, None);
            result = work(self);
            self.close_group();
            let mut margin = self.margins.remove(&owner)?;
            self.trade(&mut margin);
            self.margin_active = true;
            self.margin_parent = Some(margin);
        } else {
            let owner = self.path.clone()?;
            let mut document = self.load_margin(&owner);
            self.trade(&mut document);
            self.margin_active = true;
            self.margin_parent = Some(document);
            (self.mode, self.anchor, self.group_open, self.goal_col) = (Mode::Normal, None, false, None);
            result = work(self);
            self.close_group();
            // A margin changed from outside its pane is saved at once.
            if self.buf.is_dirty() {
                let message = self.message.take();
                let _ = self.save(None, false);
                self.message = message;
            }
            let mut margin = self.margin_parent.take()?;
            self.trade(&mut margin);
            self.margin_active = false;
            self.margins.insert(owner, margin);
        }
        (self.mode, self.anchor, self.group_open, self.goal_col) = kept;
        Some(result)
    }

    /// Runs `work` on tab `index`'s document, or on its margin, without
    /// bringing either to the front.
    pub fn with_pane<R>(&mut self, index: usize, margin: bool, work: impl FnOnce(&mut Editor) -> R) -> Option<R> {
        if index == self.active || index >= self.tab_count() {
            return if margin == self.margin_active {
                Some(work(self))
            } else {
                self.with_other_pane(work)
            };
        }
        // Another tab: its document is swapped in while this one waits.
        let here = std::mem::replace(&mut self.margin_active, false);
        let result = self.with_doc(index, |ed| {
            if margin {
                ed.with_other_pane(work)
            } else {
                Some(work(ed))
            }
        });
        self.margin_active = here;
        result
    }

    /// True once: the other pane changed and should scroll to its cursor.
    pub fn take_reveal(&mut self) -> bool {
        std::mem::take(&mut self.reveal_other)
    }

    /// Copies the selection, or the paragraph under the cursor, into the
    /// other pane; `take` removes it from this one. Text sent to the margin
    /// is appended; text sent to the document lands after the paragraph its
    /// cursor is in.
    pub fn send_to_other_pane(&mut self, take: bool) {
        if !self.margin_visible() {
            return self.error("open the margin first (:margin)");
        }
        let range = self.selection().unwrap_or_else(|| {
            // On a blank line, the paragraph just above is meant.
            let cursor = self.cursor;
            let mut line = self.buf.line_of(cursor);
            while line > 0 && self.buf.line_is_blank(line) {
                line -= 1;
            }
            self.cursor = self.buf.line_start(line);
            let lines = self.focus_lines();
            self.cursor = cursor;
            self.buf.line_start(lines.start)..self.buf.line_end(lines.end - 1)
        });
        let text = self.buf.slice(range.clone());
        let text = text.trim_matches('\n');
        if text.trim().is_empty() {
            return self.error("nothing to send");
        }
        let to_margin = !self.margin_active;
        // Relative links keep pointing at the same files from the other folder.
        let text = rewrite_destinations(text, to_margin);
        if self.with_other_pane(|ed| ed.receive(&text, to_margin)).is_none() {
            return;
        }
        self.reveal_other = true;
        self.anchor = None;
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        if take {
            self.cursor = self.edit(range, "").start;
            self.close_group();
        }
        self.clamp_cursor();
        let verb = if take { "moved" } else { "copied" };
        self.info(format!(
            "{verb} to the {}",
            if to_margin { "margin" } else { "document" }
        ));
    }

    fn receive(&mut self, text: &str, append: bool) {
        self.close_group();
        let at = if append {
            self.buf.len()
        } else {
            let lines = self.focus_lines();
            self.buf.line_end(lines.end - 1)
        };
        let before = self.buf.slice(at.saturating_sub(2)..at);
        let lead = match (at, before.as_str()) {
            (0, _) => "",
            (_, "\n\n") => "",
            (_, tail) if tail.ends_with('\n') => "\n",
            _ => "\n\n",
        };
        let trail = if at == self.buf.len() { "\n" } else { "" };
        let pos = self.raw_edit(at..at, &format!("{lead}{text}{trail}"));
        self.close_group();
        // The next thing sent lands after this one.
        self.cursor = pos.start + lead.chars().count();
        self.clamp_cursor();
    }

    /// Copies a file into the margin's own folder and links it from the
    /// margin, so research material travels with the document without
    /// sitting in it.
    pub fn attach_to_margin(&mut self, file: &Path) -> Result<(), String> {
        let link = self.keep_in_margin(file)?;
        let insert = |ed: &mut Editor| ed.receive(&link, true);
        if self.margin_active {
            insert(self);
        } else {
            self.with_other_pane(insert);
            self.reveal_other = true;
        }
        self.margin_open = true;
        Ok(())
    }

    /// Copies a file into the margin's folder and returns the Markdown that
    /// refers to it from the margin.
    pub(super) fn keep_in_margin(&mut self, file: &Path) -> Result<String, String> {
        let owner = self
            .margin_owner()
            .ok_or("save the document first; its margin is kept beside it")?;
        let folder = files_dir(&owner);
        let name = file
            .file_name()
            .ok_or("that is not a file")?
            .to_string_lossy()
            .into_owned();
        std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
        // Keep an earlier attachment of the same name.
        let (stem, extension) = match name.rsplit_once('.') {
            Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
            None => (name.clone(), String::new()),
        };
        let mut target = folder.join(&name);
        let mut copy = 2;
        while target.exists() && std::fs::read(&target).ok() != std::fs::read(file).ok() {
            target = folder.join(format!("{stem}-{copy}{extension}"));
            copy += 1;
        }
        if !target.exists() {
            std::fs::copy(file, &target).map_err(|err| format!("{}: {err}", file.display()))?;
        }
        let kept = target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(' ', "%20");
        let folder_name = folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(' ', "%20");
        let image = matches!(
            extension.to_ascii_lowercase().as_str(),
            ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".bmp" | ".svg"
        ) || super::is_video(file);
        Ok(format!(
            "{}[{stem}]({folder_name}/{kept})",
            if image { "!" } else { "" }
        ))
    }
}
