//! Touching up a picture without leaving the document: crop it, draw on
//! it, label it, hide part of it. What was done is a list of marks over the
//! original, so a hand on the mouse and a program on `stet ctl` make the
//! same picture the same way.

use crate::images::Decoded;
use crate::text::Fonts;
use image::RgbaImage;
use serde_json::Value;
use std::time::{Duration, Instant};
use stet_core::{Key, KeyEvent};

pub type Point = (f32, f32);
/// `x, y, width, height`, in the picture's own pixels.
pub type Rect = [f32; 4];

/// The texture the picture being retouched is drawn from.
pub const TEXTURE: u64 = u64::MAX;
/// Nothing longer or wider is taken on.
const MAX_SIDE: u32 = 8192;
const OPEN: Duration = Duration::from_millis(220);
const CLOSE: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Pen(Vec<Point>),
    /// A wide, see-through stroke.
    Marker(Vec<Point>),
    Arrow(Point, Point),
    Rect(Rect),
    Ellipse(Rect),
    /// Its top left corner, and what it says.
    Text(Point, String),
    /// What is under it becomes a mosaic.
    Redact(Rect),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub shape: Shape,
    pub color: [u8; 3],
    /// The stroke's width, or the height of the type.
    pub size: f32,
}

/// Everything done to a picture, in the order it was done.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    pub marks: Vec<Mark>,
    /// What is kept of the picture, in the original's pixels.
    pub crop: Option<Rect>,
}

pub const COLORS: [(&str, [u8; 3]); 6] = [
    ("red", [255, 59, 48]),
    ("yellow", [255, 204, 0]),
    ("green", [52, 199, 89]),
    ("blue", [0, 122, 255]),
    ("black", [17, 17, 17]),
    ("white", [255, 255, 255]),
];

/// Stroke widths on offer, as multiples of the picture's unit.
const SIZES: [f32; 3] = [0.6, 1.0, 2.0];

/// A stroke that reads well on a picture this large.
fn unit(width: u32, height: u32) -> f32 {
    (width.max(height) as f32 / 320.0).max(1.5)
}

/// How much larger than a stroke this kind of mark is.
fn factor(tool: Tool) -> f32 {
    match tool {
        Tool::Marker => 5.0,
        Tool::Text => 6.0,
        _ => 1.0,
    }
}

// ----- drawing ------------------------------------------------------------

/// How much of each pixel a shape covers, over the part of the picture it
/// can reach. A stroke that crosses itself is still laid down once.
struct Mask {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    cover: Vec<u8>,
}

impl Mask {
    fn new(picture: &RgbaImage, bounds: Rect) -> Mask {
        let left = bounds[0].floor().clamp(0.0, picture.width() as f32) as u32;
        let top = bounds[1].floor().clamp(0.0, picture.height() as f32) as u32;
        let right = (bounds[0] + bounds[2]).ceil().clamp(0.0, picture.width() as f32) as u32;
        let bottom = (bounds[1] + bounds[3]).ceil().clamp(0.0, picture.height() as f32) as u32;
        let (width, height) = (right.saturating_sub(left), bottom.saturating_sub(top));
        Mask {
            left,
            top,
            width,
            height,
            cover: vec![0; (width * height) as usize],
        }
    }

