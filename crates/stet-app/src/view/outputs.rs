//! What a notebook cell produced, drawn under its closing fence: a status
//! line, then each output. Text is set in the code face and not wrapped;
//! long outputs are cut to their first lines, with a count of the rest.
//!
//! An output offers several forms of itself by MIME type; the richest that
//! can be drawn wins: a picture, then a table, then plain text.

use super::{Layer, Metrics, Quad, Shown, TextItem, View};
use crate::gpu::linear;
use crate::images;
use crate::text::{Face, color};
use serde_json::Value;
use std::time::Instant;
use stet_core::Editor;
use stet_core::notebook::{self, Item, Slot, Status};
use stet_core::theme::Rgb;
use stet_kernel::TABLE;

/// Most lines of one stream or text shown before the rest is counted.
const MAX_LINES: usize = 40;
/// Most traceback lines shown under an error.
const MAX_TRACE: usize = 16;
/// Most table rows shown.
const MAX_ROWS: usize = 20;

/// One row of a cell's output area.
pub(super) enum Row {
    Text {
        key: u64,
        height: f32,
        rgb: Rgb,
        /// A light band behind it (alternate table rows).
        band: bool,
        /// A rule under it (a table's header).
        rule: bool,
        /// How wide a band or rule is, if not the whole column.
        span: Option<f32>,
    },
    Picture(Shown),
    Gap(f32),
}

/// A cell's output area, laid out.
pub(super) struct Block {
    pub rows: Vec<Row>,
    pub height: f32,
}

/// Text with ANSI colour codes taken out, and each line as a terminal would
/// leave it after carriage returns (progress bars redraw with `\r`).
pub(crate) fn plain_lines(text: &str) -> Vec<String> {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        clean.push(c);
    }
    let text = clean.strip_suffix('\n').unwrap_or(&clean);
    text.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line).replace('\t', "    ")
        })
        .collect()
}

fn seconds(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f32 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, ms / 1000 % 60)
    }
}

/// The status line: run count, how it went, how long, and who asked.
fn status_line(slot: &Slot, stale: bool) -> String {
    let mut parts = Vec::new();
    if let Some(count) = slot.count {
        parts.push(format!("[{count}]"));
    }
    parts.push(match slot.status {
        Status::Queued => "queued".to_string(),
        // No clock while it runs: nothing redraws the frame to move it on.
        Status::Running => "running…".to_string(),
        Status::Ok => slot
            .took_ms
            .map_or("done".to_string(), |ms| format!("done in {}", seconds(ms))),
        Status::Error => slot
            .took_ms
            .map_or("failed".to_string(), |ms| format!("failed after {}", seconds(ms))),
        Status::Aborted => "not run".to_string(),
    });
    if stale {
        parts.push("code changed since".to_string());
    }
    if let Some(who) = &slot.ran_by {
        parts.push(format!("run by {who}"));
    }
    parts.join("  ·  ")
}

impl View {
    /// The output area under `line`, if it closes a cell that has one.
    pub(super) fn output_block(&mut self, ed: &Editor, line: usize, m: &Metrics) -> Option<Block> {
        let book = ed.notebook()?;
        let cell = book.cell_ending_at(line)?;
        let (slot, stale) = book.output_of(cell)?;
        let slot = slot.clone();
        let files = book.files_dir();
        let c = ed.theme.colors;
        let size = (m.font * 0.86).round();
        let small = (m.font * 0.74).round();

        let mut rows = vec![Row::Gap((m.line * 0.25).round())];
        let tone = match slot.status {
            Status::Error => c.error,
            Status::Ok if !stale => c.muted,
            _ => c.marker,
        };
        rows.push(self.text_row(ed, &status_line(&slot, stale), tone, small, Face::Ui, false));
        let lines = |view: &mut View, rows: &mut Vec<Row>, text: &str, rgb: Rgb, bold: bool, max: usize| {
            let lines = plain_lines(text);
            for line in lines.iter().take(max) {
                rows.push(view.text_row(ed, line, rgb, size, Face::Mono, bold));
            }
            if lines.len() > max {
                let more = format!("… {} more lines", lines.len() - max);
                rows.push(view.text_row(ed, &more, c.muted, small, Face::Ui, false));
            }
        };
        for item in &slot.outputs {
            rows.push(Row::Gap((m.line * 0.15).round()));
            match item {
                Item::Stream { stderr, text } => lines(
                    self,
                    &mut rows,
                    text,
                    if *stderr { c.error } else { c.text },
                    false,
                    MAX_LINES,
                ),
                Item::Error { name, value, traceback } => {
                    // IPython's traceback ends with the error line itself.
                    let mut trace: Vec<String> = traceback.iter().flat_map(|part| plain_lines(part)).collect();
                    let headline = format!("{name}: {value}");
                    if trace.last().is_some_and(|last| last.trim() == headline.trim()) {
                        trace.pop();
                    }
                    let skip = trace.len().saturating_sub(MAX_TRACE);
                    if skip > 0 {
                        rows.push(self.text_row(
                            ed,
                            &format!("… {skip} lines of traceback"),
                            c.muted,
                            small,
                            Face::Ui,
                            false,
                        ));
                    }
                    for line in &trace[skip..] {
                        rows.push(self.text_row(ed, line, c.muted, size, Face::Mono, false));
                    }
                    lines(self, &mut rows, headline.trim_end_matches([':', ' ']), c.error, true, 4);
                }
                Item::Display { data } | Item::Result { data } => {
                    if let Some(shown) = self.output_picture(data, files.as_deref(), m) {
                        rows.push(Row::Picture(shown));
                    } else if let Some(table) = data.get(TABLE) {
                        self.table_rows(ed, table, size, small, &mut rows);
                    } else if let Some(text) = data
                        .get("text/markdown")
                        .or(data.get("text/plain"))
                        .and_then(Value::as_str)
                    {
                        lines(self, &mut rows, text, c.text, false, MAX_LINES);
                    }
                }
            }
        }
        rows.push(Row::Gap((m.line * 0.35).round()));
        let height = rows
            .iter()
            .map(|row| match row {
                Row::Text { height, .. } => *height,
                Row::Picture(shown) => shown.height,
                Row::Gap(gap) => *gap,
            })
            .sum();
        Some(Block { rows, height })
    }

