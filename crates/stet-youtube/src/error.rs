use std::fmt;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Something handed in is unusable, or the keychain is not there; nothing was sent.
    Config(String),
    /// A local file could not be read or written.
    File(PathBuf, std::io::Error),
    /// The request never got an answer.
    Network(String),
    /// Google no longer accepts the sign-in: revoked, lapsed, or for another client.
    SignedOut(String),
    /// The browser sign-in did not finish: refused, timed out or answered wrongly.
    SignIn(String),
    /// Any other non-2xx answer; `body` is trimmed.
    Status { status: u16, body: String },
    /// A 2xx answer that is not what was expected.
    Response(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(message) => write!(f, "{message}"),
            Error::File(path, error) => write!(f, "cannot read {}: {error}", path.display()),
            Error::Network(message) => write!(f, "network error: {message}"),
            Error::SignedOut(why) => write!(f, "Google no longer accepts the YouTube sign-in ({why}); sign in again"),
            Error::SignIn(message) => write!(f, "{message}"),
            Error::Status { status, body } => write!(f, "YouTube answered HTTP {status}: {body}"),
            Error::Response(message) => write!(f, "unexpected response: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// The answer's own error message, or its first few hundred bytes.
pub(crate) fn summary(body: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let said = parsed.as_ref().and_then(|value| {
        value["error"]["message"]
            .as_str()
            .or_else(|| value["error_description"].as_str())
            .or_else(|| value["error"].as_str())
            .map(str::to_string)
    });
    said.unwrap_or_else(|| body.chars().take(300).collect::<String>().trim().to_string())
}
