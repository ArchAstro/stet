//! Open documents (tabs), the jump list that links them, and crash recovery.
//!
//! The active document lives in the editor's own fields; the others are
//! stashed whole and swapped in when their tab is selected.

use super::{Editor, Effect, Mode};
use crate::buffer::Buffer;
use crate::markdown::Doc;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

const RECOVERY_HEADER: &str = "stet-recovery\t";

pub(super) struct Stash {
    buf: Buffer,
    cursor: usize,
    path: Option<PathBuf>,
    doc: Doc,
    doc_revision: Option<u64>,
    disk_mtime: Option<SystemTime>,
    scroll_line: usize,
    scroll_px: f32,
    doc_id: u64,
    snapshot: Option<u64>,
}

pub struct TabInfo {
    pub title: String,
    pub dirty: bool,
    pub active: bool,
}

fn title_of(path: Option<&Path>) -> String {
    path.and_then(Path::file_name)
        .map_or("Untitled".to_string(), |name| name.to_string_lossy().into_owned())
}

/// FNV-1a, stable across builds, so recovery files keep their names.
fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let temp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

enum Job {
    Write(PathBuf, String),
    Remove(PathBuf),
}

impl Job {
    fn run(self) {
        match self {
            Job::Write(file, contents) => {
                let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")))
                    .and_then(|()| write_atomic(&file, &contents));
            }
            Job::Remove(file) => {
                let _ = std::fs::remove_file(file);
            }
        }
    }
}

type Pending = Arc<(Mutex<usize>, Condvar)>;

/// Recovery files are written in order by one worker thread, so a snapshot
/// (a synced write, several milliseconds) never holds up a keystroke.
#[derive(Default)]
pub(super) struct Disk {
    /// False runs every job at once on the calling thread.
    pub(super) background: bool,
    worker: Option<(Sender<Job>, Pending)>,
}

impl Disk {
    fn submit(&mut self, job: Job) {
        if !self.background {
            return job.run();
        }
        let (sender, pending) = self.worker.get_or_insert_with(|| {
            let (sender, receiver) = channel::<Job>();
            let pending: Pending = Arc::default();
            let done = pending.clone();
            std::thread::spawn(move || {
                for job in receiver {
                    job.run();
                    *done.0.lock().unwrap() -= 1;
                    done.1.notify_all();
                }
            });
            (sender, pending)
        });
        *pending.0.lock().unwrap() += 1;
        if let Err(unsent) = sender.send(job) {
            *pending.0.lock().unwrap() -= 1;
            unsent.0.run();
        }
    }

    /// Waits until everything submitted is on disk.
    fn flush(&self) {
        if let Some((_, pending)) = &self.worker {
            let mut count = pending.0.lock().unwrap();
            while *count > 0 {
                count = pending.1.wait(count).unwrap();
            }
        }
    }
}

impl Editor {
    /// Write recovery snapshots on a worker thread instead of inline.
    pub fn recover_in_background(&mut self, on: bool) {
        self.disk.flush();
        self.disk.background = on;
    }

    /// Waits for pending recovery writes to reach the disk.
    pub fn flush_recovery(&self) {
        self.disk.flush();
    }

    fn stash(&mut self) -> Stash {
        self.close_group();
        self.write_recovery();
        self.vim.clear_pending();
        self.cmdline = None;
        self.anchor = None;
        self.code_cache.borrow_mut().0 = None;
        Stash {
            buf: std::mem::take(&mut self.buf),
            cursor: self.cursor,
            path: self.path.take(),
            doc: std::mem::take(&mut self.doc),
            doc_revision: self.doc_revision.take(),
            disk_mtime: self.disk_mtime.take(),
            scroll_line: self.scroll_line,
            scroll_px: self.scroll_px,
            doc_id: self.doc_id,
            snapshot: self.snapshot.take(),
        }
    }

    fn unstash(&mut self, stash: Stash) {
        self.buf = stash.buf;
        self.cursor = stash.cursor;
        self.path = stash.path;
        self.doc = stash.doc;
        self.doc_revision = stash.doc_revision;
        self.disk_mtime = stash.disk_mtime;
        self.scroll_line = stash.scroll_line;
        self.scroll_px = stash.scroll_px;
        self.doc_id = stash.doc_id;
        self.snapshot = stash.snapshot;
        self.anchor = None;
        self.group_open = false;
        self.code_cache.borrow_mut().0 = None;
        self.mode = if self.config.vim { Mode::Normal } else { Mode::Insert };
        self.clamp_cursor();
    }

    /// Runs `work` with document `index` in place, without the side effects
    /// of switching tabs: the user's mode, command line and pending keys in
    /// the active document are untouched.
    pub(super) fn with_doc<R>(&mut self, index: usize, work: impl FnOnce(&mut Editor) -> R) -> R {
        if index == self.active || index >= self.tabs.len() {
            return work(self);
        }
        let mut other = self.tabs[index].take().expect("inactive tabs are stashed");
        let kept = (self.mode, self.anchor, self.group_open, self.goal_col);
        self.trade(&mut other);
        (self.mode, self.anchor, self.group_open, self.goal_col) = (Mode::Normal, None, false, None);
        let result = work(self);
        self.close_group();
        self.trade(&mut other);
        (self.mode, self.anchor, self.group_open, self.goal_col) = kept;
        self.tabs[index] = Some(other);
        result
    }

