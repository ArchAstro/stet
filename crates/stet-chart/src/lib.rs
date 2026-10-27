//! Charts for stet's notebook mode. A spec in Vega-Lite's shape (see
//! [`spec`]) is laid out for the room there is and drawn as SVG in the
//! editor's theme; the picture is drawn like any other SVG. What is under
//! the pointer is answered by [`Chart::readout`], so the window can show
//! exact values over the picture.
//!
//! Marks: line, bar (stacked by colour), point, area (stacked by colour)
//! and heatmap. Scales: linear, band, and time.

pub mod scale;
pub mod spec;

use scale::Linear;
use serde_json::Value;
pub use spec::Table;
use spec::{Aggregate, Channel, Kind, Layer, Mark, Spec};
use std::collections::HashMap;
use std::fmt::Write;

/// The MIME type of a stet chart spec.
pub const MIME: &str = "application/vnd.stet.chart+json";

/// Vega-Lite's MIME types (Altair sends these: `v5+json`, `v6.json`); read
/// as stet charts.
pub fn is_vega_lite(mime: &str) -> bool {
    mime.starts_with("application/vnd.vegalite.v") && mime.ends_with("json")
}

pub type Color = [u8; 3];

/// The editor's look, for a chart drawn in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub text: Color,
    pub muted: Color,
    pub grid: Color,
    /// One per series, in order; reused when there are more series.
    pub series: Vec<Color>,
    pub font: String,
    /// Text size in points.
    pub size: f32,
}

/// One line of a readout: a series' swatch, its name, and its value.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub color: Color,
    pub label: String,
    pub value: String,
    /// Where the value sits on the chart, to mark it.
    pub at: Option<(f32, f32)>,
}

/// What the pointer is over.
#[derive(Clone, Debug, PartialEq)]
pub struct Readout {
    /// A vertical guide here, for lines and areas read across.
    pub guide: Option<f32>,
    pub title: String,
    pub rows: Vec<Reading>,
}

#[derive(Clone, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

#[derive(Debug)]
enum Hit {
    Box {
        rect: Rect,
        title: String,
        row: Reading,
    },
    Dot {
        x: f32,
        y: f32,
        title: String,
        row: Reading,
    },
}

/// A chart laid out at a size: its picture and what is where in it.
#[derive(Debug)]
pub struct Chart {
    pub svg: String,
    pub width: f32,
    pub height: f32,
    plot: Rect,
    hits: Vec<Hit>,
    /// Lines and areas, read across: each x position and every series there.
    sweep: Vec<(f32, String, Vec<Reading>)>,
}

impl Chart {
    /// What is under `(x, y)`, in points from the chart's top left.
    pub fn readout(&self, x: f32, y: f32) -> Option<Readout> {
        for hit in &self.hits {
            if let Hit::Box { rect, title, row } = hit
                && rect.contains(x, y)
            {
                return Some(Readout {
                    guide: None,
                    title: title.clone(),
                    rows: vec![row.clone()],
                });
            }
        }
        let near = self
            .hits
            .iter()
            .filter_map(|hit| match hit {
                Hit::Dot {
                    x: px,
                    y: py,
                    title,
                    row,
                } => Some(((px - x).powi(2) + (py - y).powi(2), title, row)),
                Hit::Box { .. } => None,
            })
            .filter(|(distance, ..)| *distance <= 100.0)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, title, row)) = near {
            return Some(Readout {
                guide: None,
                title: title.clone(),
                rows: vec![row.clone()],
            });
        }
        let inside = self.plot.contains(x, y.clamp(self.plot.y, self.plot.y + self.plot.h));
        if !inside || self.sweep.is_empty() {
            return None;
        }
        let (px, title, rows) = self
            .sweep
            .iter()
            .min_by(|a, b| (a.0 - x).abs().total_cmp(&(b.0 - x).abs()))?;
        Some(Readout {
            guide: Some(*px),
            title: title.clone(),
            rows: rows.clone(),
        })
    }
}

/// Draw `value` (a spec) `width` points wide. `resolve` loads rows the
/// spec names by reference.
pub fn draw(
    value: &Value,
    theme: &Theme,
    width: f32,
    resolve: &mut dyn FnMut(&str) -> Option<Table>,
) -> Result<Chart, String> {
    let spec = spec::parse(value, resolve)?;
    Ok(Painter::new(theme, width, &spec)?.paint(&spec))
}

// --- Reading values ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Num(f64),
    Cat(String),
}

impl Val {
    fn num(&self) -> f64 {
        match self {
            Val::Num(n) => *n,
            Val::Cat(_) => 0.0,
        }
    }

    fn key(&self) -> String {
        match self {
            Val::Num(n) => scale::readout(*n),
            Val::Cat(text) => text.clone(),
        }
    }
}

/// How an axis places values.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placing {
    Band,
    Number,
    Time,
}

fn read(value: &Value, placing: Placing) -> Option<Val> {
    match placing {
        Placing::Band => (!value.is_null()).then(|| Val::Cat(spec::text(value))),
        Placing::Number => value
            .as_f64()
            .or_else(|| value.as_str()?.trim().parse().ok())
            .map(Val::Num),
        Placing::Time => scale::time(value).map(Val::Num),
    }
}

