//! What a run produces, in the shapes a cell's output area draws.

use jupyter_protocol::{JupyterMessageContent, Stdio};
use serde_json::{Map, Value};

/// One output in each form the kernel offers, keyed by MIME type:
/// `text/plain` always, and often `image/png`, `text/html` or
/// `image/svg+xml` beside it. Images are base64 text, as on the wire. The
/// renderer picks the richest form it can draw.
pub type Bundle = Map<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Text the code printed.
    Stream { stderr: bool, text: String },
    /// Something the code displayed: a figure, a table, rich text.
    Display(Bundle),
    /// The value of the run's last expression.
    Result(Bundle),
    /// The exception the run stopped on. The traceback lines carry ANSI colours.
    Error {
        name: String,
        value: String,
        traceback: Vec<String>,
    },
    /// Forget what this run has shown so far; with `wait`, once the next
    /// output arrives (how progress bars redraw in place).
    Clear { wait: bool },
}

impl Output {
    /// The output carried by an iopub message, if it carries one.
    pub(crate) fn from_iopub(content: &JupyterMessageContent) -> Option<Output> {
        Some(match content {
            JupyterMessageContent::StreamContent(stream) => Output::Stream {
                stderr: matches!(stream.name, Stdio::Stderr),
                text: stream.text.clone(),
            },
            JupyterMessageContent::DisplayData(display) => Output::Display(bundle(&display.data)),
            JupyterMessageContent::ExecuteResult(result) => Output::Result(bundle(&result.data)),
            JupyterMessageContent::ErrorOutput(error) => Output::Error {
                name: error.ename.clone(),
                value: error.evalue.clone(),
                traceback: error.traceback.clone(),
            },
            JupyterMessageContent::ClearOutput(clear) => Output::Clear { wait: clear.wait },
            _ => return None,
        })
    }
}

/// The MIME type of a table: `{"columns": [..], "rows": [[..], ..],
/// "total": n}`, where `rows` may hold only the first of `total` rows.
pub const TABLE: &str = "application/vnd.stet.table+json";

/// Rows shown in a table's plain-text form.
const PLAIN_ROWS: usize = 40;

/// A table bundle, with a plain-text form beside it for anything that cannot
/// draw tables.
pub fn table(columns: Vec<String>, rows: Vec<Vec<Value>>) -> Bundle {
    let cell = |value: &Value| match value {
        Value::String(text) => text.clone(),
        Value::Null => "NULL".to_string(),
        other => other.to_string(),
    };
    let shown: Vec<Vec<String>> = rows
        .iter()
        .take(PLAIN_ROWS)
        .map(|row| row.iter().map(cell).collect())
        .collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|i| {
            shown
                .iter()
                .filter_map(|row| row.get(i))
                .chain(std::iter::once(&columns[i]))
                .map(|text| text.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String]| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(text, &width)| format!("{text:<width$}"))
            .collect();
        padded.join("  ").trim_end().to_string()
    };
    let mut plain = vec![line(&columns)];
    plain.extend(shown.iter().map(|row| line(row)));
    if rows.len() > PLAIN_ROWS {
        plain.push(format!("… {} rows in all", rows.len()));
    }
    let mut bundle = Bundle::new();
    bundle.insert("text/plain".into(), Value::String(plain.join("\n")));
    bundle.insert(
        TABLE.into(),
        serde_json::json!({ "columns": columns, "total": rows.len(), "rows": rows }),
    );
    bundle
}

fn bundle(media: &jupyter_protocol::Media) -> Bundle {
    match serde_json::to_value(media) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jupyter_protocol::{DisplayData, ErrorOutput, Media, MediaType, StreamContent};

    #[test]
    fn streams_keep_their_side() {
        let out = Output::from_iopub(&JupyterMessageContent::StreamContent(StreamContent {
            name: Stdio::Stderr,
            text: "warn\n".into(),
        }));
        assert_eq!(
            out,
            Some(Output::Stream {
                stderr: true,
                text: "warn\n".into()
            })
        );
    }

    #[test]
    fn displays_become_bundles_by_mime_type() {
        let media = Media::new(vec![
            MediaType::Plain("<Figure>".into()),
            MediaType::Png("iVBOR".into()),
        ]);
        let Some(Output::Display(bundle)) =
            Output::from_iopub(&JupyterMessageContent::DisplayData(DisplayData::new(media)))
        else {
            panic!("not a display");
        };
        assert_eq!(bundle["text/plain"], "<Figure>");
        assert_eq!(bundle["image/png"], "iVBOR");
    }

    #[test]
    fn errors_keep_their_traceback() {
        let out = Output::from_iopub(&JupyterMessageContent::ErrorOutput(ErrorOutput {
            ename: "ZeroDivisionError".into(),
            evalue: "division by zero".into(),
            traceback: vec!["line 1".into()],
        }));
        assert!(matches!(out, Some(Output::Error { name, .. }) if name == "ZeroDivisionError"));
    }

    #[test]
    fn tables_carry_a_plain_form() {
        let bundle = table(
            vec!["plan".into(), "n".into()],
            vec![vec!["free".into(), 120.into()], vec!["team".into(), Value::Null]],
        );
        assert_eq!(bundle["text/plain"], "plan  n\nfree  120\nteam  NULL");
        assert_eq!(bundle[TABLE]["total"], 2);
    }

    #[test]
    fn status_is_not_output() {
        let status = JupyterMessageContent::Status(jupyter_protocol::Status::busy());
        assert_eq!(Output::from_iopub(&status), None);
    }
}
