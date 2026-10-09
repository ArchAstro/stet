use std::fmt;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// The cookie or publication handed in is unusable; nothing was sent.
    Config(String),
    /// A local file could not be read.
    File(PathBuf, std::io::Error),
    /// A local image is missing, too big or not a format Substack takes.
    Image(String),
    /// The request never got an answer.
    Network(String),
    /// 401 or 403 without a challenge page: Substack did not accept the session.
    CookieRejected { status: u16 },
    /// Cloudflare answered with a bot challenge instead of Substack.
    Challenge { status: u16 },
    /// A redirect, which is never followed so the cookie stays on the host it was meant for.
    Redirect { status: u16, to: String },
    /// Any other non-2xx answer; `body` is trimmed.
    Status { status: u16, body: String },
    /// A 2xx answer that is not the JSON that was expected.
    Response(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(message) => write!(f, "{message}"),
            Error::File(path, error) => write!(f, "cannot read {}: {error}", path.display()),
            Error::Image(message) => write!(f, "{message}"),
            Error::Network(message) => write!(f, "network error: {message}"),
            Error::CookieRejected { status } => write!(
                f,
                "the cookie is not accepted (HTTP {status}): it is expired, copied wrongly, or for another account"
            ),
            Error::Challenge { status } => write!(
                f,
                "Cloudflare answered with a challenge page (HTTP {status}) instead of Substack; \
                 open the publication in a browser, then retry, or send the browser's User-Agent"
            ),
            Error::Redirect { status, to } => write!(
                f,
                "redirected (HTTP {status}) to {to}; use that host as the publication (a custom domain?)"
            ),
            Error::Status { status, body } => write!(f, "Substack answered HTTP {status}: {body}"),
            Error::Response(message) => write!(f, "unexpected response: {message}"),
        }
    }
}

impl std::error::Error for Error {}
