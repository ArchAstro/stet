//! Font selection and per-line text layout. One logical line becomes one
//! shaped, wrapped `LineLayout` with enough geometry to place cursors,
//! selections and decorations.

use glyphon::cosmic_text::{Align, Wrap};
use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};
use std::ops::Range;
use std::path::PathBuf;
use stet_core::Config;
use stet_core::markdown::{Block, Span, style};
use stet_core::table;
use stet_core::theme::{EditorColors, Rgb};

/// Which configured family a piece of text uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Prose,
    Mono,
    /// File browser, menu and tabs.
    Ui,
}

pub struct Fonts {
    pub system: FontSystem,
    pub swash: SwashCache,
    prose: Option<String>,
    mono: Option<String>,
    ui: Option<String>,
}

/// What a generic family name means on each platform, best first.
fn generic(name: &str) -> Option<&'static [&'static str]> {
    Some(match name.to_ascii_lowercase().as_str() {
        "system-ui" | "system" | "ui" => &[
            ".AppleSystemUIFont",
            "System Font",
            ".SF NS",
            "SF Pro Text",
            "SF Pro",
            "Helvetica Neue",
            "Segoe UI Variable",
            "Segoe UI",
            "Inter",
            "Cantarell",
            "Ubuntu",
            "Noto Sans",
            "DejaVu Sans",
            "Arial",
        ],
        "sans-serif" | "sans" => &[
            "Helvetica Neue",
            "Segoe UI",
            "Inter",
            "Noto Sans",
            "DejaVu Sans",
            "Liberation Sans",
            "Arial",
        ],
        "serif" => &[
            "New York",
            "Charter",
            "Georgia",
            "Cambria",
            "Noto Serif",
            "DejaVu Serif",
            "Liberation Serif",
            "Times New Roman",
        ],
        "monospace" | "mono" => &[
            "SF Mono",
            "Menlo",
            "Cascadia Mono",
            "Consolas",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Courier New",
        ],
        _ => return None,
    })
}

impl Fonts {
    /// Loads every installed font plus any font files in `dirs`, then
    /// resolves the configured family preferences to what is installed.
    pub fn new(config: &Config, dirs: &[PathBuf]) -> Fonts {
        let mut system = FontSystem::new();
        for dir in dirs {
            system.db_mut().load_fonts_dir(dir);
        }
        let mut fonts = Fonts {
            system,
            swash: SwashCache::new(),
            prose: None,
            mono: None,
            ui: None,
        };
        fonts.resolve(config);
        fonts
    }

    fn installed(&self, name: &str) -> Option<String> {
        self.system
            .db()
            .faces()
            .flat_map(|face| face.families.iter())
            .find(|(family, _)| family.eq_ignore_ascii_case(name))
            .map(|(family, _)| family.clone())
    }

    /// Picks the first installed family from each preference list. Generic
    /// names (`system-ui`, `serif`, `sans-serif`, `monospace`) expand to the
    /// platform's own fonts.
    pub fn resolve(&mut self, config: &Config) {
        let pick = |fonts: &Fonts, wanted: &[String]| {
            wanted.iter().find_map(|name| match generic(name) {
                Some(candidates) => candidates.iter().find_map(|candidate| fonts.installed(candidate)),
                None => fonts.installed(name),
            })
        };
        self.prose = pick(self, &config.prose_font);
        self.mono = pick(self, &config.mono_font);
        self.ui = pick(self, &config.ui_font).or_else(|| self.prose.clone());
    }

    /// Every installed family that can be chosen, sorted.
    pub fn families(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .system
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|(family, _)| family.clone()))
            .filter(|family| !family.starts_with('.'))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        names
    }

    fn family(&self, face: Face) -> Family<'_> {
        let name = match face {
            Face::Prose => &self.prose,
            Face::Mono => &self.mono,
            Face::Ui => &self.ui,
        };
        name.as_deref().map_or(Family::Monospace, Family::Name)
    }

    pub fn names(&self) -> [&str; 3] {
        [&self.prose, &self.mono, &self.ui].map(|name| name.as_deref().unwrap_or("monospace"))
    }
}