    /// Raises the cover of every pixel in `bounds` to what `cover` says.
    fn fill(&mut self, bounds: Rect, cover: impl Fn(f32, f32) -> f32) {
        let x0 = (bounds[0].floor().max(self.left as f32) as u32).min(self.left + self.width);
        let y0 = (bounds[1].floor().max(self.top as f32) as u32).min(self.top + self.height);
        let x1 = ((bounds[0] + bounds[2]).ceil().max(0.0) as u32).min(self.left + self.width);
        let y1 = ((bounds[1] + bounds[3]).ceil().max(0.0) as u32).min(self.top + self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let value = (cover(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
                let at = ((y - self.top) * self.width + (x - self.left)) as usize;
                self.cover[at] = self.cover[at].max(value);
            }
        }
    }

    /// A line from `a` to `b` with round ends.
    fn stroke(&mut self, a: Point, b: Point, width: f32) {
        let radius = width / 2.0;
        let bounds = [
            a.0.min(b.0) - radius - 1.0,
            a.1.min(b.1) - radius - 1.0,
            (a.0 - b.0).abs() + width + 2.0,
            (a.1 - b.1).abs() + width + 2.0,
        ];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx * dx + dy * dy;
        self.fill(bounds, |x, y| {
            let along = if length > 0.0 {
                (((x - a.0) * dx + (y - a.1) * dy) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            radius - (x - a.0 - dx * along).hypot(y - a.1 - dy * along) + 0.5
        });
    }

    fn path(&mut self, points: &[Point], width: f32) {
        match points {
            [only] => self.stroke(*only, *only, width),
            _ => points.windows(2).for_each(|pair| self.stroke(pair[0], pair[1], width)),
        }
    }

    fn triangle(&mut self, corners: [Point; 3]) {
        let xs = corners.map(|corner| corner.0);
        let ys = corners.map(|corner| corner.1);
        let (left, top) = (
            xs.iter().copied().fold(f32::MAX, f32::min),
            ys.iter().copied().fold(f32::MAX, f32::min),
        );
        let (right, bottom) = (
            xs.iter().copied().fold(f32::MIN, f32::max),
            ys.iter().copied().fold(f32::MIN, f32::max),
        );
        // Whichever way round the corners were given.
        let turn = ((corners[1].0 - corners[0].0) * (corners[2].1 - corners[0].1)
            - (corners[1].1 - corners[0].1) * (corners[2].0 - corners[0].0))
            .signum();
        self.fill(
            [left - 1.0, top - 1.0, right - left + 2.0, bottom - top + 2.0],
            |x, y| {
                let inside = (0..3)
                    .map(|at| {
                        let (a, b) = (corners[at], corners[(at + 1) % 3]);
                        let edge = (b.0 - a.0).hypot(b.1 - a.1).max(f32::EPSILON);
                        turn * ((b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)) / edge
                    })
                    .fold(f32::MAX, f32::min);
                inside + 0.5
            },
        );
    }

    fn paint(&self, picture: &mut RgbaImage, color: [u8; 3], strength: f32) {
        for y in 0..self.height {
            for x in 0..self.width {
                let cover = self.cover[(y * self.width + x) as usize];
                if cover > 0 {
                    blend(
                        picture.get_pixel_mut(self.left + x, self.top + y),
                        color,
                        cover as f32 / 255.0 * strength,
                    );
                }
            }
        }
    }
}

fn blend(pixel: &mut image::Rgba<u8>, color: [u8; 3], alpha: f32) {
    for (channel, ink) in pixel.0[..3].iter_mut().zip(color) {
        *channel = (*channel as f32 + (ink as f32 - *channel as f32) * alpha).round() as u8;
    }
    pixel.0[3] = (pixel.0[3] as f32 + (255.0 - pixel.0[3] as f32) * alpha).round() as u8;
}

fn corners(rect: Rect) -> [Point; 5] {
    let [x, y, width, height] = rect;
    [(x, y), (x + width, y), (x + width, y + height), (x, y + height), (x, y)]
}

fn oval(rect: Rect) -> Vec<Point> {
    let (rx, ry) = (rect[2] / 2.0, rect[3] / 2.0);
    let steps = ((rx + ry) * 0.8).clamp(24.0, 360.0) as usize;
    (0..=steps)
        .map(|step| {
            let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
            (rect[0] + rx + rx * angle.cos(), rect[1] + ry + ry * angle.sin())
        })
        .collect()
}

fn around(points: &[Point], pad: f32) -> Rect {
    let fold = |pick: fn(&Point) -> f32, least: bool| {
        points.iter().map(pick).fold(
            if least { f32::MAX } else { f32::MIN },
            if least { f32::min } else { f32::max },
        )
    };
    let (left, top) = (fold(|point| point.0, true), fold(|point| point.1, true));
    let (right, bottom) = (fold(|point| point.0, false), fold(|point| point.1, false));
    [
        left - pad,
        top - pad,
        right - left + pad * 2.0,
        bottom - top + pad * 2.0,
    ]
}

/// A rectangle dragged in any direction, the right way round.
fn upright(a: Point, b: Point) -> Rect {
    [a.0.min(b.0), a.1.min(b.1), (a.0 - b.0).abs(), (a.1 - b.1).abs()]
}

/// How wide and tall a line of type comes out.
fn type_size(fonts: &mut Fonts, text: &str, size: f32) -> (f32, f32) {
    let buffer = crate::text::shape_label(fonts, text, size);
    let width = buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max);
    (width, (size * 1.25).round())
}

impl Mark {
    /// The part of the picture this mark touches.
    fn bounds(&self, fonts: &mut Fonts) -> Rect {
        let half = self.size / 2.0 + 1.0;
        match &self.shape {
            Shape::Pen(points) | Shape::Marker(points) => around(points, half),
            Shape::Arrow(from, to) => around(&[*from, *to], self.size * 2.4 + 1.0),
            Shape::Rect(rect) | Shape::Ellipse(rect) => around(&corners(*rect), half),
            Shape::Redact(rect) => *rect,
            Shape::Text(at, text) => {
                let (width, height) = type_size(fonts, text, self.size);
                [at.0, at.1, width.max(self.size * 0.5), height]
            }
        }
    }

    fn nudge(&mut self, dx: f32, dy: f32) {
        let shift = |point: &mut Point| *point = (point.0 + dx, point.1 + dy);
        match &mut self.shape {
            Shape::Pen(points) | Shape::Marker(points) => points.iter_mut().for_each(shift),
            Shape::Arrow(from, to) => {
                shift(from);
                shift(to);
            }
            Shape::Rect(rect) | Shape::Ellipse(rect) | Shape::Redact(rect) => {
                rect[0] += dx;
                rect[1] += dy;
            }
            Shape::Text(at, _) => shift(at),
        }
    }

    fn tool(&self) -> Tool {
        match self.shape {
            Shape::Pen(_) => Tool::Pen,
            Shape::Marker(_) => Tool::Marker,
            Shape::Arrow(..) => Tool::Arrow,
            Shape::Rect(_) => Tool::Rect,
            Shape::Ellipse(_) => Tool::Ellipse,
            Shape::Text(..) => Tool::Text,
            Shape::Redact(_) => Tool::Redact,
        }
    }

    fn draw(&self, picture: &mut RgbaImage, fonts: &mut Fonts) {
        let bounds = self.bounds(fonts);
        let mut mask = Mask::new(picture, bounds);
        match &self.shape {
            Shape::Pen(points) => mask.path(points, self.size),
            Shape::Marker(points) => {
                mask.path(points, self.size);
                return mask.paint(picture, self.color, 0.4);
            }
            Shape::Rect(rect) => mask.path(&corners(*rect), self.size),
            Shape::Ellipse(rect) => mask.path(&oval(*rect), self.size),
            Shape::Arrow(from, to) => {
                let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                let length = dx.hypot(dy).max(f32::EPSILON);
                let (ux, uy) = (dx / length, dy / length);
                let head = (self.size * 4.5).min(length * 0.6);
                let wing = head * 0.5;
                let neck = (to.0 - ux * head, to.1 - uy * head);
                mask.stroke(*from, (to.0 - ux * head * 0.9, to.1 - uy * head * 0.9), self.size);
                mask.triangle([
                    *to,
                    (neck.0 - uy * wing, neck.1 + ux * wing),
                    (neck.0 + uy * wing, neck.1 - ux * wing),
                ]);
            }
            Shape::Redact(rect) => return mosaic(picture, *rect),
            Shape::Text(at, text) => {
                let mut buffer = crate::text::shape_label(fonts, text, self.size);
                let (left, top) = (at.0.round() as i32, at.1.round() as i32);
                let ink = glyphon::Color::rgb(self.color[0], self.color[1], self.color[2]);
                let (width, height) = (picture.width() as i32, picture.height() as i32);
                buffer.draw(&mut fonts.system, &mut fonts.swash, ink, |x, y, w, h, color| {
                    for py in (top + y).max(0)..(top + y + h as i32).min(height) {
                        for px in (left + x).max(0)..(left + x + w as i32).min(width) {
                            let pixel = picture.get_pixel_mut(px as u32, py as u32);
                            blend(pixel, [color.r(), color.g(), color.b()], color.a() as f32 / 255.0);
                        }
                    }
                });
                return;
            }
        }
        mask.paint(picture, self.color, 1.0);
    }
}

/// Replaces what is in `rect` with squares of its average colour.
fn mosaic(picture: &mut RgbaImage, rect: Rect) {
    let left = rect[0].round().clamp(0.0, picture.width() as f32) as u32;
    let top = rect[1].round().clamp(0.0, picture.height() as f32) as u32;
    let right = (rect[0] + rect[2]).round().clamp(0.0, picture.width() as f32) as u32;
    let bottom = (rect[1] + rect[3]).round().clamp(0.0, picture.height() as f32) as u32;
    if right <= left || bottom <= top {
        return;
    }
    let tile = (((right - left).min(bottom - top)) / 4).clamp(6, 48);
    for y0 in (top..bottom).step_by(tile as usize) {
        for x0 in (left..right).step_by(tile as usize) {
            let (x1, y1) = ((x0 + tile).min(right), (y0 + tile).min(bottom));
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    for (total, channel) in sum.iter_mut().zip(picture.get_pixel(x, y).0) {
                        *total += channel as u32;
                    }
                }
            }
            let count = (x1 - x0) * (y1 - y0);
            let average = image::Rgba(sum.map(|total| (total / count) as u8));
            for y in y0..y1 {
                for x in x0..x1 {
                    picture.put_pixel(x, y, average);
                }
            }
        }
    }
}

