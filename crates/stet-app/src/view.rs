//! Turns editor state into a `Frame`, and answers the layout questions the
//! core cannot: display-line motion, hit testing and scrolling.

use crate::gpu::linear;
use crate::images::{self, Decoded, Images};
use crate::retouch::{self, Button, Retouch, Tool};
use crate::text::{Face, Fonts, Layer as Depth, LineLayout, LineSpec, RowKind, TableRow, color, layout_line};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::time::{Duration, Instant};
use stet_core::editor::{CmdKind, EntryKind, Field, MenuAt, PaletteKind, Sheet};
use stet_core::markdown::{Block, Span};
use stet_core::theme::Rgb;
use stet_core::{Editor, Mode, ScrollTo};

pub struct Quad {
    pub rect: [f32; 4],
    pub color: [f32; 4],
    pub radius: f32,
}

pub struct TextItem {
    pub key: u64,
    pub left: f32,
    pub top: f32,
    /// Clip rectangle: left, top, right, bottom.
    pub clip: [f32; 4],
    pub color: glyphon::Color,
}

/// Drawn in order: back quads, images, text, front quads.
#[derive(Default)]
pub struct Layer {
    pub back: Vec<Quad>,
    pub images: Vec<(u64, [f32; 4])>,
    /// Images are scissored to this rectangle (x, y, width, height).
    pub image_clip: [f32; 4],
    pub texts: Vec<TextItem>,
    pub front: Vec<Quad>,
}

#[derive(Default)]
pub struct Frame {
    pub clear: wgpu::Color,
    pub base: Layer,
    /// The menu, above everything else.
    pub over: Layer,
}

/// Something clickable outside the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Tab(usize),
    SidebarRow(usize),
    Sidebar,
    PaletteRow(usize),
    Palette,
    ContextRow(usize),
    ContextMenu,
    /// A margin note shown beside its pin.
    Note(usize),
    /// A video's frame; the index is into this frame's videos.
    Video(usize),
    /// The handle above a table's column: `(a line of the table, column)`.
    TableColumn(usize, usize),
    /// The handle beside a table's row, by its line.
    TableRow(usize),
    /// The strips that add a column or a row at the end of the table on this line.
    TableAddColumn(usize),
    TableAddRow(usize),
    /// The card risen from the bottom of the window, and the dimmed area
    /// around it; its rows, a choice's options `(row, option)`, its buttons.
    Sheet,
    SheetScrim,
    SheetRow(usize),
    SheetOption(usize, usize),
    SheetButton(usize),
    /// The dimmed area around the menu.
    Scrim,
    MenuHint,
}

const SHEET_RISE: Duration = Duration::from_millis(240);
const SHEET_FALL: Duration = Duration::from_millis(160);

/// The sheet as last seen, so it can still be drawn sinking away once the
/// editor has let go of it.
struct Risen {
    sheet: Sheet,
    since: Instant,
    closed: Option<Instant>,
}

/// A picture drawn in the text this frame.
#[derive(Clone)]
pub struct Picture {
    /// Where all of it would be, and the part of that on screen.
    pub rect: [f32; 4],
    seen: [f32; 4],
    /// The line that refers to it, and how.
    pub line: usize,
    pub url: String,
}

/// A picture under a line: its texture, its size on screen, and where it
/// comes from.
struct Shown {
    id: u64,
    width: f32,
    height: f32,
    url: String,
    source: images::Source,
}

#[derive(Clone, Copy)]
struct Metrics {
    font: f32,
    line: f32,
    advance: f32,
    column: f32,
    left: f32,
    /// Left and right edges of this pane's text area.
    origin: f32,
    right: f32,
    /// Left edge of the editor area as a whole (the file browser's width).
    chrome: f32,
    /// Top of the text area (below the tab strip).
    top: f32,
    top_pad: f32,
    /// Bottom of the text area; the status line sits below.
    bottom: f32,
    /// Interface font size and row height.
    ui: f32,
    row: f32,
}

pub struct View {
    pub fonts: Fonts,
    pub layouts: HashMap<u64, LineLayout>,
    pub images: Images,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub zoom: f32,
    /// Height the window's own title bar buttons cover, in pixels.
    pub titlebar: f32,
    /// Sticky x for consecutive display-line moves.
    pub goal_x: Option<f32>,
    frame: u64,
    advance: Option<(u32, f32)>,
    /// Width of a monospace character, for table columns.
    mono_advance: Option<(u32, f32)>,
    focus: Option<Range<usize>>,
    targets: Vec<([f32; 4], Target)>,
    /// What each video drawn this frame opens.
    videos: Vec<String>,
    /// The pictures drawn this frame.
    pictures: Vec<Picture>,
    /// The picture taken out of the text to be retouched.
    pub retouch: Option<Retouch>,
    risen: Option<Risen>,
    sidebar_selected: usize,
    palette_scroll: usize,
    /// Bottom-left of the text cursor in the last frame, if it was on screen.
    cursor_point: Option<(f32, f32)>,
    /// Margin sections and where they are pinned, for this frame.
    pins: Vec<stet_core::editor::Pin>,
    /// Window y of each pin's anchor, for the pins on screen this frame.
    pin_tops: Vec<(usize, f32)>,
    /// Where the mouse is, for what only shows under it.
    pointer: (f32, f32),
    /// Tables drawn this frame.
    tables: Vec<TableBox>,
    /// What of a table the pointer is over: `(first line, row line, column, part)`.
    hover: Option<(usize, usize, usize, Part)>,
}

/// A table on screen, in window coordinates.
struct TableBox {
    first: usize,
    /// Column edges, left to right.
    edges: Vec<f32>,
    /// `(line, top, bottom)` of each row on screen.
    rows: Vec<(usize, f32, f32)>,
    /// The pane's text area: left, top, right, bottom.
    clip: [f32; 4],
    /// Drawn in the pane that does not have the keyboard.
    other: bool,
}

/// The part of a table's controls the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Cells,
    Column,
    Row,
    AddColumn,
    AddRow,
}

impl View {
    pub fn new(fonts: Fonts, images: Images) -> View {
        View {
            fonts,
            layouts: HashMap::new(),
            images,
            width: 1000.0,
            height: 700.0,
            scale: 1.0,
            zoom: 1.0,
            titlebar: 0.0,
            goal_x: None,
            frame: 0,
            advance: None,
            mono_advance: None,
            focus: None,
            targets: Vec::new(),
            videos: Vec::new(),
            pictures: Vec::new(),
            retouch: None,
            risen: None,
            sidebar_selected: usize::MAX,
            palette_scroll: 0,
            cursor_point: None,
            pins: Vec::new(),
            pin_tops: Vec::new(),
            pointer: (-1.0, -1.0),
            tables: Vec::new(),
            hover: None,
        }
    }