    fn text_row(&mut self, ed: &Editor, text: &str, rgb: Rgb, size: f32, face: Face, bold: bool) -> Row {
        let key = self.label(ed, if text.is_empty() { " " } else { text }, rgb, size, face, bold);
        Row::Text {
            key,
            height: self.layouts[&key].height,
            rgb,
            band: false,
            rule: false,
            span: None,
        }
    }

    /// A picture from an output bundle: a file in the notebook's output
    /// folder, or base64 inline for a document not yet saved.
    fn output_picture(
        &mut self,
        data: &serde_json::Map<String, Value>,
        files: Option<&std::path::Path>,
        m: &Metrics,
    ) -> Option<Shown> {
        let url = ["image/png", "image/jpeg", "image/gif"].iter().find_map(|mime| {
            let value = data.get(*mime)?;
            match (notebook::file_ref(value), value.as_str()) {
                (Some(name), _) => Some(files?.join(name).to_string_lossy().into_owned()),
                (None, Some(encoded)) => Some(format!("data:{mime};base64,{}", encoded.replace(['\n', '\r'], ""))),
                _ => None,
            }
        })?;
        let source = images::resolve(&url, None, false)?;
        let (id, width, height) = self.images.get(&source)?;
        // Kernels draw figures for a 2x screen at most; show them at their
        // size in points, never wider than the text.
        let (width, height) = (width as f32 * self.scale / 2.0, height as f32 * self.scale / 2.0);
        let max_height = (self.height * 0.6).max(m.line);
        let fit = (m.column / width).min(max_height / height).min(1.0);
        Some(Shown {
            id,
            width: (width * fit).round(),
            height: (height * fit).round(),
            url,
            source,
        })
    }

