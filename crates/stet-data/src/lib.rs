//! A first look at a data file: its columns and their types, how many rows
//! it has, and the first of them. For stet's data refs, which show a file's
//! shape in the prose, preview its rows, and feed charts with no kernel.
//!
//! CSV and TSV, JSON (a list of objects, or stet's `{columns, rows}`), JSON
//! lines, Parquet, Arrow IPC (Feather) and SQLite. Parquet and SQLite say
//! their row count without being read through; CSV and JSON lines are
//! counted by lines.

use serde_json::{Map, Value, json};
use std::path::Path;

/// File extensions read as data, lower case.
pub const EXTENSIONS: &[&str] = &[
    "csv", "tsv", "json", "jsonl", "ndjson", "parquet", "arrow", "feather", "ipc", "sqlite", "sqlite3", "db",
];

/// JSON files larger than this are not read whole.
const MAX_JSON: u64 = 256 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    /// `int`, `float`, `bool`, `date`, `time`, `text`, or the file's own
    /// name for anything else.
    pub kind: String,
}

/// What a file holds, as far as it was read.
#[derive(Clone, Debug, PartialEq)]
pub struct Peek {
    /// `csv`, `json`, `parquet`, `arrow` or `sqlite`.
    pub format: &'static str,
    pub columns: Vec<Column>,
    /// The first rows, each as long as `columns`.
    pub rows: Vec<Vec<Value>>,
    /// Every row, when the file says or they were counted.
    pub total: Option<u64>,
    /// A database's tables; the one read is `table`.
    pub tables: Vec<String>,
    pub table: Option<String>,
}

impl Peek {
    /// The shape stet draws tables in: `{columns, rows, total}`, with the
    /// columns' types alongside.
    pub fn to_json(&self) -> Value {
        json!({
            "columns": self.columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>(),
            "types": self.columns.iter().map(|column| column.kind.clone()).collect::<Vec<_>>(),
            "rows": self.rows,
            "total": self.total.unwrap_or(self.rows.len() as u64),
        })
    }
}

/// A path that names data: by its extension, after any `#table`.
pub fn is_data(path: &str) -> bool {
    let path = path.split('#').next().unwrap_or(path);
    let extension = Path::new(path).extension().and_then(|ext| ext.to_str()).unwrap_or("");
    EXTENSIONS.iter().any(|known| extension.eq_ignore_ascii_case(known))
}

/// Read `path`'s shape and its first `limit` rows. `table` picks a table
/// of a database (the first, if not given).
pub fn peek(path: &Path, table: Option<&str>, limit: usize) -> Result<Peek, String> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = path.display();
    if !path.exists() {
        return Err(format!("{name}: no such file"));
    }
    let result = match extension.as_str() {
        "csv" => csv_peek(path, b',', limit),
        "tsv" => csv_peek(path, b'\t', limit),
        "json" => json_peek(path, limit),
        "jsonl" | "ndjson" => lines_peek(path, limit),
        "parquet" => parquet_peek(path, limit),
        "arrow" | "feather" | "ipc" => arrow_peek(path, limit),
        "sqlite" | "sqlite3" | "db" => sqlite_peek(path, table, limit),
        other => Err(format!("stet does not read .{other} files")),
    };
    result.map_err(|err| format!("{name}: {err}"))
}

// --- Values and types ------------------------------------------------------------

/// Text from a CSV cell as the value it looks like.
fn guess(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    if let Ok(int) = trimmed.parse::<i64>() {
        return json!(int);
    }
    if let Ok(float) = trimmed.parse::<f64>()
        && float.is_finite()
        && !trimmed.eq_ignore_ascii_case("nan")
    {
        return json!(float);
    }
    match trimmed {
        "true" | "TRUE" | "True" => json!(true),
        "false" | "FALSE" | "False" => json!(false),
        _ => json!(text),
    }
}

fn looks_like_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// A column's type, from the values seen in it.
fn kind_of<'a>(values: impl Iterator<Item = &'a Value>) -> String {
    let (mut ints, mut floats, mut bools, mut dates, mut times, mut texts) = (0, 0, 0, 0, 0, 0);
    for value in values {
        match value {
            Value::Number(number) if number.is_i64() || number.is_u64() => ints += 1,
            Value::Number(_) => floats += 1,
            Value::Bool(_) => bools += 1,
            Value::String(text) if looks_like_date(text) && text.len() == 10 => dates += 1,
            Value::String(text) if looks_like_date(text) => times += 1,
            Value::Null => {}
            _ => texts += 1,
        }
    }
    let seen = ints + floats + bools + dates + times + texts;
    let kind = if seen == 0 || texts > 0 {
        "text"
    } else if ints == seen {
        "int"
    } else if ints + floats == seen {
        "float"
    } else if bools == seen {
        "bool"
    } else if dates == seen {
        "date"
    } else if dates + times == seen {
        "time"
    } else {
        "text"
    };
    kind.to_string()
}

