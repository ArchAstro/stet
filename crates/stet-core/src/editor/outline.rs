//! The outline: the document's headings as a foldable tree. Two views share
//! one state — the pop-up menu (`g o`, keyboard first) and the panel beside
//! the text (mouse first, follows the cursor) — and one set of folds, keyed
//! by heading so an edit that moves text around keeps them.

use super::{Act, Editor, Item, Palette};
use crate::input::{Key, KeyEvent};
use crate::markdown::Heading;
use std::collections::HashSet;

/// A folded heading, by its level and text. The text keeps the fold on the
/// right heading as lines move; renaming or removing it drops the fold.
pub(super) type Fold = (u8, String);

/// One row of the outline, in both the menu and the panel.
pub struct OutlineRow {
    /// 0 for a top-level heading.
    pub depth: u8,
    /// The heading's first line.
    pub line: usize,
    /// Headings nest under this one, so it can be folded.
    pub foldable: bool,
    pub folded: bool,
    pub text: String,
}

/// The panel beside the text: the document's headings, following the cursor.
#[derive(Default)]
pub struct OutlinePanel {
    pub visible: bool,
    pub scroll: usize,
}

/// The visible headings as rows, subtrees of folded headings dropped.
pub(super) fn rows(entries: &[Heading], folded: &HashSet<Fold>) -> Vec<OutlineRow> {
    let mut out = Vec::with_capacity(entries.len());
    // A folded heading hides everything under it until one of its own level
    // or shallower turns up.
    let mut hidden: Option<u8> = None;
    for (index, heading) in entries.iter().enumerate() {
        if let Some(level) = hidden {
            if heading.level > level {
                continue;
            }
            hidden = None;
        }
        let foldable = entries.get(index + 1).is_some_and(|next| next.level > heading.level);
        let folds = foldable
            && folded
                .iter()
                .any(|(level, text)| *level == heading.level && text == &heading.text);
        if folds {
            hidden = Some(heading.level);
        }
        out.push(OutlineRow {
            depth: heading.level.saturating_sub(1),
            line: heading.line,
            foldable,
            folded: folds,
            text: heading.text.clone(),
        });
    }
    out
}

/// The visible headings as menu items, with a row beside each.
pub(super) fn build(entries: &[Heading], folded: &HashSet<Fold>) -> (Vec<Item>, Vec<OutlineRow>) {
    let rows = rows(entries, folded);
    let items = rows
        .iter()
        .map(|row| Item {
            label: row.text.clone(),
            detail: String::new(),
            section: String::new(),
            act: Act::Goto { line: row.line },
        })
        .collect();
    (items, rows)
}

/// Index of the row the cursor is in: the last heading at or above it.
pub(super) fn at_cursor(rows: &[OutlineRow], line: usize) -> usize {
    rows.iter().rposition(|row| row.line <= line).unwrap_or(0)
}

impl Editor {
    /// Opens the outline menu, starting on the heading the cursor is in.
    pub(super) fn open_outline(&mut self) {
        self.refresh();
        let entries = self.doc.headings();
        if entries.is_empty() {
            return self.info("no headings in this document");
        }
        let (items, rows) = build(&entries, &self.outline_folded);
        let mut palette = Palette::outline(items, rows);
        palette.selected = at_cursor(&palette.outline, self.buf.line_of(self.cursor));
        self.palette = Some(palette);
    }

    /// Shows or hides the outline panel.
    pub fn toggle_outline_panel(&mut self) {
        self.outline_panel.visible = !self.outline_panel.visible;
        self.outline_panel.scroll = 0;
        // The rows are rebuilt on the next frame's refresh.
        self.outline_rows_key = None;
    }

    /// The panel's rows for the current analysis.
    pub fn outline_rows(&self) -> &[OutlineRow] {
        &self.outline_rows
    }

    /// Rebuilds the panel's rows when the analysis, or the document, changed.
    pub(super) fn refresh_outline_rows(&mut self) {
        if !self.outline_panel.visible {
            return;
        }
        let key = (self.doc_id, self.buf.revision());
        if self.outline_rows_key == Some(key) {
            return;
        }
        self.outline_rows = rows(&self.doc.headings(), &self.outline_folded);
        self.outline_rows_key = Some(key);
    }