    /// Exchanges the active document's state with a stashed one.
    fn trade(&mut self, other: &mut Stash) {
        use std::mem::swap;
        swap(&mut self.buf, &mut other.buf);
        swap(&mut self.cursor, &mut other.cursor);
        swap(&mut self.path, &mut other.path);
        swap(&mut self.doc, &mut other.doc);
        swap(&mut self.doc_revision, &mut other.doc_revision);
        swap(&mut self.disk_mtime, &mut other.disk_mtime);
        swap(&mut self.scroll_line, &mut other.scroll_line);
        swap(&mut self.scroll_px, &mut other.scroll_px);
        swap(&mut self.doc_id, &mut other.doc_id);
        swap(&mut self.snapshot, &mut other.snapshot);
        self.code_cache.borrow_mut().0 = None;
    }

    /// Resets the active slot to an empty, untitled document.
    pub(super) fn blank_document(&mut self) {
        self.set_text("");
        self.path = None;
        self.disk_mtime = None;
        self.fresh_identity();
    }

    pub(super) fn fresh_identity(&mut self) {
        self.next_doc_id += 1;
        self.doc_id = self.next_doc_id;
        self.snapshot = None;
        self.scroll_line = 0;
        self.scroll_px = 0.0;
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn tabs(&self) -> Vec<TabInfo> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| match tab {
                Some(stash) => TabInfo {
                    title: title_of(stash.path.as_deref()),
                    dirty: stash.buf.is_dirty(),
                    active: false,
                },
                None => TabInfo {
                    title: self.file_name(),
                    dirty: self.buf.is_dirty(),
                    active: index == self.active,
                },
            })
            .collect()
    }

    pub fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        let stash = self.stash();
        self.tabs[self.active] = Some(stash);
        let next = self.tabs[index].take().expect("inactive tabs are stashed");
        self.active = index;
        self.unstash(next);
    }

    pub fn cycle_tab(&mut self, delta: isize) {
        let count = self.tabs.len() as isize;
        if count > 1 {
            self.switch_tab((self.active as isize + delta).rem_euclid(count) as usize);
        }
    }

    /// Opens an empty document in a new tab after the active one.
    pub fn new_tab(&mut self) {
        let stash = self.stash();
        self.tabs[self.active] = Some(stash);
        self.active += 1;
        self.tabs.insert(self.active, None);
        self.blank_document();
    }

    fn tab_with(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.as_ref().is_some_and(|stash| stash.path.as_deref() == Some(path)))
    }

    /// Shows `path`: its tab if it is already open, in place when the active
    /// document has no unsaved changes, otherwise in a new tab.
    pub fn open_path(&mut self, path: &Path) -> Result<(), String> {
        self.open_with(path, false)
    }

    /// Like `open_path`, but never replaces the active document.
    pub fn open_in_tab(&mut self, path: &Path) -> Result<(), String> {
        self.open_with(path, true)
    }

    fn open_with(&mut self, path: &Path, new_tab: bool) -> Result<(), String> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if self.path.as_deref() == Some(&path) {
            return Ok(());
        }
        if let Some(index) = self.tab_with(&path) {
            self.switch_tab(index);
            return Ok(());
        }
        let pristine = self.path.is_none() && self.buf.is_empty();
        if self.buf.is_dirty() || (new_tab && !pristine) {
            self.new_tab();
            let opened = self.open(&path);
            if opened.is_err() {
                self.close_tab(true);
            }
            return opened;
        }
        self.open(&path)
    }

    /// Closes the active tab; the last one quits. False if unsaved changes
    /// block it.
    pub fn close_tab(&mut self, force: bool) -> bool {
        if self.buf.is_dirty() && !force {
            self.error("No write since last change (add ! to override)");
            return false;
        }
        self.close_group();
        self.remove_recovery();
        if self.tabs.len() == 1 {
            self.effects.push(Effect::Quit);
            return true;
        }
        self.tabs.remove(self.active);
        self.active = self.active.min(self.tabs.len() - 1);
        let next = self.tabs[self.active].take().expect("inactive tabs are stashed");
        self.unstash(next);
        true
    }

    /// Tabs with unsaved changes.
    pub fn dirty_tabs(&self) -> Vec<usize> {
        self.tabs()
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.dirty)
            .map(|(index, _)| index)
            .collect()
    }

    /// Saves every named document with unsaved changes.
    pub fn save_all(&mut self) -> Result<(), String> {
        let start = self.active;
        let mut result = Ok(());
        for index in self.dirty_tabs() {
            self.switch_tab(index);
            if self.path.is_none() {
                result = Err("an untitled document needs a name (:w <file>)".to_string());
            } else if let Err(err) = self.save(None, false) {
                result = Err(err);
            }
        }
        self.switch_tab(start);
        result
    }

    // ----- jump list -------------------------------------------------------

    fn here(&self) -> Option<(PathBuf, usize)> {
        Some((self.path.clone()?, self.cursor))
    }

    /// Remembers the current place before following a link.
    pub(super) fn push_jump(&mut self) {
        if let Some(here) = self.here() {
            self.back.push(here);
            self.forward.clear();
            if self.back.len() > 200 {
                self.back.remove(0);
            }
        }
    }

    pub(super) fn jump(&mut self, back: bool) {
        let target = if back { self.back.pop() } else { self.forward.pop() };
        let Some((path, cursor)) = target else {
            return self.info(if back {
                "no earlier location"
            } else {
                "no later location"
            });
        };
        let here = self.here();
        match self.open_path(&path) {
            Ok(()) => {
                let other = if back { &mut self.forward } else { &mut self.back };
                other.extend(here);
                self.cursor = cursor.min(self.buf.len());
                self.clamp_cursor();
            }
            Err(err) => self.error(err),
        }
    }

    // ----- crash recovery --------------------------------------------------

    fn recovery_file(&self) -> Option<PathBuf> {
        let dir = self.recovery_dir.as_ref()?;
        Some(match &self.path {
            Some(path) => dir.join(format!("{:016x}.md", stable_hash(&path.to_string_lossy()))),
            None => dir.join(format!("untitled-{}-{}.md", std::process::id(), self.doc_id)),
        })
    }

    /// Snapshots the active document's unsaved text so a crash or power cut
    /// loses nothing. Cheap when nothing changed; call it when input pauses.
    pub fn write_recovery(&mut self) {
        let Some(file) = self.recovery_file() else { return };
        if !self.buf.is_dirty() {
            if self.snapshot.take().is_some() {
                self.disk.submit(Job::Remove(file));
            }
            return;
        }
        if self.snapshot == Some(self.buf.revision()) {
            return;
        }
        let path = self
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let contents = format!("{RECOVERY_HEADER}{path}\n{}", self.buf.to_file_string());
        self.disk.submit(Job::Write(file, contents));
        self.snapshot = Some(self.buf.revision());
    }

    pub(super) fn remove_recovery(&mut self) {
        self.snapshot = None;
        if let Some(file) = self.recovery_file() {
            self.disk.submit(Job::Remove(file));
        }
    }

    fn read_recovery(file: &Path) -> Option<(String, String)> {
        let contents = std::fs::read_to_string(file).ok()?;
        let (header, body) = contents.split_once('\n')?;
        Some((header.strip_prefix(RECOVERY_HEADER)?.to_string(), body.to_string()))
    }

    /// After opening a file: restores a newer unsaved draft, as an undoable
    /// edit on top of what is on disk.
    pub(super) fn apply_recovery(&mut self, forced: bool) {
        let Some(file) = self.recovery_file() else { return };
        self.disk.flush();
        let Some((_, body)) = Self::read_recovery(&file) else {
            if forced {
                self.error("no recovered draft for this file");
            }
            return;
        };
        if body == self.buf.to_file_string() {
            let _ = std::fs::remove_file(file);
            return;
        }
        let draft_time = std::fs::metadata(&file).and_then(|meta| meta.modified()).ok();
        if !forced && self.disk_mtime.is_some() && draft_time < self.disk_mtime {
            return self.error("an older unsaved draft exists; :recover loads it, :w discards it");
        }
        let body = body.replace("\r\n", "\n");
        self.buf.replace(0..self.buf.len(), &body);
        self.cursor = 0;
        self.doc_revision = None;
        self.info("recovered unsaved changes; u returns to the saved version");
    }

    /// At startup: reopens untitled drafts left behind by a crash.
    pub fn recover_untitled(&mut self) {
        let Some(dir) = self.recovery_dir.clone() else { return };
        self.disk.flush();
        let own = format!("untitled-{}-", std::process::id());
        let mut drafts: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                name.starts_with("untitled-") && name.ends_with(".md") && !name.starts_with(&own)
            })
            .collect();
        drafts.sort();
        let start = self.active;
        let mut recovered = 0;
        for draft in drafts {
            let Some((_, body)) = Self::read_recovery(&draft) else {
                continue;
            };
            let _ = std::fs::remove_file(&draft);
            if body.trim().is_empty() {
                continue;
            }
            if !(self.path.is_none() && self.buf.is_empty()) {
                self.new_tab();
            }
            self.buf.replace(0..0, &body.replace("\r\n", "\n"));
            self.doc_revision = None;
            recovered += 1;
        }
        if recovered > 0 {
            self.write_recovery();
            self.switch_tab(start);
            self.info(format!(
                "recovered {recovered} unsaved draft{}",
                if recovered == 1 { "" } else { "s" }
            ));
        }
    }

    /// An orderly exit: whatever is still unsaved was discarded on purpose.
    pub fn shutdown(&mut self) {
        let mut files = Vec::new();
        for index in 0..self.tabs.len() {
            self.switch_tab(index);
            files.extend(self.recovery_file());
        }
        self.recovery_dir = None;
        self.disk.flush();
        for file in files {
            let _ = std::fs::remove_file(file);
        }
    }
}
