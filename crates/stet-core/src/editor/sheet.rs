//! A sheet: a menu of settings and the actions on them, in the same panel
//! and with the same manners as the command menu it is reached from. One
//! row is selected; the arrows move, typing fills in the selected line,
//! Enter acts. Anything settings-like is one of these. The core holds what
//! it says and takes its keys; the shell draws it, and does what its
//! actions ask when it is told one was chosen.

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
    /// What is selected: a row, or past the rows, a button.
    pub focus: usize,
    /// Actions, listed under the rows. Enter on a row presses the last.
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

    /// Whether the selection can rest here: a row to fill in, or a button
    /// that can be pressed.
    fn selectable(&self, at: usize) -> bool {
        match self.rows.get(at) {
            Some(row) => row.takes_keys(),
            None => self
                .buttons
                .get(at - self.rows.len())
                .is_some_and(|button| button.enabled),
        }
    }

    /// Selects the first row that can be filled in, or failing that the
    /// first button.
    pub fn focus_first(&mut self) {
        let all = self.rows.len() + self.buttons.len();
        self.focus = (0..all).find(|&at| self.selectable(at)).unwrap_or(0);
    }

    /// Selects the button with this id.
    pub fn focus_button(&mut self, id: &str) {
        if let Some(at) = self.buttons.iter().position(|button| button.id == id) {
            self.focus = self.rows.len() + at;
        }
    }

    /// The selected button, if the selection is on one.
    pub fn button_at_focus(&self) -> Option<usize> {
        self.focus
            .checked_sub(self.rows.len())
            .filter(|at| *at < self.buttons.len())
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
    /// The selection stays on what it was on.
    pub fn put(&mut self, at: usize, row: Row) {
        let on = self.rows.get(self.focus).map(|row| row.id);
        let button = self.button_at_focus();
        match self.rows.iter_mut().find(|old| old.id == row.id) {
            Some(old) => *old = row,
            None => self.rows.insert(at.min(self.rows.len()), row),
        }
        self.reselect(on, button);
    }

    pub fn remove(&mut self, id: &str) {
        let on = self.rows.get(self.focus).map(|row| row.id);
        let button = self.button_at_focus();
        self.rows.retain(|row| row.id != id);
        self.reselect(on, button);
    }

    /// After rows came or went: back onto the row or button that was
    /// selected, if it is still there to be.
    fn reselect(&mut self, row: Option<&'static str>, button: Option<usize>) {
        let found = match (row, button) {
            (Some(id), _) => self.rows.iter().position(|row| row.id == id),
            (None, Some(button)) => Some(self.rows.len() + button),
            (None, None) => None,
        };
        match found.filter(|&at| self.selectable(at)) {
            Some(at) => self.focus = at,
            None => self.focus_first(),
        }
    }

    fn step(&mut self, back: bool) {
        let count = self.rows.len() + self.buttons.len();
        let order = (1..=count).map(|by| (self.focus + if back { count - by } else { by }) % count.max(1));
        if let Some(next) = order.into_iter().find(|&at| self.selectable(at)) {
            self.focus = next;
        }
    }

    /// Moves a choice on; `around` goes back to the first after the last.
    fn turn(&mut self, by: isize, around: bool) {
        if let Some(Field::Choice { options, chosen }) = self.rows.get_mut(self.focus).map(|row| &mut row.field) {
            let count = options.len() as isize;
            let next = *chosen as isize + by;
            *chosen = match around {
                true => next.rem_euclid(count.max(1)),
                false => next.clamp(0, count - 1),
            } as usize;
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

    /// A click on a row: it is selected, and a choice moves on to its next.
    pub fn sheet_click(&mut self, row: usize) {
        if let Some(sheet) = &mut self.sheet
            && sheet.rows.get(row).is_some_and(Row::takes_keys)
            && !sheet.busy
        {
            sheet.focus = row;
            sheet.turn(1, true);
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
                // The selected button, or from a row, the last one.
                let pressed = sheet.button_at_focus().unwrap_or(sheet.buttons.len().saturating_sub(1));
                self.sheet_press(pressed);
            }
            Key::Tab if event.mods.shift => sheet.step(true),
            Key::Tab | Key::Down => sheet.step(false),
            Key::Up => sheet.step(true),
            Key::Left => sheet.turn(-1, false),
            Key::Right => sheet.turn(1, false),
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