fn placing(channel: Option<&Channel>, band: bool) -> Placing {
    match channel.map(|channel| channel.kind) {
        _ if band => Placing::Band,
        Some(Kind::Temporal) => Placing::Time,
        Some(Kind::Quantitative) | None => Placing::Number,
        Some(_) => Placing::Band,
    }
}

/// One mark's worth of a layer: where it goes and which series it is in.
#[derive(Clone, Debug)]
struct Datum {
    x: Option<Val>,
    y: Option<Val>,
    series: usize,
    /// A heatmap cell's value.
    shade: Option<f64>,
}

/// Values gathered for one aggregate: its x, its y, its series, and the
/// values to aggregate.
type Group = (Option<Val>, Option<Val>, usize, Vec<f64>);

/// A layer read into data, aggregated if the spec asks.
struct Prepared {
    mark: Mark,
    data: Vec<Datum>,
}

fn aggregate(how: Aggregate, values: &[f64]) -> f64 {
    match how {
        Aggregate::Count => values.len() as f64,
        Aggregate::Sum => values.iter().sum(),
        Aggregate::Mean => values.iter().sum::<f64>() / values.len().max(1) as f64,
        Aggregate::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        Aggregate::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    }
}

// --- Laying out and drawing -------------------------------------------------

struct Painter<'a> {
    theme: &'a Theme,
    width: f32,
    x_place: Placing,
    y_place: Placing,
    /// Bars lie along y: categories down the side.
    across: bool,
    series: Vec<String>,
    color_title: Option<String>,
    layers: Vec<Prepared>,
}

fn text_width(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * 0.58
}

