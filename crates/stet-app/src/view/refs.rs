//! Data refs in the document: `[signups]` in the prose drawn as a chip,
//! the definition it stands for marked with the file's shape (or what is
//! wrong with it), the schema on hover, and the first rows shown under the
//! line on a click. A ```` ```chart ```` fence is drawn as a chart of the
//! refs it names, with no kernel.

use super::outputs::{Block, Cx, Row};
use super::{Layer, Metrics, Quad, Target, TextItem, View};
use crate::data::{self, Look};
use crate::gpu::linear;
use crate::text::{Face, color};
use serde_json::Value;
use std::ops::Range;
use std::sync::Arc;
use stet_core::Editor;
use stet_core::markdown::{DataRef, style};
use stet_data::Peek;

/// A chip drawn in the last frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chip {
    pub rect: [f32; 4],
    pub name: String,
    pub line: usize,
}

/// Where data refs are written on a line, in characters, and which ref.
pub(super) fn chips_on(ed: &Editor, line: usize) -> Vec<(Range<usize>, usize)> {
    let doc = ed.doc();
    if doc.data_refs.is_empty() || doc.in_fence(line) {
        return Vec::new();
    }
    let text = ed.buf.line_text(line);
    let lower = text.to_lowercase();
    // Lower-casing can change lengths; then the line is skipped.
    if lower.len() != text.len() {
        return Vec::new();
    }
    let spans = doc.spans(line);
    let linked = |at: usize| {
        spans
            .iter()
            .any(|span| span.style & style::LINK != 0 && (span.start as usize..span.end as usize).contains(&at))
    };
    let chars = |byte: usize| text[..byte].chars().count();
    let mut found = Vec::new();
    for (index, data) in doc.data_refs.iter().enumerate() {
        if data.line == line {
            continue;
        }
        let needle = format!("[{}]", data.name.to_lowercase());
        for (at, _) in lower.match_indices(&needle) {
            // Inside a link the parser resolved, so not in code or escaped.
            if linked(at + 1) {
                found.push((chars(at)..chars(at + needle.len()), index));
            }
        }
    }
    found.sort_by_key(|(range, _)| range.start);
    found
}

/// A data ref's shape, as its chip reads.
fn shape(peek: &Peek) -> String {
    let rows = peek.total.unwrap_or(peek.rows.len() as u64);
    let table = peek
        .table
        .as_deref()
        .map(|table| format!("{table} · "))
        .unwrap_or_default();
    format!("{table}{} rows × {} cols", grouped(rows), peek.columns.len())
}

/// `12,034`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

impl View {
    fn peek_ref(&mut self, ed: &Editor, data: &DataRef, rows: usize) -> Result<Option<Arc<Peek>>, String> {
        let path = data::resolve(&data.path, ed.path.as_deref());
        match self.data.get(&path, data.table.as_deref(), rows) {
            Look::Loading => Ok(None),
            Look::Ready(peek) => Ok(Some(peek.clone())),
            Look::Failed(err) => Err(err.to_string()),
        }
    }

    /// What a definition line's chip says, and whether it is a problem.
    pub(super) fn def_chip(&mut self, ed: &Editor, line: usize) -> Option<(String, bool)> {
        let data = ed.doc().data_refs.iter().find(|data| data.line == line)?.clone();
        Some(match self.peek_ref(ed, &data, data::PREVIEW_ROWS) {
            Ok(Some(peek)) => (format!("{} · {}", peek.format, shape(&peek)), false),
            Ok(None) => ("reading…".into(), false),
            // The path is the user's own; the reason is what matters.
            Err(err) => (err.rsplit(": ").next().unwrap_or(&err).to_string(), true),
        })
    }