pub fn color(rgb: Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

#[derive(Clone, Copy, Debug)]
pub struct Cluster {
    pub start: u32,
    pub end: u32,
    pub x: f32,
    pub w: f32,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    pub width: f32,
    clusters: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Back,
    Front,
}

/// A rectangle drawn with the line, relative to the line's origin.
#[derive(Clone, Copy, Debug)]
pub struct Decoration {
    pub rect: [f32; 4],
    pub color: Rgb,
    pub layer: Layer,
    pub radius: f32,
}

/// The text of one table cell, placed inside its row.
pub struct CellText {
    pub buffer: Buffer,
    pub x: f32,
    pub y: f32,
}

pub struct LineLayout {
    pub buffer: Buffer,
    /// A table row is drawn cell by cell instead.
    pub cells: Vec<CellText>,
    grid: bool,
    /// A table row's column edges, left to right.
    pub edges: Vec<f32>,
    pub text: String,
    pub rows: Vec<Row>,
    clusters: Vec<Cluster>,
    pub height: f32,
    /// How far a heading's `#` markers hang into the left margin.
    pub hang: f32,
    pub decorations: Vec<Decoration>,
    pub last_used: u64,
}

pub struct LineSpec<'a> {
    pub text: &'a str,
    pub spans: &'a [Span],
    pub block: Block,
    pub font: f32,
    pub line: f32,
    pub width: f32,
    pub colors: &'a EditorColors,
    /// Focus mode: this line is outside the active paragraph.
    pub dim: bool,
    /// Overrides the family the block would use (interface labels).
    pub face: Option<Face>,
    pub bold: bool,
    /// The line is a row of a table drawn as a grid.
    pub table: Option<TableRow<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    Header,
    /// The `| --- | :-: |` row.
    Rule,
    Body,
}

/// Where a table row's columns are.
#[derive(Clone, Copy)]
pub struct TableRow<'a> {
    /// Width of each column's text, in pixels.
    pub widths: &'a [f32],
    pub aligns: &'a [table::Align],
    pub kind: RowKind,
    /// The last row closes the table.
    pub last: bool,
    /// Space between a column's rule and its text.
    pub pad: f32,
    /// Width of one monospace character.
    pub advance: f32,
}

fn span_color(span: &Span, base: Rgb, c: &EditorColors) -> Rgb {
    if span.syntax != 0 {
        return c.syntax[(span.syntax as usize).min(c.syntax.len() - 1)];
    }
    let bits = span.style;
    const ORDER: [u16; 10] = [
        style::COMMENT,
        style::MARKER,
        style::INS,
        style::DEL,
        style::CODE,
        style::LINK,
        style::MATH,
        style::LIST,
        style::MUTED,
        style::QUOTE,
    ];
    let role = ORDER.iter().find(|&&bit| bits & bit != 0);
    match role.copied() {
        Some(style::COMMENT) => c.comment,
        Some(style::MARKER) => c.marker,
        Some(style::INS) => c.insert,
        Some(style::DEL) => c.delete,
        Some(style::CODE) => c.code,
        Some(style::LINK) => c.link,
        Some(style::MATH) => c.math,
        Some(style::LIST) => c.list_marker,
        Some(style::MUTED) => c.muted,
        Some(style::QUOTE) => c.quote,
        _ => base,
    }
}

/// How the spans of a line are drawn.
struct Styler<'a> {
    spec: &'a LineSpec<'a>,
    block_mono: bool,
    base_color: Rgb,
    base_weight: Weight,
}