fn columns_from(names: Vec<String>, rows: &[Vec<Value>]) -> Vec<Column> {
    names
        .into_iter()
        .enumerate()
        .map(|(at, name)| Column {
            kind: kind_of(rows.iter().filter_map(|row| row.get(at))),
            name,
        })
        .collect()
}

/// Lines in a file, as a count of rows: a newline ends each, and a last
/// line without one counts too. Quoted newlines in CSV count as rows; the
/// total is "about".
fn count_lines(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; 4 << 20];
    let (mut count, mut last) = (0u64, b'\n');
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        count += memchr::memchr_iter(b'\n', &buffer[..read]).count() as u64;
        last = buffer[read - 1];
    }
    Some(count + u64::from(last != b'\n'))
}

// --- CSV --------------------------------------------------------------------------

fn csv_peek(path: &Path, delimiter: u8, limit: usize) -> Result<Peek, String> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_path(path)
        .map_err(|err| err.to_string())?;
    let names: Vec<String> = reader
        .headers()
        .map_err(|err| err.to_string())?
        .iter()
        .map(str::to_string)
        .collect();
    let mut rows = Vec::new();
    for record in reader.records().take(limit) {
        let record = record.map_err(|err| err.to_string())?;
        let mut row: Vec<Value> = record.iter().map(guess).collect();
        row.resize(names.len(), Value::Null);
        rows.push(row);
    }
    let total = count_lines(path).map(|lines| lines.saturating_sub(1));
    Ok(Peek {
        format: "csv",
        columns: columns_from(names, &rows),
        rows,
        total,
        tables: Vec::new(),
        table: None,
    })
}

// --- JSON -------------------------------------------------------------------------

