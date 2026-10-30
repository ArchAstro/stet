//! Notebook mode in the editor: which documents are notebooks, running
//! their cells, and taking back what the runs produce.
//!
//! The core knows cells and their outputs ([`crate::notebook`]); the shell
//! owns the kernels. Asking to run cells queues them in the document's
//! notebook and leaves an [`Effect::Run`]; the shell reports back through
//! [`Editor::notebook_started`], [`Editor::notebook_output`] and
//! [`Editor::notebook_finished`], by document id and slot uid, so a run keeps
//! landing in the right place while the writer edits or switches tabs.

use super::{Editor, Effect};
use crate::input::{Key, KeyEvent};
use crate::notebook::{self, Cell, Item, Match, Notebook, Status};
use std::path::PathBuf;
use std::time::SystemTime;

/// One document's notebook state.
#[derive(Debug, Default)]
pub struct Book {
    /// Notebook mode is on: cells show their outputs and can run.
    pub on: bool,
    pub notebook: Notebook,
    /// The document's cells and their slots, as of `revision`.
    pub cells: Vec<Cell>,
    pub matches: Vec<Option<Match>>,
    revision: Option<u64>,
    /// Where the document is, for its output file.
    pub path: Option<PathBuf>,
    /// What the shell says of each kernel this document uses.
    pub kernels: Vec<KernelState>,
    /// The front matter asked for notebook mode, as of the last look.
    asked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelState {
    /// What to show: `python3`.
    pub name: String,
    pub busy: bool,
    /// Starting, or `None` once it answers.
    pub starting: bool,
    /// Why it stopped, if it did.
    pub dead: Option<String>,
}

impl Book {
    /// The slot showing under cell `at`, and whether it is stale.
    pub fn output_of(&self, at: usize) -> Option<(&notebook::Slot, bool)> {
        let found = self.matches.get(at).copied().flatten()?;
        Some((&self.notebook.slots[found.slot], found.stale))
    }

    /// A cell named the way an assistant names one: `#id` or `id`, or its
    /// number among the document's cells, from 1.
    pub fn find_cell(&self, name: &str) -> Result<usize, String> {
        let name = name.trim();
        let id = name.strip_prefix('#').unwrap_or(name);
        if let Some(at) = self.cells.iter().position(|cell| cell.info.id.as_deref() == Some(id)) {
            return Ok(at);
        }
        match name.parse::<usize>() {
            Ok(n) if (1..=self.cells.len()).contains(&n) => Ok(n - 1),
            Ok(n) => Err(format!("there is no cell {n}; the document has {}", self.cells.len())),
            Err(_) => Err(format!("no cell has the id {id}")),
        }
    }

    /// The cell whose output goes under `line`, the closing fence.
    pub fn cell_ending_at(&self, line: usize) -> Option<usize> {
        let at = self.cells.partition_point(|cell| cell.last_line() < line);
        self.cells.get(at).filter(|cell| cell.last_line() == line).map(|_| at)
    }

    pub fn cell_at_line(&self, line: usize) -> Option<usize> {
        self.cells.iter().position(|cell| cell.fence.contains(&line))
    }

    /// Cell `at` is shown folded: its opening fence and outputs, no code.
    pub fn is_folded(&self, at: usize) -> bool {
        self.on
            && self
                .cells
                .get(at)
                .is_some_and(|cell| self.notebook.folded.contains(&cell.fold_key()))
    }

    /// The folded cell whose hidden lines include `line`.
    pub fn folded_over(&self, line: usize) -> Option<usize> {
        let at = self.cell_at_line(line)?;
        (self.is_folded(at) && self.cells[at].folded_lines().contains(&line)).then_some(at)
    }

    /// `line` is inside a folded cell and takes no room on screen.
    pub fn hidden(&self, line: usize) -> bool {
        self.folded_over(line).is_some()
    }

    /// The folder pictures from runs are written to.
    pub fn files_dir(&self) -> Option<PathBuf> {
        self.path.as_deref().map(notebook::files_dir)
    }
}

/// One cell to run, as the shell receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellRun {
    /// The slot the run reports to.
    pub uid: u64,
    pub lang: String,
    /// The `kernel=` attribute, if the cell names one.
    pub kernel: Option<String>,
    pub code: String,
}

