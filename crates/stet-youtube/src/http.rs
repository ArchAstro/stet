//! The seam between request building and the network.

use crate::error::{Error, Result};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Body {
    None,
    /// Bytes in memory: JSON or a form. Its type is a header of the request.
    Bytes(Vec<u8>),
    /// A file, streamed rather than read in: a video can be gigabytes.
    File(PathBuf),
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Body::None => write!(f, "none"),
            Body::Bytes(bytes) => write!(f, "{} bytes", bytes.len()),
            Body::File(path) => write!(f, "{}", path.display()),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as text, for tests and for nothing that is printed.
    pub fn text(&self) -> Option<&str> {
        match &self.body {
            Body::Bytes(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }
}

/// Tokens ride in the authorization header and in form bodies; neither is printed.
impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<_> = self
            .headers
            .iter()
            .map(|(k, v)| match k.eq_ignore_ascii_case("authorization") {
                true => (k.as_str(), "<redacted>"),
                false => (k.as_str(), v.as_str()),
            })
            .collect();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body", &self.body)
            .finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Sends one request and returns whatever came back, whatever the status.
pub trait Transport {
    fn send(&self, request: &Request) -> Result<Response>;
}

/// `ureq`, with no redirects and no status-as-error. There is no overall
/// time limit, since an upload takes as long as the video is big; a
/// connection that stops answering still fails.
pub struct Ureq {
    agent: ureq::Agent,
}

impl Default for Ureq {
    fn default() -> Self {
        let minute = Some(std::time::Duration::from_secs(60));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(minute)
            .timeout_recv_response(Some(std::time::Duration::from_secs(300)))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build()
            .into();
        Ureq { agent }
    }
}

impl Transport for Ureq {
    fn send(&self, request: &Request) -> Result<Response> {
        let net = |e: ureq::Error| Error::Network(e.to_string());
        macro_rules! headers {
            ($builder:expr) => {{
                let mut builder = $builder;
                for (name, value) in &request.headers {
                    builder = builder.header(name, value);
                }
                builder
            }};
        }
        macro_rules! with_body {
            ($builder:expr) => {{
                let builder = headers!($builder);
                match &request.body {
                    Body::None => builder.send_empty().map_err(net)?,
                    Body::Bytes(bytes) => builder.send(bytes.as_slice()).map_err(net)?,
                    Body::File(path) => {
                        let file = std::fs::File::open(path).map_err(|e| Error::File(path.clone(), e))?;
                        builder.send(&file).map_err(net)?
                    }
                }
            }};
        }
        let mut response = match request.method {
            Method::Get => headers!(self.agent.get(&request.url)).call().map_err(net)?,
            Method::Post => with_body!(self.agent.post(&request.url)),
            Method::Put => with_body!(self.agent.put(&request.url)),
        };
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let body = response.body_mut().read_to_string().map_err(net)?;
        Ok(Response { status, headers, body })
    }
}

/// A transport that records requests and replays canned answers.
#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Default)]
    pub struct Mock {
        pub sent: RefCell<Vec<Request>>,
        replies: RefCell<VecDeque<Response>>,
    }

    impl Mock {
        pub fn reply(self, status: u16, body: &str) -> Self {
            self.reply_with(status, &[], body)
        }

        pub fn reply_with(self, status: u16, headers: &[(&str, &str)], body: &str) -> Self {
            self.replies.borrow_mut().push_back(Response {
                status,
                headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                body: body.into(),
            });
            self
        }

        /// Method and URL of each request sent, in order.
        pub fn steps(&self) -> Vec<(&'static str, String)> {
            self.sent
                .borrow()
                .iter()
                .map(|r| (r.method.as_str(), r.url.clone()))
                .collect()
        }
    }

    impl Transport for &Mock {
        fn send(&self, request: &Request) -> Result<Response> {
            self.sent.borrow_mut().push(request.clone());
            Ok(self.replies.borrow_mut().pop_front().expect("unexpected request"))
        }
    }
}
