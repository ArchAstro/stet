//! The file browser: a collapsible tree of the document's folder. Hidden
//! until asked for.

use super::Editor;
use crate::input::{Key, KeyEvent};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MAX_PER_FOLDER: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Folder { open: bool },
    Note,
    Image,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// Notes are shown without their extension.
    pub name: String,
    pub depth: usize,
    pub kind: EntryKind,
}

#[derive(Default)]
pub struct Sidebar {
    pub visible: bool,
    /// Keys go to the tree instead of the text.
    pub focused: bool,
    pub root: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// First visible row; the shell keeps the selection in view.
    pub scroll: usize,
    expanded: HashSet<PathBuf>,
}

fn kind_of(path: &Path) -> EntryKind {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" | "mdown" | "mdx" | "txt" => EntryKind::Note,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => EntryKind::Image,
        _ => EntryKind::Other,
    }
}

impl Sidebar {
    fn list(&mut self, dir: &Path, depth: usize) {
        let mut children: Vec<(bool, String, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                (!name.starts_with('.')).then(|| (!path.is_dir(), name.to_lowercase(), path))
            })
            .take(MAX_PER_FOLDER)
            .collect();
        children.sort();
        for (is_file, _, path) in children {
            let open = !is_file && self.expanded.contains(&path);
            let kind = if is_file {
                kind_of(&path)
            } else {
                EntryKind::Folder { open }
            };
            let name = match kind {
                EntryKind::Note => path.file_stem(),
                _ => path.file_name(),
            };
            self.entries.push(Entry {
                name: name.unwrap_or_default().to_string_lossy().into_owned(),
                path: path.clone(),
                depth,
                kind,
            });
            if open {
                self.list(&path, depth + 1);
            }
        }
    }

    /// Re-reads the tree, keeping the selection on the same path.
    pub fn reload(&mut self) {
        let selected = self.entries.get(self.selected).map(|entry| entry.path.clone());
        self.entries.clear();
        let root = self.root.clone();
        self.list(&root, 0);
        self.select_path(selected.as_deref());
    }

    fn select_path(&mut self, path: Option<&Path>) {
        let found = path.and_then(|path| self.entries.iter().position(|entry| entry.path == path));
        self.selected = found.unwrap_or(self.selected).min(self.entries.len().saturating_sub(1));
    }

    /// Opens the folders leading to `path` and selects it.
    fn reveal(&mut self, path: &Path) {
        if let Ok(relative) = path.strip_prefix(&self.root) {
            let mut dir = self.root.clone();
            for part in relative.parent().into_iter().flat_map(Path::components) {
                dir.push(part);
                self.expanded.insert(dir.clone());
            }
        }
        self.reload();
        self.select_path(Some(path));
    }
}

impl Editor {
    fn sidebar_show(&mut self) {
        let sidebar = &mut self.sidebar;
        if sidebar.root.as_os_str().is_empty() {
            sidebar.root = self
                .path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
        }
        sidebar.visible = true;
        match self.path.clone() {
            Some(path) => sidebar.reveal(&path),
            None => sidebar.reload(),
        }
    }

    /// Hidden → shown with focus → hidden. Shown without focus takes focus.
    pub fn toggle_sidebar(&mut self) {
        self.close_group();
        self.vim.clear_pending();
        if self.sidebar.visible && self.sidebar.focused {
            self.sidebar.visible = false;
            self.sidebar.focused = false;
        } else {
            self.sidebar_show();
            self.sidebar.focused = true;
        }
    }

    /// Picks up files created or removed elsewhere.
    pub fn refresh_sidebar(&mut self) {
        if self.sidebar.visible {
            self.sidebar.reload();
        }
    }