/// What `:fold` does to the cells it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    Toggle,
    Close,
    Open,
}

/// Which cells `:run` means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// The cell at the cursor.
    Here,
    /// One cell, by position among the document's cells.
    At(usize),
    All,
    /// Cells whose code changed since they ran, or that never ran.
    Stale,
    /// The cell at the cursor and every one before it.
    Above,
    /// The cell at the cursor and every one after it.
    Below,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The front matter asks for notebook mode: `kernel:` or `notebook: true`.
fn wants_notebook(text: &str) -> bool {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    lines
        .take_while(|line| line.trim_end() != "---" && line.trim_end() != "...")
        .any(|line| {
            let line = line.trim();
            line.starts_with("kernel:") || line == "notebook: true"
        })
}

impl Editor {
    /// The id of the document in front, which notebook reports name.
    pub fn doc_id(&self) -> u64 {
        self.doc_id
    }

    /// The ids of every open document.
    pub fn doc_ids(&self) -> Vec<u64> {
        let mut ids = vec![self.doc_id];
        ids.extend(self.tabs.iter().flatten().map(|stash| stash.doc_id));
        ids
    }

    /// The notebook of the document in front, if notebook mode is on.
    pub fn notebook(&self) -> Option<&Book> {
        if self.in_margin() {
            return None;
        }
        self.books.get(&self.doc_id).filter(|book| book.on)
    }

    pub fn notebook_of(&self, doc: u64) -> Option<&Book> {
        self.books.get(&doc)
    }

    /// Bring the notebook of the document in front up to date with its text.
    /// Run from [`Editor::refresh`].
    pub(super) fn refresh_book(&mut self) {
        if self.in_margin() {
            return;
        }
        let revision = self.buf.revision();
        let path = self.path.clone();
        let fresh = !self.books.contains_key(&self.doc_id);
        let book = self.books.entry(self.doc_id).or_default();
        if fresh || book.path != path {
            book.path = path.clone();
            if let Some(path) = &path {
                let stored = notebook::load(path);
                if !stored.slots.is_empty() || !stored.folded.is_empty() {
                    book.notebook = stored;
                    book.on = true;
                }
            }
            book.revision = None;
        }
        if book.revision == Some(revision) {
            return;
        }
        let text = self.buf.text();
        // Asking in the front matter turns it on; the writer can still turn
        // it off while the front matter stays.
        let asked = wants_notebook(&text);
        if asked && !book.asked {
            book.on = true;
        }
        book.asked = asked;
        let lines: Vec<&str> = text.split('\n').collect();
        book.cells = notebook::cells(&lines, &self.doc.fences);
        book.matches = book.notebook.matching(&book.cells);
        book.notebook.settle(&book.matches);
        book.revision = Some(revision);
    }

    /// Fold or unfold code cells: `which` is empty for the cell at the
    /// cursor, `all`, or a cell named as [`Book::find_cell`] takes it.
    pub fn fold(&mut self, which: &str, how: Fold) {
        self.refresh();
        let cursor_line = self.buf.line_of(self.cursor);
        let Some(book) = self.books.get_mut(&self.doc_id).filter(|book| book.on) else {
            return self.error("folding is for notebook cells; turn on notebook mode with :notebook");
        };
        let targets: Vec<usize> = match which.trim() {
            "" => match book.cell_at_line(cursor_line) {
                Some(at) => vec![at],
                None => return self.error("the cursor is not in a cell"),
            },
            "all" => (0..book.cells.len()).collect(),
            name => match book.find_cell(name) {
                Ok(at) => vec![at],
                Err(err) => return self.error(err),
            },
        };
        // Toggling several cells folds them all unless all are folded already.
        let close = match how {
            Fold::Close => true,
            Fold::Open => false,
            Fold::Toggle => !targets.iter().all(|&at| book.is_folded(at)),
        };
        for &at in &targets {
            let key = book.cells[at].fold_key();
            let changed = if close {
                book.notebook.folded.insert(key)
            } else {
                book.notebook.folded.remove(&key)
            };
            book.notebook.dirty |= changed;
        }
        if let Some(at) = book.folded_over(cursor_line) {
            self.cursor = self.buf.line_start(book.cells[at].fence.start);
        }
        let n = targets.len();
        self.info(match (close, n) {
            (true, 1) => "folded the cell".to_string(),
            (false, 1) => "unfolded the cell".to_string(),
            (true, _) => format!("folded {n} cells"),
            (false, _) => format!("unfolded {n} cells"),
        });
    }