    /// Puts the cursor on `line`'s first character, as a jump back can find.
    pub(super) fn goto_line(&mut self, line: usize) {
        self.push_jump();
        self.cursor = self.buf.line_start(line.min(self.buf.line_count() - 1));
        self.clamp_cursor();
    }

    /// A click on the panel's row `index`: jump to its heading.
    pub fn outline_panel_click(&mut self, index: usize) {
        let Some(line) = self.outline_rows.get(index).map(|row| row.line) else {
            return;
        };
        self.goto_line(line);
    }

    /// A click on the panel's row `index` fold arrow. The rows rebuild on the
    /// next frame's refresh.
    pub fn outline_panel_fold(&mut self, index: usize) {
        let Some(row) = self.outline_rows.get(index) else {
            return;
        };
        if !row.foldable {
            return;
        }
        let fold = (row.depth + 1, row.text.clone());
        if row.folded {
            self.outline_folded.remove(&fold);
        } else {
            self.outline_folded.insert(fold);
        }
        self.outline_rows_key = None;
    }

    /// Folds or unfolds the selected heading's subtree, keeping the
    /// selection on it.
    pub(super) fn outline_fold(&mut self, fold: bool) {
        let selected = self.palette.as_ref().and_then(|palette| {
            let row = palette.outline.get(palette.matches.get(palette.selected).copied()?)?;
            Some((row.line, row.depth + 1, row.text.clone(), row.foldable, row.folded))
        });
        let Some((line, level, text, foldable, folded)) = selected else {
            return;
        };
        if fold == folded || (fold && !foldable) {
            return;
        }
        if fold {
            self.outline_folded.insert((level, text));
        } else {
            self.outline_folded.remove(&(level, text));
        }
        self.outline_rebuild(Some(line));
    }

    /// Rebuilds the rows after a fold change, keeping the selection on the
    /// same heading and replaying the query.
    fn outline_rebuild(&mut self, keep: Option<usize>) {
        self.refresh();
        let (items, rows) = build(&self.doc.headings(), &self.outline_folded);
        let Some(palette) = &mut self.palette else {
            return;
        };
        palette.items = items;
        palette.outline = rows;
        palette.filter();
        if let Some(line) = keep {
            palette.selected = palette
                .matches
                .iter()
                .position(|&index| palette.outline[index].line == line)
                .unwrap_or(0);
        }
    }

    /// Keys while the outline is open. Esc and Tab put the keyboard back in
    /// the document; j/k and the arrows move; Left/Right fold; everything
    /// else filters.
    pub(super) fn outline_key(&mut self, event: KeyEvent) {
        let ctrl = event.mods.ctrl;
        let plain = event.plain_char();
        match event.key {
            Key::Esc | Key::Tab => self.close_palette(),
            Key::Char('c' | '[' | 'g') if ctrl => self.close_palette(),
            Key::Enter => self.palette_run(),
            Key::Left => self.outline_fold(true),
            Key::Right => self.outline_fold(false),
            Key::Down => self.palette_move(1),
            Key::Up => self.palette_move(-1),
            Key::PageDown => self.palette_move(8),
            Key::PageUp => self.palette_move(-8),
            Key::Char('j') if plain.is_some() => self.palette_move(1),
            Key::Char('k') if plain.is_some() => self.palette_move(-1),
            Key::Char('n') if ctrl => self.palette_move(1),
            Key::Char('p') if ctrl => self.palette_move(-1),
            Key::Char('d') if ctrl => self.palette_move(4),
            Key::Char('u') if ctrl => self.palette_move(-4),
            _ => {
                let Some(palette) = self.palette.as_mut() else {
                    return;
                };
                match event.key {
                    Key::Backspace => {
                        palette.query.pop();
                    }
                    Key::Char(c) if plain.is_some() => palette.query.push(c),
                    _ => return,
                }
                palette.filter();
            }
        }
    }
}