impl<'a> Styler<'a> {
    fn new(spec: &'a LineSpec<'a>) -> Styler<'a> {
        let c = spec.colors;
        let heading = matches!(spec.block, Block::Heading(_));
        let header = spec.table.is_some_and(|table| table.kind == RowKind::Header);
        Styler {
            spec,
            block_mono: matches!(spec.block, Block::Code | Block::Table | Block::Frontmatter),
            base_color: match spec.block {
                Block::Heading(_) => c.heading,
                Block::Frontmatter => c.muted,
                Block::Rule => c.marker,
                Block::Table if spec.table.is_some_and(|table| table.kind == RowKind::Rule) => c.marker,
                _ => c.text,
            },
            base_weight: if heading || spec.bold || header {
                Weight::BOLD
            } else {
                Weight::NORMAL
            },
        }
    }

    fn dim(&self, rgb: Rgb) -> Rgb {
        if self.spec.dim {
            rgb.mix(self.spec.colors.background, 0.3)
        } else {
            rgb
        }
    }

    /// Shapes `text`, whose `spans` are relative to it, wrapped to `width`.
    fn shape(&self, fonts: &mut Fonts, text: &str, spans: &[Span], width: f32, align: Option<Align>) -> Buffer {
        let spec = self.spec;
        let plain = Span {
            start: 0,
            end: 0,
            style: 0,
            syntax: 0,
        };
        let mut buffer = Buffer::new(&mut fonts.system, Metrics::new(spec.font, spec.line));
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_tab_width(4);
        buffer.set_size(Some(width), None);
        {
            let attrs = |span: &Span| {
                let bits = span.style;
                let mono = self.block_mono || bits & (style::CODE | style::MATH) != 0;
                let face = spec.face.unwrap_or(if mono { Face::Mono } else { Face::Prose });
                let mut attrs = Attrs::new()
                    .family(fonts.family(face))
                    .weight(if bits & style::BOLD != 0 {
                        Weight::BOLD
                    } else {
                        self.base_weight
                    })
                    .color(color(self.dim(span_color(span, self.base_color, spec.colors))));
                if span.syntax == stet_core::markdown::syntax::COMMENT {
                    attrs = attrs.style(Style::Italic);
                }
                if bits & style::ITALIC != 0 {
                    attrs = attrs.style(Style::Italic);
                }
                attrs
            };
            let mut pieces: Vec<(&str, Attrs)> = Vec::with_capacity(spans.len() * 2 + 1);
            let mut at = 0;
            for span in spans {
                let (start, end) = (span.start as usize, span.end as usize);
                if start > at {
                    pieces.push((&text[at..start], attrs(&plain)));
                }
                pieces.push((&text[start..end], attrs(span)));
                at = end;
            }
            if at < text.len() || pieces.is_empty() {
                pieces.push((&text[at..], attrs(&plain)));
            }
            let default = attrs(&plain);
            buffer.set_rich_text(pieces, &default, Shaping::Advanced, align);
        }
        buffer.shape_until_scroll(&mut fonts.system, false);
        buffer
    }

    /// Backgrounds and strokes of the spans, once the layout knows where
    /// every byte is.
    fn decorate(&self, layout: &mut LineLayout) {
        let (spec, c) = (self.spec, self.spec.colors);
        let thickness = (spec.font / 14.0).round().max(1.0);
        for span in spec.spans {
            let bits = span.style;
            let range = span.start as usize..span.end as usize;
            let background = if bits & style::INS != 0 {
                Some(c.insert_background)
            } else if bits & style::DEL != 0 {
                Some(c.delete_background)
            } else if bits & style::CODE != 0 && !self.block_mono {
                Some(c.code_background)
            } else {
                None
            };
            let line_color = self.dim(span_color(span, self.base_color, c));
            for (row, x0, x1) in layout.ranges(range) {
                let row = &layout.rows[row];
                if let Some(background) = background {
                    layout.decorations.push(Decoration {
                        rect: [x0, row.top + spec.line * 0.08, x1 - x0, row.height - spec.line * 0.16],
                        color: self.dim(background),
                        layer: Layer::Back,
                        radius: spec.font * 0.18,
                    });
                }
                let mut rule = |y: f32| {
                    layout.decorations.push(Decoration {
                        rect: [x0, (row.baseline + y).round(), x1 - x0, thickness],
                        color: line_color,
                        layer: Layer::Front,
                        radius: 0.0,
                    });
                };
                if bits & (style::STRIKE | style::DEL) != 0 && bits & style::MARKER == 0 {
                    rule(-spec.font * 0.3);
                }
                if bits & style::INS != 0 {
                    rule(spec.font * 0.16);
                }
            }
        }
    }
}

