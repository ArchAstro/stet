//! What a notebook cell produced, drawn under its closing fence: a status
//! line, then each output. Text is set in the code face and not wrapped;
//! long outputs are cut to their first lines, with a count of the rest.
//!
//! An output offers several forms of itself by MIME type; the richest that
//! can be drawn wins. [`RENDERERS`] lists them, richest first: a new kind
//! of output is a new entry there.

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

/// What an output needs to draw itself.
struct Cx<'a> {
    ed: &'a Editor,
    m: &'a Metrics,
    /// The notebook's output folder, for pictures kept as files.
    files: Option<&'a std::path::Path>,
    size: f32,
    small: f32,
}

/// Draws one form of an output into rows, or says it cannot (`false`).
type Render = fn(&mut View, &Cx, &str, &Value, &mut Vec<Row>) -> bool;

/// Every form of output that can be drawn, richest first. A type ending in
/// `*` stands for every type that starts with what comes before it.
const RENDERERS: &[(&str, Render)] = &[
    (stet_chart::MIME, View::chart_rows),
    // Altair's, in every version: `v5+json`, `v6.json`.
    ("application/vnd.vegalite.v*", View::chart_rows),
    ("image/png", View::picture_rows),
    ("image/jpeg", View::picture_rows),
    ("image/gif", View::picture_rows),
    ("image/svg+xml", View::picture_rows),
    (TABLE, View::table_render),
    ("text/markdown", View::text_render),
    ("text/plain", View::text_render),
];

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
        let cx = Cx {
            ed,
            m,
            files: files.as_deref(),
            size,
            small,
        };

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
                    for (pattern, render) in RENDERERS {
                        let found = match pattern.strip_suffix('*') {
                            Some(prefix) => data.iter().find(|(mime, _)| mime.starts_with(prefix)),
                            None => data.get_key_value(*pattern),
                        };
                        if let Some((mime, value)) = found
                            && render(self, &cx, mime, value, &mut rows)
                        {
                            break;
                        }
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

    fn text_render(&mut self, cx: &Cx, _: &str, value: &Value, rows: &mut Vec<Row>) -> bool {
        let Some(text) = value.as_str() else { return false };
        let c = cx.ed.theme.colors;
        let lines = plain_lines(text);
        for line in lines.iter().take(MAX_LINES) {
            rows.push(self.text_row(cx.ed, line, c.text, cx.size, Face::Mono, false));
        }
        if lines.len() > MAX_LINES {
            let more = format!("… {} more lines", lines.len() - MAX_LINES);
            rows.push(self.text_row(cx.ed, &more, c.muted, cx.small, Face::Ui, false));
        }
        true
    }

    fn table_render(&mut self, cx: &Cx, _: &str, value: &Value, rows: &mut Vec<Row>) -> bool {
        self.table_rows(cx.ed, value, cx.size, cx.small, rows);
        true
    }

    /// A chart, drawn as SVG the width of the text in the editor's theme.
    /// One that cannot be drawn says why, and the next form down shows.
    fn chart_rows(&mut self, cx: &Cx, _: &str, value: &Value, rows: &mut Vec<Row>) -> bool {
        let inset = (cx.m.advance * 0.6).round();
        let width = ((cx.m.column - inset) / self.scale).floor().max(120.0);
        let theme = crate::notebook::chart_theme(cx.ed, cx.small / self.scale);
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            value.to_string().hash(&mut hasher);
            (width as u32).hash(&mut hasher);
            format!("{theme:?}").hash(&mut hasher);
            hasher.finish()
        };
        let url = match self.chart_keys.get(&key) {
            Some(url) => url.clone(),
            None => match stet_chart::draw(value, &theme, width, &mut |_| None) {
                Ok(chart) => {
                    if self.charts.len() > 64 {
                        self.charts.clear();
                        self.chart_keys.clear();
                    }
                    let url = format!(
                        "data:image/svg+xml;base64,{}",
                        notebook::base64_encode(chart.svg.as_bytes())
                    );
                    self.charts.insert(url.clone(), chart);
                    self.chart_keys.insert(key, url.clone());
                    url
                }
                Err(err) => {
                    let c = cx.ed.theme.colors;
                    rows.push(self.text_row(cx.ed, &format!("chart: {err}"), c.muted, cx.small, Face::Ui, false));
                    return false;
                }
            },
        };
        match self.shown_at(url, 1.0, cx.m) {
            Some(shown) => {
                rows.push(Row::Picture(shown));
                true
            }
            None => false,
        }
    }

    /// The readout of the chart under the pointer, from the pictures of the
    /// last frame.
    pub(super) fn chart_hover(&self) -> Option<(String, stet_chart::Readout)> {
        let (x, y) = self.pointer;
        let picture = self.pictures.iter().find(|picture| {
            let [left, top, width, height] = picture.seen;
            x >= left && x < left + width && y >= top && y < top + height
        })?;
        let chart = self.charts.get(&picture.url)?;
        // The picture may be shown smaller than the chart was laid out.
        let points = chart.width / picture.rect[2];
        let readout = chart.readout((x - picture.rect[0]) * points, (y - picture.rect[1]) * points)?;
        Some((picture.url.clone(), readout))
    }

    /// Over a chart: a guide down it, a dot on each value read, and a card
    /// with the values beside the pointer.
    pub(super) fn chart_readout(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let Some((url, readout)) = self.chart_hover.clone() else {
            return;
        };
        let Some(picture) = self.pictures.iter().find(|picture| picture.url == url).cloned() else {
            return;
        };
        let Some(chart) = self.charts.get(&url) else { return };
        let px = picture.rect[2] / chart.width;
        let [left, top, _, height] = picture.rect;
        let c = ed.theme.colors;
        let rgb = |color: stet_chart::Color| Rgb(color[0], color[1], color[2]);
        let line = self.scale.round().max(1.0);
        if let Some(guide) = readout.guide {
            layer.back.push(Quad {
                rect: [(left + guide * px - line / 2.0).round(), top, line, height],
                color: linear(c.muted, 0.6),
                radius: 0.0,
            });
        }
        let dot = (4.0 * self.scale).round();
        for row in &readout.rows {
            if let Some((x, y)) = row.at {
                let (x, y) = (left + x * px, top + y * px);
                layer.back.push(Quad {
                    rect: [
                        x - dot / 2.0 - line,
                        y - dot / 2.0 - line,
                        dot + 2.0 * line,
                        dot + 2.0 * line,
                    ],
                    color: linear(c.background, 1.0),
                    radius: dot,
                });
                layer.back.push(Quad {
                    rect: [x - dot / 2.0, y - dot / 2.0, dot, dot],
                    color: linear(rgb(row.color), 1.0),
                    radius: dot,
                });
            }
        }
        // The card: the title, then a swatch, a name and a value a row.
        let small = (m.font * 0.74).round();
        let pad = (8.0 * self.scale).round();
        let swatch = (small * 0.7).round();
        let title = self.label(ed, &readout.title, c.text, small, Face::Ui, true);
        let mut lines = Vec::new();
        for row in &readout.rows {
            let name = self.label(ed, &row.label, c.muted, small, Face::Ui, false);
            let value = self.label(ed, &row.value, c.text, small, Face::Ui, true);
            lines.push((row.color, name, value));
        }
        let width_of = |view: &View, key: u64| view.layouts[&key].rows.first().map_or(0.0, |row| row.width);
        let row_height = self.layouts[&title].height;
        let names = lines
            .iter()
            .map(|(_, name, _)| width_of(self, *name))
            .fold(0.0, f32::max);
        let values = lines
            .iter()
            .map(|(.., value)| width_of(self, *value))
            .fold(0.0, f32::max);
        let inner = (swatch + pad * 0.6 + names + pad + values).max(width_of(self, title));
        let (w, h) = (inner + pad * 2.0, pad * 2.0 + row_height * (1 + lines.len()) as f32);
        let (x, y) = self.pointer;
        let mut card_x = x + pad * 2.0;
        if card_x + w > m.right {
            card_x = x - pad * 2.0 - w;
        }
        let card_y = (y - h / 2.0).clamp(m.top, (m.bottom - h).max(m.top));
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
        layer.texts.push(TextItem {
            key: title,
            left: card_x + pad,
            top: text_y,
            clip,
            color: color(c.text),
        });
        for (swatch_color, name, value) in lines {
            text_y += row_height;
            layer.back.push(Quad {
                rect: [
                    card_x + pad,
                    (text_y + (row_height - swatch) / 2.0).round(),
                    swatch,
                    swatch,
                ],
                color: linear(rgb(swatch_color), 1.0),
                radius: swatch * 0.25,
            });
            layer.texts.push(TextItem {
                key: name,
                left: card_x + pad + swatch + pad * 0.6,
                top: text_y,
                clip,
                color: color(c.muted),
            });
            let value_width = width_of(self, value);
            layer.texts.push(TextItem {
                key: value,
                left: (card_x + w - pad - value_width).round(),
                top: text_y,
                clip,
                color: color(c.text),
            });
        }
    }

    /// A picture until it has loaded is no picture: the next form down
    /// shows meanwhile.
    fn picture_rows(&mut self, cx: &Cx, mime: &str, value: &Value, rows: &mut Vec<Row>) -> bool {
        match self.output_picture(mime, value, cx.files, cx.m) {
            Some(shown) => {
                rows.push(Row::Picture(shown));
                true
            }
            None => false,
        }
    }

    /// A picture from an output bundle: a file in the notebook's output
    /// folder, or inline (base64, or SVG's own text) for a document not yet
    /// saved.
    fn output_picture(
        &mut self,
        mime: &str,
        value: &Value,
        files: Option<&std::path::Path>,
        m: &Metrics,
    ) -> Option<Shown> {
        let svg = mime == "image/svg+xml";
        let url = match (notebook::file_ref(value), value.as_str()) {
            (Some(name), _) => files?.join(name).to_string_lossy().into_owned(),
            (None, Some(text)) if svg => format!("data:{mime};base64,{}", notebook::base64_encode(text.as_bytes())),
            (None, Some(encoded)) => format!("data:{mime};base64,{}", encoded.replace(['\n', '\r'], "")),
            _ => return None,
        };
        // Kernels draw figures for a 2x screen at most; a drawing's size is
        // in points already.
        self.shown_at(url, if svg { 1.0 } else { 2.0 }, m)
    }

    /// The picture at `url`, `density` pixels to the point, shown at its
    /// size in points and never wider than the text.
    fn shown_at(&mut self, url: String, density: f32, m: &Metrics) -> Option<Shown> {
        let source = images::resolve(&url, None, false)?;
        let (id, width, height) = self.images.get(&source)?;
        let (width, height) = (
            width as f32 * self.scale / density,
            height as f32 * self.scale / density,
        );
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
