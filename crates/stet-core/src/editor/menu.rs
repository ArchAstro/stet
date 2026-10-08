//! The context menu: what can be done with the thing under the pointer or
//! the cursor. Opened by a right-click, `K` or `gm` in vim, or the
//! platform's menu key; driven by the mouse, `j`/`k` and Enter, or each
//! item's own letter.

use super::{Editor, Effect, EntryKind, Link, Mode, PaletteKind};
use crate::input::{Key, KeyEvent};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
enum Do {
    Resolve(bool),
    Follow,
    OpenTab(PathBuf),
    Open(PathBuf),
    Copy(String),
    CopySelection,
    Cut,
    Paste,
    PastePlain,
    Command(&'static str),
    Palette(PaletteKind),
    CloseTab(usize),
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    /// The letter that runs this item while the menu is open.
    pub key: Option<char>,
    /// A rule is drawn above the first item of each group.
    pub group_start: bool,
    action: Do,
}

/// Where the shell should place the menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MenuAt {
    Cursor,
    /// A window position, as the shell reported it.
    Point(f32, f32),
}

#[derive(Clone, Debug)]
pub struct ContextMenu {
    pub items: Vec<MenuItem>,
    pub selected: usize,
    pub at: MenuAt,
}

/// Letters the menu itself uses for moving around.
const RESERVED: &str = "jkhlq";

#[derive(Default)]
struct Builder {
    items: Vec<MenuItem>,
    new_group: bool,
}

impl Builder {
    fn group(&mut self) {
        self.new_group = !self.items.is_empty();
    }

    /// Adds an item, giving it the first free letter of `keys`.
    fn add(&mut self, label: &str, keys: &str, action: Do) {
        let taken = |key: char| RESERVED.contains(key) || self.items.iter().any(|item| item.key == Some(key));
        let key = keys.chars().find(|key| !taken(*key));
        self.items.push(MenuItem {
            label: label.to_string(),
            key,
            group_start: std::mem::take(&mut self.new_group),
            action,
        });
    }
}

impl Editor {
    /// The menu for the text at the cursor.
    pub fn open_context_menu(&mut self, at: MenuAt) {
        self.refresh();
        self.close_group();
        self.vim.clear_pending();
        self.cmdline = None;
        let pos = self.cursor;
        let mut menu = Builder::default();

        let suggestion = self
            .doc
            .suggestions
            .iter()
            .find(|suggestion| suggestion.span.contains(&pos))
            .cloned();
        if let Some(suggestion) = &suggestion {
            menu.add(
                &format!("Accept {}'s suggestion", suggestion.author),
                "a",
                Do::Resolve(true),
            );
            menu.add("Reject it", "r", Do::Resolve(false));
        }
        menu.group();
        match self.link_at(pos) {
            Some(Link::Url(url)) => {
                menu.add("Open link in browser", "fo", Do::Follow);
                menu.add("Copy link", "y", Do::Copy(url));
            }
            Some(Link::Wiki { note, .. }) if !note.is_empty() => {
                let path = self.resolve_note(&note);
                menu.add(&format!("Go to \"{note}\""), "fg", Do::Follow);
                menu.add("Open in new tab", "t", Do::OpenTab(path));
            }
            Some(_) => menu.add("Follow link", "fg", Do::Follow),
            None => {}
        }
        menu.group();
        let line = self.buf.line_of(pos);
        if super::list_prefix(&self.buf.line_text(line)).is_some_and(|prefix| prefix.task.is_some()) {
            menu.add("Toggle task", "x", Do::Command("task"));
        }
        menu.group();
        if self.selection().is_some() {
            menu.add("Cut", "dx", Do::Cut);
            menu.add("Copy", "yc", Do::CopySelection);
            menu.add("Paste", "pv", Do::Paste);
            menu.add("Paste as plain text", "V", Do::PastePlain);
            menu.add("Bold", "b", Do::Command("bold"));
            menu.add("Italic", "i", Do::Command("italic"));
            menu.add("Make a link", "n", Do::Command("link"));
        } else {
            menu.add("Paste", "pv", Do::Paste);
            menu.add("Paste as plain text", "V", Do::PastePlain);
        }
        if self.doc.block(line) == crate::markdown::Block::Table {
            let body = self.table_lines(line).is_some_and(|lines| line >= lines.start + 2);
            menu.group();
            Self::table_items(&mut menu, true, true, body);
            // In a table the menu is about the table.
            menu.group();
            menu.add("All commands…", "m", Do::Palette(PaletteKind::Help));
            self.context_menu = Some(ContextMenu {
                items: menu.items,
                selected: 0,
                at,
            });
            return;
        }
        if self.path.is_some() || self.in_margin() {
            menu.group();
            if self.in_margin() {
                menu.add("Go to where this is pinned", "g", Do::Command("note"));
                menu.add("Pin this section to the document's cursor", "P", Do::Command("pin"));
                menu.add("Unpin this section", "U", Do::Command("unpin"));
            } else {
                menu.add("Pin a margin note here", "P", Do::Command("pin"));
                menu.add("Go to the note pinned here", "g", Do::Command("note"));
            }
        }
        if self.margin_visible() {
            menu.group();
            let other = if self.in_margin() { "document" } else { "margin" };
            menu.add(&format!("Copy to the {other}"), ">e", Do::Command("send"));
            menu.add(&format!("Move to the {other}"), "M", Do::Command("move"));
        }
        menu.group();
        if !self.doc.suggestions.is_empty() {
            if suggestion.is_none() {
                menu.add("Next suggestion", "n", Do::Command("nextsuggestion"));
            }
            menu.add("Accept all suggestions", "A", Do::Command("acceptall"));
            menu.add("Reject all suggestions", "R", Do::Command("rejectall"));
        }
        menu.add(
            if self.suggesting {
                "Stop suggesting"
            } else {
                "Suggest edits"
            },
            "s",
            Do::Command("suggest"),
        );
        menu.group();
        if self.path.is_some() {
            menu.add("Notes linking here…", "b", Do::Palette(PaletteKind::Backlinks));
            if !self.margin_visible() {
                menu.add("Open the margin", "o", Do::Command("margin"));
            }
        }
        menu.add("All commands…", "m", Do::Palette(PaletteKind::Help));
        self.context_menu = Some(ContextMenu {
            items: menu.items,
            selected: 0,
            at,
        });
    }

