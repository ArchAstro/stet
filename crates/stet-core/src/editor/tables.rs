//! Working in a pipe table: its shape for the shell to draw, moving between
//! cells, and keeping the source tidy.

use super::{Editor, Mode};
use crate::markdown::Block;
use crate::table::{self, Row, Shape};
use std::ops::Range;
use std::sync::Arc;

/// `(analysis revision, tables found so far)`.
pub(super) type TableCache = (Option<u64>, Vec<(Range<usize>, Option<Arc<Shape>>)>);

impl Editor {
    /// The lines of the table that `line` is part of. The analysis must be
    /// current (`refresh`).
    pub fn table_lines(&self, line: usize) -> Option<Range<usize>> {
        let is_row = |line: usize| self.doc.block(line) == Block::Table;
        if !is_row(line) {
            return None;
        }
        let (mut first, mut end) = (line, line + 1);
        while first > 0 && is_row(first - 1) {
            first -= 1;
        }
        while end < self.buf.line_count() && is_row(end) {
            end += 1;
        }
        Some(first..end)
    }

    /// The table `line` is part of and what its rows share, for drawing it
    /// as a grid. `None` for a table that is shown as source.
    pub fn table_at(&self, line: usize) -> Option<(Range<usize>, Arc<Shape>)> {
        if !self.config.table_grid || self.doc_revision != Some(self.buf.revision()) {
            return None;
        }
        let mut cache = self.table_cache.borrow_mut();
        if cache.0 != self.doc_revision {
            cache.0 = self.doc_revision;
            cache.1.clear();
        }
        if let Some((lines, shape)) = cache.1.iter().find(|(lines, _)| lines.contains(&line)) {
            return shape.clone().map(|shape| (lines.clone(), shape));
        }
        let lines = self.table_lines(line)?;
        let texts: Vec<String> = lines.clone().map(|line| self.buf.line_text(line)).collect();
        let shape = Shape::of(&texts).map(Arc::new);
        cache.1.push((lines.clone(), shape.clone()));
        shape.map(|shape| (lines, shape))
    }

    /// The cell texts of a table, row by row.
    fn table_cells(&self, lines: Range<usize>) -> Vec<Vec<String>> {
        lines
            .map(|line| {
                let text = self.buf.line_text(line);
                Row::parse(&text)
                    .cells
                    .iter()
                    .map(|cell| text[cell.text.clone()].to_string())
                    .collect()
            })
            .collect()
    }

    /// `(row in the table, cell, characters into the cell's text)` of the cursor.
    fn table_place(&self, lines: &Range<usize>) -> (usize, usize, usize) {
        let line = self.buf.line_of(self.cursor);
        let text = self.buf.line_text(line);
        let byte = text
            .char_indices()
            .nth(self.cursor - self.buf.line_start(line))
            .map_or(text.len(), |(byte, _)| byte);
        let row = Row::parse(&text);
        let index = row.cell_at(byte);
        let within = row.cells.get(index).map_or(0, |cell| {
            let inside = byte.clamp(cell.text.start, cell.text.end);
            text[cell.text.start..inside].chars().count()
        });
        (line - lines.start, index, within)
    }

    /// Puts the cursor in a cell: `within` characters into its text, or at
    /// its end.
    fn table_go(&mut self, line: usize, cell: usize, within: Option<usize>) {
        let text = self.buf.line_text(line);
        let row = Row::parse(&text);
        let Some(cell) = row.cells.get(cell.min(row.cells.len().saturating_sub(1))) else {
            return;
        };
        let length = text[cell.text.clone()].chars().count();
        let before = text[..cell.text.start].chars().count();
        self.cursor = self.buf.line_start(line) + before + within.map_or(length, |within| within.min(length));
        self.anchor = None;
        self.clamp_cursor();
    }

    /// Rewrites the table at `lines` from its cells. Whitespace only, so it
    /// is never recorded as a suggestion.
    fn write_table(&mut self, lines: Range<usize>, rows: &[Vec<String>]) -> bool {
        let source: Vec<String> = rows.iter().map(|row| format!("| {} |", row.join(" | "))).collect();
        let first = self.buf.line_text(lines.start);
        let indent = &first[..first.len() - first.trim_start().len()];
        let source: Vec<String> = source.into_iter().map(|line| format!("{indent}{line}")).collect();
        let Some(tidy) = table::format(&source) else {
            return false;
        };
        let range = self.buf.line_start(lines.start)..self.buf.line_end(lines.end - 1);
        let tidy = tidy.join("\n");
        if self.buf.slice(range.clone()) != tidy {
            self.raw_edit(range, &tidy);
        }
        true
    }

