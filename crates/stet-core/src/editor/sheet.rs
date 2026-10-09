//! A sheet: a card that rises from the bottom of the window with a few
//! settings and the buttons that act on them. Anything settings-like is one
//! of these. The core holds what it says and takes its keys; the shell draws
//! it, and does what its buttons ask when it is told one was pressed.

use super::{Editor, Effect};
use crate::input::{Key, KeyEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// A line to type in. `hint` shows while it is empty; a `secret` is
    /// never shown at all.
    Text { value: String, hint: String, secret: bool },
    /// One of a few.
    Choice { options: Vec<String>, chosen: usize },
    /// Words to read.
    Note(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: &'static str,
    pub label: String,
    pub field: Field,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub id: &'static str,
    pub label: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sheet {
    /// Which sheet this is, for whoever acts on its buttons.
    pub id: &'static str,
    pub title: String,
    pub rows: Vec<Row>,
    /// The row with the keyboard.
    pub focus: usize,
    /// Left to right; Enter presses the last.
    pub buttons: Vec<Button>,
    /// One line on how it is going, and whether that is badly.
    pub status: Option<(String, bool)>,
    /// Waiting on something: the buttons rest.
    pub busy: bool,
}

impl Row {
    pub fn text(id: &'static str, label: &str, value: &str, hint: &str) -> Row {
        let field = Field::Text {
            value: value.to_string(),
            hint: hint.to_string(),
            secret: false,
        };
        Row {
            id,
            label: label.to_string(),
            field,
        }
    }

    pub fn secret(id: &'static str, label: &str, hint: &str) -> Row {
        let field = Field::Text {
            value: String::new(),
            hint: hint.to_string(),
            secret: true,
        };
        Row {
            id,
            label: label.to_string(),
            field,
        }
    }

    pub fn choice(id: &'static str, label: &str, options: &[&str], chosen: usize) -> Row {
        let field = Field::Choice {
            options: options.iter().map(|option| option.to_string()).collect(),
            chosen,
        };
        Row {
            id,
            label: label.to_string(),
            field,
        }
    }

    pub fn note(id: &'static str, label: &str, words: &str) -> Row {
        Row {
            id,
            label: label.to_string(),
            field: Field::Note(words.to_string()),
        }
    }

    fn takes_keys(&self) -> bool {
        !matches!(self.field, Field::Note(_))
    }
}

impl Button {
    pub fn new(id: &'static str, label: &str) -> Button {
        Button {
            id,
            label: label.to_string(),
            enabled: true,
        }
    }
}

impl Sheet {
    pub fn new(id: &'static str, title: &str, rows: Vec<Row>, buttons: Vec<Button>) -> Sheet {
        let mut sheet = Sheet {
            id,
            title: title.to_string(),
            rows,
            focus: 0,
            buttons,
            status: None,
            busy: false,
        };
        sheet.focus_first();
        sheet
    }

    /// Puts the keyboard on the first row that takes it.
    pub fn focus_first(&mut self) {
        self.focus = self.rows.iter().position(Row::takes_keys).unwrap_or(0);
    }

    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    /// What is typed in the row `id`.
    pub fn text(&self, id: &str) -> Option<&str> {
        match &self.row(id)?.field {
            Field::Text { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Which option the row `id` is on.
    pub fn chosen(&self, id: &str) -> Option<usize> {
        match &self.row(id)?.field {
            Field::Choice { chosen, .. } => Some(*chosen),
            _ => None,
        }
    }

    /// Replaces the row with this id, or adds it at `at` if there is none.
    pub fn put(&mut self, at: usize, row: Row) {
        match self.rows.iter_mut().find(|old| old.id == row.id) {
            Some(old) => *old = row,
            None => self.rows.insert(at.min(self.rows.len()), row),
        }
        if !self.rows.get(self.focus).is_some_and(Row::takes_keys) {
            self.focus_first();
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.rows.retain(|row| row.id != id);
        if !self.rows.get(self.focus).is_some_and(Row::takes_keys) {
            self.focus_first();
        }
    }

    fn step(&mut self, back: bool) {
        let count = self.rows.len();
        let order = (1..=count).map(|by| (self.focus + if back { count - by } else { by }) % count.max(1));
        if let Some(next) = order.into_iter().find(|&at| self.rows[at].takes_keys()) {
            self.focus = next;
        }
    }

    fn turn(&mut self, by: isize) {
        if let Some(Field::Choice { options, chosen }) = self.rows.get_mut(self.focus).map(|row| &mut row.field) {
            *chosen = (*chosen as isize + by).clamp(0, options.len() as isize - 1) as usize;
        }
    }

    fn typed(&mut self) -> Option<&mut String> {
        match self.rows.get_mut(self.focus).map(|row| &mut row.field) {
            Some(Field::Text { value, .. }) => Some(value),
            _ => None,
        }
    }
}

impl Editor {
    pub fn open_sheet(&mut self, sheet: Sheet) {
        self.close_group();
        self.vim.clear_pending();
        self.cmdline = None;
        self.close_context_menu();
        self.close_palette();
        self.sheet = Some(sheet);
    }

    pub fn close_sheet(&mut self) {
        self.sheet = None;
    }

    /// Presses a button: the shell hears of it and acts.
    pub fn sheet_press(&mut self, index: usize) {
        let Some(sheet) = &self.sheet else { return };
        if let Some(button) = sheet.buttons.get(index).filter(|button| button.enabled && !sheet.busy) {
            self.effects.push(Effect::Sheet {
                sheet: sheet.id,
                button: button.id,
            });
        }
    }

    pub fn sheet_focus(&mut self, row: usize) {
        if let Some(sheet) = &mut self.sheet
            && sheet.rows.get(row).is_some_and(Row::takes_keys)
        {
            sheet.focus = row;
        }
    }

    pub fn sheet_choose(&mut self, row: usize, option: usize) {
        self.sheet_focus(row);
        if let Some(Field::Choice { options, chosen }) = self
            .sheet
            .as_mut()
            .and_then(|sheet| sheet.rows.get_mut(row))
            .map(|row| &mut row.field)
            && option < options.len()
        {
            *chosen = option;
        }
    }

    /// Typed, composed or pasted text goes into the line with the keyboard.
    pub(super) fn sheet_text(&mut self, text: &str) {
        let line = text.lines().next().unwrap_or("").trim_matches(['\r', '\n']);
        if let Some(value) = self.sheet.as_mut().and_then(Sheet::typed) {
            value.extend(line.chars().filter(|c| !c.is_control()));
        }
    }

    pub(super) fn sheet_key(&mut self, event: KeyEvent) {
        let command = event.mods.sup || event.mods.ctrl;
        let Some(sheet) = &mut self.sheet else { return };
        match event.key {
            Key::Esc => self.close_sheet(),
            Key::Enter => {
                let last = sheet.buttons.len().saturating_sub(1);
                self.sheet_press(last);
            }
            Key::Tab if event.mods.shift => sheet.step(true),
            Key::Tab | Key::Down => sheet.step(false),
            Key::Up => sheet.step(true),
            Key::Left => sheet.turn(-1),
            Key::Right => sheet.turn(1),
            Key::Backspace => {
                if let Some(value) = sheet.typed() {
                    match command {
                        true => value.clear(),
                        false => drop(value.pop()),
                    }
                }
            }
            Key::Char('v' | 'V') if command => {
                if let Some(pasted) = self.clipboard.get() {
                    self.sheet_text(pasted.trim());
                }
            }
            Key::Char(c) if !command => self.sheet_text(c.encode_utf8(&mut [0; 4])),
            _ => {}
        }
    }
}