    fn sidebar_activate(&mut self, new_tab: bool) {
        let Some(entry) = self.sidebar.entries.get(self.sidebar.selected).cloned() else {
            return;
        };
        match entry.kind {
            EntryKind::Folder { open } => {
                if open {
                    self.sidebar.expanded.remove(&entry.path);
                } else {
                    self.sidebar.expanded.insert(entry.path);
                }
                self.sidebar.reload();
            }
            EntryKind::Note | EntryKind::Other => {
                self.push_jump();
                let opened = if new_tab {
                    self.open_in_tab(&entry.path)
                } else {
                    self.open_path(&entry.path)
                };
                match opened {
                    Ok(()) => self.sidebar.focused = false,
                    Err(err) => self.error(err),
                }
            }
            EntryKind::Image => self
                .effects
                .push(super::Effect::OpenUrl(entry.path.to_string_lossy().into_owned())),
        }
    }

    /// A click on row `index`; `new_tab` when the command key is held.
    pub fn sidebar_click(&mut self, index: usize, new_tab: bool) {
        self.palette = None;
        self.cmdline = None;
        self.sidebar.focused = true;
        if index < self.sidebar.entries.len() {
            self.sidebar.selected = index;
            self.sidebar_activate(new_tab);
        }
    }

    fn sidebar_parent(&mut self) {
        let Some(entry) = self.sidebar.entries.get(self.sidebar.selected).cloned() else {
            return;
        };
        if matches!(entry.kind, EntryKind::Folder { open: true }) {
            self.sidebar.expanded.remove(&entry.path);
            self.sidebar.reload();
        } else if entry.depth > 0 {
            let parent = entry.path.parent().map(Path::to_path_buf);
            self.sidebar.select_path(parent.as_deref());
        }
    }

    pub(super) fn sidebar_key(&mut self, event: KeyEvent) {
        let last = self.sidebar.entries.len().saturating_sub(1);
        let step = |sidebar: &mut super::Sidebar, delta: isize| {
            sidebar.selected = (sidebar.selected as isize + delta).clamp(0, last as isize) as usize;
        };
        let rows = self.view_rows.max(2) as isize - 1;
        let ctrl = event.mods.ctrl;
        match event.key {
            Key::Esc | Key::Tab => self.sidebar.focused = false,
            Key::Char('c' | '[') if ctrl => self.sidebar.focused = false,
            Key::Char('q') => self.toggle_sidebar(),
            Key::Char(':') => {
                self.sidebar.focused = false;
                self.cmdline = Some(super::CmdLine {
                    kind: super::CmdKind::Command,
                    text: String::new(),
                });
            }
            Key::Down => step(&mut self.sidebar, 1),
            Key::Up => step(&mut self.sidebar, -1),
            Key::Char('j' | 'n') if event.plain_char().is_some() || ctrl => step(&mut self.sidebar, 1),
            Key::Char('k' | 'p') if event.plain_char().is_some() || ctrl => step(&mut self.sidebar, -1),
            Key::PageDown => step(&mut self.sidebar, rows),
            Key::PageUp => step(&mut self.sidebar, -rows),
            Key::Char('d' | 'f') if ctrl => step(&mut self.sidebar, rows / 2),
            Key::Char('u' | 'b') if ctrl => step(&mut self.sidebar, -rows / 2),
            Key::Home | Key::Char('g') => self.sidebar.selected = 0,
            Key::End | Key::Char('G') => self.sidebar.selected = last,
            Key::Enter | Key::Right | Key::Char('l' | 'o' | ' ') => self.sidebar_activate(false),
            Key::Char('t') => self.sidebar_activate(true),
            Key::Left | Key::Char('h') => self.sidebar_parent(),
            Key::Backspace | Key::Char('-') => {
                // Widen the tree to the parent folder.
                if let Some(parent) = self.sidebar.root.parent().map(Path::to_path_buf) {
                    let old = std::mem::replace(&mut self.sidebar.root, parent);
                    self.sidebar.expanded.insert(old.clone());
                    self.sidebar.reload();
                    self.sidebar.select_path(Some(&old));
                }
            }
            Key::Char('r') => self.sidebar.reload(),
            _ => {}
        }
    }
}