/// Rows from records (objects), columns in the order they first appear.
fn from_records<'a>(
    records: impl Iterator<Item = &'a Map<String, Value>>,
    limit: usize,
) -> (Vec<String>, Vec<Vec<Value>>) {
    let records: Vec<&Map<String, Value>> = records.take(limit).collect();
    let mut names: Vec<String> = Vec::new();
    for record in &records {
        for key in record.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
    }
    let rows = records
        .iter()
        .map(|record| {
            names
                .iter()
                .map(|name| record.get(name).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    (names, rows)
}

fn json_peek(path: &Path, limit: usize) -> Result<Peek, String> {
    let size = std::fs::metadata(path).map_err(|err| err.to_string())?.len();
    if size > MAX_JSON {
        return Err(format!("{} MB is too large to read as one JSON document", size >> 20));
    }
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let value: Value = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let (names, rows, total) = match &value {
        Value::Array(items) => {
            let (names, rows) = from_records(items.iter().filter_map(Value::as_object), limit);
            (names, rows, items.len() as u64)
        }
        Value::Object(object) if object.contains_key("columns") && object.contains_key("rows") => {
            let names = object["columns"]
                .as_array()
                .map(|columns| columns.iter().map(|c| c.as_str().unwrap_or("").to_string()).collect())
                .unwrap_or_default();
            let all = object["rows"].as_array().cloned().unwrap_or_default();
            let rows = all
                .iter()
                .take(limit)
                .map(|row| row.as_array().cloned().unwrap_or_default())
                .collect();
            (names, rows, all.len() as u64)
        }
        // pandas' `orient="columns"`: {column: {index: value}}, or {column: [values]}.
        Value::Object(object) if !object.is_empty() && object.values().all(|v| v.is_object() || v.is_array()) => {
            let names: Vec<String> = object.keys().cloned().collect();
            let column = |value: &Value| -> Vec<Value> {
                match value {
                    Value::Array(values) => values.clone(),
                    Value::Object(values) => values.values().cloned().collect(),
                    _ => Vec::new(),
                }
            };
            let columns: Vec<Vec<Value>> = object.values().map(column).collect();
            let count = columns.iter().map(Vec::len).max().unwrap_or(0);
            let rows = (0..count.min(limit))
                .map(|at| {
                    columns
                        .iter()
                        .map(|c| c.get(at).cloned().unwrap_or(Value::Null))
                        .collect()
                })
                .collect();
            (names, rows, count as u64)
        }
        _ => return Err("the JSON is not a list of records or a table".into()),
    };
    Ok(Peek {
        format: "json",
        columns: columns_from(names, &rows),
        rows,
        total: Some(total),
        tables: Vec::new(),
        table: None,
    })
}

fn lines_peek(path: &Path, limit: usize) -> Result<Peek, String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let mut records = Vec::new();
    for line in std::io::BufReader::new(file).lines().take(limit) {
        let line = line.map_err(|err| err.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(Value::Object(record)) => records.push(record),
            Ok(_) => return Err("each line should be a JSON object".into()),
            Err(err) => return Err(err.to_string()),
        }
    }
    let (names, rows) = from_records(records.iter(), limit);
    Ok(Peek {
        format: "json",
        columns: columns_from(names, &rows),
        rows,
        total: count_lines(path),
        tables: Vec::new(),
        table: None,
    })
}

// --- Arrow and Parquet ----------------------------------------------------------

fn arrow_kind(data_type: &arrow_schema::DataType) -> String {
    use arrow_schema::DataType::*;
    match data_type {
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 => "int".into(),
        Float16 | Float32 | Float64 | Decimal32(..) | Decimal64(..) | Decimal128(..) | Decimal256(..) => "float".into(),
        Boolean => "bool".into(),
        Date32 | Date64 => "date".into(),
        Timestamp(..) => "time".into(),
        Utf8 | LargeUtf8 | Utf8View => "text".into(),
        Dictionary(_, value) => arrow_kind(value),
        other => other.to_string().to_lowercase(),
    }
}

/// One cell of an Arrow column as JSON: numbers and booleans as they are,
/// everything else as Arrow writes it (dates and times in ISO 8601).
fn arrow_cell(array: &dyn arrow_array::Array, row: usize, formatter: &arrow_cast::display::ArrayFormatter) -> Value {
    use arrow_array::cast::AsArray;
    use arrow_array::types::*;
    use arrow_schema::DataType::*;
    if array.is_null(row) {
        return Value::Null;
    }
    match array.data_type() {
        Int8 => json!(array.as_primitive::<Int8Type>().value(row)),
        Int16 => json!(array.as_primitive::<Int16Type>().value(row)),
        Int32 => json!(array.as_primitive::<Int32Type>().value(row)),
        Int64 => json!(array.as_primitive::<Int64Type>().value(row)),
        UInt8 => json!(array.as_primitive::<UInt8Type>().value(row)),
        UInt16 => json!(array.as_primitive::<UInt16Type>().value(row)),
        UInt32 => json!(array.as_primitive::<UInt32Type>().value(row)),
        UInt64 => json!(array.as_primitive::<UInt64Type>().value(row)),
        Float32 => float(array.as_primitive::<Float32Type>().value(row) as f64),
        Float64 => float(array.as_primitive::<Float64Type>().value(row)),
        Boolean => json!(array.as_boolean().value(row)),
        Decimal32(..) | Decimal64(..) | Decimal128(..) | Decimal256(..) => {
            let text = formatter.value(row).to_string();
            text.parse::<f64>().map_or(json!(text), float)
        }
        _ => json!(formatter.value(row).to_string()),
    }
}

fn float(value: f64) -> Value {
    if value.is_finite() { json!(value) } else { Value::Null }
}

fn batch_rows(batches: &[arrow_array::RecordBatch], limit: usize) -> Result<Vec<Vec<Value>>, String> {
    let options = arrow_cast::display::FormatOptions::default();
    let mut rows = Vec::new();
    for batch in batches {
        let formatters: Vec<_> = batch
            .columns()
            .iter()
            .map(|column| arrow_cast::display::ArrayFormatter::try_new(column.as_ref(), &options))
            .collect::<Result<_, _>>()
            .map_err(|err| err.to_string())?;
        for row in 0..batch.num_rows() {
            if rows.len() >= limit {
                return Ok(rows);
            }
            rows.push(
                batch
                    .columns()
                    .iter()
                    .zip(&formatters)
                    .map(|(column, formatter)| arrow_cell(column.as_ref(), row, formatter))
                    .collect(),
            );
        }
    }
    Ok(rows)
}

fn schema_columns(schema: &arrow_schema::Schema) -> Vec<Column> {
    schema
        .fields()
        .iter()
        .map(|field| Column {
            name: field.name().clone(),
            kind: arrow_kind(field.data_type()),
        })
        .collect()
}

fn parquet_peek(path: &Path, limit: usize) -> Result<Peek, String> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|err| err.to_string())?;
    // The footer says how many rows there are; only the first are read.
    let total = builder.metadata().file_metadata().num_rows().max(0) as u64;
    let columns = schema_columns(builder.schema());
    let reader = builder
        .with_batch_size(limit.clamp(1, 8192))
        .with_limit(limit)
        .build()
        .map_err(|err| err.to_string())?;
    let batches: Vec<_> = reader.collect::<Result<_, _>>().map_err(|err| err.to_string())?;
    Ok(Peek {
        format: "parquet",
        columns,
        rows: batch_rows(&batches, limit)?,
        total: Some(total),
        tables: Vec::new(),
        table: None,
    })
}