pub fn layout_line(fonts: &mut Fonts, spec: &LineSpec) -> LineLayout {
    let styler = Styler::new(spec);
    if let Some(table) = &spec.table {
        return layout_row(fonts, spec, &styler, table);
    }
    let buffer = styler.shape(fonts, spec.text, spec.spans, spec.width.max(spec.font * 4.0), None);

    let mut rows = Vec::new();
    let mut clusters = Vec::new();
    for run in buffer.layout_runs() {
        let first = clusters.len();
        clusters.extend(run.glyphs.iter().map(|glyph| Cluster {
            start: glyph.start as u32,
            end: glyph.end as u32,
            x: glyph.x,
            w: glyph.w,
        }));
        rows.push(Row {
            top: run.line_top,
            height: run.line_height,
            baseline: run.line_y,
            width: run.line_w,
            clusters: first..clusters.len(),
        });
    }
    if rows.is_empty() {
        rows.push(Row {
            top: 0.0,
            height: spec.line,
            baseline: spec.line * 0.75,
            width: 0.0,
            clusters: 0..0,
        });
    }
    let height = rows.last().map_or(spec.line, |row| row.top + row.height);
    let mut layout = LineLayout {
        buffer,
        cells: Vec::new(),
        grid: false,
        edges: Vec::new(),
        text: spec.text.to_string(),
        rows,
        clusters,
        height,
        hang: 0.0,
        decorations: Vec::new(),
        last_used: 0,
    };

    // Hang heading markers in the margin, the way iA Writer does.
    if let (Block::Heading(_), Some(first), 1) = (spec.block, spec.spans.first(), layout.rows.len())
        && first.start == 0
        && first.style & style::MARKER != 0
    {
        layout.hang = layout.x_of_byte(0, first.end as usize);
    }

    styler.decorate(&mut layout);
    layout
}

