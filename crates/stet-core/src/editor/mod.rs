//! The editor: buffer, cursor, modes and every key-driven behaviour. The
//! shell feeds it `KeyEvent`s, text and mouse positions, then drains `Effect`s
//! for the things only a shell can do (layout-aware motion, dialogs, quitting).

mod command;
mod links;
mod margin;
mod menu;
mod motion;
mod palette;
mod paste;
mod pins;
mod remote;
mod sheet;
mod sidebar;
mod suggest;
mod tables;
mod tabs;
mod vim;

#[cfg(test)]
mod tests;

pub use links::{Backlink, Link, link_in, rewrite_destinations};
pub use margin::margin_path;
pub use menu::{ContextMenu, MenuAt, MenuItem};
pub use motion::Matcher;
pub use palette::{Act, Item, Palette, PaletteKind};
pub use paste::{Clip, is_picture, is_video};
pub use pins::Pin;
pub use remote::{Applied, RemoteEdit, Session, Snapshot, Target};
pub use sheet::{Button, Field, Row, Sheet};
pub use sidebar::{Entry, EntryKind, Sidebar};
pub use tabs::TabInfo;

use crate::buffer::Buffer;
use crate::config::Config;
use crate::highlight::{Highlighter, Lines};
use crate::input::{Key, KeyEvent, Mods};
use crate::markdown::{self, Doc};
use crate::theme::{Theme, Themes};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// `(analysis revision, block index → highlighted lines)`.
type CodeCache = (Option<u64>, HashMap<usize, Option<Lines>>);

pub trait Clipboard {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str);
    /// Everything on offer: text, HTML, copied files.
    fn contents(&mut self) -> Clip {
        Clip {
            text: self.get(),
            ..Clip::default()
        }
    }
    /// The picture on the clipboard, as a PNG file's bytes.
    fn image(&mut self) -> Option<Vec<u8>> {
        None
    }
    /// Text, with an HTML rendering for programs that prefer one.
    fn set_rich(&mut self, text: &str, _html: &str) {
        self.set(text)
    }
}

#[derive(Default)]
pub struct MemoryClipboard(pub Option<String>);

impl Clipboard for MemoryClipboard {
    fn get(&mut self) -> Option<String> {
        self.0.clone()
    }
    fn set(&mut self, text: &str) {
        self.0 = Some(text.to_string());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
}

impl Mode {
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }
}

/// The platform's command modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primary {
    Super,
    Ctrl,
}

