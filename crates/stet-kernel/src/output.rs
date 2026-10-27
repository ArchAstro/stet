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
    fn status_is_not_output() {
        let status = JupyterMessageContent::Status(jupyter_protocol::Status::busy());
        assert_eq!(Output::from_iopub(&status), None);
    }
}