    /// `:table`: tidies the table under the cursor, changes it, or makes one.
    pub(super) fn table_command(&mut self, arg: &str) {
        self.refresh();
        let line = self.buf.line_of(self.cursor);
        let numbers: Vec<usize> = arg
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|word| word.parse().ok())
            .collect();
        let Some(lines) = self.table_lines(line) else {
            let (columns, rows) = match numbers[..] {
                [columns, rows, ..] => (columns, rows),
                [columns] => (columns, 2),
                _ if arg.is_empty() => (3, 2),
                _ => return self.error("the cursor is not in a table (:table 3 2 makes one)"),
            };
            let text = table::skeleton(columns.clamp(1, 40), rows.clamp(1, 200));
            // Below the line the cursor is on, never inside its words.
            let at = if self.mode == Mode::Insert {
                self.cursor
            } else {
                self.buf.line_end(line)
            };
            let text = self.apart(at..at, text);
            let lead = text.len() - text.trim_start_matches('\n').len();
            let pos = self.edit(at..at, &text);
            self.cursor = pos.start + lead + 2;
            self.close_group();
            return;
        };
        let mut rows = self.table_cells(lines.clone());
        let (row, cell, within) = self.table_place(&lines);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(1);
        let mut land = (row, cell, Some(within));
        let mut words = arg.split_whitespace();
        let (verb, how) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        // Every row gets every column before any of them moves.
        if matches!(verb, "column" | "col" | "c" | "moveleft" | "moveright") {
            for cells in &mut rows {
                cells.resize(columns.max(cells.len()), String::new());
            }
        }
        match verb {
            "" | "format" | "tidy" | "align" => {}
            "row" | "r" => {
                let at = if matches!(how, "above" | "before") {
                    row.max(2)
                } else {
                    row.max(1) + 1
                };
                rows.insert(at, vec![String::new(); columns]);
                land = (at, 0, None);
            }
            "column" | "col" | "c" => {
                let at = if matches!(how, "left" | "before") {
                    cell
                } else {
                    cell + 1
                };
                for (index, cells) in rows.iter_mut().enumerate() {
                    cells.insert(at, if index == 1 { "---".to_string() } else { String::new() });
                }
                land = (row, at, None);
            }
            "moveup" | "movedown" => {
                let to = if verb == "moveup" { row.wrapping_sub(1) } else { row + 1 };
                if row < 2 || to < 2 || to >= rows.len() {
                    return self.error("that row cannot move there");
                }
                rows.swap(row, to);
                land = (to, cell, Some(within));
            }
            "moveleft" | "moveright" => {
                let to = if verb == "moveleft" {
                    cell.wrapping_sub(1)
                } else {
                    cell + 1
                };
                if to >= columns {
                    return self.error("that column cannot move there");
                }
                for cells in &mut rows {
                    cells.swap(cell, to);
                }
                land = (row, to, Some(within));
            }
            "delcolumn" | "delcol" | "dc" => {
                if columns < 2 {
                    return self.error("a table needs a column");
                }
                for cells in &mut rows {
                    if cell < cells.len() {
                        cells.remove(cell);
                    }
                }
                land = (row, cell.min(columns - 2), None);
            }
            "delrow" | "dr" => {
                if row < 2 {
                    return self.error("that row is the table's heading");
                }
                rows.remove(row);
                land = (row.min(rows.len() - 1), cell, None);
            }
            "left" | "center" | "centre" | "right" => {
                let rule = match verb {
                    "left" => ":--",
                    "right" => "--:",
                    _ => ":-:",
                };
                if let Some(target) = rows.get_mut(1).and_then(|rule_row| rule_row.get_mut(cell)) {
                    *target = rule.to_string();
                }
            }
            _ => {
                return self.error(
                    "table: row, column, delrow, delcolumn, moveup, movedown, moveleft, moveright, left, center, right",
                );
            }
        }
        let count = rows.len();
        if !self.write_table(lines.clone(), &rows) {
            return self.error("this table is inside a quote or a list; it is left as it is");
        }
        self.close_group();
        self.table_go(lines.start + land.0.min(count - 1), land.1, land.2);
    }

    /// Tab in a table: tidy it and move to the next cell (or the one
    /// before), adding a row after the last. False if the cursor is not in
    /// a table.
    pub(super) fn table_tab(&mut self, back: bool) -> bool {
        self.refresh();
        let Some(lines) = self.table_lines(self.buf.line_of(self.cursor)) else {
            return false;
        };
        let mut rows = self.table_cells(lines.clone());
        let columns = rows.iter().map(Vec::len).max().unwrap_or(1);
        let (mut row, mut cell, _) = self.table_place(&lines);
        if back {
            if cell > 0 {
                cell -= 1;
            } else if row > 0 {
                row -= if row == 2 { 2 } else { 1 };
                cell = columns - 1;
            }
        } else if cell + 1 < columns {
            cell += 1;
        } else {
            row += if row == 0 { 2 } else { 1 };
            cell = 0;
        }
        if row == 1 {
            row = if back { 0 } else { 2 };
        }
        if row >= rows.len() {
            rows.resize(row + 1, vec![String::new(); columns]);
        }
        if !self.write_table(lines.clone(), &rows) {
            return false;
        }
        self.table_go(lines.start + row, cell, None);
        if self.mode != Mode::Insert {
            self.close_group();
        }
        true
    }

    /// A click on a table's row or column handle: the cursor goes to that
    /// row (`column` is `None`) or column, and its menu opens at the pointer.
    pub fn table_handle(&mut self, line: usize, column: Option<usize>, x: f32, y: f32) {
        self.refresh();
        let Some(lines) = self.table_lines(line) else { return };
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        self.close_group();
        match column {
            Some(column) => self.table_go(lines.start, column, Some(0)),
            None => self.table_go(line, 0, Some(0)),
        }
        self.table_menu(column.is_some(), line >= lines.start + 2, x, y);
    }

    /// A click on the strip beside or below a table: one more column, or
    /// one more row, at its end.
    pub fn table_extend(&mut self, line: usize, column: bool) {
        self.refresh();
        let Some(lines) = self.table_lines(line) else { return };
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        if column {
            let cells = Row::parse(&self.buf.line_text(lines.start)).cells.len();
            self.table_go(lines.start, cells.saturating_sub(1), None);
            self.table_command("column");
        } else {
            self.table_go(lines.end - 1, 0, None);
            self.table_command("row");
        }
    }
}
