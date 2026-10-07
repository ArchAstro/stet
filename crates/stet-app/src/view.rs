//! Turns editor state into a `Frame`, and answers the layout questions the
//! core cannot: display-line motion, hit testing and scrolling.

use crate::gpu::linear;
use crate::images::{self, Images};
use crate::text::{Face, Fonts, Layer as Depth, LineLayout, LineSpec, color, layout_line};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use stet_core::editor::{CmdKind, EntryKind, MenuAt, PaletteKind};
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
    /// The dimmed area around the menu.
    Scrim,
    MenuHint,
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
    focus: Option<Range<usize>>,
    targets: Vec<([f32; 4], Target)>,
    sidebar_selected: usize,
    palette_scroll: usize,
    /// Bottom-left of the text cursor in the last frame, if it was on screen.
    cursor_point: Option<(f32, f32)>,
    /// Margin sections and where they are pinned, for this frame.
    pins: Vec<stet_core::editor::Pin>,
    /// Window y of each pin's anchor, for the pins on screen this frame.
    pin_tops: Vec<(usize, f32)>,
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
            focus: None,
            targets: Vec::new(),
            sidebar_selected: usize::MAX,
            palette_scroll: 0,
            cursor_point: None,
            pins: Vec::new(),
            pin_tops: Vec::new(),
        }
    }

    /// Drops shaped lines, after anything that changes how text looks.
    pub fn invalidate(&mut self) {
        self.layouts.clear();
        self.advance = None;
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
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
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
            layout_line(
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
                },
            )
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

    /// Images shown under `line`: `(id, width, height)` in device pixels.
    fn line_images(&mut self, ed: &Editor, line: usize, m: &Metrics) -> Vec<(u64, f32, f32)> {
        if !ed.config.images {
            return Vec::new();
        }
        let max_height = (self.height * 0.6).max(m.line);
        ed.doc()
            .images_on(line)
            .filter_map(|image| images::resolve(&image.url, ed.path.as_deref(), ed.config.remote_images))
            .filter_map(|source| self.images.get(&source))
            .map(|(id, width, height)| {
                let (width, height) = (width as f32 * self.scale, height as f32 * self.scale);
                let fit = (m.column / width).min(max_height / height).min(1.0);
                (id, (width * fit).round(), (height * fit).round())
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
            .map(|(_, _, height)| height + Self::image_gap(m))
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

    /// Scrolls the file browser by rows.
    pub fn scroll_sidebar(&mut self, ed: &mut Editor, rows: isize) {
        let last = ed.sidebar.entries.len().saturating_sub(1) as isize;
        ed.sidebar.scroll = (ed.sidebar.scroll as isize + rows).clamp(0, last) as usize;
    }

    pub fn frame(&mut self, ed: &mut Editor) -> Frame {
        ed.refresh();
        self.frame += 1;
        self.targets.clear();
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

        self.status(ed, &m, &mut frame.base);
        self.tabs(ed, &m, &mut frame.base);
        self.sidebar(ed, &m, &mut frame.base);
        self.palette(ed, &m, &mut frame.over);
        self.context_menu(ed, &m, &mut frame.over);
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
            for (id, width, height) in images {
                let gap = Self::image_gap(&m);
                if y < m.bottom {
                    layer.images.push((id, [m.left, y + gap * 0.5, width, height]));
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