    /// Draws the chips of `line`, laid out as `key`, at `left`, `y`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_chips(
        &mut self,
        ed: &Editor,
        line: usize,
        key: u64,
        left: f32,
        y: f32,
        m: &Metrics,
        layer: &mut Layer,
    ) {
        let c = ed.theme.colors;
        let clip = |rect: [f32; 4]| {
            let top = rect[1].max(m.top);
            let bottom = (rect[1] + rect[3]).min(m.bottom);
            [rect[0], top, rect[2], (bottom - top).max(0.0)]
        };
        let pad = (m.font * 0.18).round();
        for (chars, index) in chips_on(ed, line) {
            let data = ed.doc().data_refs[index].clone();
            let name = data.name.clone();
            // A ref to a file that cannot be read says so in the prose too.
            let back = match self.peek_ref(ed, &data, data::PREVIEW_ROWS) {
                Err(_) => linear(c.error, 0.16),
                _ => linear(c.code_background, 1.0),
            };
            let layout = &self.layouts[&key];
            let bytes = layout.byte_of_col(chars.start)..layout.byte_of_col(chars.end);
            for (row, x0, x1) in layout.ranges(bytes) {
                let row = &layout.rows[row];
                let rect = [
                    left + x0 - pad,
                    y + row.top + pad * 0.5,
                    x1 - x0 + pad * 2.0,
                    row.height - pad,
                ];
                layer.back.push(Quad {
                    rect: clip(rect),
                    color: back,
                    radius: pad * 1.5,
                });
                let seen = clip(rect);
                if seen[3] > 0.0 {
                    if self.preview_at.is_none() && self.preview.as_ref() == Some(&(name.clone(), line)) {
                        self.preview_at = Some(seen);
                    }
                    self.targets.push((seen, Target::DataRef(self.chips.len())));
                    self.chips.push(Chip {
                        rect: seen,
                        name: name.clone(),
                        line,
                    });
                }
            }
        }
        // The definition says what the file holds, after its last word.
        if let Some((text, bad)) = self.def_chip(ed, line) {
            let small = (m.font * 0.74).round();
            let rgb = if bad { c.error } else { c.muted };
            let label = self.label(ed, &text, rgb, small, Face::Ui, false);
            let layout = &self.layouts[&key];
            let Some(last) = layout.rows.last() else { return };
            let (row_top, row_height) = (last.top, last.height);
            let x = left + last.width + m.advance * 1.5;
            let width = self.layouts[&label].rows[0].width;
            let height = self.layouts[&label].height;
            let middle = y + row_top + row_height / 2.0;
            let rect = [x - pad * 2.0, middle - height / 2.0, width + pad * 4.0, height];
            layer.back.push(Quad {
                rect: clip(rect),
                color: linear(c.code_background, 1.0),
                radius: height / 2.0,
            });
            layer.texts.push(TextItem {
                key: label,
                left: x.round(),
                top: (middle - height / 2.0).round(),
                clip: [m.origin, m.top, m.right, m.bottom],
                color: color(rgb),
            });
            let name = ed
                .doc()
                .data_refs
                .iter()
                .find(|data| data.line == line)
                .map(|d| d.name.clone());
            if let Some(name) = name {
                let seen = clip(rect);
                if self.preview_at.is_none() && self.preview.as_ref() == Some(&(name.clone(), line)) {
                    self.preview_at = Some(seen);
                }
                self.targets.push((seen, Target::DataRef(self.chips.len())));
                self.chips.push(Chip { rect: seen, name, line });
            }
        }
    }

    /// The chip under the pointer, from the last frame.
    pub(super) fn chip_hover(&self) -> Option<Chip> {
        let (x, y) = self.pointer;
        self.chips
            .iter()
            .find(|chip| {
                let [left, top, width, height] = chip.rect;
                x >= left && x < left + width && y >= top && y < top + height
            })
            .cloned()
    }

    /// Shows the first rows of the chip's file in the margin beside it, or
    /// hides them if they are showing. Any other index (the panel itself)
    /// hides them.
    pub fn toggle_preview(&mut self, index: usize) {
        let Some(chip) = self.chips.get(index) else {
            self.preview = None;
            return;
        };
        let preview = (chip.name.clone(), chip.line);
        self.preview = match self.preview.take() {
            Some(open) if open == preview => None,
            _ => Some(preview),
        };
    }

    /// Over a chip: its file's schema, a column a row.
    pub(super) fn chip_card(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let Some(chip) = self.chip_hover.clone() else { return };
        let Some(data) = ed.doc().data_ref(&chip.name).cloned() else {
            return;
        };
        let c = ed.theme.colors;
        let small = (m.font * 0.74).round();
        let mut lines: Vec<(String, String)> = Vec::new();
        let title = match self.peek_ref(ed, &data, data::PREVIEW_ROWS) {
            Ok(Some(peek)) => {
                lines.extend(
                    peek.columns
                        .iter()
                        .take(24)
                        .map(|col| (col.name.clone(), col.kind.clone())),
                );
                if peek.columns.len() > 24 {
                    lines.push((format!("… {} more", peek.columns.len() - 24), String::new()));
                }
                format!("{}  ·  {}", data.name, shape(&peek))
            }
            Ok(None) => format!("{}  ·  reading…", data.name),
            Err(err) => format!("{}  ·  {err}", data.name),
        };
        let title = self.label(ed, &title, c.text, small, Face::Ui, true);
        let path = self.label(ed, &data.path, c.muted, small, Face::Mono, false);
        let keys: Vec<(u64, u64)> = lines
            .iter()
            .map(|(name, kind)| {
                (
                    self.label(ed, name, c.text, small, Face::Mono, false),
                    self.label(
                        ed,
                        if kind.is_empty() { " " } else { kind },
                        c.muted,
                        small,
                        Face::Ui,
                        false,
                    ),
                )
            })
            .collect();
        let width_of = |view: &View, key: u64| view.layouts[&key].rows.first().map_or(0.0, |row| row.width);
        let pad = (8.0 * self.scale).round();
        let row_height = self.layouts[&title].height;
        let names = keys.iter().map(|(name, _)| width_of(self, *name)).fold(0.0, f32::max);
        let kinds = keys.iter().map(|(_, kind)| width_of(self, *kind)).fold(0.0, f32::max);
        let inner = (names + pad * 2.0 + kinds)
            .max(width_of(self, title))
            .max(width_of(self, path));
        let (w, h) = (inner + pad * 2.0, pad * 2.0 + row_height * (2 + keys.len()) as f32);
        let line = self.scale.round().max(1.0);
        let below = chip.rect[1] + chip.rect[3] + pad * 0.5;
        let card_y = if below + h > m.bottom {
            (chip.rect[1] - pad * 0.5 - h).max(m.top)
        } else {
            below
        };
        let card_x = chip.rect[0].min(m.right - w).max(m.origin);
        let (card_x, card_y) = (card_x.round(), card_y.round());
        layer.back.push(Quad {
            rect: [card_x - line, card_y - line, w + 2.0 * line, h + 2.0 * line],
            color: linear(c.rule, 1.0),
            radius: pad * 0.75,
        });
        layer.back.push(Quad {
            rect: [card_x, card_y, w, h],
            color: linear(c.panel, 1.0),
            radius: pad * 0.75,
        });
        let clip = [card_x, card_y, card_x + w, card_y + h];
        let mut text_y = card_y + pad;
        for (key, rgb) in [(title, c.text), (path, c.muted)] {
            layer.texts.push(TextItem {
                key,
                left: card_x + pad,
                top: text_y,
                clip,
                color: color(rgb),
            });
            text_y += row_height;
        }
        for (name, kind) in keys {
            layer.texts.push(TextItem {
                key: name,
                left: card_x + pad,
                top: text_y,
                clip,
                color: color(c.text),
            });
            layer.texts.push(TextItem {
                key: kind,
                left: (card_x + w - pad - width_of(self, kind)).round(),
                top: text_y,
                clip,
                color: color(c.muted),
            });
            text_y += row_height;
        }
    }

    /// What shows under `line` that is not a cell's output: a chart fence's
    /// chart.
    pub(super) fn data_block(&mut self, ed: &Editor, line: usize, m: &Metrics) -> Option<Block> {
        let spec = chart_fence(ed, line)?;
        let size = (m.font * 0.86).round();
        let small = (m.font * 0.74).round();
        let c = ed.theme.colors;
        let mut rows = vec![Row::Gap((m.line * 0.25).round())];
        let cx = Cx {
            ed,
            m,
            files: None,
            size,
            small,
        };
        match serde_json::from_str::<Value>(&spec) {
            Ok(spec) => {
                self.chart_rows(&cx, "", &spec, &mut rows);
            }
            Err(err) => rows.push(self.text_row(ed, &format!("chart: {err}"), c.error, small, Face::Ui, false)),
        }
        rows.push(Row::Gap((m.line * 0.35).round()));
        Some(Block::of(rows))
    }

    /// The first rows of the data ref clicked, in the margin beside its
    /// chip: right of the text, or in the notes pane when it is open. A
    /// window too narrow for it has it over the text's right edge.
    pub(super) fn preview_panel(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let Some(anchor) = self.preview_at else { return };
        let Some((name, _)) = self.preview.clone() else { return };
        let Some(data) = ed.doc().data_ref(&name).cloned() else {
            self.preview = None;
            return;
        };
        let c = ed.theme.colors;
        let small = (m.font * 0.74).round();
        let mut rows = Vec::new();
        let title = match self.peek_ref(ed, &data, data::PREVIEW_ROWS) {
            Ok(Some(peek)) => {
                self.table_rows(ed, &peek.to_json(), small, small, &mut rows);
                format!("{}  ·  {}", data.name, shape(&peek))
            }
            Ok(None) => format!("{}  ·  reading…", data.name),
            Err(err) => format!("{}  ·  {}", data.name, err.rsplit(": ").next().unwrap_or(&err)),
        };
        rows.insert(0, self.text_row(ed, &title, c.text, small, Face::Ui, true));
        rows.insert(1, Row::Gap((small * 0.4).round()));
        let pad = (10.0 * self.scale).round();
        let line = self.scale.round().max(1.0);
        let width_of = |view: &View, row: &Row| match row {
            Row::Text { key, span, .. } => span.unwrap_or(view.layouts[key].rows[0].width),
            _ => 0.0,
        };
        let inner = rows.iter().map(|row| width_of(self, row)).fold(0.0, f32::max);
        let height: f32 = rows
            .iter()
            .map(|row| match row {
                Row::Text { height, .. } => *height,
                Row::Gap(gap) => *gap,
                Row::Picture(shown) => shown.height,
            })
            .sum();
        let (w, h) = (inner + pad * 2.0, height + pad * 2.0);
        let beside = match self.split(ed) {
            Some(split) => split + pad * 2.0,
            None => m.left + m.column + m.advance * 2.0,
        };
        let x = beside.min(self.width - pad - w).max(m.origin + pad).round();
        let y = anchor[1].min(m.bottom - h).max(m.top).round();
        layer.back.push(Quad {
            rect: [x - line, y - line, w + 2.0 * line, h + 2.0 * line],
            color: linear(c.rule, 1.0),
            radius: pad * 0.6,
        });
        layer.back.push(Quad {
            rect: [x, y, w, h],
            color: linear(c.panel, 1.0),
            radius: pad * 0.6,
        });
        let clip = [x, y, x + w, (y + h).min(m.bottom)];
        let mut at = y + pad;
        for row in rows {
            match row {
                Row::Gap(gap) => at += gap,
                Row::Text {
                    key,
                    height,
                    rgb,
                    band,
                    rule,
                    span,
                } => {
                    let wide = span.unwrap_or(inner);
                    if band {
                        layer.back.push(Quad {
                            rect: [x + pad * 0.5, at, wide + pad, height],
                            color: linear(c.code_background, 1.0),
                            radius: 0.0,
                        });
                    }
                    if rule {
                        layer.back.push(Quad {
                            rect: [x + pad, at + height - line, wide, line],
                            color: linear(c.rule, 1.0),
                            radius: 0.0,
                        });
                    }
                    layer.texts.push(TextItem {
                        key,
                        left: x + pad,
                        top: at.round(),
                        clip,
                        color: color(rgb),
                    });
                    at += height;
                }
                Row::Picture(_) => {}
            }
        }
        // A click on the panel puts it away.
        self.targets.push(([x, y, w, h], Target::DataRef(usize::MAX)));
    }

    /// Rows for a chart's `data.ref`: the table, `None` while it is read.
    pub(super) fn chart_table(&mut self, ed: &Editor, name: &str) -> Result<Option<Arc<Peek>>, String> {
        let Some(data) = ed.doc().data_ref(name).cloned() else {
            return Err(format!("no data ref [{name}]: add `[{name}]: path/to/file.csv`"));
        };
        self.peek_ref(ed, &data, data::CHART_ROWS)
    }
}