/// What a drag of the mouse extends the selection by, and from where.
#[derive(Clone, Debug)]
enum Drag {
    Chars(usize),
    /// From the word a double click selected.
    Words(Range<usize>),
    /// From the line a triple click selected.
    Lines(Range<usize>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollTo {
    Center,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Move the cursor by wrapped display lines; the shell owns layout.
    VisualMove(isize),
    Scroll(ScrollTo),
    /// Exit now; nothing is left to ask.
    Quit,
    /// Close the active tab, asking about unsaved changes first.
    Close,
    /// Close the window, asking about every unsaved document first.
    CloseAll,
    /// Open a URL or a non-text file with the system's default app.
    OpenUrl(String),
    FontChanged,
    /// The writer's message for the connected assistant, with what was
    /// selected (char range and text) when they began typing it.
    AgentMessage {
        text: String,
        selection: Option<(Range<usize>, String)>,
    },
    /// Open the picture on the cursor's line for retouching.
    EditImage,
    /// Offer to publish the document.
    Publish,
    /// A sheet's button was pressed.
    Sheet {
        sheet: &'static str,
        button: &'static str,
    },
    OpenDialog,
    SaveAsDialog,
    ThemeChanged,
    /// Font size step; 0 resets.
    Zoom(i32),
    ToggleFullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdKind {
    Command,
    SearchForward,
    SearchBackward,
    /// A message for the connected assistant.
    Agent,
}

/// An assistant attached through `stet ctl wait`, as the shell reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    /// Waiting for a message, as opposed to working on one.
    pub listening: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdLine {
    pub kind: CmdKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub error: bool,
}

#[derive(Clone, Debug)]
enum Input {
    Key(KeyEvent),
    Text(String),
}

/// Where the cursor belongs after an edit. They differ only when the edit was
/// recorded as suggestion markup.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EditPos {
    /// Before the edit.
    pub start: usize,
    /// After the inserted text (inside the markup, so typing extends it), or
    /// after the markup for a deletion.
    pub end: usize,
}

pub struct Editor {
    pub buf: Buffer,
    pub cursor: usize,
    /// Selection anchor: inclusive in visual modes, exclusive otherwise.
    pub anchor: Option<usize>,
    pub mode: Mode,
    pub path: Option<PathBuf>,
    pub config: Config,
    pub themes: Themes,
    pub theme: Theme,
    pub message: Option<Message>,
    pub cmdline: Option<CmdLine>,
    /// Edits are recorded as suggestion markup instead of applied.
    pub suggesting: bool,
    pub primary: Primary,
    /// Visible text rows, kept current by the shell for page motions.
    pub view_rows: usize,
    clipboard: Box<dyn Clipboard>,
    /// The text this editor last put on the clipboard.
    copied: Option<String>,
    effects: Vec<Effect>,
    doc: Doc,
    doc_revision: Option<u64>,
    goal_col: Option<usize>,
    keep_goal: bool,
    group_open: bool,
    drag: Option<Drag>,
    disk_mtime: Option<SystemTime>,
    rng: u64,
    vim: vim::State,
    /// The pop-up menu, when open.
    pub palette: Option<Palette>,
    /// The right-click menu, when open.
    pub context_menu: Option<ContextMenu>,
    /// The card of settings risen from the bottom of the window, if any.
    pub sheet: Option<Sheet>,
    /// The connected assistant, kept current by the shell.
    pub agent: Option<Agent>,
    /// The selection when the message prompt opened.
    agent_selection: Option<(Range<usize>, String)>,
    /// User key bindings, checked before the built-in ones.
    keymap: Vec<Mapping>,
    map_pending: Vec<KeyEvent>,
    /// Keys produced by a binding are not themselves remapped.
    mapping: bool,
    pub sidebar: Sidebar,
    /// Installed font families, supplied by the shell for the font picker.
    pub font_families: Vec<String>,
    /// Scroll anchor: the first visible line, and pixels scrolled into it.
    /// The shell owns the meaning; it lives here so each tab keeps its own.
    pub scroll_line: usize,
    pub scroll_px: f32,
    /// Where crash-recovery snapshots are kept; `None` turns them off.
    pub recovery_dir: Option<PathBuf>,
    /// The margin pane is shown beside the document.
    pub margin_open: bool,
    /// The buffer in the editor's own fields is a margin.
    margin_active: bool,
    /// The document, while its margin has the keyboard.
    margin_parent: Option<tabs::Stash>,
    /// Margins of open documents that are not in a pane right now, by the
    /// document's path.
    margins: HashMap<PathBuf, tabs::Stash>,
    reveal_other: bool,
    pin_state: pins::Pins,
    /// Open documents; the active slot is `None` (its state is in `self`).
    tabs: Vec<Option<tabs::Stash>>,
    pub active: usize,
    doc_id: u64,
    next_doc_id: u64,
    disk: tabs::Disk,
    /// Buffer revision of the last recovery snapshot.
    snapshot: Option<u64>,
    back: Vec<(PathBuf, usize)>,
    forward: Vec<(PathBuf, usize)>,
    highlighter: RefCell<Highlighter>,
    /// Highlighted code blocks of the current analysis, by block index.
    code_cache: RefCell<CodeCache>,
    /// Tables of the current analysis.
    table_cache: RefCell<tables::TableCache>,
}

impl Editor {
    pub fn new(config: Config, clipboard: Box<dyn Clipboard>) -> Editor {
        let themes = Themes::default();
        let theme = themes.get(&config.theme).unwrap_or(themes.default_theme()).clone();
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |elapsed| elapsed.as_nanos() as u64);
        Editor {
            buf: Buffer::default(),
            cursor: 0,
            anchor: None,
            mode: if config.vim { Mode::Normal } else { Mode::Insert },
            path: None,
            themes,
            theme,
            message: None,
            cmdline: None,
            suggesting: false,
            primary: if cfg!(target_os = "macos") {
                Primary::Super
            } else {
                Primary::Ctrl
            },
            view_rows: 30,
            clipboard,
            copied: None,
            effects: Vec::new(),
            doc: Doc::default(),
            doc_revision: None,
            goal_col: None,
            keep_goal: false,
            group_open: false,
            drag: None,
            disk_mtime: None,
            rng: seed | 1,
            vim: vim::State::default(),
            palette: None,
            context_menu: None,
            sheet: None,
            agent: None,
            agent_selection: None,
            keymap: Mapping::compile(&config.keys),
            map_pending: Vec::new(),
            mapping: false,
            sidebar: Sidebar::default(),
            font_families: Vec::new(),
            scroll_line: 0,
            scroll_px: 0.0,
            recovery_dir: None,
            margin_open: false,
            margin_active: false,
            margin_parent: None,
            margins: HashMap::new(),
            reveal_other: false,
            pin_state: pins::Pins::default(),
            tabs: vec![None],
            active: 0,
            doc_id: 0,
            next_doc_id: 0,
            disk: tabs::Disk::default(),
            snapshot: None,
            back: Vec::new(),
            forward: Vec::new(),
            highlighter: RefCell::new(Highlighter::default()),
            code_cache: RefCell::new((None, HashMap::new())),
            table_cache: RefCell::new((None, Vec::new())),
            config,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.buf = Buffer::from_text(text);
        self.cursor = 0;
        self.anchor = None;
        self.group_open = false;
        self.mode = if self.config.vim { Mode::Normal } else { Mode::Insert };
        self.doc_revision = None;
    }

    // ----- derived state -------------------------------------------------

    /// Re-analyses the document if the text changed. Call before `doc()`.
    pub fn refresh(&mut self) {
        if self.doc_revision != Some(self.buf.revision()) {
            self.doc = markdown::update(std::mem::take(&mut self.doc), &self.buf.text());
            self.doc_revision = Some(self.buf.revision());
            // Block indices belong to this analysis only.
            self.code_cache.borrow_mut().1.clear();
        }
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Highlighted spans for a line of code, as `(block lines, index)`. The
    /// analysis must be current (`refresh`).
    pub fn code_spans(&self, line: usize) -> Option<(Lines, usize)> {
        if !self.config.highlight || self.doc_revision != Some(self.buf.revision()) {
            return None;
        }
        let (block, at) = self.doc.code_at(line)?;
        let mut cache = self.code_cache.borrow_mut();
        if cache.0 != self.doc_revision {
            cache.0 = self.doc_revision;
            cache.1.clear();
        }
        let lines = cache.1.entry(block).or_insert_with(|| {
            let code = &self.doc.code_blocks[block];
            let texts: Vec<(String, usize)> = code
                .lines
                .iter()
                .map(|&(line, start)| (self.buf.line_text(line as usize), start as usize))
                .collect();
            let sources: Vec<&str> = texts
                .iter()
                .map(|(text, start)| text.get(*start..).unwrap_or(""))
                .collect();
            let spans = self.highlighter.borrow_mut().block(&code.lang, &sources)?;
            if texts.iter().all(|(_, start)| *start == 0) {
                return Some(spans);
            }
            // Nested blocks: shift past the quote or list prefix.
            let shifted = spans.iter().zip(&texts).map(|(spans, (_, start))| {
                let shift = *start as u32;
                spans
                    .iter()
                    .map(|span| markdown::Span {
                        start: span.start + shift,
                        end: span.end + shift,
                        ..*span
                    })
                    .collect()
            });
            Some(Arc::new(shifted.collect()))
        });
        lines.clone().filter(|lines| at < lines.len()).map(|lines| (lines, at))
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The selected char range, if any.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        let (low, high) = (anchor.min(self.cursor), anchor.max(self.cursor));
        match self.mode {
            Mode::Visual | Mode::VisualBlock => Some(low..motion::next_grapheme(&self.buf, high)),
            Mode::VisualLine => {
                let last = self.buf.line_of(high);
                let end = if last + 1 < self.buf.line_count() {
                    self.buf.line_start(last + 1)
                } else {
                    self.buf.len()
                };
                Some(self.buf.line_start(self.buf.line_of(low))..end)
            }
            _ => (low < high).then_some(low..high),
        }
    }

    /// Lines of the paragraph under the cursor, for focus mode.
    pub fn focus_lines(&self) -> Range<usize> {
        let line = self.buf.line_of(self.cursor);
        if self.buf.line_is_blank(line) {
            return line..line + 1;
        }
        let (mut first, mut end) = (line, line + 1);
        while first > 0 && !self.buf.line_is_blank(first - 1) {
            first -= 1;
        }
        while end < self.buf.line_count() && !self.buf.line_is_blank(end) {
            end += 1;
        }
        first..end
    }

    /// The search to highlight: what is being typed at the prompt, else the
    /// last search while highlighting is on.
    pub fn search_highlight(&self) -> Option<&Matcher> {
        let typing = self
            .cmdline
            .as_ref()
            .is_some_and(|cmdline| matches!(cmdline.kind, CmdKind::SearchForward | CmdKind::SearchBackward));
        self.vim.matcher(typing)
    }

    pub fn pending_keys(&self) -> String {
        let held: String = self.map_pending.iter().filter_map(KeyEvent::plain_char).collect();
        format!("{held}{}", self.vim.pending_display())
    }

    pub fn file_name(&self) -> String {
        if let Some(parent) = &self.margin_parent {
            let owner = parent.path.as_ref().and_then(|path| path.file_name());
            return format!("margin of {}", owner.unwrap_or_default().to_string_lossy());
        }
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or("Untitled".to_string(), |name| name.to_string_lossy().into_owned())
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
        });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
        });
    }

    // ----- files ---------------------------------------------------------

    /// Opens `path`. A path that does not exist yet starts an empty document.
    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        // Files open in the document pane, never over a margin.
        self.focus_document();
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        self.close_group();
        self.set_text(&text);
        self.disk_mtime = mtime(&path);
        self.path = Some(path);
        self.fresh_identity();
        self.apply_recovery(false);
        Ok(())
    }

    /// Writes atomically: a sibling temp file is synced, then renamed over the
    /// target. Refuses to clobber a file that changed on disk unless forced.
    pub fn save(&mut self, path: Option<&Path>, force: bool) -> Result<(), String> {
        let same_file = path.is_none() || path == self.path.as_deref();
        let Some(path) = path.map(Path::to_path_buf).or_else(|| self.path.clone()) else {
            self.effects.push(Effect::SaveAsDialog);
            return Ok(());
        };
        if self.path.is_none() && !self.margin_active {
            self.adopt_images(&std::path::absolute(&path).unwrap_or(path.clone()));
        }
        if same_file && !force && self.disk_mtime != mtime(&path) && self.disk_mtime.is_some() {
            return Err("file changed on disk; :w! to overwrite, :e! to reload".to_string());
        }
        let mut contents = self.buf.to_file_string();
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
            std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        }
        // Write through symlinks instead of replacing them.
        let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let name = target.file_name().ok_or("not a file path")?.to_string_lossy();
        let temp = target.with_file_name(format!(".{name}.stet-save-{}", std::process::id()));
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            if let Ok(existing) = std::fs::metadata(&target) {
                std::fs::set_permissions(&temp, existing.permissions())?;
            }
            std::fs::rename(&temp, &target)
        };
        if let Err(err) = write() {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("{}: {err}", path.display()));
        }
        self.close_group();
        self.buf.mark_saved();
        self.remove_recovery();
        self.disk_mtime = mtime(&path);
        self.path = Some(std::path::absolute(&path).unwrap_or(path));
        self.remove_recovery();
        self.refresh_sidebar();
        let (lines, bytes) = (self.buf.line_count(), contents.len());
        self.info(format!("\"{}\" {lines}L, {bytes}B written", self.file_name()));
        Ok(())
    }

    /// Picks up external changes; call when the window regains focus.
    pub fn check_disk(&mut self) {
        if self.margin_active {
            return;
        }
        let Some(path) = self.path.clone() else { return };
        let on_disk = mtime(&path);
        if on_disk.is_none() || on_disk == self.disk_mtime {
            return;
        }
        if self.buf.is_dirty() {
            // `disk_mtime` stays stale, so a plain :w keeps refusing.
            self.error("file changed on disk; :e! to reload, :w! to overwrite");
            return;
        }
        let (cursor, scroll) = (self.cursor, (self.scroll_line, self.scroll_px));
        if self.open(&path).is_ok() {
            (self.scroll_line, self.scroll_px) = scroll;
            self.cursor = cursor.min(self.buf.len());
            self.clamp_cursor();
            self.info("reloaded from disk");
        }
    }

    // ----- editing primitives -------------------------------------------

    fn open_group(&mut self) {
        if !self.group_open {
            self.buf.begin(self.cursor);
            self.group_open = true;
        }
    }

    fn close_group(&mut self) {
        if self.group_open {
            self.buf.commit(self.cursor);
            self.group_open = false;
        }
    }

    /// Every user edit funnels through here so suggestion mode sees it.
    pub(crate) fn edit(&mut self, range: Range<usize>, text: &str) -> EditPos {
        self.open_group();
        if self.suggesting {
            self.suggest_edit(range, text)
        } else {
            self.raw_edit(range, text)
        }
    }

    pub(crate) fn raw_edit(&mut self, range: Range<usize>, text: &str) -> EditPos {
        self.open_group();
        let start = range.start;
        self.buf.replace(range, text);
        EditPos {
            start,
            end: start + text.chars().count(),
        }
    }

    fn vim_enabled(&self) -> bool {
        self.config.vim
    }

    /// Normal and visual cursors sit on a character, never past the line end.
    pub(crate) fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.buf.len());
        if self.mode == Mode::Insert {
            return;
        }
        let line = self.buf.line_of(self.cursor);
        let (start, end) = (self.buf.line_start(line), self.buf.line_end(line));
        if self.cursor >= end && end > start {
            self.cursor = motion::prev_grapheme(&self.buf, end);
        }
    }

    /// Sets the cursor from the shell (display-line motion).
    pub fn move_cursor_to(&mut self, pos: usize) {
        self.cursor = pos.min(self.buf.len());
        self.clamp_cursor();
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos.min(self.buf.len());
    }

    // ----- input ---------------------------------------------------------

    pub fn handle_key(&mut self, event: KeyEvent) {
        self.message = None;
        if self.remap(event) {
            return;
        }
        self.builtin_key(event);
    }

    /// Opens the prompt for a message to the connected assistant. The
    /// selection stays as it is and travels with the message.
    pub fn agent_prompt(&mut self) {
        if self.agent.is_none() {
            return self.error("no assistant is connected (one connects with `stet ctl wait`)");
        }
        self.close_group();
        self.vim.clear_pending();
        self.agent_selection = self.selection().map(|range| (range.clone(), self.buf.slice(range)));
        self.cmdline = Some(CmdLine {
            kind: CmdKind::Agent,
            text: String::new(),
        });
    }

    pub(super) fn agent_send(&mut self, text: &str) {
        let selection = self
            .agent_selection
            .take()
            .or_else(|| self.selection().map(|range| (range.clone(), self.buf.slice(range))));
        if !text.trim().is_empty() {
            self.effects.push(Effect::AgentMessage {
                text: text.trim().to_string(),
                selection,
            });
        }
    }

    /// True if the user bound this single key themselves, in any mode. The
    /// shell then leaves it out of native menus so the binding still wins.
    pub fn has_binding(&self, event: &KeyEvent) -> bool {
        self.keymap.iter().any(|mapping| mapping.keys.as_slice() == [*event])
    }

    /// The mode a key binding must be declared for to apply now, or `None`
    /// where bindings do not apply (menus, prompts, the file browser).
    fn map_mode(&self) -> Option<MapMode> {
        if self.mapping
            || self.palette.is_some()
            || self.context_menu.is_some()
            || self.sheet.is_some()
            || self.cmdline.is_some()
            || self.sidebar.focused
        {
            return None;
        }
        Some(match self.mode {
            Mode::Insert => MapMode::Insert,
            Mode::Normal => MapMode::Normal,
            _ => MapMode::Visual,
        })
    }

    /// Applies user key bindings. True if the key was consumed (it ran a
    /// binding, or is the start of one and is being held).
    fn remap(&mut self, event: KeyEvent) -> bool {
        let Some(mode) = self.map_mode().filter(|_| !self.keymap.is_empty()) else {
            self.map_pending.clear();
            return false;
        };
        let applies = |mapping: &&Mapping| mapping.mode == mode || mapping.mode == MapMode::All;
        self.map_pending.push(event);
        let pending = self.map_pending.clone();
        let exact = self
            .keymap
            .iter()
            .filter(applies)
            .find(|mapping| mapping.keys == pending)
            .cloned();
        let longer = self
            .keymap
            .iter()
            .filter(applies)
            .any(|mapping| mapping.keys.len() > pending.len() && mapping.keys.starts_with(&pending));
        // In insert mode the keys of a sequence are typed as they come, and
        // taken back if the sequence completes; nothing waits on a timer.
        let typed_through = mode == MapMode::Insert && pending.iter().all(|key| key.plain_char().is_some());
        if let (Some(mapping), false) = (&exact, longer) {
            self.map_pending.clear();
            if typed_through && pending.len() > 1 {
                let typed: String = pending[..pending.len() - 1]
                    .iter()
                    .filter_map(KeyEvent::plain_char)
                    .collect();
                let count = typed.chars().count();
                if self.cursor >= count && self.buf.slice(self.cursor - count..self.cursor) == typed {
                    self.cursor = self.raw_edit(self.cursor - count..self.cursor, "").start;
                }
            }
            self.run_mapping(&mapping.run);
            return true;
        }
        if longer {
            return !typed_through;
        }
        // Not a binding after all: the held keys go to the built-in handling.
        self.map_pending.clear();
        if typed_through || pending.len() == 1 {
            if typed_through && pending.len() > 1 {
                // The last key may itself start a sequence.
                let last = [event];
                if self
                    .keymap
                    .iter()
                    .filter(applies)
                    .any(|mapping| mapping.keys.len() > 1 && mapping.keys.starts_with(&last))
                {
                    self.map_pending.push(event);
                }
            }
            return false;
        }
        self.mapping = true;
        let mut rest = &pending[..];
        while !rest.is_empty() {
            let mode = self.map_mode_unguarded();
            let found = self
                .keymap
                .iter()
                .filter(|mapping| mapping.mode == mode || mapping.mode == MapMode::All)
                .filter(|mapping| rest.len() > mapping.keys.len() && rest.starts_with(&mapping.keys))
                .max_by_key(|mapping| mapping.keys.len())
                .cloned();
            match found {
                Some(mapping) => {
                    self.run_mapping(&mapping.run);
                    self.mapping = true;
                    rest = &rest[mapping.keys.len()..];
                }
                None => {
                    self.builtin_key(rest[0]);
                    rest = &rest[1..];
                }
            }
        }
        self.mapping = false;
        true
    }

    fn map_mode_unguarded(&self) -> MapMode {
        match self.mode {
            Mode::Insert => MapMode::Insert,
            Mode::Normal => MapMode::Normal,
            _ => MapMode::Visual,
        }
    }

    fn run_mapping(&mut self, run: &Run) {
        match run {
            Run::Nothing => {}
            Run::Command(line) => {
                self.close_group();
                self.run_command(line);
                self.after_input();
            }
            Run::Keys(keys) => {
                let outer = std::mem::replace(&mut self.mapping, true);
                for key in keys {
                    self.builtin_key(*key);
                }
                self.mapping = outer;
            }
        }
    }

    fn builtin_key(&mut self, event: KeyEvent) {
        self.keep_goal = false;
        // A sheet takes every key until it is put away.
        if self.sheet.is_some() {
            return self.sheet_key(event);
        }
        let shortcut = self.shortcut(&event);
        if self.context_menu.is_some() {
            return self.menu_key(event);
        }
        if self.palette.is_some() {
            // The menu takes every key; its own shortcut closes it again.
            match shortcut {
                Some(Shortcut::Palette(_)) => self.close_palette(),
                _ => self.palette_key(event),
            }
        } else if let Some(action) = shortcut {
            self.run_shortcut(action);
        } else if self.sidebar.focused {
            self.sidebar_key(event);
        } else {
            if let Some((_, inputs)) = &mut self.vim.macro_rec {
                inputs.push(Input::Key(event));
            }
            self.vim_record_start();
            self.vim_record(Input::Key(event));
            self.dispatch_key(event);
            self.vim_record_end();
        }
        self.after_input();
    }

    fn dispatch_key(&mut self, event: KeyEvent) {
        if self.cmdline.is_some() {
            self.cmdline_key(event);
        } else if self.mode == Mode::Insert {
            if event.key != Key::Esc {
                self.vim.collect_insert(Input::Key(event));
            }
            self.insert_key(event);
        } else {
            self.vim_key(event);
        }
    }

    fn after_input(&mut self) {
        if !self.keep_goal {
            self.goal_col = None;
        }
        if self.mode != Mode::Insert && self.vim.is_idle() {
            self.close_group();
        }
        self.clamp_cursor();
    }

    /// Committed text from the IME, a paste, or a drop.
    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.message = None;
        if self.sheet.is_some() {
            return self.sheet_text(&text);
        }
        if self.palette.is_some() {
            return self.palette_text(&text);
        }
        if let Some(cmdline) = &mut self.cmdline {
            cmdline.text.push_str(text.lines().next().unwrap_or(""));
            return self.preview_search();
        }
        if self.sidebar.focused {
            return;
        }
        if let Some((_, inputs)) = &mut self.vim.macro_rec {
            inputs.push(Input::Text(text.clone()));
        }
        if self.mode == Mode::Insert {
            self.vim.collect_insert(Input::Text(text.clone()));
        }
        self.vim_record(Input::Text(text.clone()));
        self.type_text(&text);
        self.after_input();
    }

    fn type_text(&mut self, text: &str) {
        let range = match self.selection() {
            Some(range) => range,
            None => self.cursor..self.cursor,
        };
        self.anchor = None;
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        let pos = self.edit(range, text);
        self.cursor = pos.end;
    }

    fn insert_key(&mut self, event: KeyEvent) {
        let KeyEvent { key, mods } = event;
        let word = mods.alt || (mods.ctrl && self.primary == Primary::Ctrl);
        let line_wise = mods.sup && self.primary == Primary::Super;
        match key {
            Key::Esc => {
                if self.vim_enabled() {
                    self.leave_insert();
                } else {
                    self.anchor = None;
                }
            }
            Key::Char('c' | '[') if mods.ctrl && self.vim_enabled() => self.leave_insert(),
            Key::Char(c) if event.plain_char().is_some() => {
                self.type_text(c.encode_utf8(&mut [0; 4]));
                // `[[` offers the notes that can be linked.
                if c == '['
                    && self.config.link_completion
                    && !self.vim.is_replaying()
                    && self.cursor >= 2
                    && self.buf.char_at(self.cursor - 2) == Some('[')
                    && self.path.is_some()
                {
                    self.open_palette(PaletteKind::Notes);
                }
                // Outside vim, undo steps back a word at a time.
                if !self.vim_enabled() && c.is_whitespace() {
                    self.close_group();
                }
            }
            Key::Char('w') if mods.ctrl => self.delete_back_to(motion::word_backward(&self.buf, self.cursor, false)),
            Key::Char('u') if mods.ctrl => {
                self.delete_back_to(self.buf.line_start(self.buf.line_of(self.cursor)));
            }
            Key::Char('h') if mods.ctrl => self.backspace(),
            Key::Char(_) => {}
            Key::Enter => self.newline(),
            Key::Backspace if line_wise => {
                self.delete_back_to(self.buf.line_start(self.buf.line_of(self.cursor)));
            }
            Key::Backspace if word => self.delete_back_to(motion::word_backward(&self.buf, self.cursor, false)),
            Key::Backspace => self.backspace(),
            Key::Delete => {
                let range = self
                    .selection()
                    .unwrap_or(self.cursor..motion::next_grapheme(&self.buf, self.cursor));
                self.anchor = None;
                self.cursor = self.edit(range, "").end;
            }
            Key::Tab => {
                if !self.table_tab(mods.shift) {
                    self.tab(mods.shift)
                }
            }
            _ => {
                self.close_group();
                self.navigate(key, mods, word, line_wise);
            }
        }
    }

    fn navigate(&mut self, key: Key, mods: Mods, word: bool, line_wise: bool) {
        let buf = &self.buf;
        let line = buf.line_of(self.cursor);
        let selection = self.selection().filter(|_| !mods.shift);
        let target = match key {
            Key::Left if line_wise => buf.line_start(line),
            Key::Right if line_wise => buf.line_end(line),
            Key::Up if line_wise => 0,
            Key::Down if line_wise => buf.len(),
            Key::Left if word => motion::word_backward(buf, self.cursor, false),
            Key::Right if word => {
                let end = motion::word_end(buf, self.cursor.saturating_sub(1), false);
                (end + 1).min(buf.len()).max(self.cursor)
            }
            Key::Left => selection
                .clone()
                .map_or(motion::prev_grapheme(buf, self.cursor), |range| range.start),
            Key::Right => selection
                .clone()
                .map_or(motion::next_grapheme(buf, self.cursor), |range| range.end),
            Key::Home => buf.line_start(line),
            Key::End => buf.line_end(line),
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                let rows = self.view_rows.max(2) as isize - 1;
                self.move_to(self.cursor, mods.shift);
                self.effects.push(Effect::VisualMove(match key {
                    Key::Up => -1,
                    Key::Down => 1,
                    Key::PageUp => -rows,
                    _ => rows,
                }));
                return;
            }
            _ => return,
        };
        self.move_to(target, mods.shift);
    }

    fn backspace(&mut self) {
        if let Some(range) = self.selection() {
            self.anchor = None;
            self.cursor = self.edit(range, "").start;
        } else {
            self.delete_back_to(motion::prev_grapheme(&self.buf, self.cursor));
        }
    }

    fn delete_back_to(&mut self, start: usize) {
        if start < self.cursor {
            self.anchor = None;
            self.cursor = self.edit(start..self.cursor, "").start;
        }
    }

    fn newline(&mut self) {
        if self.selection().is_some() {
            return self.type_text("\n");
        }
        let line = self.buf.line_of(self.cursor);
        let text = self.buf.line_text(line);
        let col = self.cursor - self.buf.line_start(line);
        match list_prefix(&text) {
            Some(prefix) if col >= prefix.len => {
                if prefix.empty_item {
                    // Enter on an empty item ends the list.
                    let pos = self.edit(self.buf.line_start(line)..self.buf.line_end(line), "");
                    self.cursor = pos.end;
                } else {
                    self.type_text(&format!("\n{}", prefix.next));
                }
            }
            _ => {
                let indent: String = text.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                let indent = if col >= indent.chars().count() {
                    indent
                } else {
                    String::new()
                };
                self.type_text(&format!("\n{indent}"));
            }
        }
    }

    fn tab(&mut self, back: bool) {
        let line = self.buf.line_of(self.cursor);
        let is_item = list_prefix(&self.buf.line_text(line)).is_some_and(|prefix| prefix.is_list);
        if is_item || back {
            let before = self.buf.line_len(line);
            let col = self.cursor - self.buf.line_start(line);
            self.shift_lines(line, line, !back);
            let after = self.buf.line_len(line);
            if !self.suggesting {
                let col = (col + after).saturating_sub(before);
                self.cursor = self.buf.line_start(line) + col.min(after);
            }
        } else {
            let col = self.cursor - self.buf.line_start(line);
            let width = self.config.indent - col % self.config.indent;
            self.type_text(&" ".repeat(width));
        }
    }

    /// Indents or dedents whole lines by one step.
    pub(crate) fn shift_lines(&mut self, first: usize, last: usize, indent: bool) {
        let step = self.config.indent;
        for line in (first..=last).rev() {
            let start = self.buf.line_start(line);
            if indent {
                if !self.buf.line_is_blank(line) {
                    self.cursor = self.edit(start..start, &" ".repeat(step)).end;
                }
            } else {
                let text = self.buf.line_text(line);
                let remove = if text.starts_with('\t') {
                    1
                } else {
                    text.chars().take(step).take_while(|c| *c == ' ').count()
                };
                if remove > 0 {
                    self.cursor = self.edit(start..start + remove, "").end;
                }
            }
        }
    }

    fn leave_insert(&mut self) {
        self.finish_insert();
        self.anchor = None;
        self.mode = Mode::Normal;
        let start = self.buf.line_start(self.buf.line_of(self.cursor));
        if self.cursor > start {
            self.cursor = motion::prev_grapheme(&self.buf, self.cursor);
        }
        self.close_group();
    }

    // ----- mouse ---------------------------------------------------------

    pub fn mouse_down(&mut self, pos: usize, clicks: u8, shift: bool) {
        let pos = pos.min(self.buf.len());
        self.close_group();
        self.cmdline = None;
        self.context_menu = None;
        self.map_pending.clear();
        self.close_palette();
        self.sidebar.focused = false;
        self.vim.clear_pending();
        self.message = None;
        let visual = self.mode != Mode::Insert;
        match clicks {
            1 => {
                if shift {
                    self.anchor.get_or_insert(self.cursor);
                    if self.mode == Mode::Normal {
                        self.mode = Mode::Visual;
                    }
                } else {
                    self.anchor = None;
                    if visual {
                        self.mode = Mode::Normal;
                    }
                }
                self.cursor = pos;
                self.drag = Some(Drag::Chars(pos));
            }
            2 => {
                let word = self.word_at(pos);
                self.select(word.clone(), visual);
                self.drag = Some(Drag::Words(word));
            }
            _ => {
                let line = self.buf.line_of(pos);
                let range = self.buf.line_start(line)..self.buf.line_end(line);
                if visual {
                    self.anchor = Some(range.start);
                    self.cursor = range.end;
                    self.mode = Mode::VisualLine;
                } else {
                    self.select(range.clone(), false);
                }
                self.drag = Some(Drag::Lines(range));
            }
        }
        self.clamp_cursor();
    }

    /// The word a double click on `pos` takes: the word, or the run of
    /// spaces or punctuation, that `pos` is in.
    fn word_at(&self, pos: usize) -> Range<usize> {
        motion::text_object(&self.buf, pos, false, 'w').map_or(pos..pos, |word| word.range)
    }

    fn select(&mut self, range: Range<usize>, visual: bool) {
        if range.is_empty() {
            return;
        }
        self.anchor = Some(range.start);
        if visual {
            self.mode = Mode::Visual;
            self.cursor = range.end - 1;
        } else {
            self.cursor = range.end;
        }
    }

    /// The pointer moved with the button down. A drag begun with a double
    /// click takes whole words, and one begun with a triple click whole
    /// lines; what the clicks selected stays selected whichever way it goes.
    pub fn mouse_drag(&mut self, pos: usize) {
        let pos = pos.min(self.buf.len());
        let visual = self.mode != Mode::Insert;
        let (origin, reached) = match self.drag.clone() {
            None => return,
            Some(Drag::Chars(origin)) => {
                if pos == origin && self.anchor.is_none() {
                    return;
                }
                self.anchor = Some(origin);
                self.cursor = pos;
                if self.mode == Mode::Normal {
                    self.mode = Mode::Visual;
                }
                return self.clamp_cursor();
            }
            Some(Drag::Words(origin)) => (origin, self.word_at(pos)),
            Some(Drag::Lines(origin)) => {
                let line = self.buf.line_of(pos);
                (origin, self.buf.line_start(line)..self.buf.line_end(line))
            }
        };
        // A visual selection takes in the character its ends rest on.
        let last = |range: &Range<usize>| {
            if visual {
                range.end.saturating_sub(1).max(range.start)
            } else {
                range.end
            }
        };
        if origin.is_empty() && reached.is_empty() {
            return;
        }
        if reached.start < origin.start {
            self.anchor = Some(last(&origin));
            self.cursor = reached.start;
        } else {
            self.anchor = Some(origin.start);
            self.cursor = last(&reached).max(last(&origin));
        }
        if self.mode == Mode::Normal {
            self.mode = Mode::Visual;
        }
        self.clamp_cursor();
    }

    pub fn mouse_up(&mut self) {
        self.drag = None;
    }

    // ----- shortcuts -----------------------------------------------------

    fn shortcut(&self, event: &KeyEvent) -> Option<Shortcut> {
        let mods = event.mods;
        let primary = match self.primary {
            Primary::Super => mods.sup && !mods.ctrl,
            Primary::Ctrl => mods.ctrl && !mods.sup,
        };
        if !primary || mods.alt {
            return None;
        }
        let c = match event.key {
            Key::Char(c) => c.to_ascii_lowercase(),
            Key::Enter if mods.shift => return Some(Shortcut::Send),
            Key::Enter => return Some(Shortcut::FollowLink),
            _ => return None,
        };
        // With Ctrl as the command key, vim keeps its own Ctrl chords.
        let typing = self.palette.is_none() && !self.sidebar.focused;
        if self.primary == Primary::Ctrl && self.vim_enabled() && !mods.shift && typing {
            let vim_owns = if self.mode == Mode::Insert {
                "wuh["
            } else {
                "rdufbvcnphjleyaxiow["
            };
            if vim_owns.contains(c) {
                return None;
            }
        }
        Some(match (c, mods.shift) {
            ('s', false) => Shortcut::Save,
            ('s', true) => Shortcut::SaveAs,
            ('o', false) => Shortcut::Open,
            ('n' | 't', false) => Shortcut::New,
            ('w', false) => Shortcut::Close,
            ('q', _) => Shortcut::Quit,
            ('p', false) => Shortcut::Palette(PaletteKind::Files),
            ('p', true) | ('/' | '?', _) => Shortcut::Palette(PaletteKind::Help),
            ('l', true) => Shortcut::Palette(PaletteKind::Backlinks),
            ('.', _) => Shortcut::ContextMenu,
            ('a', true) => Shortcut::Agent,
            ('m', true) => Shortcut::Margin,
            ('o', true) => Shortcut::SwitchPane,
            ('\\' | '|', _) => Shortcut::Sidebar,
            ('[', false) => Shortcut::Jump(true),
            (']', false) => Shortcut::Jump(false),
            ('[' | '{', true) => Shortcut::Tab(-1),
            (']' | '}', true) => Shortcut::Tab(1),
            ('1'..='9', false) => Shortcut::TabAt(c as usize - '1' as usize),
            ('v', true) => Shortcut::PastePlain,
            ('z', false) => Shortcut::Undo,
            ('z', true) | ('y', false) => Shortcut::Redo,
            ('x', false) => Shortcut::Cut,
            ('c', false) => Shortcut::Copy,
            ('v', false) => Shortcut::Paste,
            ('a', false) => Shortcut::SelectAll,
            ('f', false) => Shortcut::Find,
            ('g', false) => Shortcut::FindNext(true),
            ('g', true) => Shortcut::FindNext(false),
            ('=' | '+', _) => Shortcut::Zoom(1),
            ('-' | '_', _) => Shortcut::Zoom(-1),
            ('0', false) => Shortcut::Zoom(0),
            ('b', false) => Shortcut::Wrap("**"),
            ('i', false) => Shortcut::Wrap("*"),
            ('k', false) => Shortcut::Link,
            ('e', true) => Shortcut::ToggleSuggest,
            ('y', true) => Shortcut::Resolve(true),
            ('n', true) => Shortcut::Resolve(false),
            ('t', true) => Shortcut::NextTheme,
            ('d', true) => Shortcut::ToggleFocus,
            ('f', true) => Shortcut::Fullscreen,
            _ => return None,
        })
    }

    fn run_shortcut(&mut self, action: Shortcut) {
        self.close_group();
        match action {
            Shortcut::Save => {
                if let Err(err) = self.save(None, false) {
                    self.error(err);
                }
            }
            Shortcut::SaveAs => self.effects.push(Effect::SaveAsDialog),
            Shortcut::Open => self.effects.push(Effect::OpenDialog),
            Shortcut::New => self.new_tab(),
            Shortcut::Close => self.effects.push(Effect::Close),
            Shortcut::Quit => self.effects.push(Effect::CloseAll),
            Shortcut::Palette(kind) => self.open_palette(kind),
            Shortcut::Sidebar => self.toggle_sidebar(),
            Shortcut::ContextMenu => self.open_context_menu(MenuAt::Cursor),
            Shortcut::Agent => self.agent_prompt(),
            Shortcut::Margin => self.toggle_margin(),
            Shortcut::SwitchPane => self.switch_pane(),
            Shortcut::Send => self.send_to_other_pane(false),
            Shortcut::Jump(back) => self.jump(back),
            Shortcut::Tab(delta) => self.cycle_tab(delta),
            Shortcut::TabAt(index) => self.switch_tab(index),
            Shortcut::FollowLink => self.follow_link(),
            Shortcut::Undo => self.undo(1),
            Shortcut::Redo => self.redo(1),
            Shortcut::Copy | Shortcut::Cut => {
                let Some(range) = self.selection() else { return };
                let text = self.buf.slice(range.clone());
                self.copy_out(&text);
                if matches!(action, Shortcut::Cut) {
                    self.anchor = None;
                    if self.mode != Mode::Insert {
                        self.mode = Mode::Normal;
                    }
                    self.cursor = self.edit(range, "").start;
                }
            }
            Shortcut::Paste => self.paste(false),
            Shortcut::PastePlain => self.paste(true),
            Shortcut::SelectAll => {
                if self.buf.is_empty() {
                    return;
                }
                self.anchor = Some(0);
                self.cursor = self.buf.len();
                if self.mode != Mode::Insert {
                    self.mode = Mode::VisualLine;
                }
            }
            Shortcut::Find => {
                self.cmdline = Some(CmdLine {
                    kind: CmdKind::SearchForward,
                    text: String::new(),
                });
            }
            Shortcut::FindNext(forward) => self.search_step(forward),
            Shortcut::Zoom(step) => self.effects.push(Effect::Zoom(step)),
            Shortcut::Wrap(mark) => self.wrap_selection(mark, mark),
            Shortcut::Link => self.wrap_selection("[", "]()"),
            Shortcut::ToggleSuggest => self.run_command("suggest"),
            Shortcut::Resolve(accept) => self.resolve_at_cursor(accept),
            Shortcut::NextTheme => self.open_palette(PaletteKind::Themes),
            Shortcut::ToggleFocus => self.run_command("focus"),
            Shortcut::Fullscreen => self.effects.push(Effect::ToggleFullscreen),
        }
    }

    fn wrap_selection(&mut self, open: &str, close: &str) {
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        let inner = self.buf.slice(range.clone());
        self.anchor = None;
        let pos = self.edit(range.clone(), &format!("{open}{inner}{close}"));
        self.mode = Mode::Insert;
        self.cursor = if self.suggesting {
            pos.end
        } else if inner.is_empty() {
            range.start + open.chars().count()
        } else if close.len() > open.len() {
            // A link: land between the parentheses.
            pos.end - 1
        } else {
            pos.end
        };
        if !self.vim_enabled() {
            self.close_group();
        }
    }

    pub(crate) fn undo(&mut self, count: usize) {
        self.close_group();
        self.vim.suppress_record();
        for _ in 0..count.max(1) {
            match self.buf.undo() {
                Some(cursor) => self.cursor = cursor,
                None => {
                    self.info("Already at oldest change");
                    break;
                }
            }
        }
        self.anchor = None;
    }

    pub(crate) fn redo(&mut self, count: usize) {
        self.close_group();
        self.vim.suppress_record();
        for _ in 0..count.max(1) {
            match self.buf.redo() {
                Some(cursor) => self.cursor = cursor,
                None => {
                    self.info("Already at newest change");
                    break;
                }
            }
        }
        self.anchor = None;
    }

    /// Toggles the task checkbox on the cursor line.
    pub(crate) fn toggle_task(&mut self) {
        let line = self.buf.line_of(self.cursor);
        let text = self.buf.line_text(line);
        let Some(prefix) = list_prefix(&text).filter(|prefix| prefix.task.is_some()) else {
            return self.error("no task on this line");
        };
        let (col, checked) = prefix.task.unwrap();
        let at = self.buf.line_start(line) + col;
        self.edit(at..at + 1, if checked { " " } else { "x" });
    }

    pub(crate) fn next_id(&mut self) -> String {
        const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut id = String::from("s_");
        for _ in 0..8 {
            // xorshift64
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            id.push(DIGITS[(self.rng % 36) as usize] as char);
        }
        id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapMode {
    Normal,
    Insert,
    Visual,
    All,
}

#[derive(Clone, Debug)]
enum Run {
    Nothing,
    Command(String),
    Keys(Vec<KeyEvent>),
}

/// One user key binding.
#[derive(Clone, Debug)]
struct Mapping {
    mode: MapMode,
    keys: Vec<KeyEvent>,
    run: Run,
}

impl Mapping {
    fn compile(keys: &crate::config::Keys) -> Vec<Mapping> {
        let tables = [
            (MapMode::Normal, &keys.normal),
            (MapMode::Insert, &keys.insert),
            (MapMode::Visual, &keys.visual),
            (MapMode::All, &keys.all),
        ];
        let mut out = Vec::new();
        for (mode, table) in tables {
            for (from, to) in table {
                let keys = crate::input::parse_keys(from);
                if keys.is_empty() {
                    continue;
                }
                let to = to.trim();
                let run = match to.strip_prefix(':') {
                    _ if to.is_empty() || to.eq_ignore_ascii_case("<nop>") => Run::Nothing,
                    Some(command) => {
                        Run::Command(command.trim_end_matches("<CR>").trim_end_matches("<cr>").to_string())
                    }
                    None => Run::Keys(crate::input::parse_keys(to)),
                };
                out.push(Mapping { mode, keys, run });
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
enum Shortcut {
    Save,
    SaveAs,
    Open,
    New,
    Close,
    Quit,
    Palette(PaletteKind),
    Sidebar,
    ContextMenu,
    Agent,
    Margin,
    SwitchPane,
    Send,
    Jump(bool),
    Tab(isize),
    TabAt(usize),
    FollowLink,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PastePlain,
    SelectAll,
    Find,
    FindNext(bool),
    Zoom(i32),
    Wrap(&'static str),
    Link,
    ToggleSuggest,
    Resolve(bool),
    NextTheme,
    ToggleFocus,
    Fullscreen,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

struct ListPrefix {
    /// Chars before the item's content.
    len: usize,
    /// Prefix for the next line when continuing.
    next: String,
    empty_item: bool,
    /// False for a bare block quote.
    is_list: bool,
    /// Column of the task state character, and whether it is checked.
    task: Option<(usize, bool)>,
}

/// Recognises `  > - [ ] ` style prefixes: indent, quote markers, a bullet or
/// number, and an optional task box.
fn list_prefix(line: &str) -> Option<ListPrefix> {
    let chars: Vec<char> = line.chars().collect();
    let mut at = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
    let mut quoted = false;
    while chars.get(at) == Some(&'>') {
        quoted = true;
        at += 1;
        if chars.get(at) == Some(&' ') {
            at += 1;
        }
    }
    let mut next: String = chars[..at].iter().collect();
    let mut is_list = false;
    let mut task = None;
    let digits = chars[at..].iter().take_while(|c| c.is_ascii_digit()).count();
    if matches!(chars.get(at), Some('-' | '*' | '+')) && chars.get(at + 1) == Some(&' ') {
        next.extend(&chars[at..at + 2]);
        at += 2;
        is_list = true;
    } else if (1..=9).contains(&digits)
        && matches!(chars.get(at + digits), Some('.' | ')'))
        && chars.get(at + digits + 1) == Some(&' ')
    {
        let number: u64 = chars[at..at + digits].iter().collect::<String>().parse().ok()?;
        next.push_str(&format!("{}{} ", number + 1, chars[at + digits]));
        at += digits + 2;
        is_list = true;
    }
    if is_list
        && chars.get(at) == Some(&'[')
        && matches!(chars.get(at + 1), Some(' ' | 'x' | 'X'))
        && chars.get(at + 2) == Some(&']')
        && chars.get(at + 3) == Some(&' ')
    {
        task = Some((at + 1, chars[at + 1] != ' '));
        next.push_str("[ ] ");
        at += 4;
    }
    (is_list || quoted).then(|| ListPrefix {
        len: at,
        next,
        empty_item: chars[at..].iter().all(|c| c.is_whitespace()),
        is_list,
        task,
    })
}