/// The crop, kept inside the picture and at least a pixel each way.
fn crop_within(crop: Rect, width: u32, height: u32) -> Option<[u32; 4]> {
    let left = crop[0].round().clamp(0.0, width as f32) as u32;
    let top = crop[1].round().clamp(0.0, height as f32) as u32;
    let right = (crop[0] + crop[2]).round().clamp(0.0, width as f32) as u32;
    let bottom = (crop[1] + crop[3]).round().clamp(0.0, height as f32) as u32;
    (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
}

/// The picture with its marks on it. `cropped` cuts it down to the crop.
pub fn render(base: &RgbaImage, marks: &Marks, cropped: bool, fonts: &mut Fonts) -> RgbaImage {
    let mut picture = base.clone();
    for mark in &marks.marks {
        mark.draw(&mut picture, fonts);
    }
    match marks
        .crop
        .filter(|_| cropped)
        .and_then(|crop| crop_within(crop, base.width(), base.height()))
    {
        Some([left, top, width, height]) => image::imageops::crop_imm(&picture, left, top, width, height).to_image(),
        None => picture,
    }
}

pub fn decode(bytes: &[u8]) -> Result<RgbaImage, String> {
    let picture = image::load_from_memory(bytes).map_err(|err| format!("that picture cannot be read: {err}"))?;
    if picture.width().max(picture.height()) > MAX_SIDE {
        return Err(format!(
            "that picture is too large to retouch (over {MAX_SIDE} pixels a side)"
        ));
    }
    Ok(picture.into_rgba8())
}

pub fn encode(picture: &RgbaImage) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            picture,
            picture.width(),
            picture.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|err| err.to_string())?;
    Ok(png)
}

// ----- marks as JSON, for `stet ctl annotate` -------------------------------