fn hex(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn mix(a: Color, b: Color, weight: f32) -> Color {
    let channel = |i: usize| (a[i] as f32 * weight + b[i] as f32 * (1.0 - weight)).round() as u8;
    [channel(0), channel(1), channel(2)]
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl<'a> Painter<'a> {
    fn new(theme: &'a Theme, width: f32, spec: &Spec) -> Result<Painter<'a>, String> {
        let first = &spec.layers[0];
        let discrete = |channel: &Option<Channel>| channel.as_ref().is_some_and(|channel| channel.kind.discrete());
        let has_bar = spec.layers.iter().any(|layer| layer.mark == Mark::Bar);
        let heat = first.mark == Mark::Heatmap;
        // Bars stand on categories: x's, unless only y has them.
        let across = has_bar && !discrete(&first.x) && discrete(&first.y);
        let x_band = heat || discrete(&first.x) || (has_bar && !across);
        let y_band = heat || discrete(&first.y) && !x_band || across;
        let x_place = placing(first.x.as_ref(), x_band);
        let y_place = placing(first.y.as_ref(), y_band);
        let mut painter = Painter {
            theme,
            width,
            x_place,
            y_place,
            across,
            series: Vec::new(),
            color_title: None,
            layers: Vec::new(),
        };
        for layer in &spec.layers {
            let prepared = painter.prepare(layer)?;
            painter.layers.push(prepared);
        }
        if painter.layers.iter().all(|layer| layer.data.is_empty()) {
            return Err("the chart's data has no rows it can draw".into());
        }
        Ok(painter)
    }

    fn prepare(&mut self, layer: &Layer) -> Result<Prepared, String> {
        let column = |channel: &Option<Channel>| -> Result<Option<usize>, String> {
            match channel {
                Some(channel) if !channel.field.is_empty() => layer
                    .data
                    .column(&channel.field)
                    .map(Some)
                    .ok_or_else(|| format!("the data has no column `{}`", channel.field)),
                _ => Ok(None),
            }
        };
        let (x_col, y_col, color_col) = (column(&layer.x)?, column(&layer.y)?, column(&layer.color)?);
        let heat = layer.mark == Mark::Heatmap;
        // A colour by category splits the data into series; a heatmap's
        // colour is its value.
        let split = layer
            .color
            .as_ref()
            .is_some_and(|color| !heat && color.kind != Kind::Quantitative);
        if split {
            self.color_title = layer.color.as_ref().map(Channel::title);
        }
        let x_agg = layer.x.as_ref().and_then(|channel| channel.aggregate);
        let y_agg = layer.y.as_ref().and_then(|channel| channel.aggregate);
        let shade_agg = layer.color.as_ref().and_then(|channel| channel.aggregate);
        let mut data = Vec::new();
        let mut groups: Vec<Group> = Vec::new();
        let mut index: HashMap<(String, String, usize), usize> = HashMap::new();
        for row in &layer.data.rows {
            let cell = |col: Option<usize>| col.and_then(|col| row.get(col)).unwrap_or(&Value::Null);
            let series = match (split, color_col) {
                (true, Some(col)) => self.series_index(&spec::text(&row[col])),
                _ => 0,
            };
            let x = read(cell(x_col), self.x_place);
            let y = read(cell(y_col), self.y_place);
            let shade = heat.then(|| cell(color_col).as_f64()).flatten();
            match (x_agg, y_agg, shade_agg) {
                (None, None, None) => {
                    if (x_col.is_some() && x.is_none()) || (y_col.is_some() && y.is_none()) {
                        continue;
                    }
                    data.push(Datum { x, y, series, shade });
                }
                _ => {
                    // The aggregated channel's values pile up per group of the others.
                    let (x_key, y_key, value) = if x_agg.is_some() {
                        (None, y.clone(), x.as_ref().map(Val::num))
                    } else if y_agg.is_some() {
                        (x.clone(), None, y.as_ref().map(Val::num))
                    } else {
                        (x.clone(), y.clone(), shade)
                    };
                    let key = (
                        x_key.as_ref().map(Val::key).unwrap_or_default(),
                        y_key.as_ref().map(Val::key).unwrap_or_default(),
                        series,
                    );
                    let at = *index.entry(key).or_insert_with(|| {
                        groups.push((x_key.clone(), y_key.clone(), series, Vec::new()));
                        groups.len() - 1
                    });
                    let count = matches!(x_agg.or(y_agg).or(shade_agg), Some(Aggregate::Count));
                    match (count, value) {
                        (true, _) => groups[at].3.push(1.0),
                        (false, Some(value)) => groups[at].3.push(value),
                        _ => {}
                    }
                }
            }
        }
        for (x, y, series, values) in groups {
            let how = x_agg.or(y_agg).or(shade_agg).expect("grouped only when aggregating");
            let value = aggregate(how, &values);
            let datum = if x_agg.is_some() {
                Datum {
                    x: Some(Val::Num(value)),
                    y,
                    series,
                    shade: None,
                }
            } else if y_agg.is_some() {
                Datum {
                    x,
                    y: Some(Val::Num(value)),
                    series,
                    shade: None,
                }
            } else {
                Datum {
                    x,
                    y,
                    series,
                    shade: Some(value),
                }
            };
            data.push(datum);
        }
        Ok(Prepared { mark: layer.mark, data })
    }

    fn series_index(&mut self, name: &str) -> usize {
        match self.series.iter().position(|known| known == name) {
            Some(at) => at,
            None => {
                self.series.push(name.to_string());
                self.series.len() - 1
            }
        }
    }

    fn color(&self, series: usize) -> Color {
        let palette = &self.theme.series;
        palette
            .get(series % palette.len().max(1))
            .copied()
            .unwrap_or(self.theme.text)
    }

    fn stacked(mark: Mark) -> bool {
        matches!(mark, Mark::Bar | Mark::Area)
    }

    /// Band categories along one axis, in the order they first appear
    /// (sorted when they are numbers or dates).
    fn categories(&self, along_x: bool) -> Vec<String> {
        let mut seen: Vec<Val> = Vec::new();
        for datum in self.layers.iter().flat_map(|layer| &layer.data) {
            if let Some(value) = if along_x { &datum.x } else { &datum.y }
                && !seen.contains(value)
            {
                seen.push(value.clone());
            }
        }
        let numbers: Option<Vec<f64>> = seen.iter().map(|value| value.key().parse::<f64>().ok()).collect();
        let times: Option<Vec<f64>> = seen
            .iter()
            .map(|value| scale::time(&Value::String(value.key())))
            .collect();
        let mut keys: Vec<String> = seen.iter().map(Val::key).collect();
        if let Some(numbers) = numbers {
            let mut paired: Vec<(f64, String)> = numbers.into_iter().zip(keys).collect();
            paired.sort_by(|a, b| a.0.total_cmp(&b.0));
            keys = paired.into_iter().map(|(_, key)| key).collect();
        } else if let Some(times) = times {
            let mut paired: Vec<(f64, String)> = times.into_iter().zip(keys).collect();
            paired.sort_by(|a, b| a.0.total_cmp(&b.0));
            keys = paired.into_iter().map(|(_, key)| key).collect();
        }
        keys
    }

    /// The low and high of a continuous axis, stacks summed and bars and
    /// areas grounded at zero.
    fn extent<'b>(&'b self, along_x: bool) -> (f64, f64) {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for layer in &self.layers {
            let measure = |datum: &'b Datum| if along_x { datum.x.as_ref() } else { datum.y.as_ref() };
            let stacked = Self::stacked(layer.mark) && !self.series.is_empty();
            if stacked {
                let mut totals: HashMap<String, (f64, f64)> = HashMap::new();
                for datum in &layer.data {
                    let base = if along_x { &datum.y } else { &datum.x };
                    let value = measure(datum).map_or(0.0, Val::num);
                    let total = totals
                        .entry(base.as_ref().map(Val::key).unwrap_or_default())
                        .or_default();
                    if value >= 0.0 {
                        total.1 += value;
                    } else {
                        total.0 += value;
                    }
                }
                for (down, up) in totals.values() {
                    lo = lo.min(*down);
                    hi = hi.max(*up);
                }
            } else {
                for value in layer.data.iter().filter_map(measure) {
                    lo = lo.min(value.num());
                    hi = hi.max(value.num());
                }
            }
            // Bars and areas stand on zero along the axis that measures them.
            let measured = match layer.mark {
                Mark::Bar => along_x == self.across,
                Mark::Area => !along_x,
                _ => false,
            };
            if measured {
                lo = lo.min(0.0);
                hi = hi.max(0.0);
            }
        }
        if !lo.is_finite() { (0.0, 1.0) } else { (lo, hi) }
    }

    fn paint(&self, spec: &Spec) -> Chart {
        let t = self.theme;
        let size = t.size;
        let small = (size * 0.92).round();
        let line_gap = size * 1.7;
        let first = &spec.layers[0];
        let heat = first.mark == Mark::Heatmap;

        // Axes: categories, or nice ticks.
        let x_cats = (self.x_place == Placing::Band).then(|| self.categories(true));
        let y_cats = (self.y_place == Placing::Band).then(|| self.categories(false));
        let (x_lo, x_hi) = self.extent(true);
        let (y_lo, y_hi) = self.extent(false);
        let y_nice = scale::nice(y_lo, y_hi, 5);
        let y_ticks: Vec<(f64, String)> = match &y_cats {
            Some(_) => Vec::new(),
            None if self.y_place == Placing::Time => scale::time_ticks(y_lo, y_hi, 5),
            None => scale::ticks(y_nice.0, y_nice.1, y_nice.2)
                .into_iter()
                .map(|v| (v, scale::number(v, y_nice.2)))
                .collect(),
        };
        let y_domain = match self.y_place {
            Placing::Time => (y_lo, y_hi),
            _ => (y_nice.0, y_nice.1),
        };

        // Margins: the title, a legend, y's labels, x's labels, and room for
        // naming each line at its end.
        let label_lines = !heat
            && self.layers.iter().any(|layer| layer.mark == Mark::Line)
            && self.series.len() > 1
            && self.series.len() <= 8
            && self.x_place != Placing::Band;
        let legend = !self.series.is_empty() && !label_lines || heat;
        let mut top = 6.0;
        if spec.title.is_some() {
            top += line_gap;
        }
        if legend {
            top += line_gap;
        }
        let y_title = first
            .y
            .as_ref()
            .map(Channel::title)
            .filter(|title| !self.across && !title.is_empty());
        if y_title.is_some() {
            top += line_gap * 0.85;
        }
        let y_labels: Vec<String> = match &y_cats {
            Some(cats) => cats.clone(),
            None => y_ticks.iter().map(|(_, label)| label.clone()).collect(),
        };
        let left = y_labels
            .iter()
            .map(|label| text_width(label, small))
            .fold(0.0, f32::max)
            .min(self.width * 0.35)
            + 10.0;
        let right = if label_lines {
            self.series
                .iter()
                .map(|name| text_width(name, small))
                .fold(0.0, f32::max)
                .min(self.width * 0.25)
                + 14.0
        } else {
            10.0
        };
        let x_title = first
            .x
            .as_ref()
            .map(Channel::title)
            .or_else(|| self.across.then(String::new));
        let bottom = line_gap
            + if x_title.as_deref().is_some_and(|title| !title.is_empty()) {
                line_gap
            } else {
                0.0
            };
        let height = spec
            .height
            .unwrap_or((self.width * 0.42).clamp(170.0, 300.0))
            .max(top + bottom + 40.0);
        let plot = Rect {
            x: left,
            y: top,
            w: (self.width - left - right).max(40.0),
            h: (height - top - bottom).max(40.0),
        };

        let mut svg = String::new();
        let font = escape(&t.font);
        let _ = write!(
            svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="{font}" font-size="{small}">"#,
            w = self.width,
            h = height,
        );
        let text = |svg: &mut String, x: f32, y: f32, anchor: &str, color: Color, size: f32, body: &str| {
            let _ = write!(
                svg,
                r#"<text x="{x:.1}" y="{y:.1}" text-anchor="{anchor}" fill="{}" font-size="{size}">{}</text>"#,
                hex(color),
                escape(body)
            );
        };
        let mut row_y = 6.0 + size;
        if let Some(title) = &spec.title {
            let _ = write!(
                svg,
                r#"<text x="0" y="{row_y:.1}" fill="{}" font-size="{size}" font-weight="600">{}</text>"#,
                hex(t.text),
                escape(title)
            );
            row_y += line_gap;
        }

        // Scales.
        let x_scale = Linear {
            domain: match self.x_place {
                Placing::Time => (x_lo, x_hi),
                _ => {
                    let (lo, hi, _) = scale::nice(x_lo, x_hi, 6);
                    (lo, hi)
                }
            },
            range: (plot.x, plot.x + plot.w),
        };
        let y_scale = Linear {
            domain: y_domain,
            range: (plot.y + plot.h, plot.y),
        };
        let band = |cats: &[String], lo: f32, length: f32| {
            let step = length / cats.len().max(1) as f32;
            (step, move |key: &str, cats: &[String]| {
                cats.iter().position(|cat| cat == key).map(|at| lo + step * at as f32)
            })
        };
        let x_band = x_cats.as_ref().map(|cats| band(cats, plot.x, plot.w));
        let y_band = y_cats.as_ref().map(|cats| band(cats, plot.y, plot.h));
        // The centre of a value along x, and along y.
        let px = |value: &Val| -> Option<f32> {
            match (&x_band, &x_cats) {
                (Some((step, at)), Some(cats)) => at(&value.key(), cats).map(|start| start + step / 2.0),
                _ => Some(x_scale.at(value.num())),
            }
        };
        let py = |value: &Val| -> Option<f32> {
            match (&y_band, &y_cats) {
                (Some((step, at)), Some(cats)) => at(&value.key(), cats).map(|start| start + step / 2.0),
                _ => Some(y_scale.at(value.num())),
            }
        };

        // Grid and axes.
        if let Some(title) = &y_title {
            text(&mut svg, 0.0, plot.y - size * 0.7, "start", t.muted, small, title);
        }
        match &y_cats {
            Some(cats) => {
                let (step, _) = y_band.as_ref().expect("band with categories");
                for (i, cat) in cats.iter().enumerate() {
                    let y = plot.y + step * (i as f32 + 0.5) + small * 0.35;
                    text(&mut svg, plot.x - 8.0, y, "end", t.muted, small, cat);
                }
            }
            None => {
                for (value, label) in &y_ticks {
                    let y = y_scale.at(*value);
                    let _ = write!(
                        svg,
                        r#"<line x1="{:.1}" x2="{:.1}" y1="{y:.1}" y2="{y:.1}" stroke="{}" stroke-width="1"/>"#,
                        plot.x,
                        plot.x + plot.w,
                        hex(t.grid)
                    );
                    text(&mut svg, plot.x - 8.0, y + small * 0.35, "end", t.muted, small, label);
                }
            }
        }
        let baseline = plot.y + plot.h;
        let label_y = baseline + size * 1.35;
        match &x_cats {
            Some(cats) => {
                let (step, _) = x_band.as_ref().expect("band with categories");
                let widest = cats.iter().map(|cat| text_width(cat, small)).fold(0.0, f32::max) + 8.0;
                let every = (widest / step).ceil().max(1.0) as usize;
                for (i, cat) in cats.iter().enumerate().filter(|(i, _)| i % every == 0) {
                    let x = plot.x + step * (i as f32 + 0.5);
                    text(&mut svg, x, label_y, "middle", t.muted, small, cat);
                }
            }
            None => {
                let ticks: Vec<(f64, String)> = if self.x_place == Placing::Time {
                    let count = (plot.w / 90.0).floor().max(2.0) as usize;
                    scale::time_ticks(x_lo, x_hi, count)
                } else {
                    let count = (plot.w / 70.0).floor().max(2.0) as usize;
                    let (lo, hi, step) = scale::nice(x_lo, x_hi, count.min(8));
                    scale::ticks(lo, hi, step)
                        .into_iter()
                        .map(|v| (v, scale::number(v, step)))
                        .collect()
                };
                for (value, label) in ticks {
                    let x = x_scale.at(value);
                    if self.across {
                        let _ = write!(
                            svg,
                            r#"<line x1="{x:.1}" x2="{x:.1}" y1="{:.1}" y2="{baseline:.1}" stroke="{}" stroke-width="1"/>"#,
                            plot.y,
                            hex(t.grid)
                        );
                    }
                    text(&mut svg, x, label_y, "middle", t.muted, small, &label);
                }
            }
        }
        let _ = write!(
            svg,
            r#"<line x1="{:.1}" x2="{:.1}" y1="{baseline:.1}" y2="{baseline:.1}" stroke="{}" stroke-width="1"/>"#,
            plot.x,
            plot.x + plot.w,
            hex(t.muted)
        );
        if let Some(title) = x_title.filter(|title| !title.is_empty()) {
            text(
                &mut svg,
                plot.x + plot.w / 2.0,
                label_y + line_gap,
                "middle",
                t.muted,
                small,
                &title,
            );
        }

        // Marks.
        let mut hits = Vec::new();
        let mut sweep: Vec<(f32, String, Vec<Reading>)> = Vec::new();
        let x_text = |value: &Val| match (self.x_place, value) {
            (Placing::Time, Val::Num(ms)) => scale::moment(*ms),
            (_, value) => value.key(),
        };
        let y_text = |value: &Val| match (self.y_place, value) {
            (Placing::Time, Val::Num(ms)) => scale::moment(*ms),
            (Placing::Number, Val::Num(n)) => scale::readout(*n),
            (_, value) => value.key(),
        };
        let name = |series: usize| -> String {
            self.series
                .get(series)
                .cloned()
                .or_else(|| first.y.as_ref().map(Channel::title))
                .unwrap_or_default()
        };
        let shade_range = self
            .layers
            .iter()
            .flat_map(|layer| layer.data.iter().filter_map(|datum| datum.shade))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let shade_color = |value: f64| {
            let (lo, hi) = shade_range;
            let t_ = if hi > lo {
                ((value - lo) / (hi - lo)) as f32
            } else {
                1.0
            };
            mix(self.color(0), t.background, 0.12 + 0.88 * t_)
        };
        // Filled marks first, so lines and points stay on top of them.
        let mut order: Vec<&Prepared> = self.layers.iter().collect();
        order.sort_by_key(|layer| match layer.mark {
            Mark::Heatmap => 0,
            Mark::Bar => 1,
            Mark::Area => 2,
            Mark::Line => 3,
            Mark::Point => 4,
        });
        for layer in order {
            let mut by_series: Vec<Vec<&Datum>> = vec![Vec::new(); self.series.len().max(1)];
            let last = by_series.len() - 1;
            for datum in &layer.data {
                by_series[datum.series.min(last)].push(datum);
            }
            match layer.mark {
                Mark::Heatmap => {
                    let ((x_step, _), (y_step, _)) = match (&x_band, &y_band) {
                        (Some(x), Some(y)) => (x, y),
                        _ => continue,
                    };
                    for datum in &layer.data {
                        let (Some(x), Some(y), Some(shade)) = (&datum.x, &datum.y, datum.shade) else {
                            continue;
                        };
                        let (Some(cx), Some(cy)) = (px(x), py(y)) else { continue };
                        let rect = Rect {
                            x: cx - x_step / 2.0 + 0.5,
                            y: cy - y_step / 2.0 + 0.5,
                            w: x_step - 1.0,
                            h: y_step - 1.0,
                        };
                        let _ = write!(
                            svg,
                            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="1.5" fill="{}"/>"#,
                            rect.x,
                            rect.y,
                            rect.w,
                            rect.h,
                            hex(shade_color(shade))
                        );
                        let label = first.color.as_ref().map(Channel::title).unwrap_or_default();
                        hits.push(Hit::Box {
                            rect,
                            title: format!("{} · {}", x_text(x), y_text(y)),
                            row: Reading {
                                color: shade_color(shade),
                                label,
                                value: scale::readout(shade),
                                at: None,
                            },
                        });
                    }
                }
                Mark::Bar => {
                    // Categories along one axis, values along the other,
                    // stacked by series.
                    let Some((step, _)) = (if self.across { &y_band } else { &x_band }) else {
                        continue;
                    };
                    let thick = (step * 0.72).min(48.0);
                    let mut stack: HashMap<String, (f64, f64)> = HashMap::new();
                    for (series, data) in by_series.iter().enumerate() {
                        for datum in data {
                            let (cat, value) = if self.across {
                                (&datum.y, &datum.x)
                            } else {
                                (&datum.x, &datum.y)
                            };
                            let (Some(cat), Some(value)) = (cat, value) else {
                                continue;
                            };
                            let center = if self.across { py(cat) } else { px(cat) };
                            let Some(center) = center else { continue };
                            let value = value.num();
                            let pile = stack.entry(cat.key()).or_default();
                            let base = if value >= 0.0 { pile.1 } else { pile.0 };
                            let end = base + value;
                            if value >= 0.0 {
                                pile.1 = end;
                            } else {
                                pile.0 = end;
                            }
                            let rect = if self.across {
                                let (a, b) = (x_scale.at(base), x_scale.at(end));
                                Rect {
                                    x: a.min(b),
                                    y: center - thick / 2.0,
                                    w: (a - b).abs(),
                                    h: thick,
                                }
                            } else {
                                let (a, b) = (y_scale.at(base), y_scale.at(end));
                                Rect {
                                    x: center - thick / 2.0,
                                    y: a.min(b),
                                    w: thick,
                                    h: (a - b).abs(),
                                }
                            };
                            let color = self.color(series);
                            let _ = write!(
                                svg,
                                r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="2" fill="{}"/>"#,
                                rect.x,
                                rect.y,
                                rect.w,
                                rect.h,
                                hex(color)
                            );
                            hits.push(Hit::Box {
                                rect,
                                title: cat.key(),
                                row: Reading {
                                    color,
                                    label: name(series),
                                    value: scale::readout(value),
                                    at: None,
                                },
                            });
                        }
                    }
                }
                Mark::Line | Mark::Area | Mark::Point => {
                    let stacking = layer.mark == Mark::Area && self.series.len() > 1;
                    let mut floor: HashMap<String, f64> = HashMap::new();
                    for (series, data) in by_series.iter().enumerate() {
                        let color = self.color(series);
                        let mut points: Vec<(f32, f32, &Datum, f64)> = Vec::new();
                        for datum in data {
                            let (Some(x), Some(y)) = (&datum.x, &datum.y) else {
                                continue;
                            };
                            let base = if stacking {
                                *floor.get(&x.key()).unwrap_or(&0.0)
                            } else {
                                0.0
                            };
                            let top = base + y.num();
                            if stacking {
                                floor.insert(x.key(), top);
                            }
                            let y_at = if stacking { Some(y_scale.at(top)) } else { py(y) };
                            let (Some(cx), Some(cy)) = (px(x), y_at) else { continue };
                            points.push((cx, cy, datum, base));
                        }
                        if layer.mark != Mark::Point {
                            points.sort_by(|a, b| a.0.total_cmp(&b.0));
                        }
                        let path: Vec<String> = points.iter().map(|(x, y, ..)| format!("{x:.1},{y:.1}")).collect();
                        match layer.mark {
                            Mark::Area if !points.is_empty() => {
                                let mut outline = path.join(" ");
                                for (x, _, _, base) in points.iter().rev() {
                                    let _ = write!(outline, " {x:.1},{:.1}", y_scale.at(*base));
                                }
                                let _ = write!(
                                    svg,
                                    r#"<polygon points="{outline}" fill="{}" fill-opacity="0.28"/>"#,
                                    hex(color)
                                );
                                let _ = write!(
                                    svg,
                                    r#"<polyline points="{}" fill="none" stroke="{}" stroke-width="1.6" stroke-linejoin="round"/>"#,
                                    path.join(" "),
                                    hex(color)
                                );
                            }
                            Mark::Line => {
                                let _ = write!(
                                    svg,
                                    r#"<polyline points="{}" fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>"#,
                                    path.join(" "),
                                    hex(color)
                                );
                                if label_lines && let Some((x, y, ..)) = points.last() {
                                    let _ =
                                        write!(svg, r#"<circle cx="{x:.1}" cy="{y:.1}" r="3" fill="{}"/>"#, hex(color));
                                    text(
                                        &mut svg,
                                        x + 8.0,
                                        y + small * 0.35,
                                        "start",
                                        color,
                                        small,
                                        &name(series),
                                    );
                                }
                            }
                            _ => {
                                for (x, y, ..) in &points {
                                    let _ = write!(
                                        svg,
                                        r#"<circle cx="{x:.1}" cy="{y:.1}" r="3.2" fill="{}" fill-opacity="0.85"/>"#,
                                        hex(color)
                                    );
                                }
                            }
                        }
                        for (x, y, datum, _) in &points {
                            let (Some(xv), Some(yv)) = (&datum.x, &datum.y) else {
                                continue;
                            };
                            let reading = Reading {
                                color,
                                label: name(series),
                                value: y_text(yv),
                                at: Some((*x, *y)),
                            };
                            if layer.mark == Mark::Point {
                                hits.push(Hit::Dot {
                                    x: *x,
                                    y: *y,
                                    title: x_text(xv),
                                    row: reading,
                                });
                                continue;
                            }
                            match sweep.iter_mut().find(|(sx, ..)| (sx - x).abs() < 0.5) {
                                Some((_, _, rows)) => rows.push(reading),
                                None => sweep.push((*x, x_text(xv), vec![reading])),
                            }
                        }
                    }
                }
            }
        }

        // Legend, across the top.
        if legend {
            let mut x = 0.0;
            if heat {
                let (lo, hi) = shade_range;
                if lo.is_finite() {
                    text(&mut svg, x, row_y, "start", t.muted, small, &scale::readout(lo));
                    x += text_width(&scale::readout(lo), small) + 6.0;
                    for i in 0..5 {
                        let value = lo + (hi - lo) * i as f64 / 4.0;
                        let _ = write!(
                            svg,
                            r#"<rect x="{x:.1}" y="{:.1}" width="14" height="{:.1}" fill="{}"/>"#,
                            row_y - small * 0.8,
                            small,
                            hex(shade_color(value))
                        );
                        x += 14.0;
                    }
                    text(&mut svg, x + 6.0, row_y, "start", t.muted, small, &scale::readout(hi));
                }
            } else {
                if let Some(title) = &self.color_title {
                    text(&mut svg, x, row_y, "start", t.muted, small, title);
                    x += text_width(title, small) + 10.0;
                }
                for (series, name) in self.series.iter().enumerate() {
                    let _ = write!(
                        svg,
                        r#"<rect x="{x:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="2" fill="{}"/>"#,
                        row_y - small * 0.75,
                        small * 0.8,
                        small * 0.8,
                        hex(self.color(series))
                    );
                    x += small * 0.8 + 5.0;
                    text(&mut svg, x, row_y, "start", t.text, small, name);
                    x += text_width(name, small) + 14.0;
                }
            }
        }
        svg.push_str("</svg>");
        sweep.sort_by(|a, b| a.0.total_cmp(&b.0));
        Chart {
            svg,
            width: self.width,
            height,
            plot,
            hits,
            sweep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme() -> Theme {
        Theme {
            background: [255, 255, 255],
            text: [20, 20, 20],
            muted: [120, 120, 120],
            grid: [230, 230, 230],
            series: vec![[30, 100, 200], [220, 90, 40], [40, 160, 80]],
            font: "Helvetica".into(),
            size: 11.0,
        }
    }

    fn none(_: &str) -> Option<Table> {
        None
    }

    fn churn() -> Value {
        json!({
            "data": {"values": [
                {"week": 1, "plan": "free", "churn": 9.8}, {"week": 2, "plan": "free", "churn": 10.4},
                {"week": 1, "plan": "pro", "churn": 4.1}, {"week": 2, "plan": "pro", "churn": 3.8}
            ]},
            "mark": "line",
            "encoding": {
                "x": {"field": "week", "type": "quantitative"},
                "y": {"field": "churn", "type": "quantitative"},
                "color": {"field": "plan", "type": "nominal"}
            }
        })
    }

    #[test]
    fn lines_are_named_at_their_ends_and_read_across() {
        let chart = draw(&churn(), &theme(), 500.0, &mut none).unwrap();
        assert!(chart.svg.starts_with("<svg") && chart.svg.ends_with("</svg>"));
        assert_eq!(chart.svg.matches("<polyline").count(), 2);
        assert!(chart.svg.contains(">free</text>") && chart.svg.contains(">pro</text>"));
        // Hovering near week 2 reads both series there.
        let right = chart.plot.x + chart.plot.w - 2.0;
        let readout = chart.readout(right, chart.plot.y + 10.0).unwrap();
        assert_eq!(readout.title, "2");
        let values: Vec<(&str, &str)> = readout
            .rows
            .iter()
            .map(|row| (row.label.as_str(), row.value.as_str()))
            .collect();
        assert_eq!(values, [("free", "10.4"), ("pro", "3.8")]);
        assert!(readout.guide.is_some());
        assert!(chart.readout(1.0, 1.0).is_none(), "nothing outside the plot");
    }

    #[test]
    fn bars_stack_by_series_and_aggregate() {
        let spec = json!({
            "data": {"columns": ["plan", "kind", "n"], "rows": [
                ["free", "a", 2], ["free", "a", 3], ["free", "b", 1], ["pro", "a", 4]
            ]},
            "mark": "bar",
            "encoding": {
                "x": {"field": "plan", "type": "nominal"},
                "y": {"field": "n", "aggregate": "sum"},
                "color": {"field": "kind"}
            }
        });
        let chart = draw(&spec, &theme(), 400.0, &mut none).unwrap();
        let bars: Vec<(&str, &str, &str)> = chart
            .hits
            .iter()
            .filter_map(|hit| match hit {
                Hit::Box { title, row, .. } => Some((title.as_str(), row.label.as_str(), row.value.as_str())),
                Hit::Dot { .. } => None,
            })
            .collect();
        assert_eq!(bars, [("free", "a", "5"), ("pro", "a", "4"), ("free", "b", "1")]);
        // The stack's top bar sits on the one below it.
        let rects: Vec<&Rect> = chart
            .hits
            .iter()
            .filter_map(|hit| match hit {
                Hit::Box { rect, .. } => Some(rect),
                _ => None,
            })
            .collect();
        assert!((rects[2].y + rects[2].h - rects[0].y).abs() < 0.2);
        // And a legend names the series.
        assert!(chart.svg.contains(">kind</text>"));
    }

    #[test]
    fn horizontal_bars_points_areas_and_heatmaps_draw() {
        let across = json!({
            "data": {"values": [{"n": 3, "who": "a"}, {"n": 5, "who": "b"}]},
            "mark": "bar",
            "encoding": {"x": {"field": "n", "type": "quantitative"}, "y": {"field": "who", "type": "nominal"}}
        });
        let chart = draw(&across, &theme(), 400.0, &mut none).unwrap();
        let widths: Vec<f32> = chart
            .hits
            .iter()
            .filter_map(|hit| match hit {
                Hit::Box { rect, .. } => Some(rect.w),
                _ => None,
            })
            .collect();
        assert!(widths[1] > widths[0]);

        let points = json!({
            "data": {"values": [{"a": 1, "b": 2}, {"a": 3, "b": 1}]},
            "mark": "point",
            "encoding": {"x": {"field": "a"}, "y": {"field": "b"}}
        });
        let chart = draw(&points, &theme(), 400.0, &mut none).unwrap();
        assert_eq!(chart.svg.matches("<circle").count(), 2);
        let Some(Hit::Dot { x, y, .. }) = chart.hits.first() else {
            panic!("no dots")
        };
        assert_eq!(chart.readout(x + 2.0, *y).unwrap().rows[0].value, "2");

        let area = json!({
            "data": {"values": [{"d": "2024-01-01", "v": 1}, {"d": "2024-03-01", "v": 4}]},
            "mark": "area",
            "encoding": {"x": {"field": "d", "type": "temporal"}, "y": {"field": "v"}}
        });
        let chart = draw(&area, &theme(), 400.0, &mut none).unwrap();
        assert!(chart.svg.contains("<polygon"));
        // Two months across 400 points: a tick a fortnight, on Mondays' dates.
        assert!(chart.svg.contains(">Jan 15</text>"), "{}", chart.svg);

        let heat = json!({
            "data": {"values": [{"x": "a", "y": "p", "v": 1}, {"x": "b", "y": "p", "v": 9}]},
            "mark": "rect",
            "encoding": {"x": {"field": "x"}, "y": {"field": "y"}, "color": {"field": "v", "type": "quantitative"}}
        });
        let chart = draw(&heat, &theme(), 400.0, &mut none).unwrap();
        let Some(Hit::Box { rect, row, .. }) = chart.hits.get(1) else {
            panic!("no cells")
        };
        assert_eq!(row.value, "9");
        assert_eq!(chart.readout(rect.x + 1.0, rect.y + 1.0).unwrap().title, "b · p");
    }

    #[test]
    fn bad_specs_say_why() {
        let missing = json!({"data": {"values": [{"a": 1}]}, "mark": "line", "encoding": {"x": {"field": "b"}}});
        assert!(draw(&missing, &theme(), 300.0, &mut none).unwrap_err().contains("`b`"));
        let empty = json!({"data": {"values": []}, "mark": "line", "encoding": {"x": {"field": "b"}}});
        assert!(draw(&empty, &theme(), 300.0, &mut none).is_err());
    }
}