fn arrow_peek(path: &Path, limit: usize) -> Result<Peek, String> {
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let (columns, batches, counted) = match arrow_ipc::reader::FileReader::try_new(file, None) {
        Ok(reader) => {
            let columns = schema_columns(&reader.schema());
            let (mut batches, mut total, mut kept) = (Vec::new(), 0u64, 0usize);
            for batch in reader {
                let batch = batch.map_err(|err| err.to_string())?;
                total += batch.num_rows() as u64;
                if kept < limit {
                    kept += batch.num_rows();
                    batches.push(batch);
                }
            }
            (columns, batches, Some(total))
        }
        // Not the file format: perhaps the stream format.
        Err(_) => {
            let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
            let reader = arrow_ipc::reader::StreamReader::try_new(std::io::BufReader::new(file), None)
                .map_err(|err| err.to_string())?;
            let columns = schema_columns(&reader.schema());
            let mut batches = Vec::new();
            let mut kept = 0;
            for batch in reader {
                let batch = batch.map_err(|err| err.to_string())?;
                kept += batch.num_rows();
                batches.push(batch);
                if kept >= limit {
                    break;
                }
            }
            (columns, batches, None)
        }
    };
    Ok(Peek {
        format: "arrow",
        columns,
        rows: batch_rows(&batches, limit)?,
        total: counted,
        tables: Vec::new(),
        table: None,
    })
}

// --- SQLite -----------------------------------------------------------------------