fn color_named(name: &str) -> Option<[u8; 3]> {
    if let Some(&(_, color)) = COLORS.iter().find(|(known, _)| known.eq_ignore_ascii_case(name)) {
        return Some(color);
    }
    let hex = name.strip_prefix('#').filter(|hex| hex.len() == 6)?;
    let channel = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// Reads marks from JSON: a list of `{"tool": …}` objects, positions in the
/// picture's own pixels. `width` and `height` are the picture's.
pub fn parse(ops: &Value, width: u32, height: u32) -> Result<Marks, String> {
    let list = ops.as_array().ok_or("`ops` is a list of marks")?;
    let mut marks = Marks::default();
    for (index, op) in list.iter().enumerate() {
        let wrong = |what: &str| format!("mark {}: {what}", index + 1);
        let number = |key: &str| op.get(key).and_then(Value::as_f64).map(|value| value as f32);
        let point = |value: &Value| Some((value.get(0)?.as_f64()? as f32, value.get(1)?.as_f64()? as f32));
        let rect = || Some([number("x")?, number("y")?, number("w")?, number("h")?]);
        let points = || -> Option<Vec<Point>> {
            let points: Option<Vec<Point>> = op.get("points")?.as_array()?.iter().map(point).collect();
            points.filter(|points| !points.is_empty())
        };
        let ends = || Some((point(op.get("from")?)?, point(op.get("to")?)?));
        let name = op.get("tool").and_then(Value::as_str).unwrap_or("");
        let (tool, shape) = match name {
            "pen" => (
                Tool::Pen,
                Shape::Pen(points().ok_or(wrong("pen needs `points`: [[x, y], …]"))?),
            ),
            "marker" | "highlight" => {
                // A highlight over a rectangle is one level stroke through it.
                let points = match (points(), rect()) {
                    (Some(points), _) => points,
                    (None, Some([x, y, w, h])) => {
                        let reach = (w - h).max(0.0) / 2.0;
                        let middle = (x + w / 2.0, y + h / 2.0);
                        marks.marks.push(Mark {
                            shape: Shape::Marker(vec![(middle.0 - reach, middle.1), (middle.0 + reach, middle.1)]),
                            color: color_of(op, 1).map_err(|err| wrong(&err))?,
                            size: h.min(w),
                        });
                        continue;
                    }
                    _ => return Err(wrong("highlight needs `points`, or `x`, `y`, `w` and `h`")),
                };
                (Tool::Marker, Shape::Marker(points))
            }
            "arrow" => {
                let (from, to) = ends().ok_or(wrong("arrow needs `from` and `to`: [x, y]"))?;
                (Tool::Arrow, Shape::Arrow(from, to))
            }
            "rect" | "box" => (
                Tool::Rect,
                Shape::Rect(rect().ok_or(wrong("rect needs `x`, `y`, `w` and `h`"))?),
            ),
            "ellipse" | "oval" => (
                Tool::Ellipse,
                Shape::Ellipse(rect().ok_or(wrong("ellipse needs `x`, `y`, `w` and `h`"))?),
            ),
            "redact" | "blur" => (
                Tool::Redact,
                Shape::Redact(rect().ok_or(wrong("redact needs `x`, `y`, `w` and `h`"))?),
            ),
            "text" => {
                let at = op
                    .get("at")
                    .and_then(point)
                    .or_else(|| Some((number("x")?, number("y")?)));
                let text = op.get("text").and_then(Value::as_str).filter(|text| !text.is_empty());
                match (at, text) {
                    (Some(at), Some(text)) => (Tool::Text, Shape::Text(at, text.replace('\n', " "))),
                    _ => return Err(wrong("text needs `at`: [x, y] and `text`")),
                }
            }
            "crop" => {
                let crop = rect().ok_or(wrong("crop needs `x`, `y`, `w` and `h`"))?;
                if crop_within(crop, width, height).is_none() {
                    return Err(wrong("the crop leaves nothing of the picture"));
                }
                marks.crop = Some(crop);
                continue;
            }
            other => {
                return Err(wrong(&format!(
                    "unknown tool `{other}` (pen, highlight, arrow, rect, ellipse, text, redact, crop)"
                )));
            }
        };
        marks.marks.push(Mark {
            shape,
            color: color_of(op, (tool == Tool::Marker) as usize).map_err(|err| wrong(&err))?,
            size: number("size")
                .filter(|size| *size > 0.0)
                .unwrap_or(unit(width, height) * factor(tool)),
        });
    }
    Ok(marks)
}

fn color_of(op: &Value, default: usize) -> Result<[u8; 3], String> {
    match op.get("color").and_then(Value::as_str) {
        None => Ok(COLORS[default].1),
        Some(name) => color_named(name).ok_or(format!(
            "unknown color `{name}` (red, yellow, green, blue, black, white, or #rrggbb)"
        )),
    }
}

// ----- the picture on the desk --------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Crop,
    Pen,
    Marker,
    Arrow,
    Rect,
    Ellipse,
    Text,
    Redact,
}

/// The tools in the order they are offered, with their names and keys:
/// where Photoshop and Illustrator agree on a key, it is theirs.
pub const TOOLS: [(Tool, &str, char); 9] = [
    (Tool::Select, "Select", 'v'),
    (Tool::Crop, "Crop", 'c'),
    (Tool::Pen, "Pen", 'b'),
    (Tool::Marker, "Marker", 'h'),
    (Tool::Arrow, "Arrow", 'a'),
    (Tool::Rect, "Box", 'r'),
    (Tool::Ellipse, "Oval", 'o'),
    (Tool::Text, "Text", 't'),
    (Tool::Redact, "Redact", 'x'),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Tool(Tool),
    Color(usize),
    Size(usize),
    Undo,
    Redo,
    Done,
}

enum Drag {
    /// Shaping the newest mark, begun here.
    Draw(Point),
    /// Moving a mark; where the pointer last was.
    Move(usize, Point),
    Crop(Point),
}

/// What the keyboard or the mouse asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Put the picture back in the text, keeping what was done to it.
    Done,
}

pub struct Retouch {
    base: RgbaImage,
    pub marks: Marks,
    undone: Vec<Marks>,
    redone: Vec<Marks>,
    pub tool: Tool,
    pub color: usize,
    pub size: usize,
    drag: Option<Drag>,
    pub selected: Option<usize>,
    /// The label being typed.
    typing: Option<usize>,
    /// The crop being dragged out, not yet taken.
    pub cropping: Option<Rect>,
    /// The line of the text that refers to the picture, and how.
    pub line: usize,
    pub url: String,
    /// Where the picture sat in the text, and where it sits while open.
    pub from: [f32; 4],
    pub place: [f32; 4],
    opened: Instant,
    closing: Option<Instant>,
    /// No animation: a screenshot wants the end state.
    pub still: bool,
    stale: bool,
    /// The size of the picture as last drawn.
    pub shown: (u32, u32),
    /// Where last frame's buttons were.
    pub buttons: Vec<([f32; 4], Button)>,
}

impl Retouch {
    pub fn new(base: RgbaImage, line: usize, url: String, from: Option<[f32; 4]>) -> Retouch {
        let shown = (base.width(), base.height());
        Retouch {
            base,
            marks: Marks::default(),
            undone: Vec::new(),
            redone: Vec::new(),
            tool: Tool::Arrow,
            color: 0,
            size: 1,
            drag: None,
            selected: None,
            typing: None,
            cropping: None,
            line,
            url,
            from: from.unwrap_or([0.0; 4]),
            place: [0.0; 4],
            opened: Instant::now(),
            closing: None,
            still: from.is_none(),
            stale: true,
            shown,
            buttons: Vec::new(),
        }
    }

