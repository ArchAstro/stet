//! Pipe tables: where a row's cells are, how wide its columns want to be,
//! and how to write the source tidily.

use std::ops::Range;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// One cell of a row, as byte ranges into its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Everything between the pipes, padding included.
    pub outer: Range<usize>,
    /// The content, without the padding.
    pub text: Range<usize>,
}

/// A row of a table, split at its unescaped pipes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Byte offset of every pipe.
    pub pipes: Vec<usize>,
    /// The row opens with a pipe, so pipe `n` is the left edge of cell `n`.
    pub leading: bool,
}

impl Row {
    pub fn parse(line: &str) -> Row {
        let bytes = line.as_bytes();
        let mut pipes = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'\\' => at += 1,
                b'|' => pipes.push(at),
                _ => {}
            }
            at += 1;
        }
        let mut pieces: Vec<Range<usize>> = Vec::with_capacity(pipes.len() + 1);
        let mut start = 0;
        for &pipe in &pipes {
            pieces.push(start..pipe);
            start = pipe + 1;
        }
        pieces.push(start..line.len());
        let blank = |piece: &Range<usize>| line[piece.clone()].trim().is_empty();
        // The pieces outside the outer pipes are not cells.
        let leading = !pipes.is_empty() && blank(&pieces[0]);
        if !pipes.is_empty() && pieces.last().is_some_and(blank) {
            pieces.pop();
        }
        if leading {
            pieces.remove(0);
        }
        let cells = pieces
            .into_iter()
            .map(|outer| {
                let raw = &line[outer.clone()];
                let lead = raw.len() - raw.trim_start().len();
                let text = if raw.trim().is_empty() {
                    let at = outer.start + lead.min(1);
                    at..at
                } else {
                    outer.start + lead..outer.start + raw.trim_end().len()
                };
                Cell { outer, text }
            })
            .collect();
        Row { cells, pipes, leading }
    }

    /// The cell holding byte `at`, counting each pipe with the cell it opens.
    pub fn cell_at(&self, at: usize) -> usize {
        let last = self.cells.len().saturating_sub(1);
        self.cells
            .iter()
            .position(|cell| at <= cell.outer.end)
            .unwrap_or(last)
            .min(last)
    }
}