/// A table row: every cell shaped on its own and wrapped inside its column,
/// with a cluster for each byte of the source, so cursors, selections and
/// clicks work on the grid as they do on a line of text.
fn layout_row(fonts: &mut Fonts, spec: &LineSpec, styler: &Styler, table: &TableRow) -> LineLayout {
    let c = spec.colors;
    let row = table::Row::parse(spec.text);
    let columns = table.widths.len().max(row.cells.len());
    let width_of = |column: usize| table.widths.get(column).copied().unwrap_or(table.advance * 3.0);
    // Left edge of each column, and the right edge of the last.
    let mut edges = Vec::with_capacity(columns + 1);
    let mut x = 0.0;
    for column in 0..columns {
        edges.push(x);
        x += width_of(column) + table.pad * 2.0;
    }
    edges.push(x);
    let total = x;
    let pipe = table.pad.min(table.advance);

    let mut cells = Vec::new();
    // Clusters by visual row; cells come in order, so each stays sorted.
    let mut lines: Vec<Vec<Cluster>> = vec![Vec::new()];
    let mut baseline = spec.line * 0.75;
    for (index, cell) in row.cells.iter().enumerate() {
        let (left, right) = (edges[index], edges[index + 1]);
        let (origin, width) = (left + table.pad, width_of(index));
        if row.leading || index > 0 {
            lines[0].push(Cluster {
                start: cell.outer.start as u32 - 1,
                end: cell.outer.start as u32,
                x: left,
                w: pipe,
            });
        }
        let align = table.aligns.get(index).copied().unwrap_or_default();
        let content = &spec.text[cell.text.clone()];
        // The rule row never wraps: what does not fit is not drawn.
        let shown = if table.kind == RowKind::Rule {
            let fits = (width / table.advance).floor().max(1.0) as usize;
            &content[..content.char_indices().nth(fits).map_or(content.len(), |(byte, _)| byte)]
        } else {
            content
        };
        // Where the text starts and ends: first row's left, last row's right.
        let anchor = match align {
            table::Align::Right => origin + width,
            table::Align::Center => origin + width / 2.0,
            _ => origin,
        };
        let (mut first_x, mut last_x, mut last_row) = (anchor, anchor, 0);
        let mut drawn: Vec<Vec<Cluster>> = Vec::new();
        if !shown.is_empty() {
            let spans: Vec<Span> = spec
                .spans
                .iter()
                .filter(|span| {
                    (span.start as usize) < cell.text.start + shown.len() && cell.text.start < span.end as usize
                })
                .map(|span| Span {
                    start: (span.start as usize).max(cell.text.start) as u32 - cell.text.start as u32,
                    end: ((span.end as usize).min(cell.text.start + shown.len()) - cell.text.start) as u32,
                    ..*span
                })
                .collect();
            let aligned = match align {
                table::Align::Right => Some(Align::Right),
                table::Align::Center => Some(Align::Center),
                _ => None,
            };
            // A little slack, so a cell exactly as wide as its column stays on one line.
            let buffer = styler.shape(fonts, shown, &spans, width + table.advance * 0.3, aligned);
            let shift = cell.text.start as u32;
            for (at, run) in buffer.layout_runs().enumerate() {
                baseline = run.line_y - run.line_top;
                let start = run.glyphs.first().map_or(anchor, |glyph| origin + glyph.x);
                let end = run.glyphs.last().map_or(anchor, |glyph| origin + glyph.x + glyph.w);
                if at == 0 {
                    first_x = start;
                }
                (last_x, last_row) = (end, at);
                let clusters = run.glyphs.iter().map(|glyph| Cluster {
                    start: glyph.start as u32 + shift,
                    end: glyph.end as u32 + shift,
                    x: origin + glyph.x,
                    w: glyph.w,
                });
                drawn.push(clusters.collect());
            }
            if !drawn.is_empty() {
                cells.push(CellText {
                    buffer,
                    x: origin,
                    y: 0.0,
                });
            }
        }
        // What is not drawn still has a place: the padding around the
        // text shares the gap it sits in.
        let squeeze = |bytes: Range<usize>, from: f32, to: f32| -> Vec<Cluster> {
            let count = spec.text[bytes.clone()].chars().count().max(1);
            let step = ((to - from) / count as f32).clamp(0.0, table.advance);
            let made = spec.text[bytes.clone()]
                .char_indices()
                .enumerate()
                .map(|(nth, (byte, c))| Cluster {
                    start: (bytes.start + byte) as u32,
                    end: (bytes.start + byte + c.len_utf8()) as u32,
                    x: from + step * nth as f32,
                    w: step,
                });
            made.collect()
        };
        let lead = cell.outer.start..cell.text.start;
        let lead_width = table.advance * spec.text[lead.clone()].chars().count() as f32;
        lines[0].extend(squeeze(lead, (first_x - lead_width).max(left + pipe), first_x));
        for (at, clusters) in drawn.into_iter().enumerate() {
            if lines.len() <= at {
                lines.push(Vec::new());
            }
            lines[at].extend(clusters);
        }
        let hidden = cell.text.start + shown.len()..cell.outer.end;
        lines[last_row].extend(squeeze(hidden, last_x, right));
    }
    // The closing pipe, and anything after it.
    if let Some(&last) = row
        .pipes
        .last()
        .filter(|last| row.cells.last().is_none_or(|cell| **last >= cell.outer.end))
    {
        let at = edges[row.cells.len().min(columns)];
        lines[0].push(Cluster {
            start: last as u32,
            end: last as u32 + 1,
            x: at,
            w: pipe,
        });
    }

    let mut rows = Vec::with_capacity(lines.len());
    let mut clusters = Vec::new();
    for (at, line) in lines.into_iter().enumerate() {
        let first = clusters.len();
        clusters.extend(line);
        rows.push(Row {
            top: at as f32 * spec.line,
            height: spec.line,
            baseline: at as f32 * spec.line + baseline,
            width: total,
            clusters: first..clusters.len(),
        });
    }
    let height = rows.len() as f32 * spec.line;
    let mut layout = LineLayout {
        buffer: Buffer::new(&mut fonts.system, Metrics::new(spec.font, spec.line)),
        cells,
        grid: true,
        edges: edges.clone(),
        text: spec.text.to_string(),
        rows,
        clusters,
        height,
        hang: 0.0,
        decorations: Vec::new(),
        last_used: 0,
    };

    let hair = (spec.font / 16.0).round().max(1.0);
    let rule = styler.dim(c.rule);
    let mut line = |rect: [f32; 4], color: Rgb, layer: Layer| {
        layout.decorations.push(Decoration {
            rect,
            color,
            layer,
            radius: 0.0,
        })
    };
    if table.kind != RowKind::Body {
        line([0.0, 0.0, total, height], styler.dim(c.code_background), Layer::Back);
    }
    // Rules: around the table, under its heading, faintly between rows.
    match table.kind {
        RowKind::Header => line([0.0, 0.0, total + hair, hair], rule, Layer::Front),
        RowKind::Rule => line([0.0, height - hair, total + hair, hair], rule, Layer::Front),
        RowKind::Body => {}
    }
    if table.kind == RowKind::Body && !table.last {
        line(
            [0.0, height - hair, total, hair],
            rule.mix(c.background, 0.55),
            Layer::Front,
        );
    }
    if table.last {
        line([0.0, height - hair, total + hair, hair], rule, Layer::Front);
    }
    for edge in &edges {
        line([*edge, 0.0, hair, height], rule, Layer::Front);
    }
    styler.decorate(&mut layout);
    layout
}