    /// A folded cell the cursor has landed in, by a jump or an edit, opens:
    /// the cursor is never somewhere the writer cannot see.
    pub(super) fn reveal_cursor(&mut self) {
        let line = self.buf.line_of(self.cursor);
        let Some(book) = self.books.get_mut(&self.doc_id) else {
            return;
        };
        if let Some(at) = book.folded_over(line) {
            let key = book.cells[at].fold_key();
            book.notebook.folded.remove(&key);
            book.notebook.dirty = true;
        }
    }

    /// The line `n` lines up or down from `line`, a folded cell counting as
    /// its opening fence alone.
    pub(super) fn step_over_folds(&self, line: usize, down: bool, n: usize) -> usize {
        let last = self.buf.line_count() - 1;
        let Some(book) = self.books.get(&self.doc_id) else {
            return if down {
                (line + n).min(last)
            } else {
                line.saturating_sub(n)
            };
        };
        let mut at = line;
        for _ in 0..n {
            let next = if down { (at + 1).min(last) } else { at.saturating_sub(1) };
            at = match book.folded_over(next) {
                Some(cell) if down => book.cells[cell].fence.end.min(last),
                Some(cell) => book.cells[cell].fence.start,
                None => next,
            };
            // A folded cell at the end of the document: stay on its summary.
            if book.hidden(at) {
                at = book.cells[book.folded_over(at).unwrap_or_default()].fence.start;
            }
        }
        at
    }

    /// `:notebook`: turn notebook mode on or off for this document.
    pub fn toggle_notebook(&mut self, on: Option<bool>) {
        self.refresh();
        let Some(book) = self.books.get_mut(&self.doc_id) else {
            return self.error("the margin cannot be a notebook");
        };
        book.on = on.unwrap_or(!book.on);
        let (on, count) = (book.on, book.cells.len());
        if on {
            self.info(format!("notebook mode · {count} cells · shift-enter runs one"));
        } else {
            self.info("notebook mode off");
        }
    }

    /// The cell the cursor is in, by position.
    pub fn cell_at_cursor(&mut self) -> Option<usize> {
        self.refresh();
        let line = self.buf.line_of(self.cursor);
        self.notebook()?.cell_at_line(line)
    }

    /// A cell can run if it names a language and the shell has something
    /// for it (or has not said yet), and it is not marked `.norun`.
    fn runnable(&self, cell: &Cell) -> bool {
        let lang = &cell.info.lang;
        !lang.is_empty()
            && !cell.info.has_class("norun")
            && (cell.info.attr("kernel").is_some()
                || self.runnable_tags.as_ref().is_none_or(|tags| tags.contains(lang)))
    }

    /// Queue cells to run and hand them to the shell. `ran_by` names the
    /// assistant that asked, if one did. Returns the slot uids, in order.
    pub fn run_cells(&mut self, which: Which, ran_by: Option<String>) -> Result<Vec<u64>, String> {
        self.refresh();
        let line = self.buf.line_of(self.cursor);
        let doc = self.doc_id;
        let Some(book) = self.books.get(&doc).filter(|book| book.on) else {
            return Err("notebook mode is off (:notebook turns it on)".into());
        };
        let here = book.cell_at_line(line);
        let count = book.cells.len();
        let chosen: Vec<usize> = match which {
            Which::Here => vec![here.ok_or("the cursor is not in a cell")?],
            Which::At(at) if at < count => vec![at],
            Which::At(at) => return Err(format!("there is no cell {}", at + 1)),
            Which::All => (0..count).collect(),
            Which::Stale => (0..count)
                .filter(|&at| match book.output_of(at) {
                    None => true,
                    Some((slot, stale)) => stale || slot.status != Status::Ok,
                })
                .collect(),
            Which::Above => (0..=here.ok_or("the cursor is not in a cell")?).collect(),
            Which::Below => (here.ok_or("the cursor is not in a cell")?..count).collect(),
        };
        let only = (chosen.len() == 1).then(|| book.cells[chosen[0]].info.lang.clone());
        let chosen: Vec<usize> = chosen
            .into_iter()
            .filter(|&at| self.runnable(&book.cells[at]))
            .collect();
        if chosen.is_empty() {
            return Err(match only {
                Some(lang) if lang.is_empty() => "this block names no language to run".into(),
                Some(lang) => format!("nothing on this machine runs {lang}"),
                None => "no cells to run".into(),
            });
        }
        let book = self.books.get_mut(&doc).expect("checked");
        let mut runs = Vec::new();
        for at in chosen {
            let uid = book.notebook.queue(&book.cells, at, ran_by.clone());
            let cell = &book.cells[at];
            runs.push(CellRun {
                uid,
                lang: cell.info.lang.clone(),
                kernel: cell.info.attr("kernel").map(str::to_string),
                code: cell.code.clone(),
            });
        }
        book.matches = book.notebook.matching(&book.cells);
        let uids = runs.iter().map(|run| run.uid).collect();
        let cwd = book.path.as_deref().and_then(|path| path.parent()).map(PathBuf::from);
        self.effects.push(Effect::Run { doc, cwd, runs });
        Ok(uids)
    }

