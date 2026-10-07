//! Font selection and per-line text layout. One logical line becomes one
//! shaped, wrapped `LineLayout` with enough geometry to place cursors,
//! selections and decorations.

use glyphon::cosmic_text::{Align, Wrap};
use glyphon::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};
use std::ops::Range;
use std::path::PathBuf;
use stet_core::Config;
use stet_core::markdown::{Block, Span, style};
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

pub struct LineLayout {
    pub buffer: Buffer,
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

pub fn layout_line(fonts: &mut Fonts, spec: &LineSpec) -> LineLayout {
    let c = spec.colors;
    let dim = |rgb: Rgb| if spec.dim { rgb.mix(c.background, 0.3) } else { rgb };
    let block_mono = matches!(spec.block, Block::Code | Block::Table | Block::Frontmatter);
    let base_color = match spec.block {
        Block::Heading(_) => c.heading,
        Block::Frontmatter => c.muted,
        Block::Rule => c.marker,
        _ => c.text,
    };
    let heading = matches!(spec.block, Block::Heading(_));
    let base_weight = if heading || spec.bold {
        Weight::BOLD
    } else {
        Weight::NORMAL
    };
    let plain = Span {
        start: 0,
        end: 0,
        style: 0,
        syntax: 0,
    };

    let mut buffer = Buffer::new(&mut fonts.system, Metrics::new(spec.font, spec.line));
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_tab_width(4);
    buffer.set_size(Some(spec.width.max(spec.font * 4.0)), None);
    {
        let attrs = |span: &Span| {
            let bits = span.style;
            let mono = block_mono || bits & (style::CODE | style::MATH) != 0;
            let face = spec.face.unwrap_or(if mono { Face::Mono } else { Face::Prose });
            let mut attrs = Attrs::new()
                .family(fonts.family(face))
                .weight(if bits & style::BOLD != 0 {
                    Weight::BOLD
                } else {
                    base_weight
                })
                .color(color(dim(span_color(span, base_color, c))));
            if span.syntax == stet_core::markdown::syntax::COMMENT {
                attrs = attrs.style(Style::Italic);
            }
            if bits & style::ITALIC != 0 {
                attrs = attrs.style(Style::Italic);
            }
            attrs
        };
        let mut pieces: Vec<(&str, Attrs)> = Vec::with_capacity(spec.spans.len() * 2 + 1);
        let mut at = 0;
        for span in spec.spans {
            let (start, end) = (span.start as usize, span.end as usize);
            if start > at {
                pieces.push((&spec.text[at..start], attrs(&plain)));
            }
            pieces.push((&spec.text[start..end], attrs(span)));
            at = end;
        }
        if at < spec.text.len() || pieces.is_empty() {
            pieces.push((&spec.text[at..], attrs(&plain)));
        }
        let default = attrs(&plain);
        buffer.set_rich_text(pieces, &default, Shaping::Advanced, None::<Align>);
    }
    buffer.shape_until_scroll(&mut fonts.system, false);

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

    let thickness = (spec.font / 14.0).round().max(1.0);
    for span in spec.spans {
        let bits = span.style;
        let range = span.start as usize..span.end as usize;
        let background = if bits & style::INS != 0 {
            Some(c.insert_background)
        } else if bits & style::DEL != 0 {
            Some(c.delete_background)
        } else if bits & style::CODE != 0 && !block_mono {
            Some(c.code_background)
        } else {
            None
        };
        let line_color = dim(span_color(span, base_color, c));
        for (row, x0, x1) in layout.ranges(range) {
            let row = &layout.rows[row];
            if let Some(background) = background {
                layout.decorations.push(Decoration {
                    rect: [x0, row.top + spec.line * 0.08, x1 - x0, row.height - spec.line * 0.16],
                    color: dim(background),
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