    /// A table as aligned rows of the code face: a header with a rule under
    /// it, then the first rows on alternating bands.
    fn table_rows(&mut self, ed: &Editor, table: &Value, size: f32, small: f32, rows: &mut Vec<Row>) {
        let c = ed.theme.colors;
        let cell = |value: &Value| match value {
            Value::String(text) => text.replace('\n', " "),
            Value::Null => "NULL".to_string(),
            other => other.to_string(),
        };
        let columns: Vec<String> = table["columns"]
            .as_array()
            .map(|columns| columns.iter().map(cell).collect())
            .unwrap_or_default();
        let body: Vec<Vec<String>> = table["rows"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .take(MAX_ROWS)
                    .map(|row| {
                        row.as_array()
                            .map(|cells| cells.iter().map(cell).collect())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let width = |i: usize| {
            body.iter()
                .filter_map(|row| row.get(i))
                .chain(columns.get(i))
                .map(|text| text.chars().count().min(40))
                .max()
                .unwrap_or(0)
        };
        let widths: Vec<usize> = (0..columns.len()).map(width).collect();
        let numeric: Vec<bool> = (0..columns.len())
            .map(|i| {
                table["rows"].as_array().is_some_and(|rows| {
                    rows.iter()
                        .take(MAX_ROWS)
                        .all(|row| row[i].is_number() || row[i].is_null())
                })
            })
            .collect();
        let line = |cells: &[String]| {
            let padded: Vec<String> = cells
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    let text: String = text.chars().take(40).collect();
                    let width = widths.get(i).copied().unwrap_or(0);
                    if numeric.get(i) == Some(&true) {
                        format!("{text:>width$}")
                    } else {
                        format!("{text:<width$}")
                    }
                })
                .collect();
            padded.join("   ").trim_end().to_string()
        };
        let mut header = self.text_row(ed, &line(&columns), c.heading, size, Face::Mono, true);
        let mut wide = 0.0f32;
        if let Row::Text { rule, key, .. } = &mut header {
            *rule = true;
            wide = self.layouts[key].rows[0].width;
        }
        let mut body_rows = Vec::new();
        for (at, cells) in body.iter().enumerate() {
            let mut row = self.text_row(ed, &line(cells), c.text, size, Face::Mono, false);
            if let Row::Text { band, key, .. } = &mut row {
                *band = at % 2 == 1;
                wide = wide.max(self.layouts[key].rows[0].width);
            }
            body_rows.push(row);
        }
        for row in std::iter::once(&mut header).chain(body_rows.iter_mut()) {
            if let Row::Text { span, .. } = row {
                *span = Some(wide);
            }
        }
        rows.push(header);
        rows.extend(body_rows);
        let total = table["total"].as_u64().unwrap_or(body.len() as u64) as usize;
        let note = if total > body.len() {
            format!("{} of {total} rows  ·  {} columns", body.len(), columns.len())
        } else {
            format!("{total} rows  ·  {} columns", columns.len())
        };
        rows.push(self.text_row(ed, &note, c.muted, small, Face::Ui, false));
    }

    /// Draw `block` with its top at `y`. Returns its height.
    pub(super) fn draw_output(
        &mut self,
        ed: &Editor,
        block: Block,
        line: usize,
        mut y: f32,
        m: &Metrics,
        layer: &mut Layer,
    ) -> f32 {
        let c = ed.theme.colors;
        let top = y;
        let pad = (m.advance * 0.8).round();
        let inset = (m.advance * 0.6).round();
        let clipped = |rect: [f32; 4]| {
            let top = rect[1].max(m.top);
            let bottom = (rect[1] + rect[3]).min(m.bottom);
            [rect[0], top, rect[2], (bottom - top).max(0.0)]
        };
        let text_clip = [m.origin, m.top, (m.left + m.column + pad).min(m.right), m.bottom];
        for row in block.rows {
            match row {
                Row::Gap(gap) => y += gap,
                Row::Text {
                    key,
                    height,
                    rgb,
                    band,
                    rule,
                    span,
                } => {
                    let wide = span.map_or(m.column - inset, |span| (span + pad).min(m.column - inset));
                    if y + height >= m.top && y < m.bottom {
                        if band {
                            layer.back.push(Quad {
                                rect: clipped([m.left + inset - pad * 0.5, y, wide + pad, height]),
                                color: linear(c.code_background, 1.0),
                                radius: 0.0,
                            });
                        }
                        if rule {
                            layer.back.push(Quad {
                                rect: clipped([m.left + inset, y + height - 1.0, wide, (self.scale).round().max(1.0)]),
                                color: linear(c.rule, 1.0),
                                radius: 0.0,
                            });
                        }
                        layer.texts.push(TextItem {
                            key,
                            left: (m.left + inset).round(),
                            top: y.round(),
                            clip: text_clip,
                            color: color(rgb),
                        });
                    }
                    y += height;
                }
                Row::Picture(shown) => {
                    if y < m.bottom && y + shown.height > m.top {
                        let rect = [m.left + inset, y, shown.width, shown.height];
                        layer.images.push((shown.id, rect));
                        self.images.touch(&shown.source, Instant::now());
                        let (seen_top, seen_bottom) = (rect[1].max(m.top), (rect[1] + shown.height).min(m.bottom));
                        if seen_bottom > seen_top {
                            self.pictures.push(super::Picture {
                                rect,
                                seen: [rect[0], seen_top, shown.width, seen_bottom - seen_top],
                                line,
                                url: shown.url,
                            });
                        }
                    }
                    y += shown.height;
                }
            }
        }
        // A rule down the left ties the outputs to their cell.
        let bar = (2.0 * self.scale).round().max(1.0);
        layer.back.push(Quad {
            rect: clipped([m.left - pad, top, bar, y - top - (m.line * 0.2).round()]),
            color: linear(c.code_background, 1.0),
            radius: 0.0,
        });
        y - top
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_and_carriage_returns_are_resolved() {
        assert_eq!(plain_lines("\u{1b}[0;31mred\u{1b}[0m text\n"), ["red text"]);
        assert_eq!(plain_lines("10%\r50%\r100%\ndone"), ["100%", "done"]);
        assert_eq!(plain_lines("a\tb"), ["a    b"]);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(seconds(120), "120 ms");
        assert_eq!(seconds(4200), "4.2s");
        assert_eq!(seconds(125_000), "2m 5s");
    }
}