    /// `:run [all|stale|above|below|N]`.
    pub(super) fn run_command_cells(&mut self, arg: &str) {
        let which = match arg {
            "" => Which::Here,
            "all" => Which::All,
            "stale" => Which::Stale,
            "above" => Which::Above,
            "below" => Which::Below,
            number => match number.parse::<usize>() {
                Ok(n) if n >= 1 => Which::At(n - 1),
                _ => {
                    return self.error(format!(
                        "not a cell to run: {number} (all, stale, above, below or a number)"
                    ));
                }
            },
        };
        match self.run_cells(which, None) {
            Ok(uids) if uids.len() > 1 => self.info(format!("running {} cells", uids.len())),
            Ok(_) => {}
            Err(err) => self.error(err),
        }
    }

    /// Shift-Enter, or the command key with Enter, in a cell: run it.
    pub(super) fn notebook_key(&mut self, event: &KeyEvent) -> bool {
        let primary = match self.primary {
            super::Primary::Super => event.mods.sup,
            super::Primary::Ctrl => event.mods.ctrl,
        };
        let run_key = event.key == Key::Enter && !event.mods.alt && (event.mods.shift ^ primary);
        if !run_key || self.cmdline.is_some() || self.notebook().is_none() {
            return false;
        }
        if self.cell_at_cursor().is_none() {
            return false;
        }
        self.close_group();
        if let Err(err) = self.run_cells(Which::Here, None) {
            self.error(err);
        }
        true
    }

    /// Clear the outputs of the cell at the cursor, or of every cell.
    pub(super) fn clear_outputs(&mut self, all: bool) {
        let here = self.cell_at_cursor();
        let Some(book) = self.books.get_mut(&self.doc_id).filter(|book| book.on) else {
            return self.error("notebook mode is off");
        };
        let slots: Vec<usize> = match (all, here) {
            (true, _) => (0..book.notebook.slots.len()).collect(),
            (false, Some(at)) => book.matches[at].map(|found| found.slot).into_iter().collect(),
            (false, None) => return self.error("the cursor is not in a cell"),
        };
        let mut keep: Vec<bool> = vec![true; book.notebook.slots.len()];
        for slot in slots {
            // A run still going keeps its slot; it would only come back.
            if book.notebook.slots[slot].status.is_done() {
                keep[slot] = false;
            }
        }
        let mut at = 0;
        book.notebook.slots.retain(|_| {
            at += 1;
            keep[at - 1]
        });
        book.notebook.dirty = true;
        book.matches = book.notebook.matching(&book.cells);
        self.save_book(self.doc_id);
    }

    /// Tell the shell to stop this document's runs, or restart its kernels.
    pub(super) fn kernel_command(&mut self, restart: bool) {
        if self.notebook().is_none() {
            return self.error("notebook mode is off");
        }
        let doc = self.doc_id;
        self.effects.push(if restart {
            Effect::RestartKernels { doc }
        } else {
            Effect::Interrupt { doc }
        });
    }

    // ----- reports from the shell ---------------------------------------