    /// How far open the picture is: 0 in the text, 1 on the desk.
    pub fn openness(&self) -> f32 {
        let ease = |t: f32| 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
        match self.closing {
            Some(_) if self.still => 0.0,
            Some(since) => 1.0 - ease(since.elapsed().as_secs_f32() / CLOSE.as_secs_f32()),
            None if self.still => 1.0,
            None => ease(self.opened.elapsed().as_secs_f32() / OPEN.as_secs_f32()),
        }
    }

    pub fn animating(&self) -> bool {
        !self.still
            && match self.closing {
                Some(since) => since.elapsed() < CLOSE,
                None => self.opened.elapsed() < OPEN,
            }
    }

    pub fn close(&mut self) {
        self.closing.get_or_insert_with(Instant::now);
    }

    pub fn closing(&self) -> bool {
        self.closing.is_some()
    }

    /// Closed, and done shrinking back into the text.
    pub fn gone(&self) -> bool {
        self.closing.is_some_and(|since| self.still || since.elapsed() >= CLOSE)
    }

    pub fn changed(&self) -> bool {
        self.marks != Marks::default()
    }

    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The part of the original on show: all of it while cropping, so the
    /// crop can be taken again.
    fn window(&self) -> Rect {
        let whole = [0.0, 0.0, self.base.width() as f32, self.base.height() as f32];
        match self
            .marks
            .crop
            .and_then(|crop| crop_within(crop, self.base.width(), self.base.height()))
        {
            Some(crop) if self.tool != Tool::Crop => crop.map(|side| side as f32),
            _ => whole,
        }
    }

    fn to_picture(&self, x: f32, y: f32) -> Point {
        let (window, place) = (self.window(), self.place);
        (
            window[0] + (x - place[0]) / place[2].max(1.0) * window[2],
            window[1] + (y - place[1]) / place[3].max(1.0) * window[3],
        )
    }

    /// A rectangle of the picture, as it falls in the window.
    pub fn on_screen(&self, rect: Rect) -> [f32; 4] {
        let (window, place) = (self.window(), self.place);
        let (sx, sy) = (place[2] / window[2].max(1.0), place[3] / window[3].max(1.0));
        [
            place[0] + (rect[0] - window[0]) * sx,
            place[1] + (rect[1] - window[1]) * sy,
            rect[2] * sx,
            rect[3] * sy,
        ]
    }

    /// The picture as it should now be shown, if it changed.
    pub fn texture(&mut self, fonts: &mut Fonts) -> Option<Decoded> {
        if !std::mem::take(&mut self.stale) {
            return None;
        }
        let picture = render(&self.base, &self.marks, self.tool != Tool::Crop, fonts);
        self.shown = (picture.width(), picture.height());
        Some(Decoded {
            id: TEXTURE,
            width: picture.width(),
            height: picture.height(),
            rgba: picture.into_raw(),
        })
    }

    /// The finished picture as a PNG, if anything was done to it.
    pub fn finished(&self, fonts: &mut Fonts) -> Option<Result<Vec<u8>, String>> {
        self.changed()
            .then(|| encode(&render(&self.base, &self.marks, true, fonts)))
    }

    /// The outline of the selected mark, and the caret after a label being typed.
    pub fn outlines(&self, fonts: &mut Fonts) -> (Option<Rect>, Option<Rect>) {
        let selected = self
            .selected
            .and_then(|index| self.marks.marks.get(index))
            .map(|mark| mark.bounds(fonts));
        let caret = self.typing.and_then(|index| self.marks.marks.get(index)).map(|mark| {
            let bounds = mark.bounds(fonts);
            let width = match &mark.shape {
                Shape::Text(_, text) if text.is_empty() => 0.0,
                _ => bounds[2],
            };
            [bounds[0] + width, bounds[1], (mark.size / 12.0).max(1.0), bounds[3]]
        });
        (selected, caret)
    }

    fn stroke(&self, tool: Tool) -> f32 {
        unit(self.base.width(), self.base.height()) * SIZES[self.size] * factor(tool)
    }

    fn remember(&mut self) {
        self.undone.push(self.marks.clone());
        self.redone.clear();
    }

    fn undo(&mut self) {
        self.settle();
        if let Some(before) = self.undone.pop() {
            self.redone.push(std::mem::replace(&mut self.marks, before));
        }
        self.selected = None;
        self.stale = true;
    }

    fn redo(&mut self) {
        self.settle();
        if let Some(after) = self.redone.pop() {
            self.undone.push(std::mem::replace(&mut self.marks, after));
        }
        self.selected = None;
        self.stale = true;
    }

    /// Ends whatever was half done: a label keeps what was typed, an empty
    /// one is forgotten.
    fn settle(&mut self) {
        self.drag = None;
        self.cropping = None;
        if let Some(index) = self.typing.take()
            && matches!(&self.marks.marks.get(index), Some(Mark { shape: Shape::Text(_, text), .. }) if text.trim().is_empty())
        {
            self.marks.marks.remove(index);
            self.undone.pop();
            self.stale = true;
        }
    }

    fn pick(&mut self, tool: Tool) {
        self.settle();
        // Showing the whole picture, or the crop, changes what is drawn.
        self.stale |= (self.tool == Tool::Crop) != (tool == Tool::Crop);
        self.tool = tool;
        if tool != Tool::Select {
            self.selected = None;
        }
    }

    /// Changes the selected mark, or what the next one will be.
    fn restyle(&mut self, color: Option<usize>, size: Option<usize>) {
        self.color = color.unwrap_or(self.color);
        self.size = size.unwrap_or(self.size);
        let Some(index) = self
            .typing
            .or(self.selected)
            .filter(|index| *index < self.marks.marks.len())
        else {
            return;
        };
        if self.typing.is_none() {
            self.remember();
        }
        let stroke = self.stroke(self.marks.marks[index].tool());
        let mark = &mut self.marks.marks[index];
        if color.is_some() {
            mark.color = COLORS[self.color].1;
        }
        if size.is_some() {
            mark.size = stroke;
        }
        self.stale = true;
    }