    /// A right-click in the text at `pos`. A click inside the selection
    /// keeps it; anywhere else moves the cursor there first.
    pub fn context_menu_at(&mut self, pos: usize, x: f32, y: f32) {
        let pos = pos.min(self.buf.len());
        if !self.selection().is_some_and(|selection| selection.contains(&pos)) {
            self.anchor = None;
            if self.mode.is_visual() {
                self.mode = Mode::Normal;
            }
            self.cursor = pos;
            self.clamp_cursor();
        }
        self.palette = None;
        self.sidebar.focused = false;
        self.open_context_menu(MenuAt::Point(x, y));
    }

    /// What can be done to the row and the column the cursor is in.
    fn table_items(menu: &mut Builder, rows: bool, columns: bool, body: bool) {
        if rows {
            menu.add("Insert row above", "O", Do::Command("table row above"));
            menu.add("Insert row below", "o", Do::Command("table row"));
            if body {
                menu.add("Move row up", "K", Do::Command("table moveup"));
                menu.add("Move row down", "J", Do::Command("table movedown"));
                menu.add("Delete row", "D", Do::Command("table delrow"));
            }
            menu.group();
        }
        if columns {
            menu.add("Insert column left", "I", Do::Command("table column left"));
            menu.add("Insert column right", "a", Do::Command("table column"));
            menu.add("Move column left", "H", Do::Command("table moveleft"));
            menu.add("Move column right", "L", Do::Command("table moveright"));
            menu.group();
            menu.add("Align left", "[", Do::Command("table left"));
            menu.add("Align centre", "=", Do::Command("table center"));
            menu.add("Align right", "]", Do::Command("table right"));
            menu.group();
            menu.add("Delete column", "X", Do::Command("table delcolumn"));
            menu.group();
        }
        menu.add("Tidy the table's source", "T", Do::Command("table"));
    }

    /// The menu of a table's row or column handle.
    pub(super) fn table_menu(&mut self, column: bool, body: bool, x: f32, y: f32) {
        let mut menu = Builder::default();
        Self::table_items(&mut menu, !column, column, body);
        self.palette = None;
        self.sidebar.focused = false;
        self.context_menu = Some(ContextMenu {
            items: menu.items,
            selected: 0,
            at: MenuAt::Point(x, y),
        });
    }