fn sqlite_peek(path: &Path, table: Option<&str>, limit: usize) -> Result<Peek, String> {
    use rusqlite::{Connection, OpenFlags, types::ValueRef};
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|err| err.to_string())?;
    let tables: Vec<String> = db
        .prepare(
            "select name from sqlite_master where type in ('table', 'view') and name not like 'sqlite_%' order by name",
        )
        .and_then(|mut query| query.query_map([], |row| row.get(0))?.collect())
        .map_err(|err| err.to_string())?;
    let chosen = match table {
        Some(name) if tables.iter().any(|t| t == name) => name.to_string(),
        Some(name) => return Err(format!("there is no table {name} (there are: {})", tables.join(", "))),
        None => tables.first().cloned().ok_or("the database has no tables")?,
    };
    let quoted = format!("\"{}\"", chosen.replace('"', "\"\""));
    let total: i64 = db
        .query_row(&format!("select count(*) from {quoted}"), [], |row| row.get(0))
        .map_err(|err| err.to_string())?;
    let mut query = db
        .prepare(&format!("select * from {quoted} limit {limit}"))
        .map_err(|err| err.to_string())?;
    let names: Vec<String> = query.column_names().map(str::to_string).collect();
    let declared: Vec<Option<String>> = {
        let mut info = db
            .prepare(&format!("pragma table_info({quoted})"))
            .map_err(|err| err.to_string())?;
        let types: Vec<(String, String)> = info
            .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
            .and_then(|rows| rows.collect())
            .map_err(|err| err.to_string())?;
        names
            .iter()
            .map(|name| types.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone()))
            .collect()
    };
    let width = names.len();
    let rows: Vec<Vec<Value>> = query
        .query_map([], |row| {
            (0..width)
                .map(|at| {
                    Ok(match row.get_ref(at)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(int) => json!(int),
                        ValueRef::Real(real) => float(real),
                        ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
                        ValueRef::Blob(blob) => json!(format!("({} bytes)", blob.len())),
                    })
                })
                .collect()
        })
        .and_then(|rows| rows.collect())
        .map_err(|err| err.to_string())?;
    let mut columns = columns_from(names, &rows);
    // A column with no rows to judge by takes its declared type.
    for (column, declared) in columns.iter_mut().zip(declared) {
        if rows.is_empty()
            && let Some(declared) = declared
        {
            column.kind = declared.to_lowercase();
        }
    }
    Ok(Peek {
        format: "sqlite",
        columns,
        rows,
        total: Some(total.max(0) as u64),
        tables,
        table: Some(chosen),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dir(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("stet-data-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn csv_types_and_counts() {
        let path = dir("csv").join("signups.csv");
        std::fs::write(
            &path,
            "day,plan,n,rate\n2024-01-01,free,3,0.5\n2024-01-02,pro,,1\n2024-01-03,team,7,2.25",
        )
        .unwrap();
        let peek = peek(&path, None, 2).unwrap();
        let kinds: Vec<&str> = peek.columns.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["date", "text", "int", "float"]);
        assert_eq!(peek.rows[1], [json!("2024-01-02"), json!("pro"), Value::Null, json!(1)]);
        assert_eq!((peek.rows.len(), peek.total), (2, Some(3)));
        assert_eq!(peek.to_json()["columns"], json!(["day", "plan", "n", "rate"]));
        assert!(is_data("data/signups.csv") && is_data("app.db#users") && !is_data("notes.md"));
    }

    #[test]
    fn json_shapes() {
        let records = dir("json").join("r.json");
        std::fs::write(&records, r#"[{"a": 1, "b": "x"}, {"a": 2, "c": true}]"#).unwrap();
        let peek = peek(&records, None, 10).unwrap();
        let mut names: Vec<&str> = peek.columns.iter().map(|c| c.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(peek.total, Some(2));
        let table = dir("json").join("t.json");
        std::fs::write(&table, r#"{"columns": ["x"], "rows": [[1], [2], [3]]}"#).unwrap();
        assert_eq!(super::peek(&table, None, 2).unwrap().total, Some(3));
        let lines = dir("json").join("l.jsonl");
        std::fs::write(&lines, "{\"a\": 1}\n{\"a\": 2}\n").unwrap();
        let peek = super::peek(&lines, None, 10).unwrap();
        assert_eq!((peek.rows.len(), peek.total), (2, Some(2)));
    }

    #[test]
    fn parquet_and_arrow() {
        use arrow_array::{Float64Array, Int64Array, RecordBatch, StringArray};
        use arrow_schema::{DataType, Field, Schema};
        let schema = Arc::new(Schema::new(vec![
            Field::new("plan", DataType::Utf8, false),
            Field::new("n", DataType::Int64, true),
            Field::new("rate", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["free", "pro", "team"])),
                Arc::new(Int64Array::from(vec![Some(3), None, Some(7)])),
                Arc::new(Float64Array::from(vec![0.5, 1.0, 2.25])),
            ],
        )
        .unwrap();
        let path = dir("parquet").join("p.parquet");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = parquet::arrow::ArrowWriter::try_new(file, schema.clone(), None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        let peek = peek(&path, None, 2).unwrap();
        assert_eq!((peek.format, peek.total, peek.rows.len()), ("parquet", Some(3), 2));
        assert_eq!(peek.rows[1], [json!("pro"), Value::Null, json!(1.0)]);
        assert_eq!(peek.columns[1].kind, "int");

        let path = dir("parquet").join("a.arrow");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = arrow_ipc::writer::FileWriter::try_new(file, &schema).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
        let peek = super::peek(&path, None, 10).unwrap();
        assert_eq!((peek.format, peek.total), ("arrow", Some(3)));
        assert_eq!(peek.rows[2][0], json!("team"));
    }

    #[test]
    fn sqlite_tables() {
        let path = dir("sqlite").join("app.db");
        let _ = std::fs::remove_file(&path);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "create table users(id integer, name text); insert into users values (1, 'ada'), (2, 'bo'); \
             create table empty(x real);",
        )
        .unwrap();
        drop(db);
        let peek = peek(&path, None, 10).unwrap();
        assert_eq!(peek.tables, ["empty", "users"]);
        assert_eq!(peek.table.as_deref(), Some("empty"));
        assert_eq!(peek.columns[0].kind, "real", "no rows: the declared type");
        let users = super::peek(&path, Some("users"), 1).unwrap();
        assert_eq!(
            (users.total, users.rows[0].clone()),
            (Some(2), vec![json!(1), json!("ada")])
        );
        assert!(super::peek(&path, Some("nope"), 1).unwrap_err().contains("users"));
    }

    #[test]
    fn missing_files_say_so() {
        assert!(
            peek(Path::new("/no/such/file.csv"), None, 1)
                .unwrap_err()
                .ends_with(": no such file")
        );
    }
}