    fn press_button(&mut self, button: Button) -> Outcome {
        match button {
            Button::Tool(tool) => self.pick(tool),
            Button::Color(color) => self.restyle(Some(color), None),
            Button::Size(size) => self.restyle(None, Some(size)),
            Button::Undo => self.undo(),
            Button::Redo => self.redo(),
            Button::Done => {
                self.settle();
                return Outcome::Done;
            }
        }
        Outcome::Stay
    }

    pub fn press(&mut self, x: f32, y: f32, fonts: &mut Fonts) -> Outcome {
        let inside = |rect: &[f32; 4]| x >= rect[0] && x < rect[0] + rect[2] && y >= rect[1] && y < rect[1] + rect[3];
        if let Some(&(_, button)) = self.buttons.iter().find(|(rect, _)| inside(rect)) {
            return self.press_button(button);
        }
        let typing = self.typing.is_some();
        self.settle();
        if !inside(&self.place) || self.closing.is_some() {
            return Outcome::Stay;
        }
        let at = self.to_picture(x, y);
        let color = COLORS[self.color].1;
        let size = self.stroke(self.tool);
        let begin = |shape: Shape| Mark { shape, color, size };
        match self.tool {
            Tool::Select => {
                let hit = (0..self.marks.marks.len()).rev().find(|&index| {
                    let [left, top, width, height] = self.marks.marks[index].bounds(fonts);
                    at.0 >= left && at.0 < left + width && at.1 >= top && at.1 < top + height
                });
                self.selected = hit;
                if let Some(index) = hit {
                    self.remember();
                    self.drag = Some(Drag::Move(index, at));
                }
            }
            Tool::Crop => {
                self.cropping = Some([at.0, at.1, 0.0, 0.0]);
                self.drag = Some(Drag::Crop(at));
            }
            // A click away from a label ends it; the next click starts another.
            Tool::Text if typing => {}
            Tool::Text => {
                self.remember();
                self.marks
                    .marks
                    .push(begin(Shape::Text((at.0, at.1 - size * 0.6), String::new())));
                self.typing = Some(self.marks.marks.len() - 1);
            }
            tool => {
                self.remember();
                self.marks.marks.push(begin(match tool {
                    Tool::Pen => Shape::Pen(vec![at]),
                    Tool::Marker => Shape::Marker(vec![at]),
                    Tool::Arrow => Shape::Arrow(at, at),
                    Tool::Rect => Shape::Rect([at.0, at.1, 0.0, 0.0]),
                    Tool::Ellipse => Shape::Ellipse([at.0, at.1, 0.0, 0.0]),
                    _ => Shape::Redact([at.0, at.1, 0.0, 0.0]),
                }));
                self.drag = Some(Drag::Draw(at));
            }
        }
        self.stale = true;
        Outcome::Stay
    }

    pub fn drag(&mut self, x: f32, y: f32) {
        let at = self.to_picture(x, y);
        match &mut self.drag {
            None => return,
            Some(Drag::Crop(from)) => self.cropping = Some(upright(*from, at)),
            Some(Drag::Move(index, last)) => {
                if let Some(mark) = self.marks.marks.get_mut(*index) {
                    mark.nudge(at.0 - last.0, at.1 - last.1);
                }
                *last = at;
            }
            Some(Drag::Draw(from)) => {
                let from = *from;
                if let Some(mark) = self.marks.marks.last_mut() {
                    match &mut mark.shape {
                        Shape::Pen(points) | Shape::Marker(points) => points.push(at),
                        Shape::Arrow(_, to) => *to = at,
                        Shape::Rect(rect) | Shape::Ellipse(rect) | Shape::Redact(rect) => *rect = upright(from, at),
                        Shape::Text(..) => {}
                    }
                }
            }
        }
        self.stale = true;
    }

    pub fn release(&mut self) {
        match self.drag.take() {
            // A click that went nowhere leaves no mark.
            Some(Drag::Draw(_)) => {
                let slight = match self.marks.marks.last().map(|mark| &mark.shape) {
                    Some(Shape::Arrow(from, to)) => (from.0 - to.0).hypot(from.1 - to.1) < 3.0,
                    Some(Shape::Rect(rect) | Shape::Ellipse(rect) | Shape::Redact(rect)) => {
                        rect[2] < 2.0 || rect[3] < 2.0
                    }
                    _ => false,
                };
                if slight {
                    self.marks.marks.pop();
                    self.undone.pop();
                    self.stale = true;
                }
            }
            Some(Drag::Crop(_)) => {
                if self.cropping.is_some_and(|crop| crop[2] < 4.0 || crop[3] < 4.0) {
                    self.cropping = None;
                }
            }
            Some(Drag::Move(..)) | None => {}
        }
    }

    /// Typed or composed text: it goes into the label being written.
    pub fn text(&mut self, text: &str) {
        if let Some(Mark {
            shape: Shape::Text(_, label),
            ..
        }) = self.typing.and_then(|index| self.marks.marks.get_mut(index))
        {
            label.extend(text.chars().filter(|c| !c.is_control()));
            self.stale = true;
        }
    }

