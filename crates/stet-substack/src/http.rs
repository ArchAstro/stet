//! The seam between request building and the network.

use crate::error::{Error, Result};
use std::fmt;

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
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// A JSON document, sent with `Content-Type: application/json`.
    pub body: Option<String>,
}

impl Request {
    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The cookie header and the image body can be large or secret; neither is printed.
impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<_> = self
            .headers
            .iter()
            .map(|(k, v)| {
                if k.eq_ignore_ascii_case("cookie") {
                    (k.as_str(), "<redacted>")
                } else {
                    (k.as_str(), v.as_str())
                }
            })
            .collect();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body_bytes", &self.body.as_ref().map(String::len))
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
/// Redirects must not be followed: the cookie belongs to the host it was aimed at.
pub trait Transport {
    fn send(&self, request: &Request) -> Result<Response>;
}

/// `ureq`, with no redirects and no status-as-error.
pub struct Ureq {
    agent: ureq::Agent,
}

impl Default for Ureq {
    fn default() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(60)))
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
        let body = request.body.as_deref().unwrap_or("");
        let mut response = match request.method {
            Method::Get => headers!(self.agent.get(&request.url)).call().map_err(net)?,
            Method::Post => headers!(self.agent.post(&request.url)).send(body).map_err(net)?,
            Method::Put => headers!(self.agent.put(&request.url)).send(body).map_err(net)?,
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
            self.replies.borrow_mut().push_back(Response {
                status,
                headers: Vec::new(),
                body: body.into(),
            });
            self
        }
    }

    impl Transport for &Mock {
        fn send(&self, request: &Request) -> Result<Response> {
            self.sent.borrow_mut().push(request.clone());
            Ok(self.replies.borrow_mut().pop_front().expect("unexpected request"))
        }
    }
}