impl LineLayout {
    pub fn byte_of_col(&self, col: usize) -> usize {
        self.text
            .char_indices()
            .nth(col)
            .map_or(self.text.len(), |(byte, _)| byte)
    }

    pub fn col_of_byte(&self, byte: usize) -> usize {
        self.text[..byte.min(self.text.len())].chars().count()
    }

    fn row_clusters(&self, row: usize) -> &[Cluster] {
        &self.clusters[self.rows[row].clusters.clone()]
    }

    /// The row a byte offset is drawn on. A wrap point belongs to the row it starts.
    pub fn row_of_byte(&self, byte: usize) -> usize {
        if self.grid {
            // Cells wrap side by side, so rows do not hold consecutive bytes.
            let holds = |row: &usize| {
                self.row_clusters(*row)
                    .iter()
                    .any(|cluster| (cluster.start as usize..cluster.end as usize).contains(&byte))
            };
            return (0..self.rows.len()).find(holds).unwrap_or_else(|| {
                // Past the last character: stay with what comes before it.
                (0..self.rows.len())
                    .rev()
                    .find(|row| {
                        self.row_clusters(*row)
                            .iter()
                            .any(|cluster| cluster.end as usize == byte)
                    })
                    .unwrap_or(0)
            });
        }
        (0..self.rows.len())
            .rev()
            .find(|&row| {
                self.row_clusters(row)
                    .first()
                    .is_some_and(|first| first.start as usize <= byte)
            })
            .unwrap_or(0)
    }

    pub fn x_of_byte(&self, row: usize, byte: usize) -> f32 {
        let clusters = self.row_clusters(row);
        for cluster in clusters {
            let (start, end) = (cluster.start as usize, cluster.end as usize);
            if byte <= start {
                return cluster.x;
            }
            if byte < end {
                return cluster.x + cluster.w * (byte - start) as f32 / (end - start) as f32;
            }
        }
        clusters.last().map_or(0.0, |last| last.x + last.w)
    }

    /// Width of the cluster at `byte`, for a block cursor.
    pub fn width_at(&self, row: usize, byte: usize) -> Option<f32> {
        self.row_clusters(row)
            .iter()
            .find(|cluster| (cluster.start as usize..cluster.end as usize).contains(&byte))
            .map(|cluster| cluster.w)
            .filter(|w| *w > 0.5)
    }

    pub fn row_at_y(&self, y: f32) -> usize {
        self.rows
            .iter()
            .position(|row| y < row.top + row.height)
            .unwrap_or(self.rows.len() - 1)
    }

    /// The byte offset nearest to `x` on `row`. A bar cursor snaps to the
    /// closest boundary; a block cursor lands on the cluster under `x`.
    pub fn byte_at(&self, row: usize, x: f32, bar: bool) -> usize {
        let clusters = self.row_clusters(row);
        let Some(last) = clusters.last() else {
            return if row == 0 { 0 } else { self.text.len() };
        };
        let is_last_row = row + 1 == self.rows.len();
        for cluster in clusters {
            if x < cluster.x + cluster.w {
                let past_middle = bar && x > cluster.x + cluster.w / 2.0;
                // Stay on this row: a wrap point would draw on the next one.
                let wraps = !is_last_row && cluster.end == last.end;
                return if past_middle && !wraps {
                    cluster.end
                } else {
                    cluster.start
                } as usize;
            }
        }
        if bar && is_last_row {
            last.end as usize
        } else {
            last.start as usize
        }
    }

    /// `(row, x0, x1)` extents covering a byte range.
    pub fn ranges(&self, bytes: Range<usize>) -> Vec<(usize, f32, f32)> {
        let mut out = Vec::new();
        for row in 0..self.rows.len() {
            let mut extent: Option<(f32, f32)> = None;
            for cluster in self.row_clusters(row) {
                if (cluster.start as usize) < bytes.end && bytes.start < cluster.end as usize {
                    let (x0, x1) = extent.unwrap_or((f32::MAX, f32::MIN));
                    extent = Some((x0.min(cluster.x), x1.max(cluster.x + cluster.w)));
                }
            }
            if let Some((x0, x1)) = extent {
                out.push((row, x0, x1));
            }
        }
        out
    }
}