    /// A right-click on a file browser row.
    pub fn sidebar_menu(&mut self, index: usize, x: f32, y: f32) {
        let Some(entry) = self.sidebar.entries.get(index).cloned() else {
            return;
        };
        self.sidebar.selected = index;
        self.sidebar.focused = true;
        let mut menu = Builder::default();
        match entry.kind {
            EntryKind::Folder { open } => menu.add(
                if open { "Close folder" } else { "Open folder" },
                "o",
                Do::Open(entry.path.clone()),
            ),
            _ => {
                menu.add("Open", "o", Do::Open(entry.path.clone()));
                menu.add("Open in new tab", "t", Do::OpenTab(entry.path.clone()));
            }
        }
        menu.group();
        menu.add("Copy path", "yc", Do::Copy(entry.path.to_string_lossy().into_owned()));
        self.context_menu = Some(ContextMenu {
            items: menu.items,
            selected: 0,
            at: MenuAt::Point(x, y),
        });
    }

    /// A right-click on a tab.
    pub fn tab_menu(&mut self, index: usize, x: f32, y: f32) {
        let mut menu = Builder::default();
        menu.add("Close tab", "cw", Do::CloseTab(index));
        self.context_menu = Some(ContextMenu {
            items: menu.items,
            selected: 0,
            at: MenuAt::Point(x, y),
        });
    }

    pub fn close_context_menu(&mut self) {
        self.context_menu = None;
    }

    pub(super) fn menu_key(&mut self, event: KeyEvent) {
        let Some(menu) = &mut self.context_menu else { return };
        let count = menu.items.len();
        let ctrl = event.mods.ctrl;
        let step = |menu: &mut ContextMenu, delta: isize| {
            menu.selected = (menu.selected as isize + delta).rem_euclid(count.max(1) as isize) as usize;
        };
        match event.key {
            Key::Esc | Key::Left => self.context_menu = None,
            Key::Char('c' | '[') if ctrl => self.context_menu = None,
            Key::Down | Key::Tab if !event.mods.shift => step(menu, 1),
            Key::Up | Key::Tab => step(menu, -1),
            Key::Char('n') if ctrl => step(menu, 1),
            Key::Char('p') if ctrl => step(menu, -1),
            Key::Enter | Key::Right | Key::Char(' ') => self.menu_run(),
            Key::Char(c) if event.plain_char().is_some() => match c {
                'j' => step(menu, 1),
                'k' => step(menu, -1),
                'l' => self.menu_run(),
                'h' | 'q' => self.context_menu = None,
                _ => {
                    if let Some(at) = menu.items.iter().position(|item| item.key == Some(c)) {
                        menu.selected = at;
                        self.menu_run();
                    }
                }
            },
            _ => {}
        }
    }

    /// Runs item `index` (a click).
    pub fn menu_click(&mut self, index: usize) {
        if let Some(menu) = &mut self.context_menu
            && index < menu.items.len()
        {
            menu.selected = index;
            self.menu_run();
        }
    }

    fn menu_run(&mut self) {
        let Some(menu) = self.context_menu.take() else { return };
        let Some(item) = menu.items.get(menu.selected) else {
            return;
        };
        match item.action.clone() {
            Do::Resolve(accept) => self.resolve_at_cursor(accept),
            Do::Follow => self.follow_link(),
            Do::Open(path) => {
                if let Some(index) = self.sidebar.entries.iter().position(|entry| entry.path == path) {
                    self.sidebar_click(index, false);
                }
            }
            Do::OpenTab(path) => {
                self.push_jump();
                match self.open_in_tab(&path) {
                    Ok(()) => self.sidebar.focused = false,
                    Err(err) => self.error(err),
                }
            }
            Do::Copy(text) => {
                self.clipboard.set(&text);
                self.info("copied");
            }
            Do::CopySelection | Do::Cut => {
                let Some(range) = self.selection() else { return };
                let text = self.buf.slice(range.clone());
                self.copy_out(&text);
                if item.action == Do::Cut {
                    self.anchor = None;
                    if self.mode != Mode::Insert {
                        self.mode = Mode::Normal;
                    }
                    self.cursor = self.edit(range, "").start;
                    self.close_group();
                }
            }
            Do::Paste | Do::PastePlain => {
                self.paste(item.action == Do::PastePlain);
                self.close_group();
            }
            Do::Command(line) => self.run_command(line),
            Do::Palette(kind) => self.open_palette(kind),
            Do::CloseTab(index) => {
                self.switch_tab(index);
                self.effects.push(Effect::Close);
            }
        }
        self.clamp_cursor();
    }
}
