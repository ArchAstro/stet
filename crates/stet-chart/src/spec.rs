//! Reading a chart spec. The shape is Vega-Lite's, cut down: a `mark`, an
//! `encoding` with `x`, `y` and `color`, and `data`, or a `layer` of those.
//! Altair's output reads as is, its rows kept in `datasets`. Rows can also
//! come as a table (`{"columns": [..], "rows": [[..]]}`), the shape stet
//! uses for tables everywhere.

use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Line,
    Bar,
    Point,
    Area,
    /// Vega-Lite's `rect`: a grid of cells coloured by value.
    Heatmap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Quantitative,
    Temporal,
    Ordinal,
    Nominal,
}

impl Kind {
    pub fn discrete(self) -> bool {
        matches!(self, Kind::Ordinal | Kind::Nominal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    Count,
    Sum,
    Mean,
    Min,
    Max,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// Empty for `count` with no field.
    pub field: String,
    pub kind: Kind,
    pub title: Option<String>,
    pub aggregate: Option<Aggregate>,
}

impl Channel {
    pub fn title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        match (self.aggregate, self.field.as_str()) {
            (Some(Aggregate::Count), "") => "count".into(),
            (Some(aggregate), field) => format!("{} of {field}", format!("{aggregate:?}").to_lowercase()),
            (None, field) => field.to_string(),
        }
    }
}

/// Rows of named values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column == name)
    }

    /// From Vega-Lite's `values`: a list of objects. Columns are in the
    /// order they first appear.
    fn from_records(records: &[Value]) -> Table {
        let mut table = Table::default();
        for record in records.iter().filter_map(Value::as_object) {
            for key in record.keys() {
                if table.column(key).is_none() {
                    table.columns.push(key.clone());
                }
            }
        }
        for record in records.iter().filter_map(Value::as_object) {
            let row = table
                .columns
                .iter()
                .map(|column| record.get(column).cloned().unwrap_or(Value::Null))
                .collect();
            table.rows.push(row);
        }
        table
    }

    fn from_columns(value: &Value) -> Option<Table> {
        let columns = value.get("columns")?.as_array()?;
        let rows = value.get("rows")?.as_array()?;
        Some(Table {
            columns: columns.iter().map(text).collect(),
            rows: rows.iter().filter_map(|row| row.as_array().cloned()).collect(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub mark: Mark,
    pub x: Option<Channel>,
    pub y: Option<Channel>,
    pub color: Option<Channel>,
    pub data: Table,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub title: Option<String>,
    /// In points; the width is the room there is.
    pub height: Option<f32>,
    pub layers: Vec<Layer>,
}

/// A value as text: strings as they are, numbers as JSON writes them.
pub fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Where a spec names its rows: a table of its own, `data.values`,
/// `data.name` in `datasets`, or a ref (`data.ref`, `data.url`) that
/// `resolve` turns into rows.
fn table_of(
    spec: &Map<String, Value>,
    datasets: Option<&Map<String, Value>>,
    resolve: &mut dyn FnMut(&str) -> Option<Table>,
) -> Option<Table> {
    let data = spec.get("data")?;
    if let Some(table) = Table::from_columns(data) {
        return Some(table);
    }
    if let Some(values) = data.get("values").and_then(Value::as_array) {
        return Some(Table::from_records(values));
    }
    if let Some(name) = data.get("name").and_then(Value::as_str)
        && let Some(values) = datasets.and_then(|sets| sets.get(name)).and_then(Value::as_array)
    {
        return Some(Table::from_records(values));
    }
    let named = data.get("ref").or(data.get("url")).and_then(Value::as_str)?;
    resolve(named)
}

fn mark(value: &Value) -> Result<Mark, String> {
    let name = value
        .as_str()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .ok_or("the chart has no mark")?;
    Ok(match name {
        "line" | "trail" => Mark::Line,
        "bar" => Mark::Bar,
        "point" | "circle" | "square" | "tick" => Mark::Point,
        "area" => Mark::Area,
        "rect" | "heatmap" => Mark::Heatmap,
        other => return Err(format!("stet does not draw `{other}` marks yet")),
    })
}

fn channel(value: Option<&Value>) -> Option<Channel> {
    let value = value?.as_object()?;
    let aggregate = value.get("aggregate").and_then(Value::as_str).and_then(|name| {
        Some(match name {
            "count" => Aggregate::Count,
            "sum" => Aggregate::Sum,
            "mean" | "average" => Aggregate::Mean,
            "min" => Aggregate::Min,
            "max" => Aggregate::Max,
            _ => return None,
        })
    });
    let field = value.get("field").and_then(Value::as_str).unwrap_or("").to_string();
    if field.is_empty() && aggregate != Some(Aggregate::Count) {
        return None;
    }
    let kind = match value.get("type").and_then(Value::as_str) {
        Some("temporal") => Kind::Temporal,
        Some("ordinal") => Kind::Ordinal,
        Some("nominal") => Kind::Nominal,
        Some(_) => Kind::Quantitative,
        None if aggregate.is_some() => Kind::Quantitative,
        // No type: decided from the values once they are read.
        None => Kind::Nominal,
    };
    let title = value.get("title").and_then(Value::as_str).map(str::to_string);
    Some(Channel {
        field,
        kind,
        title,
        aggregate,
    })
}

/// Read `value` as a chart. `resolve` loads rows a spec names but does not
/// carry (a data ref).
pub fn parse(value: &Value, resolve: &mut dyn FnMut(&str) -> Option<Table>) -> Result<Spec, String> {
    let top = value.as_object().ok_or("a chart spec is a JSON object")?;
    let datasets = top.get("datasets").and_then(Value::as_object);
    let shared = table_of(top, datasets, resolve);
    let shared_encoding = top.get("encoding");
    let mut parts: Vec<&Map<String, Value>> = Vec::new();
    match top.get("layer").and_then(Value::as_array) {
        Some(layers) => parts.extend(layers.iter().filter_map(Value::as_object)),
        None => parts.push(top),
    }
    let mut layers = Vec::new();
    for part in parts {
        let encoding = |key: &str| {
            part.get("encoding")
                .and_then(|encoding| encoding.get(key))
                .or_else(|| shared_encoding.and_then(|encoding| encoding.get(key)))
        };
        // A spec with no layers is its own one layer: its data is read once.
        let own = if std::ptr::eq(part, top) {
            None
        } else {
            table_of(part, datasets, resolve)
        };
        let data = own.or_else(|| shared.clone()).ok_or("the chart has no data")?;
        let mut layer = Layer {
            mark: mark(part.get("mark").ok_or("the chart has no mark")?)?,
            x: channel(encoding("x")),
            y: channel(encoding("y")),
            color: channel(encoding("color")),
            data,
        };
        for channel in [&mut layer.x, &mut layer.y, &mut layer.color].into_iter().flatten() {
            infer_kind(channel, &layer.data);
        }
        if layer.x.is_none() && layer.y.is_none() {
            return Err("the chart encodes neither x nor y".into());
        }
        layers.push(layer);
    }
    let title = match top.get("title") {
        Some(Value::String(title)) => Some(title.clone()),
        Some(title) => title.get("text").and_then(Value::as_str).map(str::to_string),
        None => None,
    };
    let height = top.get("height").and_then(Value::as_f64).map(|height| height as f32);
    Ok(Spec { title, height, layers })
}

/// A channel with no `type` is quantitative when all its values are numbers.
fn infer_kind(channel: &mut Channel, data: &Table) {
    if channel.kind != Kind::Nominal || channel.aggregate.is_some() {
        return;
    }
    let Some(column) = data.column(&channel.field) else {
        return;
    };
    let mut values = data
        .rows
        .iter()
        .filter_map(|row| row.get(column))
        .filter(|v| !v.is_null());
    if values.clone().next().is_some() && values.all(Value::is_number) {
        channel.kind = Kind::Quantitative;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn none(_: &str) -> Option<Table> {
        None
    }

    #[test]
    fn reads_altair_output() {
        let spec = json!({
            "$schema": "https://vega.github.io/schema/vega-lite/v5.json",
            "data": {"name": "data-1"},
            "datasets": {"data-1": [{"week": "W1", "churn": 2.5}, {"week": "W2", "churn": 3}]},
            "mark": {"type": "bar"},
            "encoding": {"x": {"field": "week", "type": "nominal"}, "y": {"field": "churn", "type": "quantitative"}},
            "title": "Churn"
        });
        let spec = parse(&spec, &mut none).unwrap();
        assert_eq!(spec.title.as_deref(), Some("Churn"));
        let layer = &spec.layers[0];
        assert_eq!(layer.mark, Mark::Bar);
        let (week, churn) = (layer.data.column("week").unwrap(), layer.data.column("churn").unwrap());
        assert_eq!(
            (&layer.data.rows[1][week], &layer.data.rows[1][churn]),
            (&json!("W2"), &json!(3))
        );
        assert_eq!(layer.y.as_ref().unwrap().kind, Kind::Quantitative);
    }

    #[test]
    fn layers_share_data_and_encoding() {
        let spec = json!({
            "data": {"columns": ["x", "y"], "rows": [[1, 2], [2, 4]]},
            "encoding": {"x": {"field": "x"}, "y": {"field": "y"}},
            "layer": [{"mark": "line"}, {"mark": "point"}]
        });
        let spec = parse(&spec, &mut none).unwrap();
        assert_eq!(spec.layers.len(), 2);
        assert_eq!(spec.layers[1].mark, Mark::Point);
        // No type given, and every value a number.
        assert_eq!(spec.layers[0].x.as_ref().unwrap().kind, Kind::Quantitative);
    }

    #[test]
    fn data_refs_are_resolved_by_the_caller() {
        let spec = json!({"data": {"ref": "signups"}, "mark": "line", "encoding": {"x": {"field": "d"}}});
        let mut asked = Vec::new();
        let mut resolve = |name: &str| {
            asked.push(name.to_string());
            Some(Table {
                columns: vec!["d".into()],
                rows: vec![vec![json!(1)]],
            })
        };
        assert!(parse(&spec, &mut resolve).is_ok());
        assert_eq!(asked, ["signups"]);
        assert!(parse(&json!({"mark": "arc", "data": {"values": []}}), &mut none).is_err());
    }
}