/// The spec of the ```` ```chart ```` fence `line` closes.
fn chart_fence(ed: &Editor, line: usize) -> Option<String> {
    let doc = ed.doc();
    let at = doc.fences.partition_point(|fence| fence.end <= line);
    let fence = doc
        .fences
        .get(at)
        .filter(|fence| fence.end == line + 1 && fence.len() >= 2)?;
    let head = ed.buf.line_text(fence.start);
    let info = head.trim_start().trim_start_matches(['`', '~']);
    let lang = info.split([' ', '\t', '{']).next().unwrap_or("");
    if !lang.eq_ignore_ascii_case("chart") {
        return None;
    }
    let close = ed.buf.line_text(line);
    let close = close.trim();
    if close.len() < 3 || !(close.bytes().all(|b| b == b'`') || close.bytes().all(|b| b == b'~')) {
        return None;
    }
    Some(
        (fence.start + 1..line)
            .map(|line| ed.buf.line_text(line))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(
            stet_core::Config::default(),
            Box::new(stet_core::MemoryClipboard::default()),
        );
        ed.set_text(text);
        ed.refresh();
        ed
    }

    #[test]
    fn counts_read_in_thousands() {
        assert_eq!(grouped(12034), "12,034");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000_000), "1,000,000");
    }

    #[test]
    fn chips_sit_on_resolved_references_only() {
        let ed = editor("See [signups] and `[signups]` and [Signups][].\n\n[signups]: s.csv\n");
        let found = chips_on(&ed, 0);
        assert_eq!(
            found.iter().map(|(range, _)| range.clone()).collect::<Vec<_>>(),
            [4..13, 34..43]
        );
        assert!(chips_on(&ed, 2).is_empty(), "not on the definition itself");
    }

    #[test]
    fn chart_fences_are_found_by_their_closing_line() {
        let ed = editor("```chart\n{\"mark\": \"bar\"}\n```\n\n```python\nx\n```\n");
        assert_eq!(chart_fence(&ed, 2).as_deref(), Some("{\"mark\": \"bar\"}"));
        assert_eq!(chart_fence(&ed, 1), None);
        assert_eq!(chart_fence(&ed, 6), None);
    }
}