/// The alignment a delimiter cell (`---`, `:--`, `:-:`, `--:`) asks for.
fn delimiter(cell: &str) -> Option<Align> {
    let dashes = cell.trim_start_matches(':').trim_end_matches(':');
    if dashes.is_empty() || !dashes.bytes().all(|byte| byte == b'-') {
        return None;
    }
    Some(match (cell.starts_with(':'), cell.ends_with(':')) {
        (true, true) => Align::Center,
        (true, false) => Align::Left,
        (false, true) => Align::Right,
        (false, false) => Align::None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Column {
    /// The widest cell, in monospace cells.
    pub width: usize,
    /// The widest run that cannot wrap.
    pub word: usize,
    pub align: Align,
}

/// What a table's rows have in common.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Shape {
    pub columns: Vec<Column>,
}

impl Shape {
    /// Reads a table from its lines: header, delimiter row, then the body.
    /// `None` if the lines are not a table this module understands (one in
    /// a quote or a list, say), which is then shown as plain source.
    pub fn of<S: AsRef<str>>(lines: &[S]) -> Option<Shape> {
        let header = Row::parse(lines.first()?.as_ref());
        let rule_line = lines.get(1)?.as_ref();
        let rule = Row::parse(rule_line);
        let aligns: Vec<Align> = rule
            .cells
            .iter()
            .map(|cell| delimiter(&rule_line[cell.text.clone()]))
            .collect::<Option<_>>()?;
        if aligns.is_empty() || header.cells.len() != aligns.len() {
            return None;
        }
        let mut columns: Vec<Column> = aligns
            .into_iter()
            .map(|align| Column {
                width: 3,
                word: 1,
                align,
            })
            .collect();
        for (_, line) in lines.iter().enumerate().filter(|(index, _)| *index != 1) {
            let line = line.as_ref();
            for (at, cell) in Row::parse(line).cells.iter().enumerate() {
                if at == columns.len() {
                    columns.push(Column {
                        width: 3,
                        word: 1,
                        align: Align::None,
                    });
                }
                let text = &line[cell.text.clone()];
                let column = &mut columns[at];
                column.width = column.width.max(text.width());
                let word = text.split_whitespace().map(UnicodeWidthStr::width).max().unwrap_or(0);
                column.word = column.word.max(word);
            }
        }
        Some(Shape { columns })
    }

    /// Column widths that fit `available` cells of content: every column at
    /// its natural width if there is room, otherwise the wide ones give way
    /// first and wrap.
    pub fn fit(&self, available: usize) -> Vec<usize> {
        let natural: usize = self.columns.iter().map(|column| column.width).sum();
        if natural <= available {
            return self.columns.iter().map(|column| column.width).collect();
        }
        // No column is squeezed below a short word, or below what it needs.
        let floor = |column: &Column| column.width.min(column.word.clamp(4, 14));
        let mut widths: Vec<usize> = self.columns.iter().map(floor).collect();
        let mut spare = available.saturating_sub(widths.iter().sum());
        while spare > 0 {
            let narrowest = widths
                .iter()
                .zip(&self.columns)
                .filter(|(width, column)| **width < column.width)
                .map(|(width, _)| *width)
                .min();
            let Some(narrowest) = narrowest else { break };
            for (width, column) in widths.iter_mut().zip(&self.columns) {
                if spare > 0 && *width == narrowest && *width < column.width {
                    *width += 1;
                    spare -= 1;
                }
            }
        }
        widths
    }
}

/// Lines longer than this are written without padding: alignment that only
/// works in a very wide window helps nobody.
const ALIGN_UP_TO: usize = 100;

/// Rewrites a table's source with one space of padding and, when it fits,
/// the pipes lined up. `None` if `lines` is not a table.
pub fn format<S: AsRef<str>>(lines: &[S]) -> Option<Vec<String>> {
    let shape = Shape::of(lines)?;
    let first = lines[0].as_ref();
    let indent = &first[..first.len() - first.trim_start().len()];
    let rows: Vec<Vec<&str>> = lines
        .iter()
        .map(|line| {
            let line = line.as_ref();
            Row::parse(line)
                .cells
                .iter()
                .map(|cell| &line[cell.text.clone()])
                .collect()
        })
        .collect();
    let count = shape.columns.len();
    let widths: Vec<usize> = shape.columns.iter().map(|column| column.width).collect();
    let aligned = indent.width() + widths.iter().sum::<usize>() + count * 3 < ALIGN_UP_TO;
    let mut out = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let mut line = String::from(indent);
        for (at, column) in shape.columns.iter().enumerate() {
            let width = if aligned { widths[at] } else { 0 };
            line.push_str("| ");
            if index == 1 {
                let dashes = width.max(3)
                    - matches!(column.align, Align::Left | Align::Center) as usize
                    - matches!(column.align, Align::Right | Align::Center) as usize;
                if matches!(column.align, Align::Left | Align::Center) {
                    line.push(':');
                }
                line.extend(std::iter::repeat_n('-', dashes));
                if matches!(column.align, Align::Right | Align::Center) {
                    line.push(':');
                }
            } else {
                let text = row.get(at).copied().unwrap_or("");
                let room = width.saturating_sub(text.width());
                let before = match column.align {
                    Align::Right => room,
                    Align::Center => room / 2,
                    _ => 0,
                };
                line.extend(std::iter::repeat_n(' ', before));
                line.push_str(text);
                line.extend(std::iter::repeat_n(' ', room - before));
            }
            line.push(' ');
        }
        line.push('|');
        out.push(line);
    }
    Some(out)
}

/// An empty table of `columns` by `rows` body rows.
pub fn skeleton(columns: usize, rows: usize) -> String {
    let row = |cell: &str| format!("|{}", format!(" {cell} |").repeat(columns));
    let mut lines = vec![row("   "), row("---")];
    lines.extend(std::iter::repeat_n(row("   "), rows));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(line: &str) -> Vec<&str> {
        Row::parse(line)
            .cells
            .iter()
            .map(|cell| &line[cell.text.clone()])
            .collect()
    }

    #[test]
    fn rows_split_at_unescaped_pipes_with_or_without_outer_ones() {
        assert_eq!(cells("| a | b |"), ["a", "b"]);
        assert_eq!(cells("a | b"), ["a", "b"]);
        assert_eq!(cells("|a|b"), ["a", "b"]);
        assert_eq!(cells("  | a \\| b | `c` |  "), ["a \\| b", "`c`"]);
        assert_eq!(cells("| a |  | c |"), ["a", "", "c"]);
        assert_eq!(cells("||"), [""]);
        let row = Row::parse("| ab | c |");
        assert!(row.leading);
        assert_eq!(row.pipes, [0, 5, 9]);
        assert_eq!(
            (row.cell_at(0), row.cell_at(3), row.cell_at(5), row.cell_at(9)),
            (0, 0, 0, 1)
        );
    }

    #[test]
    fn a_shape_needs_a_header_and_a_matching_delimiter_row() {
        let shape = Shape::of(&[
            "| Name | Qty |",
            "|:--|--:|",
            "| Apple pie | 3 |",
            "| Fig | 12 | extra |",
        ])
        .unwrap();
        let widths: Vec<usize> = shape.columns.iter().map(|column| column.width).collect();
        assert_eq!(widths, [9, 3, 5]);
        assert_eq!(shape.columns[0].align, Align::Left);
        assert_eq!(shape.columns[1].align, Align::Right);
        assert_eq!(shape.columns[0].word, 5);
        assert!(Shape::of(&["| a | b |", "| - |"]).is_none());
        assert!(Shape::of(&["> | a |", "> | - |"]).is_none());
        assert!(Shape::of(&["| a |"]).is_none());
        // Wide characters count for two cells.
        let wide = Shape::of(&["| 日本語 |", "| - |"]).unwrap();
        assert_eq!(wide.columns[0].width, 6);
    }

    #[test]
    fn columns_that_do_not_fit_give_way_widest_first() {
        let shape = Shape {
            columns: [(4, 4), (60, 9), (30, 7)]
                .map(|(width, word)| Column {
                    width,
                    word,
                    align: Align::None,
                })
                .to_vec(),
        };
        assert_eq!(shape.fit(100), [4, 60, 30]);
        assert_eq!(shape.fit(54), [4, 25, 25]);
        assert_eq!(shape.fit(44), [4, 20, 20]);
        // Never below what a word needs, even when that overflows.
        assert_eq!(shape.fit(10), [4, 9, 7]);
    }

    #[test]
    fn formatting_lines_the_pipes_up_and_keeps_alignment() {
        let tidy = format(&["|Name|Qty|Note", "|:-|-:|:-:|", "|Apple pie|3|ok|", "|Fig|12"]).unwrap();
        assert_eq!(
            tidy,
            [
                "| Name      | Qty | Note |",
                "| :-------- | --: | :--: |",
                "| Apple pie |   3 |  ok  |",
                "| Fig       |  12 |      |",
            ]
        );
        // Tidy source is left alone.
        assert_eq!(format(&tidy).unwrap(), tidy);
        // A table too wide to line up is written compactly.
        let long = "word ".repeat(30);
        let wide = format(&[
            "| a | b |".to_string(),
            "|---|---|".to_string(),
            format!("| {long}| x |"),
        ])
        .unwrap();
        assert_eq!(wide[0], "| a | b |");
        assert_eq!(wide[1], "| --- | --- |");
        assert_eq!(wide[2], format!("| {} | x |", long.trim()));
        assert!(format(&["plain", "text"]).is_none());
    }

    #[test]
    fn a_skeleton_is_a_table() {
        let text = skeleton(3, 2);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(Shape::of(&lines).unwrap().columns.len(), 3);
    }
}