    pub fn key(&mut self, event: KeyEvent) -> Outcome {
        let command = event.mods.sup || event.mods.ctrl;
        if self.typing.is_some() && !command {
            match event.key {
                Key::Char(c) => self.text(c.encode_utf8(&mut [0; 4])),
                Key::Backspace => {
                    if let Some(Mark {
                        shape: Shape::Text(_, label),
                        ..
                    }) = self.typing.and_then(|index| self.marks.marks.get_mut(index))
                    {
                        label.pop();
                        self.stale = true;
                    }
                }
                Key::Enter | Key::Esc | Key::Tab => self.settle(),
                _ => {}
            }
            return Outcome::Stay;
        }
        match event.key {
            Key::Char(c) if command => match (c.to_ascii_lowercase(), event.mods.shift) {
                ('z', false) => self.undo(),
                ('z', true) | ('y', _) | ('r', _) => self.redo(),
                ('s', _) => {
                    self.settle();
                    return Outcome::Done;
                }
                _ => {}
            },
            Key::Enter => match self.cropping.take() {
                Some(crop) => {
                    self.remember();
                    self.marks.crop = Some(crop);
                    self.pick(Tool::Select);
                }
                None => {
                    self.settle();
                    return Outcome::Done;
                }
            },
            Key::Esc => {
                if self.cropping.is_some() || self.drag.is_some() {
                    self.settle();
                } else if self.selected.take().is_none() {
                    return Outcome::Done;
                }
            }
            Key::Backspace | Key::Delete => {
                if let Some(index) = self.selected.take().filter(|index| *index < self.marks.marks.len()) {
                    self.remember();
                    self.marks.marks.remove(index);
                } else if self.tool == Tool::Crop && self.marks.crop.is_some() {
                    // Back to the whole picture.
                    self.remember();
                    self.marks.crop = None;
                }
                self.stale = true;
            }
            Key::Char('u') => self.undo(),
            Key::Char('[') => self.restyle(None, Some(self.size.saturating_sub(1))),
            Key::Char(']') => self.restyle(None, Some((self.size + 1).min(SIZES.len() - 1))),
            Key::Char(c @ '1'..='6') => self.restyle(Some(c as usize - '1' as usize), None),
            Key::Char(c) => {
                if let Some(&(tool, ..)) = TOOLS.iter().find(|(_, _, key)| *key == c.to_ascii_lowercase()) {
                    self.pick(tool);
                }
            }
            _ => {}
        }
        Outcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use stet_core::Config;

    fn fonts() -> Fonts {
        Fonts::new(&Config::default(), &[])
    }

    fn white(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]))
    }

    #[test]
    fn marks_are_read_from_json_and_drawn() {
        let ops = json!([
            { "tool": "rect", "x": 20, "y": 20, "w": 100, "h": 60, "color": "blue", "size": 4 },
            { "tool": "arrow", "from": [20, 150], "to": [180, 150], "size": 4 },
            { "tool": "highlight", "x": 10, "y": 100, "w": 120, "h": 20 },
            { "tool": "ellipse", "x": 200, "y": 20, "w": 80, "h": 40, "color": "#00ff00", "size": 3 },
            { "tool": "pen", "points": [[250, 150], [270, 170], [290, 150]], "color": "black", "size": 4 },
            { "tool": "redact", "x": 0, "y": 180, "w": 40, "h": 20 },
        ]);
        let marks = parse(&ops, 300, 200).unwrap();
        assert_eq!(marks.marks.len(), 6);
        assert_eq!(marks.marks[1].color, COLORS[0].1);
        assert_eq!(marks.marks[2].color, COLORS[1].1);
        let picture = render(&white(300, 200), &marks, true, &mut fonts());
        let pixel = |x: u32, y: u32| picture.get_pixel(x, y).0;
        // The box is an outline: blue on its edge, paper inside.
        assert_eq!(pixel(20, 50), [0, 122, 255, 255]);
        assert_eq!(pixel(70, 50), [255, 255, 255, 255]);
        // The arrow's shaft and the point of its head are red.
        assert_eq!(pixel(60, 150), [255, 59, 48, 255]);
        assert_eq!(pixel(176, 150), [255, 59, 48, 255]);
        assert_eq!(pixel(60, 140), [255, 255, 255, 255]);
        // The highlight lets the paper through.
        let [r, g, b, _] = pixel(70, 110);
        assert!(r == 255 && g > 204 && g < 255 && b > 0 && b < 255, "{r} {g} {b}");
        assert_eq!(pixel(240, 20), [0, 255, 0, 255]);
        assert_eq!(pixel(270, 170), [17, 17, 17, 255]);
    }

    #[test]
    fn a_crop_cuts_the_picture_down_and_bad_marks_are_refused() {
        let ops = json!([
            { "tool": "rect", "x": 10, "y": 10, "w": 30, "h": 30, "size": 4 },
            { "tool": "crop", "x": 5, "y": 5, "w": 50, "h": 40 },
        ]);
        let marks = parse(&ops, 100, 100).unwrap();
        let picture = render(&white(100, 100), &marks, true, &mut fonts());
        assert_eq!((picture.width(), picture.height()), (50, 40));
        // The box moved with the crop.
        assert_eq!(picture.get_pixel(5, 20).0, [255, 59, 48, 255]);
        let whole = render(&white(100, 100), &marks, false, &mut fonts());
        assert_eq!((whole.width(), whole.height()), (100, 100));

        let refused = |ops: Value| parse(&ops, 100, 100).unwrap_err();
        assert!(refused(json!([{ "tool": "lasso" }])).contains("unknown tool `lasso`"));
        assert!(refused(json!([{ "tool": "arrow", "from": [1, 2] }])).starts_with("mark 1: arrow needs"));
        assert!(
            refused(json!([{ "tool": "rect", "x": 1, "y": 1, "w": 5, "h": 5, "color": "mauve" }])).contains("mauve")
        );
        assert!(refused(json!([{ "tool": "crop", "x": 200, "y": 0, "w": 5, "h": 5 }])).contains("nothing"));
        assert!(refused(json!({ "tool": "rect" })).contains("list"));
    }

    #[test]
    fn redacting_hides_what_was_there_and_text_is_set_in_its_color() {
        let mut base = white(120, 60);
        for x in (0..120).step_by(2) {
            for y in 0..60 {
                base.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        let ops = json!([
            { "tool": "redact", "x": 0, "y": 0, "w": 60, "h": 60 },
            { "tool": "text", "at": [70, 10], "text": "HHHH", "color": "red", "size": 30 },
        ]);
        let picture = render(&base, &parse(&ops, 120, 60).unwrap(), true, &mut fonts());
        // Stripes average out to grey under the mosaic and survive beside it.
        let [grey, ..] = picture.get_pixel(10, 10).0;
        assert!((100..160).contains(&grey), "{grey}");
        assert_eq!(picture.get_pixel(11, 10).0, picture.get_pixel(10, 10).0);
        assert_eq!(picture.get_pixel(64, 50).0, [0, 0, 0, 255]);
        let red = picture.pixels().filter(|pixel| pixel.0 == [255, 59, 48, 255]).count();
        // A machine with no fonts at all sets no type.
        let any_font = fonts().system.db().faces().next().is_some();
        assert!(red > 40 || !any_font, "only {red} red pixels of type");
    }

    fn open(width: u32, height: u32) -> Retouch {
        let mut retouch = Retouch::new(white(width, height), 0, "a.png".into(), None);
        // Shown at twice its size, away from the corner.
        retouch.place = [100.0, 50.0, width as f32 * 2.0, height as f32 * 2.0];
        retouch
    }

    fn keys(retouch: &mut Retouch, text: &str) -> Outcome {
        text.chars()
            .map(|c| retouch.key(KeyEvent::new(Key::Char(c))))
            .last()
            .unwrap_or(Outcome::Stay)
    }

    /// The first mark's rectangle, to the nearest pixel.
    fn boxed(retouch: &Retouch) -> Rect {
        match &retouch.marks.marks[0].shape {
            Shape::Rect(rect) => rect.map(f32::round),
            other => panic!("not a box: {other:?}"),
        }
    }

    #[test]
    fn the_mouse_draws_moves_and_undoes() {
        let mut fonts = fonts();
        let mut retouch = open(200, 100);
        assert!(!retouch.changed());
        keys(&mut retouch, "r3");
        retouch.press(120.0, 70.0, &mut fonts);
        retouch.drag(220.0, 150.0);
        retouch.release();
        assert_eq!(boxed(&retouch), [10.0, 10.0, 50.0, 40.0]);
        assert_eq!(retouch.marks.marks[0].color, COLORS[2].1);
        // A click that goes nowhere leaves nothing, and nothing to undo.
        retouch.press(300.0, 100.0, &mut fonts);
        retouch.release();
        assert_eq!((retouch.marks.marks.len(), retouch.undone.len()), (1, 1));

        // Select it, drag it, recolour it, delete it.
        keys(&mut retouch, "v");
        retouch.press(121.0, 100.0, &mut fonts);
        assert_eq!(retouch.selected, Some(0));
        retouch.drag(141.0, 110.0);
        retouch.release();
        assert_eq!(boxed(&retouch), [20.0, 15.0, 50.0, 40.0]);
        keys(&mut retouch, "4");
        assert_eq!(retouch.marks.marks[0].color, COLORS[3].1);
        retouch.key(KeyEvent::new(Key::Backspace));
        assert!(retouch.marks.marks.is_empty());
        keys(&mut retouch, "u");
        assert_eq!(retouch.marks.marks.len(), 1);
        let redo = KeyEvent {
            key: Key::Char('z'),
            mods: stet_core::Mods {
                sup: true,
                shift: true,
                ..Default::default()
            },
        };
        retouch.key(redo);
        assert!(retouch.marks.marks.is_empty());
        // Nothing is left on it, so there is nothing to keep.
        assert!(!retouch.changed());
    }

    #[test]
    fn labels_are_typed_and_crops_are_taken_with_enter() {
        let mut fonts = fonts();
        let mut retouch = open(200, 100);
        keys(&mut retouch, "t");
        retouch.press(140.0, 90.0, &mut fonts);
        assert!(retouch.typing());
        // Letters are the label's now, not tools.
        assert_eq!(keys(&mut retouch, "rev"), Outcome::Stay);
        retouch.text(" 2");
        retouch.key(KeyEvent::new(Key::Backspace));
        assert_eq!(retouch.key(KeyEvent::new(Key::Enter)), Outcome::Stay);
        assert!(matches!(&retouch.marks.marks[0].shape, Shape::Text(_, text) if text == "rev "));
        // An empty label is forgotten.
        retouch.press(300.0, 200.0, &mut fonts);
        retouch.key(KeyEvent::new(Key::Esc));
        assert_eq!((retouch.marks.marks.len(), retouch.undone.len()), (1, 1));

        keys(&mut retouch, "c");
        retouch.press(110.0, 60.0, &mut fonts);
        retouch.drag(300.0, 200.0);
        retouch.release();
        assert_eq!(
            retouch.cropping.map(|crop| crop.map(f32::round)),
            Some([5.0, 5.0, 95.0, 70.0])
        );
        assert_eq!(retouch.key(KeyEvent::new(Key::Enter)), Outcome::Stay);
        assert_eq!((retouch.marks.crop.is_some(), retouch.tool), (true, Tool::Select));
        let shown = retouch.texture(&mut fonts).unwrap();
        assert_eq!((shown.width, shown.height), (95, 70));
        // The window now shows the crop: its corner is the crop's corner.
        let corner = retouch.on_screen([5.0, 5.0, 95.0, 70.0]);
        assert!(
            corner.iter().zip(retouch.place).all(|(a, b)| (a - b).abs() < 0.01),
            "{corner:?}"
        );
        assert_eq!(retouch.key(KeyEvent::new(Key::Enter)), Outcome::Done);
        let png = retouch.finished(&mut fonts).unwrap().unwrap();
        let saved = decode(&png).unwrap();
        assert_eq!((saved.width(), saved.height()), (95, 70));
    }
}