    pub fn notebook_started(&mut self, doc: u64, uid: u64) {
        if let Some(book) = self.books.get_mut(&doc) {
            book.notebook.started(uid, now_ms());
        }
    }

    /// An output for slot `uid`. Pictures are written to the notebook's
    /// output folder when the document has a place on disk.
    pub fn notebook_output(&mut self, doc: u64, uid: u64, mut item: Item) {
        let Some(book) = self.books.get_mut(&doc) else { return };
        if let (Item::Display { data } | Item::Result { data }, Some(dir)) = (&mut item, book.files_dir()) {
            notebook::store_files(data, &dir);
        }
        book.notebook.output(uid, item);
    }

    pub fn notebook_clear(&mut self, doc: u64, uid: u64, wait: bool) {
        if let Some(book) = self.books.get_mut(&doc) {
            book.notebook.clear(uid, wait);
        }
    }

    /// A run is over. `took_ms`, as the kernel measured it, beats the time
    /// between reports, which arrive in batches.
    pub fn notebook_finished(
        &mut self,
        doc: u64,
        uid: u64,
        status: Status,
        count: Option<usize>,
        took_ms: Option<u64>,
    ) {
        let Some(book) = self.books.get_mut(&doc) else { return };
        book.notebook.finished(uid, status, count, now_ms());
        if let Some(took) = took_ms {
            book.notebook.took(uid, took);
        }
        book.matches = book.notebook.matching(&book.cells);
        if book.notebook.busy().next().is_none() {
            self.save_book(doc);
        }
    }

    /// The shell's view of the document's kernels, for the status line.
    pub fn set_kernels(&mut self, doc: u64, kernels: Vec<KernelState>) {
        if let Some(book) = self.books.get_mut(&doc) {
            book.kernels = kernels;
        }
    }

    /// The languages the shell can run, as fence tags. `None` until known.
    pub fn set_runnable(&mut self, tags: Option<Vec<String>>) {
        self.runnable_tags = tags;
    }

    /// Forget the notebooks of documents that are no longer open. Returns
    /// their ids, for the shell to shut their kernels down.
    pub fn closed_notebooks(&mut self) -> Vec<u64> {
        let open = self.doc_ids();
        let closed: Vec<u64> = self.books.keys().copied().filter(|doc| !open.contains(doc)).collect();
        for doc in &closed {
            self.books.remove(doc);
        }
        closed
    }