    /// Notes where the mouse is. True if what it is over in a table
    /// changed, so the frame needs drawing again.
    pub fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        let hover = self.table_hover();
        std::mem::replace(&mut self.hover, hover) != hover
    }

    /// True if the table under the pointer is in the pane without the keyboard.
    pub fn hover_in_other_pane(&self) -> bool {
        self.hover
            .and_then(|(first, ..)| self.tables.iter().find(|table| table.first == first))
            .is_some_and(|table| table.other)
    }

    fn table_hover(&self) -> Option<(usize, usize, usize, Part)> {
        let (x, y) = self.pointer;
        let reach = 22.0 * self.scale;
        self.tables.iter().find_map(|table| {
            let (left, right) = (*table.edges.first()?, *table.edges.last()?);
            let (top, bottom) = (table.rows.first()?.1, table.rows.last()?.2);
            if x < left - reach || x > right + reach || y < top - reach || y > bottom + reach {
                return None;
            }
            let row = table
                .rows
                .iter()
                .find(|(_, _, bottom)| y < *bottom)
                .or(table.rows.last())
                .map(|(line, ..)| *line)?;
            let column = table
                .edges
                .windows(2)
                .position(|edge| x < edge[1])
                .unwrap_or(table.edges.len().saturating_sub(2));
            let part = if x > right {
                Part::AddColumn
            } else if y > bottom {
                Part::AddRow
            } else if y < top {
                Part::Column
            } else if x < left {
                Part::Row
            } else {
                Part::Cells
            };
            Some((table.first, row, column, part))
        })
    }

    /// The controls of the table under the pointer: a handle on its column
    /// and its row, and a strip on each open side that adds one more.
    fn table_controls(&mut self, ed: &Editor, layer: &mut Layer) {
        let Some((first, row, column, part)) = self.table_hover() else {
            self.hover = None;
            return;
        };
        self.hover = Some((first, row, column, part));
        if ed.palette.is_some() || ed.context_menu.is_some() {
            return;
        }
        let Some(table) = self.tables.iter().find(|table| table.first == first) else {
            return;
        };
        let (c, s) = (ed.theme.colors, self.scale);
        let (left, right) = (table.edges[0], table.edges[table.edges.len() - 1]);
        let (top, bottom) = (table.rows[0].1, table.rows[table.rows.len() - 1].2);
        let clip = table.clip;
        let Some(&(line, row_top, row_bottom)) = table.rows.iter().find(|(line, ..)| *line == row) else {
            return;
        };
        let (column_left, column_right) = (table.edges[column], table.edges[column + 1]);
        let first_on_screen = table.rows[0].0 == first;
        let mut targets = Vec::new();
        let mut plus = Vec::new();
        let mut pill = |rect: [f32; 4], hit: [f32; 4], on: bool, target: Target, sign: bool| {
            // Nothing pokes out of the pane's text area.
            if hit[1] < clip[1] || hit[1] + hit[3] > clip[3] || hit[0] < clip[0] || hit[0] + hit[2] > clip[2] {
                return;
            }
            // A strip is a quiet surface with a sign on it; a handle is a solid grip.
            let color = match (sign, on) {
                (true, true) => c.cursor.mix(c.background, 0.75),
                (true, false) => c.rule.mix(c.background, 0.45),
                (false, true) => c.cursor,
                (false, false) => c.rule.mix(c.text, 0.25),
            };
            layer.back.push(Quad {
                rect,
                color: linear(color, 1.0),
                radius: rect[2].min(rect[3]) / 2.0,
            });
            targets.push((hit, target));
            if sign {
                plus.push((rect, on));
            }
        };
        let thick = (5.0 * s).round();
        if first_on_screen {
            pill(
                [
                    column_left + 4.0 * s,
                    top - 10.0 * s,
                    column_right - column_left - 8.0 * s,
                    thick,
                ],
                [column_left, top - 16.0 * s, column_right - column_left, 16.0 * s],
                part == Part::Column,
                Target::TableColumn(first, column),
                false,
            );
        }
        pill(
            [
                left - 10.0 * s,
                row_top + 4.0 * s,
                thick,
                row_bottom - row_top - 8.0 * s,
            ],
            [left - 16.0 * s, row_top, 16.0 * s, row_bottom - row_top],
            part == Part::Row,
            Target::TableRow(line),
            false,
        );
        let strip = (14.0 * s).round();
        pill(
            [right + 5.0 * s, top, strip, bottom - top],
            [right + 2.0 * s, top, strip + 6.0 * s, bottom - top],
            part == Part::AddColumn,
            Target::TableAddColumn(first),
            true,
        );
        pill(
            [left, bottom + 5.0 * s, right - left, strip],
            [left, bottom + 2.0 * s, right - left, strip + 6.0 * s],
            part == Part::AddRow,
            Target::TableAddRow(first),
            true,
        );
        self.targets.extend(targets);
        for (rect, on) in plus {
            let rgb = if on { c.cursor } else { c.status };
            let key = self.label(ed, "+", rgb, 13.0 * s, Face::Ui, true);
            let width = self.layouts[&key].rows[0].width;
            let middle = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
            self.put(layer, key, middle.0 - width / 2.0, middle.1, clip, rgb);
        }
    }

    /// Drops shaped lines, after anything that changes how text looks.
    pub fn invalidate(&mut self) {
        self.layouts.clear();
        self.advance = None;
        self.mono_advance = None;
    }

    pub fn set_zoom(&mut self, step: i32) {
        self.zoom = if step == 0 {
            1.0
        } else {
            (self.zoom * 1.1f32.powi(step)).clamp(0.5, 4.0)
        };
        self.invalidate();
    }

    fn sidebar_width(&self, ed: &Editor) -> f32 {
        if ed.sidebar.visible {
            (250.0 * self.scale).min(self.width * 0.38).round()
        } else {
            0.0
        }
    }

    /// Where the document pane ends and the margin begins, if it is open.
    fn split(&self, ed: &Editor) -> Option<f32> {
        let chrome = self.sidebar_width(ed);
        ed.margin_visible()
            .then(|| (chrome + (self.width - chrome) * 0.58).round())
    }

    /// True if a window x position is over the pane that does not have the
    /// keyboard.
    pub fn over_other_pane(&self, ed: &Editor, x: f32) -> bool {
        self.split(ed)
            .is_some_and(|split| (x >= split) != ed.in_margin() && x >= self.sidebar_width(ed))
    }

    fn metrics(&mut self, ed: &Editor) -> Metrics {
        let font = (ed.config.font_size * self.zoom * self.scale).round().max(4.0);
        let line = (font * ed.config.line_height).round();
        let advance = match self.advance {
            Some((size, advance)) if size == font.to_bits() => advance,
            _ => {
                let layout = layout_line(
                    &mut self.fonts,
                    &LineSpec {
                        text: "0000000000",
                        spans: &[],
                        block: Block::Text,
                        font,
                        line,
                        width: font * 100.0,
                        colors: &ed.theme.colors,
                        dim: false,
                        face: None,
                        bold: false,
                        table: None,
                    },
                );
                let advance = (layout.rows[0].width / 10.0).max(1.0);
                self.advance = Some((font.to_bits(), advance));
                advance
            }
        };
        // With the margin open the editor area is two panes; these are the
        // metrics of whichever one `ed` currently holds.
        let chrome = self.sidebar_width(ed);
        let (origin, right) = match self.split(ed) {
            Some(split) if ed.in_margin() => (split, self.width),
            Some(split) => (chrome, split),
            None => (chrome, self.width),
        };
        let available = right - origin;
        // Leave room for hanging heading markers; less in a narrow pane.
        let margin = advance * if available < advance * 64.0 { 4.0 } else { 7.0 };
        let column = (ed.config.line_width as f32 * advance)
            .min(available - margin * 2.0)
            .max(advance * 12.0)
            .round();
        let ui = (13.0 * self.scale).round();
        let row = (28.0 * self.scale).round();
        let top = if ed.tab_count() > 1 {
            (36.0 * self.scale).round()
        } else {
            0.0
        };
        Metrics {
            font,
            line,
            advance,
            column,
            left: origin + ((available - column) / 2.0).round().max(advance),
            origin,
            right,
            chrome,
            top,
            top_pad: top + (line * 1.6).round(),
            bottom: (self.height - line * 1.5).round(),
            ui,
            row,
        }
    }

    fn mono_advance(&mut self, ed: &Editor, m: &Metrics) -> f32 {
        match self.mono_advance {
            Some((size, advance)) if size == m.font.to_bits() => advance,
            _ => {
                let layout = layout_line(
                    &mut self.fonts,
                    &LineSpec {
                        text: "0000000000",
                        spans: &[],
                        block: Block::Code,
                        font: m.font,
                        line: m.line,
                        width: m.font * 100.0,
                        colors: &ed.theme.colors,
                        dim: false,
                        face: None,
                        bold: false,
                        table: None,
                    },
                );
                let advance = (layout.rows[0].width / 10.0).max(1.0);
                self.mono_advance = Some((m.font.to_bits(), advance));
                advance
            }
        }
    }

    /// Lays out `line` if needed and returns its cache key.
    fn line(&mut self, ed: &Editor, line: usize, m: &Metrics) -> u64 {
        let text = ed.buf.line_text(line);
        let doc = ed.doc();
        let block = doc.block(line);
        let code = ed.code_spans(line);
        // A pin line in the margin is bookkeeping, and reads as such.
        let pin_line = [Span {
            start: 0,
            end: text.len() as u32,
            style: stet_core::markdown::style::MUTED,
            syntax: 0,
        }];
        let spans: &[Span] = match &code {
            _ if ed.in_margin() && text.starts_with("@ ") => &pin_line,
            Some((lines, at)) => &lines[*at],
            None => doc.spans(line),
        };
        let dim = self.focus.as_ref().is_some_and(|focus| !focus.contains(&line));
        // A table row is laid out on the columns its table settled on.
        let grid = match block {
            Block::Table if code.is_none() => ed.table_at(line),
            _ => None,
        }
        .map(|(lines, shape)| {
            let advance = self.mono_advance(ed, m);
            let pad = advance.round();
            let count = shape.columns.len() as f32;
            let natural: usize = shape.columns.iter().map(|column| column.width).sum();
            let fits = |width: f32| ((width - count * pad * 2.0 - 2.0) / advance).floor().max(0.0) as usize;
            // A table too wide for the column may use the space beside it.
            let beside = (m.left - m.origin).min(m.right - m.left - m.column) - advance * 2.0;
            let room = if natural <= fits(m.column) {
                fits(m.column)
            } else {
                fits(m.column + beside.max(0.0) * 2.0)
            };
            let widths: Vec<f32> = shape.fit(room).iter().map(|cells| *cells as f32 * advance).collect();
            let aligns: Vec<stet_core::table::Align> = shape.columns.iter().map(|column| column.align).collect();
            let kind = match line - lines.start {
                0 => RowKind::Header,
                1 => RowKind::Rule,
                _ => RowKind::Body,
            };
            (widths, aligns, kind, line + 1 == lines.end, pad, advance)
        });
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        if let Some((widths, aligns, kind, last, ..)) = &grid {
            for width in widths {
                width.to_bits().hash(&mut hasher);
            }
            (aligns, kind, last).hash(&mut hasher);
        }
        for span in spans {
            (span.start, span.end, span.style, span.syntax).hash(&mut hasher);
        }
        let block_id = match block {
            Block::Heading(level) => 10 + level,
            Block::Text => 0,
            Block::Code => 1,
            Block::Table => 2,
            Block::Frontmatter => 3,
            Block::Rule => 4,
        };
        (
            block_id,
            m.column.to_bits(),
            m.font.to_bits(),
            m.line.to_bits(),
            dim,
            &ed.theme.name,
        )
            .hash(&mut hasher);
        let key = hasher.finish();
        let fonts = &mut self.fonts;
        let layout = self.layouts.entry(key).or_insert_with(|| {
            let table = grid
                .as_ref()
                .map(|(widths, aligns, kind, last, pad, advance)| TableRow {
                    widths,
                    aligns,
                    kind: *kind,
                    last: *last,
                    pad: *pad,
                    advance: *advance,
                });
            let mut layout = layout_line(
                fonts,
                &LineSpec {
                    text: &text,
                    spans,
                    block,
                    font: m.font,
                    line: m.line,
                    width: m.column,
                    colors: &ed.theme.colors,
                    dim,
                    face: None,
                    bold: false,
                    table,
                },
            );
            if table.is_some() {
                // Centred on the column when it is wider than it.
                let width = layout.rows[0].width;
                layout.hang = ((width - m.column) / 2.0).max(0.0).round();
            }
            layout
        });
        layout.last_used = self.frame;
        key
    }

    /// A single-row label, cached like any other line.
    fn label(&mut self, ed: &Editor, text: &str, rgb: Rgb, size: f32, face: Face, bold: bool) -> u64 {
        let font = size.round();
        let mut hasher = DefaultHasher::new();
        (
            "label",
            text,
            rgb.0,
            rgb.1,
            rgb.2,
            font.to_bits(),
            face,
            bold,
            &ed.theme.name,
        )
            .hash(&mut hasher);
        let key = hasher.finish();
        let fonts = &mut self.fonts;
        let layout = self.layouts.entry(key).or_insert_with(|| {
            let mut colors = ed.theme.colors;
            colors.text = rgb;
            layout_line(
                fonts,
                &LineSpec {
                    text,
                    spans: &[],
                    block: Block::Text,
                    font,
                    line: (font * 1.4).round(),
                    width: 100_000.0,
                    colors: &colors,
                    dim: false,
                    face: Some(face),
                    bold,
                    table: None,
                },
            )
        });
        layout.last_used = self.frame;
        key
    }

    /// Places a label and returns its width.
    #[allow(clippy::too_many_arguments)]
    fn put(&mut self, layer: &mut Layer, key: u64, left: f32, middle: f32, clip: [f32; 4], rgb: Rgb) -> f32 {
        let layout = &self.layouts[&key];
        layer.texts.push(TextItem {
            key,
            left: left.round(),
            top: (middle - layout.height / 2.0).round(),
            clip,
            color: color(rgb),
        });
        layout.rows[0].width
    }

    /// Images shown under `line`, sized in device pixels.
    fn line_images(&mut self, ed: &Editor, line: usize, m: &Metrics) -> Vec<Shown> {
        if !ed.config.images {
            return Vec::new();
        }
        let max_height = (self.height * 0.6).max(m.line);
        ed.doc()
            .images_on(line)
            .filter_map(|image| {
                let source = images::resolve(&image.url, ed.path.as_deref(), ed.config.remote_images)?;
                let (id, width, height) = self.images.get(&source)?;
                let (width, height) = (width as f32 * self.scale, height as f32 * self.scale);
                let fit = (m.column / width).min(max_height / height).min(1.0);
                Some(Shown {
                    id,
                    width: (width * fit).round(),
                    height: (height * fit).round(),
                    url: image.url.clone(),
                    source,
                })
            })
            .collect()
    }

    fn image_gap(m: &Metrics) -> f32 {
        (m.line * 0.4).round()
    }

    /// Full height of a line: wrapped text plus any images below it.
    fn line_height(&mut self, ed: &Editor, line: usize, m: &Metrics) -> f32 {
        let key = self.line(ed, line, m);
        let images: f32 = self
            .line_images(ed, line, m)
            .iter()
            .map(|shown| shown.height + Self::image_gap(m))
            .sum();
        self.layouts[&key].height + images
    }

    /// Keeps the scroll position valid, and the end of the document from
    /// rising above the middle of the view.
    fn normalize(&mut self, ed: &mut Editor, m: &Metrics) {
        self.anchor(ed, m);
        let keep = ((m.bottom - m.top_pad) * 0.5).round();
        let (mut below, mut line) = (-ed.scroll_px, ed.scroll_line);
        while below < keep && line < ed.buf.line_count() {
            below += self.line_height(ed, line, m);
            line += 1;
        }
        if below < keep {
            ed.scroll_px -= keep - below;
            self.anchor(ed, m);
        }
    }

    /// Re-anchors the scroll position so `scroll_px` lies within its line.
    fn anchor(&mut self, ed: &mut Editor, m: &Metrics) {
        let last = ed.buf.line_count() - 1;
        ed.scroll_line = ed.scroll_line.min(last);
        while ed.scroll_px < 0.0 && ed.scroll_line > 0 {
            ed.scroll_line -= 1;
            ed.scroll_px += self.line_height(ed, ed.scroll_line, m);
        }
        if ed.scroll_px < 0.0 {
            ed.scroll_px = 0.0;
        }
        loop {
            let height = self.line_height(ed, ed.scroll_line, m);
            if ed.scroll_px < height {
                break;
            }
            if ed.scroll_line == last {
                // The last line may scroll up to the top, never out of view.
                ed.scroll_px = (height - m.line).max(0.0);
                break;
            }
            ed.scroll_px -= height;
            ed.scroll_line += 1;
        }
    }

    pub fn scroll_by(&mut self, ed: &mut Editor, pixels: f32) {
        ed.refresh();
        let m = self.metrics(ed);
        ed.scroll_px += pixels;
        self.normalize(ed, &m);
    }

    /// `(layout key, row, byte)` of the cursor.
    fn cursor_place(&mut self, ed: &Editor, m: &Metrics) -> (usize, u64, usize, usize) {
        let line = ed.buf.line_of(ed.cursor);
        let key = self.line(ed, line, m);
        let layout = &self.layouts[&key];
        let byte = layout.byte_of_col(ed.cursor - ed.buf.line_start(line));
        (line, key, layout.row_of_byte(byte), byte)
    }

    /// Y of the cursor row's top relative to the window, if it is near the view.
    fn cursor_y(&mut self, ed: &Editor, m: &Metrics) -> Option<f32> {
        let (line, key, row, _) = self.cursor_place(ed, m);
        if line < ed.scroll_line || line - ed.scroll_line > 300 {
            return None;
        }
        let mut y = m.top_pad - ed.scroll_px;
        for above in ed.scroll_line..line {
            y += self.line_height(ed, above, m);
        }
        Some(y + self.layouts[&key].rows[row].top)
    }

    /// Scrolls the minimum needed to keep the cursor row on screen.
    pub fn follow_cursor(&mut self, ed: &mut Editor) {
        ed.refresh();
        let m = self.metrics(ed);
        self.focus = ed.config.focus.then(|| ed.focus_lines());
        let (line, key, row, _) = self.cursor_place(ed, &m);
        let row_top = self.layouts[&key].rows[row].top;
        let margin = m.top + m.line;
        let centre = (m.top + (m.bottom - m.top - m.line) / 2.0).round();
        match self.cursor_y(ed, &m) {
            None => {
                // Far away: jump, landing a third of the way down.
                ed.scroll_line = line;
                let want = if ed.config.typewriter {
                    centre
                } else {
                    m.top + (m.bottom - m.top) / 3.0
                };
                ed.scroll_px = row_top + m.top_pad - want;
            }
            Some(y) if ed.config.typewriter => ed.scroll_px += y - centre,
            Some(y) if y < margin => ed.scroll_px -= margin - y,
            Some(y) if y + m.line > m.bottom - m.line => ed.scroll_px += y + m.line - (m.bottom - m.line),
            Some(_) => {}
        }
        self.normalize(ed, &m);
    }

    pub fn scroll_cursor_to(&mut self, ed: &mut Editor, to: ScrollTo) {
        ed.refresh();
        let m = self.metrics(ed);
        let (line, key, row, _) = self.cursor_place(ed, &m);
        let target = match to {
            ScrollTo::Top => m.top + m.line,
            ScrollTo::Center => m.top + (m.bottom - m.top - m.line) / 2.0,
            ScrollTo::Bottom => m.bottom - m.line * 2.0,
        };
        ed.scroll_line = line;
        ed.scroll_px = self.layouts[&key].rows[row].top - target + m.top_pad;
        self.normalize(ed, &m);
    }

    /// Moves the cursor by wrapped display rows, keeping a sticky x.
    pub fn visual_move(&mut self, ed: &mut Editor, delta: isize) {
        ed.refresh();
        let m = self.metrics(ed);
        let (mut line, key, row, byte) = self.cursor_place(ed, &m);
        let x = self.goal_x.unwrap_or_else(|| self.layouts[&key].x_of_byte(row, byte));
        let last = ed.buf.line_count() - 1;
        let mut row = row;
        let mut key = key;
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 {
                if row + 1 < self.layouts[&key].rows.len() {
                    row += 1;
                } else if line < last {
                    line += 1;
                    key = self.line(ed, line, &m);
                    row = 0;
                } else {
                    break;
                }
            } else if row > 0 {
                row -= 1;
            } else if line > 0 {
                line -= 1;
                key = self.line(ed, line, &m);
                row = self.layouts[&key].rows.len() - 1;
            } else {
                break;
            }
        }
        let layout = &self.layouts[&key];
        let col = layout.col_of_byte(layout.byte_at(row, x, ed.mode == Mode::Insert));
        ed.move_cursor_to(ed.buf.line_start(line) + col);
        self.goal_x = Some(x);
    }

    /// The buffer position under a window point.
    pub fn hit(&mut self, ed: &mut Editor, x: f32, y: f32) -> usize {
        ed.refresh();
        let m = self.metrics(ed);
        let last = ed.buf.line_count() - 1;
        let mut top = m.top_pad - ed.scroll_px;
        let mut line = ed.scroll_line;
        loop {
            let height = self.line_height(ed, line, &m);
            if y < top + height || line == last {
                break;
            }
            top += height;
            line += 1;
        }
        let key = self.line(ed, line, &m);
        let layout = &self.layouts[&key];
        let row = layout.row_at_y(y - top);
        let byte = layout.byte_at(row, x - m.left + layout.hang, ed.mode == Mode::Insert);
        ed.buf.line_start(line) + layout.col_of_byte(byte)
    }

    /// What a click at this point lands on, other than text. Uses the
    /// geometry of the last frame.
    pub fn target_at(&self, x: f32, y: f32) -> Option<Target> {
        self.targets
            .iter()
            .rev()
            .find(|([left, top, width, height], _)| x >= *left && x < left + width && y >= *top && y < top + height)
            .map(|(_, target)| *target)
    }

    /// The picture drawn at this point of the window.
    pub fn picture_at(&self, x: f32, y: f32) -> Option<Picture> {
        let inside =
            |[left, top, width, height]: [f32; 4]| x >= left && x < left + width && y >= top && y < top + height;
        self.pictures.iter().find(|picture| inside(picture.seen)).cloned()
    }

    /// Where the picture `url` on `line` was drawn, if it is on screen.
    pub fn picture_rect(&self, line: usize, url: &str) -> Option<[f32; 4]> {
        let found = self
            .pictures
            .iter()
            .find(|picture| picture.line == line && picture.url == url);
        found.map(|picture| picture.rect)
    }

    /// Decoded pictures ready for the renderer: those of the text, and the
    /// one being retouched whenever it changes.
    pub fn uploads(&mut self) -> Vec<Decoded> {
        let mut ready = self.images.poll();
        if let Some(retouch) = &mut self.retouch {
            ready.extend(retouch.texture(&mut self.fonts));
        }
        ready
    }

    /// Something is in motion: the next frame differs without any input.
    pub fn animating(&self) -> bool {
        let sheet = self
            .risen
            .as_ref()
            .is_some_and(|risen| risen.closed.is_some() || risen.since.elapsed() < SHEET_RISE);
        let retouch = self.retouch.as_ref();
        sheet || retouch.is_some_and(|retouch| retouch.animating() || retouch.gone())
    }

    /// What the video behind `Target::Video(index)` opens.
    pub fn video(&self, index: usize) -> Option<&str> {
        self.videos.get(index).map(String::as_str)
    }

    /// Scrolls the file browser by rows.
    pub fn scroll_sidebar(&mut self, ed: &mut Editor, rows: isize) {
        let last = ed.sidebar.entries.len().saturating_sub(1) as isize;
        ed.sidebar.scroll = (ed.sidebar.scroll as isize + rows).clamp(0, last) as usize;
    }

    pub fn frame(&mut self, ed: &mut Editor) -> Frame {
        ed.refresh();
        self.frame += 1;
        self.targets.clear();
        self.videos.clear();
        self.pictures.clear();
        if self.retouch.as_ref().is_some_and(Retouch::gone) {
            self.retouch = None;
        }
        let background = linear(ed.theme.colors.background, 1.0);
        let mut frame = Frame {
            clear: wgpu::Color {
                r: background[0] as f64,
                g: background[1] as f64,
                b: background[2] as f64,
                a: 1.0,
            },
            ..Frame::default()
        };
        self.cursor_point = None;
        self.tables.clear();
        self.pin_tops.clear();
        self.pins = if ed.margin_visible() { ed.pins() } else { Vec::new() };
        // The document first: where its lines fall decides where pinned
        // notes go.
        let reveal = ed.margin_visible() && ed.take_reveal();
        if ed.margin_visible() && ed.in_margin() {
            let mut layer = std::mem::take(&mut frame.base);
            ed.with_other_pane(|document| {
                document.refresh();
                if reveal {
                    self.follow_cursor(document);
                }
                self.body(document, &mut layer, false);
            });
            frame.base = layer;
            self.body(ed, &mut frame.base, true);
        } else {
            self.body(ed, &mut frame.base, true);
            if ed.margin_visible() {
                let mut layer = std::mem::take(&mut frame.base);
                let beside = ed.config.pinned_notes && self.pins.iter().any(|pin| pin.pinned());
                ed.with_other_pane(|margin| {
                    margin.refresh();
                    if beside {
                        self.notes(margin, &mut layer);
                    } else {
                        if reveal {
                            self.follow_cursor(margin);
                        }
                        self.body(margin, &mut layer, false);
                    }
                });
                frame.base = layer;
            }
        }
        let m = self.metrics(ed);
        frame.base.image_clip = [m.chrome, m.top, self.width - m.chrome, m.bottom - m.top];
        self.table_controls(ed, &mut frame.base);

        self.status(ed, &m, &mut frame.base);
        self.tabs(ed, &m, &mut frame.base);
        self.sidebar(ed, &m, &mut frame.base);
        self.palette(ed, &m, &mut frame.over);
        self.context_menu(ed, &m, &mut frame.over);
        self.retouching(ed, m.ui, &mut frame.over);
        self.sheet(ed, &m, &mut frame.over);
        if self.layouts.len() > 3000 {
            let keep = self.frame - 1;
            self.layouts.retain(|_, layout| layout.last_used >= keep);
        }
        frame
    }

    /// The margin's own surface, with a quiet label.
    fn margin_surface(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer, focused: bool) {
        let c = ed.theme.colors;
        let hairline = self.scale.max(1.0);
        layer.back.push(Quad {
            rect: [m.origin, m.top, m.right - m.origin, self.height - m.top],
            color: linear(c.panel, 1.0),
            radius: 0.0,
        });
        layer.back.push(Quad {
            rect: [m.origin, m.top, hairline, self.height - m.top],
            color: linear(c.rule, 0.7),
            radius: 0.0,
        });
        let rgb = if focused { c.cursor } else { c.status };
        let label = self.label(ed, "MARGIN", rgb, m.ui * 0.78, Face::Ui, true);
        let width = self.layouts[&label].rows[0].width;
        let clip = [m.origin, m.top, m.right, m.bottom];
        self.put(
            layer,
            label,
            m.right - width - 14.0 * self.scale,
            m.top + 16.0 * self.scale,
            clip,
            rgb,
        );
    }

    /// The margin as marginalia: each pinned section drawn beside the text
    /// it is pinned to, moving with the document. `ed` holds the margin.
    fn notes(&mut self, ed: &mut Editor, layer: &mut Layer) {
        let m = self.metrics(ed);
        self.focus = None;
        self.margin_surface(ed, &m, layer, false);
        let c = ed.theme.colors;
        let clip = [m.origin, m.top, m.right, m.bottom];
        let gap = (m.line * 0.7).round();
        let bar = (2.0 * self.scale).round().max(1.0);
        let mut tops = self.pin_tops.clone();
        tops.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut floor = m.top + gap;
        for (index, anchor_y) in tops {
            let pin = self.pins[index].clone();
            let top = anchor_y.max(floor);
            if top >= m.bottom {
                break;
            }
            // The section without its `@` line, and without trailing blanks.
            let mut lines: Vec<usize> = pin
                .lines
                .clone()
                .filter(|&line| line < ed.buf.line_count() && !ed.buf.line_text(line).starts_with("@ "))
                .collect();
            while lines.last().is_some_and(|&line| ed.buf.line_is_blank(line)) {
                lines.pop();
            }
            let more = lines.len() > 12;
            lines.truncate(12);
            let mut y = top;
            for line in lines {
                if y >= m.bottom {
                    break;
                }
                let key = self.line(ed, line, &m);
                let layout = &self.layouts[&key];
                // Clear of the stroke down the side.
                let left = (m.left - layout.hang).max(m.origin + 24.0 * self.scale);
                for decoration in &layout.decorations {
                    let [x, dy, width, height] = decoration.rect;
                    let quad = Quad {
                        rect: [left + x, y + dy, width, height],
                        color: linear(decoration.color, 1.0),
                        radius: decoration.radius,
                    };
                    match decoration.layer {
                        Depth::Back => layer.back.push(quad),
                        Depth::Front => layer.front.push(quad),
                    }
                }
                layer.texts.push(TextItem {
                    key,
                    left,
                    top: y,
                    clip,
                    color: color(c.text),
                });
                y += layout.height;
            }
            if more {
                let key = self.label(ed, "…", c.status, m.font, Face::Prose, false);
                self.put(layer, key, m.left, y + m.line / 2.0, clip, c.status);
                y += m.line;
            }
            let height = (y - top).min(m.bottom - top);
            // The proofreader's stroke down the side of the note.
            layer.back.push(Quad {
                rect: [m.origin + (10.0 * self.scale).round(), top, bar, height],
                color: linear(c.cursor, 0.85),
                radius: bar / 2.0,
            });
            self.targets
                .push(([m.origin, top, m.right - m.origin, height], Target::Note(index)));
            floor = y + gap;
        }
        // What is not beside anything: say so, quietly.
        let loose = self.pins.iter().filter(|pin| !pin.pinned()).count();
        let unseen = self
            .pins
            .iter()
            .filter(|pin| pin.pinned())
            .count()
            .saturating_sub(self.pin_tops.len());
        let mut parts = Vec::new();
        if unseen > 0 {
            parts.push(format!("{unseen} pinned elsewhere"));
        }
        if loose > 0 {
            parts.push(format!("{loose} not pinned"));
        }
        if !parts.is_empty() {
            let text = format!("{}  ·  {} opens the margin", parts.join("  ·  "), ed.chord(true, "O"));
            let key = self.label(ed, &text, c.status, m.ui * 0.82, Face::Ui, false);
            let clip = [m.origin, m.top, m.right, self.height];
            self.put(
                layer,
                key,
                m.origin + 22.0 * self.scale,
                m.bottom - m.line * 0.6,
                clip,
                c.status,
            );
        }
    }

    /// Draws the text of the buffer `ed` holds into its pane. `focused` is
    /// the pane with the keyboard, which shows the cursor.
    fn body(&mut self, ed: &mut Editor, layer: &mut Layer, focused: bool) {
        let m = self.metrics(ed);
        self.focus = ed.config.focus.then(|| ed.focus_lines());
        self.normalize(ed, &m);
        if focused {
            ed.view_rows = ((m.bottom - m.top_pad) / m.line).max(1.0) as usize;
        }
        let c = ed.theme.colors;
        if ed.in_margin() {
            self.margin_surface(ed, &m, layer, focused);
        }
        let clip = [m.origin, m.top, m.right, m.bottom];
        // Quads stay inside the text area.
        let clipped = |rect: [f32; 4]| {
            let top = rect[1].max(m.top);
            let bottom = (rect[1] + rect[3]).min(m.bottom);
            [rect[0], top, rect[2], (bottom - top).max(0.0)]
        };
        let selections = ed.selection_ranges();
        let cursor_line = ed.buf.line_of(ed.cursor);
        let bar_width = (2.0 * self.scale).round().max(1.0);
        let editing = focused && ed.cmdline.is_none() && ed.palette.is_none() && !ed.sidebar.focused;

        let mut y = m.top_pad - ed.scroll_px;
        let mut line = ed.scroll_line;
        while y < m.bottom && line < ed.buf.line_count() {
            let key = self.line(ed, line, &m);
            let images = self.line_images(ed, line, &m);
            let layout = &self.layouts[&key];
            let left = m.left - layout.hang.min(m.left - m.origin - m.advance);
            let (line_start, line_end) = (ed.buf.line_start(line), ed.buf.line_end(line));
            let bytes = |chars: Range<usize>| layout.byte_of_col(chars.start)..layout.byte_of_col(chars.end);

            if !layout.edges.is_empty()
                && let Some((lines, _)) = ed.table_at(line)
            {
                let row = (line, y.max(m.top), (y + layout.height).min(m.bottom));
                match self
                    .tables
                    .last_mut()
                    .filter(|table| table.first == lines.start && table.other != focused)
                {
                    Some(table) => table.rows.push(row),
                    None => self.tables.push(TableBox {
                        first: lines.start,
                        edges: layout.edges.iter().map(|edge| left + edge).collect(),
                        rows: vec![row],
                        clip,
                        other: !focused,
                    }),
                }
            }
            if ed.doc().block(line) == Block::Code {
                let pad = (m.advance * 0.8).round();
                layer.back.push(Quad {
                    rect: clipped([m.left - pad, y, m.column + pad * 2.0, layout.height]),
                    color: linear(c.code_background, 1.0),
                    radius: 0.0,
                });
            }
            for decoration in &layout.decorations {
                let [x, top, width, height] = decoration.rect;
                let quad = Quad {
                    rect: clipped([left + x, y + top, width, height]),
                    color: linear(decoration.color, 1.0),
                    radius: decoration.radius,
                };
                match decoration.layer {
                    Depth::Back => layer.back.push(quad),
                    Depth::Front => layer.front.push(quad),
                }
            }
            if let Some(matcher) = ed.search_highlight() {
                for found in matcher.find_all(&layout.text) {
                    for (row, x0, x1) in layout.ranges(bytes(found)) {
                        let row = &layout.rows[row];
                        layer.back.push(Quad {
                            rect: clipped([left + x0, y + row.top, x1 - x0, row.height]),
                            color: linear(c.search, 1.0),
                            radius: m.font * 0.15,
                        });
                    }
                }
            }
            for selection in selections.iter().filter(|s| s.start <= line_end && line_start < s.end) {
                let chars = selection.start.max(line_start) - line_start..selection.end.min(line_end) - line_start;
                let through_newline = selection.end > line_end;
                let ranges = layout.ranges(bytes(chars));
                let count = ranges.len();
                for (index, (row, x0, x1)) in ranges.into_iter().enumerate() {
                    let row = &layout.rows[row];
                    // A selected line break shows as a sliver past the text.
                    let tail = if through_newline && index + 1 == count {
                        m.advance * 0.5
                    } else {
                        0.0
                    };
                    layer.back.push(Quad {
                        rect: clipped([left + x0, y + row.top, x1 - x0 + tail, row.height]),
                        color: linear(c.selection, 1.0),
                        radius: 0.0,
                    });
                }
                if count == 0 && through_newline {
                    layer.back.push(Quad {
                        rect: clipped([left, y, m.advance * 0.5, m.line]),
                        color: linear(c.selection, 1.0),
                        radius: 0.0,
                    });
                }
            }
            if !ed.in_margin() {
                // Text a margin note is pinned to carries the stet mark: a
                // row of dots beneath it.
                let dot = (m.font * 0.11).round().max(2.0);
                for (index, pin) in self.pins.iter().enumerate() {
                    let Some(at) = pin
                        .at
                        .clone()
                        .filter(|at| at.start <= line_end && line_start < at.end.max(at.start + 1))
                    else {
                        continue;
                    };
                    let chars = at.start.max(line_start) - line_start..at.end.min(line_end) - line_start;
                    for (row, x0, x1) in layout.ranges(bytes(chars)) {
                        let row = &layout.rows[row];
                        if at.start >= line_start && !self.pin_tops.iter().any(|(seen, _)| *seen == index) {
                            self.pin_tops.push((index, y + row.top));
                        }
                        let base = y + row.baseline + m.font * 0.22;
                        let mut x = left + x0;
                        while x + dot <= left + x1 {
                            layer.front.push(Quad {
                                rect: clipped([x, base, dot, dot]),
                                color: linear(c.cursor, 1.0),
                                radius: dot / 2.0,
                            });
                            x += dot * 2.4;
                        }
                    }
                }
            }
            if line == cursor_line && focused {
                let byte = layout.byte_of_col(ed.cursor - line_start);
                let row = layout.row_of_byte(byte);
                self.cursor_point = Some((left + layout.x_of_byte(row, byte), y + layout.rows[row].top + m.line));
            }
            if line == cursor_line && editing {
                let byte = layout.byte_of_col(ed.cursor - line_start);
                let row = layout.row_of_byte(byte);
                let x = left + layout.x_of_byte(row, byte);
                let top = y + layout.rows[row].top;
                if ed.mode == Mode::Insert {
                    layer.front.push(Quad {
                        rect: clipped([x.round() - bar_width / 2.0, top, bar_width, m.line]),
                        color: linear(c.cursor, 1.0),
                        radius: bar_width / 2.0,
                    });
                } else {
                    let width = layout.width_at(row, byte).unwrap_or(m.advance);
                    layer.back.push(Quad {
                        rect: clipped([x, top, width, m.line]),
                        color: linear(c.cursor, 0.45),
                        radius: m.font * 0.12,
                    });
                }
            }
            layer.texts.push(TextItem {
                key,
                left,
                top: y,
                clip,
                color: color(c.text),
            });
            y += layout.height;
            for Shown {
                id,
                width,
                height,
                url,
                source,
            } in images
            {
                let gap = Self::image_gap(&m);
                if y < m.bottom {
                    let rect = [m.left, y + gap * 0.5, width, height];
                    layer.images.push((id, rect));
                    let (top, bottom) = (rect[1].max(m.top), (rect[1] + height).min(m.bottom));
                    let seen = [m.left, top, width, bottom - top];
                    match source.video() {
                        _ if bottom <= top => {}
                        Some(video) => {
                            self.targets.push((seen, Target::Video(self.videos.len())));
                            self.videos.push(video);
                        }
                        None => self.pictures.push(Picture { rect, seen, line, url }),
                    }
                }
                y += height + gap;
            }
            line += 1;
        }
    }

    fn status(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let c = ed.theme.colors;
        let pad = (m.advance * 2.0).round();
        let size = m.font * 0.78;
        let middle = (m.bottom + self.height) / 2.0;
        let clip = [m.chrome, m.bottom, self.width, self.height];

        let (left_text, left_color) = if let Some(cmdline) = &ed.cmdline {
            let prefix = match cmdline.kind {
                CmdKind::Command => ":".to_string(),
                CmdKind::SearchForward => "/".to_string(),
                CmdKind::SearchBackward => "?".to_string(),
                CmdKind::Agent => {
                    let name = ed.agent.as_ref().map_or("assistant", |agent| agent.name.as_str());
                    let about = if ed.selection().is_some() {
                        " (about the selection)"
                    } else {
                        ""
                    };
                    format!("→ {name}{about}: ")
                }
            };
            (format!("{prefix}{}", cmdline.text), c.text)
        } else if let Some(message) = &ed.message {
            (message.text.clone(), if message.error { c.error } else { c.status })
        } else {
            let mode = match ed.mode {
                _ if ed.sidebar.focused => "FILES",
                _ if !ed.config.vim => "",
                Mode::Normal => "NORMAL",
                Mode::Insert => "INSERT",
                Mode::Visual => "VISUAL",
                Mode::VisualLine => "V-LINE",
                Mode::VisualBlock => "V-BLOCK",
            };
            let suggesting = if ed.suggesting { "SUGGESTING" } else { "" };
            let recording = ed
                .recording_macro()
                .map(|register| format!("REC @{register}"))
                .unwrap_or_default();
            let joined: Vec<&str> = [mode, suggesting, &recording]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect();
            (joined.join("  ·  "), if ed.suggesting { c.insert } else { c.status })
        };
        let mut left_width = 0.0;
        if !left_text.is_empty() || ed.cmdline.is_some() {
            let key = self.label(ed, &left_text, left_color, size, Face::Mono, false);
            left_width = self.put(layer, key, m.chrome + pad, middle, clip, left_color);
            if ed.cmdline.is_some() {
                let height = self.layouts[&key].height;
                let bar = (2.0 * self.scale).round().max(1.0);
                layer.front.push(Quad {
                    rect: [
                        m.chrome + pad + left_width + bar,
                        (middle - height / 2.0).round(),
                        bar,
                        height,
                    ],
                    color: linear(c.cursor, 1.0),
                    radius: bar / 2.0,
                });
            }
        }

        let line = ed.buf.line_of(ed.cursor);
        let col = ed.cursor - ed.buf.line_start(line);
        let pending = ed.pending_keys();
        let dirty = if ed.buf.is_dirty() { " •" } else { "" };
        let suggestions = match ed.doc().suggestions.len() {
            0 => String::new(),
            1 => "1 suggestion    ".to_string(),
            count => format!("{count} suggestions    "),
        };
        let hint = format!("{} menu", ed.chord(false, "/"));
        // Who is on the other end of `stet ctl wait`, if anyone.
        let agent = match &ed.agent {
            Some(agent) if agent.listening => format!("● {}    ", agent.name),
            Some(agent) => format!("◌ {} working    ", agent.name),
            None => String::new(),
        };
        let right_text = format!(
            "{pending}    {agent}{suggestions}{}{dirty}    {} words    {}:{}    {hint}",
            ed.file_name(),
            ed.doc().words,
            line + 1,
            col + 1
        );
        let key = self.label(ed, right_text.trim_start(), c.status, size, Face::Mono, false);
        let width = self.layouts[&key].rows[0].width;
        // Yield to a long message rather than overlap it.
        if m.chrome + pad + left_width + pad < self.width - pad - width {
            self.put(layer, key, self.width - pad - width, middle, clip, c.status);
            let hint_width =
                hint.chars().count() as f32 * width / right_text.trim_start().chars().count().max(1) as f32;
            self.targets.push((
                [
                    self.width - pad - hint_width,
                    m.bottom,
                    hint_width + pad,
                    self.height - m.bottom,
                ],
                Target::MenuHint,
            ));
        }
    }

    /// The tab strip, shown once a second document is open.
    fn tabs(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        if ed.tab_count() < 2 {
            return;
        }
        let c = ed.theme.colors;
        let pad = (10.0 * self.scale).round();
        let gap = (4.0 * self.scale).round();
        let max_width = (180.0 * self.scale).round();
        let pill = (24.0 * self.scale).round();
        let middle = (m.top / 2.0 + 2.0 * self.scale).round();
        let tabs: Vec<(u64, f32, bool)> = ed
            .tabs()
            .iter()
            .map(|tab| {
                let title = tab.title.strip_suffix(".md").unwrap_or(&tab.title);
                let text = if tab.dirty {
                    format!("{title} •")
                } else {
                    title.to_string()
                };
                let rgb = if tab.active { c.text } else { c.status };
                let key = self.label(ed, &text, rgb, m.ui, Face::Ui, false);
                (key, self.layouts[&key].rows[0].width.min(max_width), tab.active)
            })
            .collect();
        let total: f32 = tabs.iter().map(|(_, width, _)| width + pad * 2.0 + gap).sum::<f32>() - gap;
        // Keep clear of the window buttons when nothing else is on the left.
        let floor = m.chrome
            + if m.chrome == 0.0 && self.titlebar > 0.0 {
                84.0 * self.scale
            } else {
                pad
            };
        let mut x = (m.chrome + (self.width - m.chrome - total) / 2.0).max(floor).round();
        for (index, (key, width, active)) in tabs.into_iter().enumerate() {
            let rect = [x, (middle - pill / 2.0).round(), width + pad * 2.0, pill];
            if active {
                layer.back.push(Quad {
                    rect,
                    color: linear(c.panel_active, 1.0),
                    radius: pill / 2.0,
                });
            }
            let clip = [x + pad, 0.0, x + pad + width, m.top];
            self.put(
                layer,
                key,
                x + pad,
                middle,
                clip,
                if active { c.text } else { c.status },
            );
            self.targets.push((rect, Target::Tab(index)));
            x += width + pad * 2.0 + gap;
        }
    }

    /// The file browser.
    fn sidebar(&mut self, ed: &mut Editor, m: &Metrics, layer: &mut Layer) {
        if !ed.sidebar.visible {
            return;
        }
        let c = ed.theme.colors;
        let width = m.chrome;
        layer.back.push(Quad {
            rect: [0.0, 0.0, width, self.height],
            color: linear(c.panel, 1.0),
            radius: 0.0,
        });
        layer.back.push(Quad {
            rect: [width - self.scale.max(1.0), 0.0, self.scale.max(1.0), self.height],
            color: linear(c.rule, 0.6),
            radius: 0.0,
        });
        self.targets.push(([0.0, 0.0, width, self.height], Target::Sidebar));

        let pad = (14.0 * self.scale).round();
        let indent = (14.0 * self.scale).round();
        let header_top = self.titlebar.max(10.0 * self.scale) + 6.0 * self.scale;
        let root = ed
            .sidebar
            .root
            .file_name()
            .map_or("/".to_string(), |name| name.to_string_lossy().to_uppercase());
        let key = self.label(ed, &root, c.status, m.ui * 0.82, Face::Ui, true);
        self.put(
            layer,
            key,
            pad,
            header_top + m.row / 2.0,
            [0.0, 0.0, width - pad, self.height],
            c.status,
        );

        let top = (header_top + m.row + 4.0 * self.scale).round();
        let visible = (((self.height - top) / m.row).floor() as usize).max(1);
        let count = ed.sidebar.entries.len();
        // Follow the selection when it moves; leave wheel scrolling alone.
        if ed.sidebar.selected != self.sidebar_selected {
            self.sidebar_selected = ed.sidebar.selected;
            if ed.sidebar.selected < ed.sidebar.scroll {
                ed.sidebar.scroll = ed.sidebar.selected;
            } else if ed.sidebar.selected >= ed.sidebar.scroll + visible {
                ed.sidebar.scroll = ed.sidebar.selected + 1 - visible;
            }
        }
        ed.sidebar.scroll = ed.sidebar.scroll.min(count.saturating_sub(visible));

        let inset = (6.0 * self.scale).round();
        for (slot, index) in (ed.sidebar.scroll..count).take(visible + 1).enumerate() {
            let entry = &ed.sidebar.entries[index];
            let y = top + slot as f32 * m.row;
            let rect = [inset, y, width - inset * 2.0, m.row];
            let open = ed.path.as_deref() == Some(entry.path.as_path());
            let selected = ed.sidebar.focused && index == ed.sidebar.selected;
            if selected || open {
                let tint = if selected {
                    linear(c.selection, 1.0)
                } else {
                    linear(c.panel_active, 1.0)
                };
                layer.back.push(Quad {
                    rect: [inset, y + self.scale, width - inset * 2.0, m.row - self.scale * 2.0],
                    color: tint,
                    radius: 6.0 * self.scale,
                });
            }
            let x = pad + entry.depth as f32 * indent;
            let middle = y + m.row / 2.0;
            let clip = [0.0, top, width - pad, self.height];
            let (name, rgb) = match entry.kind {
                EntryKind::Folder { .. } => (entry.name.clone(), c.text),
                EntryKind::Note => (entry.name.clone(), c.text),
                _ => (entry.name.clone(), c.status),
            };
            if let EntryKind::Folder { open } = entry.kind {
                let chevron = self.label(ed, if open { "▾" } else { "▸" }, c.status, m.ui * 1.25, Face::Ui, false);
                self.put(layer, chevron, x, middle, clip, c.status);
            }
            let folder = matches!(entry.kind, EntryKind::Folder { .. });
            let key = self.label(ed, &name, rgb, m.ui, Face::Ui, open || folder);
            self.put(layer, key, x + indent * 1.15, middle, clip, rgb);
            self.targets.push((rect, Target::SidebarRow(index)));
        }
        if count == 0 {
            let key = self.label(ed, "Empty folder", c.status, m.ui, Face::Ui, false);
            self.put(
                layer,
                key,
                pad,
                top + m.row / 2.0,
                [0.0, 0.0, width - pad, self.height],
                c.status,
            );
        }
    }

    /// The right-click menu, at the pointer or under the cursor.
    fn context_menu(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let Some(menu) = &ed.context_menu else { return };
        let c = ed.theme.colors;
        let s = self.scale;
        let row = (28.0 * s).round();
        let pad = (12.0 * s).round();
        let gap = (9.0 * s).round();
        let labels: Vec<(u64, Option<u64>)> = menu
            .items
            .iter()
            .map(|item| {
                let label = self.label(ed, &item.label, c.text, m.ui, Face::Ui, false);
                let key = item
                    .key
                    .map(|key| self.label(ed, &key.to_string(), c.status, m.ui * 0.92, Face::Mono, false));
                (label, key)
            })
            .collect();
        let widest = labels
            .iter()
            .map(|(label, _)| self.layouts[label].rows[0].width)
            .fold(0.0, f32::max);
        let width = (widest + pad * 2.0 + 44.0 * s).max(180.0 * s).round();
        let groups = menu.items.iter().filter(|item| item.group_start).count();
        let height = menu.items.len() as f32 * row + groups as f32 * gap + 12.0 * s;
        let (x, y) = match menu.at {
            MenuAt::Point(x, y) => (x, y),
            MenuAt::Cursor => self.cursor_point.unwrap_or((m.left, m.top_pad)),
        };
        // Keep it on screen: flip above the anchor if there is no room below.
        let left = x.min(self.width - width - 8.0 * s).max(8.0 * s).round();
        let top = if y + height + 8.0 * s > self.height {
            (y - height - m.line).max(8.0 * s)
        } else {
            y + 4.0 * s
        }
        .round();

        self.targets.push(([0.0, 0.0, self.width, self.height], Target::Scrim));
        let radius = 9.0 * s;
        layer.back.push(Quad {
            rect: [left - s, top - s, width + s * 2.0, height + s * 2.0],
            color: linear(c.rule, 1.0),
            radius: radius + s,
        });
        layer.back.push(Quad {
            rect: [left, top, width, height],
            color: linear(c.panel, 1.0),
            radius,
        });
        self.targets.push(([left, top, width, height], Target::ContextMenu));
        let clip = [left, top, left + width, top + height];
        let mut at = top + 6.0 * s;
        for (index, (item, (label, key))) in menu.items.iter().zip(labels).enumerate() {
            if item.group_start {
                layer.back.push(Quad {
                    rect: [left + pad, (at + gap / 2.0).round(), width - pad * 2.0, s.max(1.0)],
                    color: linear(c.rule, 0.8),
                    radius: 0.0,
                });
                at += gap;
            }
            let rect = [left + 5.0 * s, at, width - 10.0 * s, row];
            if index == menu.selected {
                layer.back.push(Quad {
                    rect,
                    color: linear(c.panel_active, 1.0),
                    radius: 6.0 * s,
                });
            }
            self.targets.push((rect, Target::ContextRow(index)));
            self.put(layer, label, left + pad, at + row / 2.0, clip, c.text);
            if let Some(key) = key {
                let key_width = self.layouts[&key].rows[0].width;
                self.put(
                    layer,
                    key,
                    left + width - pad - key_width,
                    at + row / 2.0,
                    clip,
                    c.status,
                );
            }
            at += row;
        }
    }

    /// The sheet: a card of settings that rises from the bottom of the
    /// window over a dimmed page, and sinks back when it is put away.
    fn sheet(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let now = Instant::now();
        // A screenshot wants the card where it comes to rest.
        let still = self.images.blocking;
        match (&ed.sheet, &mut self.risen) {
            (Some(sheet), Some(risen)) => {
                risen.sheet = sheet.clone();
                if risen.closed.take().is_some() {
                    risen.since = now;
                }
            }
            (Some(sheet), None) => {
                self.risen = Some(Risen {
                    sheet: sheet.clone(),
                    since: now,
                    closed: None,
                })
            }
            (None, Some(risen)) => {
                let closed = *risen.closed.get_or_insert(now);
                if still || closed.elapsed() >= SHEET_FALL {
                    self.risen = None;
                }
            }
            (None, None) => {}
        }
        let Some(risen) = &self.risen else { return };
        let ease = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
        let open = match risen.closed {
            _ if still => 1.0,
            Some(closed) => 1.0 - ease(closed.elapsed().as_secs_f32() / SHEET_FALL.as_secs_f32()),
            None => ease(risen.since.elapsed().as_secs_f32() / SHEET_RISE.as_secs_f32()),
        };
        let live = risen.closed.is_none();
        let sheet = risen.sheet.clone();

        let c = ed.theme.colors;
        let s = self.scale;
        let (pad, row, gap) = ((22.0 * s).round(), (38.0 * s).round(), (8.0 * s).round());
        let (title_height, status_height, button_height) = ((28.0 * s).round(), (28.0 * s).round(), (32.0 * s).round());
        let width = (580.0 * s).min(self.width - 24.0 * s).round();
        let left = ((self.width - width) / 2.0).round();
        let status = sheet.status.as_ref().map_or(0.0, |_| status_height);
        let height =
            pad + title_height + gap + sheet.rows.len() as f32 * row + status + gap * 2.0 + button_height + pad;
        let rest = self.height - height - 18.0 * s;
        let top = (rest + (self.height - rest + 8.0 * s) * (1.0 - open)).round();

        layer.back.push(Quad {
            rect: [0.0, 0.0, self.width, self.height],
            color: [0.0, 0.0, 0.0, 0.34 * open],
            radius: 0.0,
        });
        let radius = 16.0 * s;
        layer.back.push(Quad {
            rect: [left - s, top - s, width + s * 2.0, height + s * 2.0],
            color: linear(c.rule, 1.0),
            radius: radius + s,
        });
        layer.back.push(Quad {
            rect: [left, top, width, height],
            color: linear(c.panel, 1.0),
            radius,
        });
        if live {
            self.targets
                .push(([0.0, 0.0, self.width, self.height], Target::SheetScrim));
            self.targets.push(([left, top, width, height], Target::Sheet));
        }
        let clip = [left, top.max(0.0), left + width, (top + height).min(self.height)];
        let title = self.label(ed, &sheet.title, c.text, m.ui * 1.12, Face::Ui, true);
        self.put(layer, title, left + pad, top + pad + title_height / 2.0, clip, c.text);

        let labels: Vec<u64> = sheet
            .rows
            .iter()
            .map(|row| self.label(ed, &row.label, c.status, m.ui, Face::Ui, false))
            .collect();
        let widest = labels
            .iter()
            .map(|key| self.layouts[key].rows[0].width)
            .fold(0.0, f32::max);
        let column = if widest > 0.0 { (widest + 18.0 * s).round() } else { 0.0 };
        let hairline = s.max(1.0);
        let mut y = top + pad + title_height + gap;
        for (index, (item, label)) in sheet.rows.iter().zip(labels).enumerate() {
            let middle = y + row / 2.0;
            let focused = live && index == sheet.focus && !sheet.busy;
            // A row with nothing to call it uses the whole width.
            let named = !item.label.is_empty();
            let x = left + pad + if named { column } else { 0.0 };
            let room = left + width - pad - x;
            if named {
                self.put(layer, label, left + pad, middle, clip, c.status);
            }
            match &item.field {
                Field::Note(words) => {
                    let key = self.label(ed, words, c.status, m.ui * 0.94, Face::Ui, false);
                    self.put(layer, key, x, middle, [x, clip[1], x + room, clip[3]], c.status);
                }
                Field::Text { value, hint, secret } => {
                    let frame = [x, y + 4.0 * s, room, row - 8.0 * s];
                    layer.back.push(Quad {
                        rect: frame,
                        color: linear(if focused { c.cursor } else { c.rule }, 1.0),
                        radius: 8.0 * s,
                    });
                    layer.back.push(Quad {
                        rect: [
                            frame[0] + hairline,
                            frame[1] + hairline,
                            room - hairline * 2.0,
                            frame[3] - hairline * 2.0,
                        ],
                        color: linear(c.background, 1.0),
                        radius: 8.0 * s - hairline,
                    });
                    let (shown, rgb) = match (value.is_empty(), secret) {
                        (true, _) => (hint.clone(), c.status),
                        // However long it is, it reads the same.
                        (false, true) => ("•".repeat(value.chars().count().min(24)), c.text),
                        (false, false) => (value.clone(), c.text),
                    };
                    let key = self.label(ed, &shown, rgb, m.ui, Face::Ui, false);
                    let inset = 10.0 * s;
                    let typed = if value.is_empty() {
                        0.0
                    } else {
                        self.layouts[&key].rows[0].width
                    };
                    // Too long for the field: its end stays in sight.
                    let start = x + inset - (typed - (room - inset * 2.0)).max(0.0);
                    let inside = [x + inset, clip[1], x + room - inset, clip[3]];
                    self.put(layer, key, start, middle, inside, rgb);
                    if focused {
                        layer.front.push(Quad {
                            rect: [
                                (start + typed + s).round(),
                                y + 10.0 * s,
                                (1.5 * s).round().max(1.0),
                                row - 20.0 * s,
                            ],
                            color: linear(c.cursor, 1.0),
                            radius: 0.0,
                        });
                    }
                    if live {
                        self.targets.push((frame, Target::SheetRow(index)));
                    }
                }
                Field::Choice { options, chosen } => {
                    let mut at = x;
                    for (option, name) in options.iter().enumerate() {
                        let rgb = if option == *chosen { c.text } else { c.status };
                        let key = self.label(ed, name, rgb, m.ui, Face::Ui, option == *chosen);
                        let pill = [
                            at,
                            y + 5.0 * s,
                            self.layouts[&key].rows[0].width + 24.0 * s,
                            row - 10.0 * s,
                        ];
                        if option == *chosen {
                            layer.back.push(Quad {
                                rect: pill,
                                color: linear(if focused { c.cursor } else { c.rule }, 1.0),
                                radius: pill[3] / 2.0,
                            });
                            layer.back.push(Quad {
                                rect: [
                                    pill[0] + hairline,
                                    pill[1] + hairline,
                                    pill[2] - hairline * 2.0,
                                    pill[3] - hairline * 2.0,
                                ],
                                color: linear(c.panel_active, 1.0),
                                radius: pill[3] / 2.0 - hairline,
                            });
                        }
                        self.put(layer, key, at + 12.0 * s, middle, clip, rgb);
                        if live {
                            self.targets.push((pill, Target::SheetOption(index, option)));
                        }
                        at += pill[2] + 4.0 * s;
                    }
                }
            }
            y += row;
        }
        if let Some((words, bad)) = &sheet.status {
            let rgb = if *bad { c.error } else { c.status };
            let key = self.label(ed, words, rgb, m.ui * 0.94, Face::Ui, false);
            let inside = [left + pad, clip[1], left + width - pad, clip[3]];
            self.put(layer, key, left + pad, y + status_height / 2.0, inside, rgb);
        }

        // Buttons from the right: the one Enter presses is filled in.
        let baseline = top + height - pad - button_height;
        let mut right = left + width - pad;
        let last = sheet.buttons.len().saturating_sub(1);
        for (index, button) in sheet.buttons.iter().enumerate().rev() {
            let ready = button.enabled && !sheet.busy;
            let chief = index == last;
            let rgb = match (chief, ready) {
                (true, _) => c.background,
                (false, true) => c.text,
                (false, false) => c.status,
            };
            let key = self.label(ed, &button.label, rgb, m.ui, Face::Ui, chief);
            let wide = self.layouts[&key].rows[0].width + 30.0 * s;
            let rect = [right - wide, baseline, wide, button_height];
            let fill = if chief { c.cursor } else { c.panel_active };
            layer.back.push(Quad {
                rect,
                color: linear(fill, if ready { 1.0 } else { 0.4 }),
                radius: 9.0 * s,
            });
            self.put(
                layer,
                key,
                rect[0] + 15.0 * s,
                baseline + button_height / 2.0,
                clip,
                rgb,
            );
            if live {
                self.targets.push((rect, Target::SheetButton(index)));
            }
            right -= wide + 8.0 * s;
        }
    }

    /// The picture being retouched: the rest of the window dimmed, the
    /// picture grown out of the text into the middle, the tools above it
    /// and their settings below.
    fn retouching(&mut self, ed: &Editor, ui: f32, layer: &mut Layer) {
        let Some(mut retouch) = self.retouch.take() else { return };
        let s = self.scale;
        let open = retouch.openness();
        let black = |alpha: f32| [0.0, 0.0, 0.0, alpha];
        layer.back.push(Quad {
            rect: [0.0, 0.0, self.width, self.height],
            color: black(0.78 * open),
            radius: 0.0,
        });
        layer.image_clip = [0.0, 0.0, self.width, self.height];

        let bar = (40.0 * s).round();
        let gap = (14.0 * s).round();
        let above = (self.titlebar + 10.0 * s).round();
        let below = (self.height - bar - 14.0 * s).round();
        let room = [
            32.0 * s,
            above + bar + gap,
            (self.width - 64.0 * s).max(1.0),
            (below - gap - (above + bar + gap)).max(1.0),
        ];
        let (width, height) = (retouch.shown.0.max(1) as f32, retouch.shown.1.max(1) as f32);
        // A small picture is enlarged, but not into a blur.
        let fit = (room[2] / width).min(room[3] / height).min(4.0);
        let size = ((width * fit).round(), (height * fit).round());
        retouch.place = [
            (room[0] + (room[2] - size.0) / 2.0).round(),
            (room[1] + (room[3] - size.1) / 2.0).round(),
            size.0,
            size.1,
        ];
        if retouch.still {
            retouch.from = retouch.place;
        }
        let (from, place) = (retouch.from, retouch.place);
        let rect: [f32; 4] = std::array::from_fn(|at| from[at] + (place[at] - from[at]) * open);
        layer.images.push((retouch::TEXTURE, rect));

        retouch.buttons.clear();
        // The tools wait until the picture has nearly arrived.
        if open > 0.85 && !retouch.closing() {
            self.retouch_marks(ed, &retouch, layer);
            self.retouch_tools(ed, &mut retouch, layer, [above, bar, ui]);
            self.retouch_settings(ed, &mut retouch, layer, [below, bar, ui]);
        }
        self.retouch = Some(retouch);
    }

    /// What is drawn over the picture itself: the crop being taken, the
    /// outline of the selected mark, the caret of a label being typed.
    fn retouch_marks(&mut self, ed: &Editor, retouch: &Retouch, layer: &mut Layer) {
        let c = ed.theme.colors;
        let hairline = (1.5 * self.scale).round().max(1.0);
        let outline = |layer: &mut Layer, [x, y, w, h]: [f32; 4], color: [f32; 4]| {
            for rect in [
                [x - hairline, y - hairline, w + hairline * 2.0, hairline],
                [x - hairline, y + h, w + hairline * 2.0, hairline],
                [x - hairline, y, hairline, h],
                [x + w, y, hairline, h],
            ] {
                layer.front.push(Quad {
                    rect,
                    color,
                    radius: 0.0,
                });
            }
        };
        let crop = retouch
            .cropping
            .or(retouch.marks.crop)
            .filter(|_| retouch.tool == Tool::Crop);
        if let Some(crop) = crop {
            let [left, top, width, height] = retouch.place;
            let [x, y, w, h] = retouch.on_screen(crop);
            // Kept inside the picture, as the crop itself will be.
            let (x0, y0) = (x.clamp(left, left + width), y.clamp(top, top + height));
            let (x1, y1) = ((x + w).clamp(left, left + width), (y + h).clamp(top, top + height));
            for rect in [
                [left, top, width, y0 - top],
                [left, y1, width, top + height - y1],
                [left, y0, x0 - left, y1 - y0],
                [x1, y0, left + width - x1, y1 - y0],
            ] {
                layer.front.push(Quad {
                    rect,
                    color: [0.0, 0.0, 0.0, 0.6],
                    radius: 0.0,
                });
            }
            outline(layer, [x0, y0, x1 - x0, y1 - y0], [1.0, 1.0, 1.0, 0.9]);
        }
        let (selected, caret) = retouch.outlines(&mut self.fonts);
        if let Some(selected) = selected {
            // A little clear of the mark, so it is not mistaken for one.
            let [x, y, w, h] = retouch.on_screen(selected);
            let clear = hairline * 3.0;
            let around = [x - clear, y - clear, w + clear * 2.0, h + clear * 2.0];
            outline(layer, around, [1.0, 1.0, 1.0, 0.95]);
        }
        if let Some(caret) = caret {
            let [x, y, w, h] = retouch.on_screen(caret);
            layer.front.push(Quad {
                rect: [x + hairline, y, w.max(hairline), h],
                color: linear(c.cursor, 1.0),
                radius: 0.0,
            });
        }
    }

    /// A rounded bar of the interface's own colour, centred on the window.
    fn retouch_bar(&mut self, ed: &Editor, layer: &mut Layer, top: f32, width: f32, height: f32) -> f32 {
        let c = ed.theme.colors;
        let s = self.scale;
        let left = ((self.width - width) / 2.0).max(4.0 * s).round();
        let radius = 10.0 * s;
        layer.back.push(Quad {
            rect: [left - s, top - s, width + s * 2.0, height + s * 2.0],
            color: linear(c.rule, 1.0),
            radius: radius + s,
        });
        layer.back.push(Quad {
            rect: [left, top, width, height],
            color: linear(c.panel, 1.0),
            radius,
        });
        left
    }

    /// The tools, each with the key that picks it.
    fn retouch_tools(&mut self, ed: &Editor, retouch: &mut Retouch, layer: &mut Layer, bar: [f32; 3]) {
        let [top, bar, size] = bar;
        let c = ed.theme.colors;
        let s = self.scale;
        let (pad, inner, between) = ((11.0 * s).round(), (5.0 * s).round(), (6.0 * s).round());
        let items: Vec<(Tool, u64, u64)> = retouch::TOOLS
            .iter()
            .map(|(tool, name, key)| {
                let name = self.label(ed, name, c.text, size, Face::Ui, *tool == retouch.tool);
                let hint = key.to_ascii_uppercase().to_string();
                (
                    *tool,
                    name,
                    self.label(ed, &hint, c.status, size * 0.8, Face::Ui, false),
                )
            })
            .collect();
        let widths = |view: &View, hints: bool| -> Vec<f32> {
            let of = |key: &u64| view.layouts[key].rows[0].width;
            let item = |(_, name, hint): &(Tool, u64, u64)| {
                of(name) + pad * 2.0 + if hints { of(hint) + between } else { 0.0 }
            };
            items.iter().map(item).collect()
        };
        // In a narrow window the keys go, and the names stay.
        let mut hints = true;
        let mut each = widths(self, hints);
        if each.iter().sum::<f32>() + inner * 2.0 > self.width - 16.0 * s {
            hints = false;
            each = widths(self, hints);
        }
        let width = each.iter().sum::<f32>() + inner * 2.0;
        let left = self.retouch_bar(ed, layer, top, width, bar);
        let clip = [left, top, left + width, top + bar];
        let middle = top + bar / 2.0;
        let mut x = left + inner;
        for ((tool, name, hint), item) in items.into_iter().zip(each) {
            let rect = [x, top + inner, item, bar - inner * 2.0];
            if tool == retouch.tool {
                layer.back.push(Quad {
                    rect,
                    color: linear(c.panel_active, 1.0),
                    radius: 6.0 * s,
                });
            }
            retouch.buttons.push((rect, Button::Tool(tool)));
            let name_width = self.put(layer, name, x + pad, middle, clip, c.text);
            if hints {
                self.put(layer, hint, x + pad + name_width + between, middle + s, clip, c.status);
            }
            x += item;
        }
    }

    /// Colour and stroke, then undo, redo and done.
    fn retouch_settings(&mut self, ed: &Editor, retouch: &mut Retouch, layer: &mut Layer, bar: [f32; 3]) {
        let [top, bar, size] = bar;
        let c = ed.theme.colors;
        let s = self.scale;
        let (pad, inner) = ((11.0 * s).round(), (5.0 * s).round());
        let cell = bar - inner * 2.0;
        let rule = (13.0 * s).round();
        let words: Vec<(Button, u64, Rgb)> = [
            (Button::Undo, "Undo", c.text, false),
            (Button::Redo, "Redo", c.text, false),
            (Button::Done, "Done", c.cursor, true),
        ]
        .into_iter()
        .map(|(button, word, rgb, bold)| (button, self.label(ed, word, rgb, size, Face::Ui, bold), rgb))
        .collect();
        let word_width = |view: &View, key: &u64| view.layouts[key].rows[0].width + pad * 2.0;
        let words_width: f32 = words.iter().map(|(_, key, _)| word_width(self, key)).sum();
        let swatches = retouch::COLORS.len() as f32 * cell;
        let strokes = 3.0 * cell;
        let width = inner * 2.0 + swatches + rule + strokes + rule + words_width;
        let left = self.retouch_bar(ed, layer, top, width, bar);
        let clip = [left, top, left + width, top + bar];
        let middle = top + bar / 2.0;
        let mut x = left + inner;
        let chosen = |layer: &mut Layer, rect: [f32; 4]| {
            layer.back.push(Quad {
                rect,
                color: linear(c.panel_active, 1.0),
                radius: 6.0 * s,
            })
        };
        let dot = |layer: &mut Layer, x: f32, diameter: f32, color: [f32; 4]| {
            layer.back.push(Quad {
                rect: [
                    (x - diameter / 2.0).round(),
                    (middle - diameter / 2.0).round(),
                    diameter,
                    diameter,
                ],
                color,
                radius: diameter / 2.0,
            })
        };
        for (index, (_, rgb)) in retouch::COLORS.iter().enumerate() {
            let rect = [x, top + inner, cell, cell];
            if index == retouch.color {
                chosen(layer, rect);
            }
            // A ring, so white shows on a light bar and black on a dark one.
            dot(layer, x + cell / 2.0, (18.0 * s).round(), linear(c.rule, 1.0));
            dot(
                layer,
                x + cell / 2.0,
                (15.0 * s).round(),
                linear(Rgb(rgb[0], rgb[1], rgb[2]), 1.0),
            );
            retouch.buttons.push((rect, Button::Color(index)));
            x += cell;
        }
        let divider = |layer: &mut Layer, x: f32| {
            layer.back.push(Quad {
                rect: [
                    (x + rule / 2.0).round(),
                    top + inner * 2.0,
                    s.max(1.0),
                    bar - inner * 4.0,
                ],
                color: linear(c.rule, 1.0),
                radius: 0.0,
            })
        };
        divider(layer, x);
        x += rule;
        for (index, diameter) in [5.0, 9.0, 14.0].into_iter().enumerate() {
            let rect = [x, top + inner, cell, cell];
            if index == retouch.size {
                chosen(layer, rect);
            }
            dot(layer, x + cell / 2.0, (diameter * s).round(), linear(c.text, 1.0));
            retouch.buttons.push((rect, Button::Size(index)));
            x += cell;
        }
        divider(layer, x);
        x += rule;
        for (button, key, rgb) in words {
            let item = word_width(self, &key);
            retouch.buttons.push(([x, top + inner, item, cell], button));
            self.put(layer, key, x + pad, middle, clip, rgb);
            x += item;
        }
    }

    /// The pop-up menu, drawn above everything.
    fn palette(&mut self, ed: &Editor, m: &Metrics, layer: &mut Layer) {
        let Some(palette) = &ed.palette else {
            self.palette_scroll = 0;
            return;
        };
        const ROWS: usize = 11;
        let c = ed.theme.colors;
        let s = self.scale;
        let width = (640.0 * s).min(self.width - 32.0 * s).round();
        let left = ((self.width - width) / 2.0).round();
        let top = (self.height * 0.14).max(self.titlebar + 12.0 * s).round();
        let input_height = (48.0 * s).round();
        let row = (32.0 * s).round();
        let header = (26.0 * s).round();
        let footer = (30.0 * s).round();
        let pad = (16.0 * s).round();

        let count = palette.matches.len();
        if palette.selected < self.palette_scroll {
            self.palette_scroll = palette.selected;
        } else if palette.selected >= self.palette_scroll + ROWS {
            self.palette_scroll = palette.selected + 1 - ROWS;
        }
        self.palette_scroll = self.palette_scroll.min(count.saturating_sub(ROWS));
        let shown: Vec<usize> = (self.palette_scroll..count).take(ROWS).collect();
        let sectioned = palette.kind == PaletteKind::Help;
        let starts_section = |slot: usize| {
            sectioned
                && (slot == 0
                    || palette.items[palette.matches[shown[slot]]].section
                        != palette.items[palette.matches[shown[slot - 1]]].section)
        };
        let headers = (0..shown.len()).filter(|&slot| starts_section(slot)).count();
        let list_height = (shown.len().max(1) as f32 * row + headers as f32 * header).round();
        let height = input_height + list_height + footer + 8.0 * s;

        layer.back.push(Quad {
            rect: [0.0, 0.0, self.width, self.height],
            color: [0.0, 0.0, 0.0, 0.28],
            radius: 0.0,
        });
        self.targets.push(([0.0, 0.0, self.width, self.height], Target::Scrim));
        let radius = 12.0 * s;
        layer.back.push(Quad {
            rect: [left - s, top - s, width + s * 2.0, height + s * 2.0],
            color: linear(c.rule, 1.0),
            radius: radius + s,
        });
        layer.back.push(Quad {
            rect: [left, top, width, height],
            color: linear(c.panel, 1.0),
            radius,
        });
        self.targets.push(([left, top, width, height], Target::Palette));
        let clip = [left + pad, top, left + width - pad, top + height];

        // The search field.
        let middle = top + input_height / 2.0;
        let (text, rgb) = if palette.query.is_empty() {
            (palette.placeholder.as_str(), c.status)
        } else {
            (palette.query.as_str(), c.text)
        };
        let key = self.label(ed, text, rgb, m.ui * 1.2, Face::Ui, false);
        let bar = (2.0 * s).round().max(1.0);
        let typed = self.put(layer, key, left + pad + bar * 2.0, middle, clip, rgb);
        let caret = if palette.query.is_empty() {
            0.0
        } else {
            typed + bar * 3.0
        };
        layer.front.push(Quad {
            rect: [
                left + pad + caret,
                (middle - m.ui * 0.8).round(),
                bar,
                (m.ui * 1.6).round(),
            ],
            color: linear(c.cursor, 1.0),
            radius: bar / 2.0,
        });
        layer.back.push(Quad {
            rect: [left, top + input_height - s, width, s.max(1.0)],
            color: linear(c.rule, 0.7),
            radius: 0.0,
        });

        let mut y = top + input_height + 4.0 * s;
        for (slot, &at) in shown.iter().enumerate() {
            let item = &palette.items[palette.matches[at]];
            if starts_section(slot) {
                let key = self.label(ed, &item.section.to_uppercase(), c.status, m.ui * 0.8, Face::Ui, true);
                self.put(layer, key, left + pad, y + header * 0.58, clip, c.status);
                y += header;
            }
            let rect = [left + 6.0 * s, y, width - 12.0 * s, row];
            if at == palette.selected {
                layer.back.push(Quad {
                    rect,
                    color: linear(c.panel_active, 1.0),
                    radius: 7.0 * s,
                });
            }
            self.targets.push((rect, Target::PaletteRow(at)));
            let middle = y + row / 2.0;
            let face = if sectioned && item.section != "Commands" {
                Face::Mono
            } else {
                Face::Ui
            };
            let detail = self.label(ed, &item.detail, c.status, m.ui * 0.92, face, false);
            let detail_width = self.layouts[&detail].rows[0].width.min(width * 0.62);
            let detail_left = left + width - pad - detail_width;
            self.put(
                layer,
                detail,
                detail_left,
                middle,
                [detail_left, top, left + width - pad, top + height],
                c.status,
            );
            let key = self.label(ed, &item.label, c.text, m.ui * 1.05, Face::Ui, false);
            self.put(
                layer,
                key,
                left + pad,
                middle,
                [left + pad, top, detail_left - pad * 0.5, top + height],
                c.text,
            );
            y += row;
        }
        if shown.is_empty() {
            let key = self.label(ed, "No matches", c.status, m.ui * 1.05, Face::Ui, false);
            self.put(layer, key, left + pad, y + row / 2.0, clip, c.status);
            y += row;
        }
        let position = if count > ROWS {
            format!("{} of {count}    ", palette.selected + 1)
        } else {
            String::new()
        };
        let hint = format!("{position}↑↓ move    ↵ choose    esc close");
        let key = self.label(ed, &hint, c.status, m.ui * 0.85, Face::Ui, false);
        let hint_width = self.layouts[&key].rows[0].width;
        self.put(
            layer,
            key,
            left + width - pad - hint_width,
            y + 4.0 * s + footer / 2.0,
            clip,
            c.status,
        );
    }
}