    /// Write the document's outputs beside it, if it has a place on disk.
    fn save_book(&mut self, doc: u64) {
        let Some(book) = self.books.get_mut(&doc) else { return };
        let Some(path) = book.path.clone().filter(|_| book.notebook.dirty) else {
            return;
        };
        match notebook::save(&path, &book.notebook, &book.cells) {
            Ok(()) => book.notebook.dirty = false,
            Err(err) => self.error(format!("could not keep the outputs: {err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_turns_notebook_mode_on() {
        assert!(wants_notebook("---\ntitle: x\nkernel: python3\n---\n# hi"));
        assert!(wants_notebook("---\nnotebook: true\n---\n"));
        assert!(!wants_notebook("---\ntitle: x\n---\nkernel: in the body\n"));
        assert!(!wants_notebook("# kernel: no front matter"));
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;
    use crate::input::parse_keys;
    use crate::{Config, MemoryClipboard};

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(Config::default(), Box::new(MemoryClipboard::default()));
        ed.primary = super::super::Primary::Super;
        let cursor = text.find('|').map_or(0, |at| text[..at].chars().count());
        ed.set_text(&text.replacen('|', "", 1));
        ed.cursor = cursor;
        ed
    }

    fn runs(ed: &mut Editor) -> Vec<CellRun> {
        ed.take_effects()
            .into_iter()
            .flat_map(|effect| match effect {
                Effect::Run { runs, .. } => runs,
                _ => Vec::new(),
            })
            .collect()
    }

    const DOC: &str = "---\nkernel: python3\n---\n# t\n\n```python\nx = 1\n```\n\n```sh {kernel=bash}\necho |hi\n```\n\n```\nplain\n```\n";

    #[test]
    fn shift_enter_runs_the_cell_at_the_cursor() {
        let mut ed = editor(DOC);
        ed.refresh();
        assert!(ed.notebook().is_some(), "front matter turns it on");
        for event in parse_keys("<S-CR>") {
            ed.handle_key(event);
        }
        let runs = runs(&mut ed);
        assert_eq!(runs.len(), 1);
        assert_eq!(
            (runs[0].lang.as_str(), runs[0].kernel.as_deref(), runs[0].code.as_str()),
            ("sh", Some("bash"), "echo hi")
        );
        assert_eq!(ed.buf.text(), DOC.replace('|', ""), "the text is untouched");
        let book = ed.notebook().unwrap();
        assert_eq!(book.output_of(1).unwrap().0.status, Status::Queued);
    }

    fn keys(ed: &mut Editor, notation: &str) {
        for event in parse_keys(notation) {
            ed.handle_key(event);
        }
        ed.refresh();
    }

    fn line(ed: &Editor) -> usize {
        ed.buf.line_of(ed.cursor)
    }

    // DOC's cells: python on lines 5-7, sh on 9-11 (cursor on 10), plain on 13-15.

    #[test]
    fn fold_hides_the_code_and_keeps_the_opening_fence() {
        let mut ed = editor(DOC);
        ed.refresh();
        ed.run_command("fold");
        ed.refresh();
        let book = ed.notebook().unwrap();
        assert!(book.is_folded(1));
        assert!(!book.hidden(9), "the opening fence is the summary");
        assert!(book.hidden(10) && book.hidden(11), "code and closing fence are hidden");
        assert!(!book.hidden(12));
        assert_eq!(line(&ed), 9, "the cursor moves out of what it hid, onto the summary");
        assert_eq!(ed.buf.text(), DOC.replace('|', ""), "folding never touches the text");
    }

    #[test]
    fn vim_fold_keys_toggle_one_cell_or_all() {
        let mut ed = editor(DOC);
        ed.refresh();
        keys(&mut ed, "za");
        assert!(ed.notebook().unwrap().is_folded(1));
        keys(&mut ed, "za");
        assert!(!ed.notebook().unwrap().is_folded(1));
        keys(&mut ed, "zM");
        assert!((0..3).all(|at| ed.notebook().unwrap().is_folded(at)));
        keys(&mut ed, "zR");
        assert!((0..3).all(|at| !ed.notebook().unwrap().is_folded(at)));
    }

    #[test]
    fn line_motion_steps_over_a_folded_cell() {
        let mut ed = editor(DOC);
        ed.config.visual_line_motion = false;
        ed.refresh();
        ed.run_command("fold 2");
        ed.cursor = ed.buf.line_start(8);
        keys(&mut ed, "j");
        assert_eq!(line(&ed), 9, "onto the summary");
        keys(&mut ed, "j");
        assert_eq!(line(&ed), 12, "past the hidden code");
        keys(&mut ed, "k");
        assert_eq!(line(&ed), 9, "back onto the summary");
        assert!(ed.notebook().unwrap().is_folded(1), "stepping over leaves it folded");
    }

    #[test]
    fn landing_inside_a_folded_cell_opens_it() {
        let mut ed = editor(DOC);
        ed.refresh();
        ed.run_command("fold all");
        ed.run_command("11");
        ed.refresh();
        let book = ed.notebook().unwrap();
        assert!(!book.is_folded(1), "a jump into hidden code unfolds that cell");
        assert!(book.is_folded(0) && book.is_folded(2), "and only that cell");
    }

    #[test]
    fn a_fold_follows_its_cell_through_edits() {
        let mut ed = editor(&DOC.replace("```sh {kernel=bash}", "```sh {#greet kernel=bash}"));
        ed.refresh();
        ed.run_command("fold greet");
        ed.cursor = 0;
        keys(&mut ed, "Onew line<Esc>");
        let book = ed.notebook().unwrap();
        assert!(book.is_folded(1), "keyed by #id, not by line");
        assert!(book.hidden(11) && !book.hidden(10));
    }

    #[test]
    fn folds_are_kept_with_the_outputs_and_forgotten_with_their_cells() {
        let mut ed = editor(DOC);
        ed.refresh();
        ed.run_command("fold all");
        let book = ed.notebook().unwrap();
        let value = book.notebook.to_json(&book.cells);
        assert_eq!(value["folded"].as_array().map(Vec::len), Some(3));
        let restored = Notebook::from_json(&value);
        assert_eq!(restored.folded, book.notebook.folded);
        let fewer = &book.cells[..1];
        assert_eq!(book.notebook.to_json(fewer)["folded"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn folding_needs_notebook_mode() {
        let mut ed = editor("# plain\n\n```python\nx|\n```\n");
        ed.refresh();
        ed.run_command("fold");
        assert!(ed.notebook().is_none_or(|book| !book.is_folded(0)));
    }

    #[test]
    fn cells_are_found_by_id_or_number() {
        let mut ed = editor(&DOC.replace("```python", "```python {#load}"));
        ed.refresh();
        let book = ed.notebook().unwrap();
        assert_eq!(book.find_cell("#load"), Ok(0));
        assert_eq!(book.find_cell("load"), Ok(0));
        assert_eq!(book.find_cell("2"), Ok(1));
        assert!(book.find_cell("9").unwrap_err().contains("has 3"));
        assert!(book.find_cell("nope").unwrap_err().contains("nope"));
    }

    #[test]
    fn run_all_skips_blocks_without_a_language_and_unrunnable_ones() {
        let mut ed = editor(DOC);
        ed.set_runnable(Some(vec!["python".into()]));
        ed.run_command(":run all".trim_start_matches(':'));
        let langs: Vec<String> = runs(&mut ed).into_iter().map(|run| run.lang).collect();
        // `sh` has no runner, but names its kernel, so it is the shell's to judge.
        assert_eq!(langs, ["python", "sh"]);
    }

    #[test]
    fn reports_land_in_their_slot_and_stale_follows_edits() {
        let mut ed = editor(DOC);
        let doc = ed.doc_id();
        let uid = ed.run_cells(Which::At(0), Some("Claude".into())).unwrap()[0];
        ed.notebook_started(doc, uid);
        ed.notebook_output(
            doc,
            uid,
            Item::Stream {
                stderr: false,
                text: "1\n".into(),
            },
        );
        ed.notebook_finished(doc, uid, Status::Ok, Some(1), None);
        let (slot, stale) = ed.notebook().unwrap().output_of(0).unwrap();
        assert_eq!(
            (slot.status, slot.count, stale, slot.ran_by.as_deref()),
            (Status::Ok, Some(1), false, Some("Claude"))
        );
        assert_eq!(ed.notebook().unwrap().cell_ending_at(7), Some(0));

        // Edit the code: the output stays, stale; `:run stale` picks it up.
        ed.cursor = ed.buf.text().find("x = 1").unwrap() + 4;
        ed.insert_text("2");
        ed.refresh();
        assert!(ed.notebook().unwrap().output_of(0).unwrap().1);
        ed.take_effects();
        ed.run_command("run stale");
        assert_eq!(runs(&mut ed).len(), 2, "the edited cell and the one that never ran");
    }

    #[test]
    fn errors_are_plain() {
        let mut ed = editor("# no notebook\n\n```python\nx\n```\n");
        assert!(ed.run_cells(Which::All, None).unwrap_err().contains(":notebook"));
        ed.run_command("notebook");
        ed.set_runnable(Some(vec![]));
        assert!(ed.run_cells(Which::At(0), None).unwrap_err().contains("runs python"));
    }

    #[test]
    fn outputs_are_kept_beside_the_document_and_come_back() {
        let dir = std::env::temp_dir().join(format!("stet-cells-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("q3.md");
        std::fs::write(&path, "```sh\necho a\n```\n").unwrap();
        let mut ed = editor("");
        ed.open(&path).unwrap();
        ed.run_command("notebook");
        let doc = ed.doc_id();
        let uid = ed.run_cells(Which::At(0), None).unwrap()[0];
        ed.notebook_output(
            doc,
            uid,
            Item::Stream {
                stderr: false,
                text: "a\n".into(),
            },
        );
        ed.notebook_finished(doc, uid, Status::Ok, Some(1), None);
        assert!(notebook::outputs_path(&path).exists());

        let mut again = editor("");
        again.open(&path).unwrap();
        again.refresh();
        let book = again.notebook().expect("outputs on disk turn notebook mode on");
        assert_eq!(
            book.output_of(0).unwrap().0.outputs,
            vec![Item::Stream {
                stderr: false,
                text: "a\n".into()
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
