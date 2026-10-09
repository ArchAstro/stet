//! The calls the web editor makes, one method each. Every method builds a
//! `Request`, hands it to the `Transport`, and reads the JSON that comes back.

use crate::convert::{Document, Hosted};
use crate::error::{Error, Result};
use crate::http::{Method, Request, Response, Transport, Ureq};
use crate::image;
use serde_json::{Value, json};
use std::cell::Cell;
use std::fmt;

const PROFILE_URL: &str = "https://substack.com/api/v1/user/profile/self";

/// Cloudflare turns away clients that do not look like a browser.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

/// The session cookie's value, as the browser stores it (usually still
/// percent-encoded, starting `s%3A`). Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Cookie(String);

impl Cookie {
    /// Accepts the bare value, or `substack.sid=value` pasted from DevTools.
    pub fn new(value: &str) -> Result<Cookie> {
        let value = value.trim();
        let value = value
            .strip_prefix("substack.sid=")
            .or_else(|| value.strip_prefix("connect.sid="))
            .unwrap_or(value);
        let bad = |c: char| c.is_whitespace() || c.is_control() || matches!(c, ';' | ',' | '"' | '\\');
        if value.is_empty() || value.contains(bad) {
            return Err(Error::Config(
                "the cookie must be the bare value of substack.sid: no spaces, quotes or semicolons".into(),
            ));
        }
        Ok(Cookie(value.to_string()))
    }

    /// Sent under both names reported for the session cookie; the wrong one is ignored.
    fn header(&self) -> String {
        format!("substack.sid={0}; connect.sid={0}", self.0)
    }
}

impl fmt::Debug for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cookie(<redacted>)")
    }
}

/// `https://name.substack.com` or a custom domain, without a trailing slash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    origin: String,
}

impl Publication {
    /// Takes `name`, `name.substack.com`, `news.example.com` or a full URL.
    pub fn new(input: &str) -> Result<Publication> {
        let lower = input.trim().to_ascii_lowercase();
        let host = lower.trim_start_matches("https://").trim_start_matches("http://");
        let host = host.split(['/', '?', '#']).next().unwrap_or("").to_string();
        let ok = !host.is_empty()
            && !host.starts_with(['.', '-'])
            && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
        if !ok {
            return Err(Error::Config(format!("{input:?} is not a publication name or domain")));
        }
        let host = if host.contains('.') {
            host
        } else {
            format!("{host}.substack.com")
        };
        Ok(Publication {
            origin: format!("https://{host}"),
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationInfo {
    pub id: Option<u64>,
    pub name: String,
    pub subdomain: String,
    pub custom_domain: Option<String>,
    /// Where its editor and API live: the custom domain when there is one.
    pub origin: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub user_id: u64,
    pub publications: Vec<PublicationInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedImage {
    pub url: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

impl From<UploadedImage> for Hosted {
    fn from(image: UploadedImage) -> Hosted {
        Hosted {
            url: image.url,
            width: image.width,
            height: image.height,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRef {
    pub id: u64,
    pub edit_url: String,
}

/// What a draft call writes. On update, only the `Some` fields are sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    /// A `{"type":"doc",...}` tree; sent as a JSON string.
    pub body: Option<Value>,
}

impl From<&Document> for Fields {
    fn from(doc: &Document) -> Fields {
        Fields {
            title: doc.title.clone(),
            subtitle: doc.subtitle.clone(),
            body: Some(doc.body.clone()),
        }
    }
}

pub struct Client<T: Transport = Ureq> {
    transport: T,
    cookie: Cookie,
    publication: Publication,
    user_agent: String,
    /// The account's own profile lists this publication. Until it does, the
    /// cookie goes to Substack's hosts and nowhere else.
    confirmed: Cell<bool>,
}

impl<T: Transport> fmt::Debug for Client<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("publication", &self.publication)
            .finish_non_exhaustive()
    }
}

impl Client<Ureq> {
    pub fn new(cookie: &str, publication: &str) -> Result<Self> {
        Self::with_transport(Ureq::default(), Cookie::new(cookie)?, Publication::new(publication)?)
    }

    /// With the cookie from the login keychain, where `keychain::store` put it.
    pub fn from_keychain(publication: &str) -> Result<Self> {
        Self::with_transport(
            Ureq::default(),
            crate::keychain::cookie()?,
            Publication::new(publication)?,
        )
    }
}

impl<T: Transport> Client<T> {
    pub fn with_transport(transport: T, cookie: Cookie, publication: Publication) -> Result<Self> {
        Ok(Client {
            transport,
            cookie,
            publication,
            user_agent: USER_AGENT.into(),
            confirmed: Cell::new(false),
        })
    }

    pub fn user_agent(mut self, user_agent: &str) -> Self {
        self.user_agent = user_agent.to_string();
        self
    }

    pub fn publication(&self) -> &Publication {
        &self.publication
    }

    /// GET substack.com/api/v1/user/profile/self: the user id and their publications.
    pub fn profile(&self) -> Result<Profile> {
        let value = self.call(Method::Get, PROFILE_URL, "https://substack.com/", None)?;
        let user_id = value["id"]
            .as_u64()
            .ok_or_else(|| Error::Response("the profile has no numeric id".into()))?;
        let mut publications: Vec<PublicationInfo> = Vec::new();
        let mut add = |p: &Value| {
            if let Some(info) = publication_info(p)
                && !publications.iter().any(|seen| seen.origin == info.origin)
            {
                publications.push(info);
            }
        };
        for entry in value["publicationUsers"].as_array().into_iter().flatten() {
            add(&entry["publication"]);
        }
        add(&value["primaryPublication"]);
        if publications.iter().any(|info| self.is_mine(info)) {
            self.confirmed.set(true);
        }
        Ok(Profile { user_id, publications })
    }

    pub fn publications(&self) -> Result<Vec<PublicationInfo>> {
        Ok(self.profile()?.publications)
    }

    /// POST {pub}/api/v1/image with `{"image": "data:...;base64,..."}`.
    pub fn upload_image(&self, bytes: &[u8]) -> Result<UploadedImage> {
        let data_uri = image::data_uri(bytes)?;
        let value = self.publication_call(
            Method::Post,
            "/api/v1/image",
            "/publish/post",
            Some(json!({"image": data_uri})),
        )?;
        let url = value["url"]
            .as_str()
            .filter(|url| url.starts_with("http"))
            .ok_or_else(|| Error::Response("the image upload returned no url".into()))?;
        Ok(UploadedImage {
            url: url.to_string(),
            width: value["imageWidth"].as_u64(),
            height: value["imageHeight"].as_u64(),
        })
    }

    /// POST {pub}/api/v1/drafts. The draft is not published.
    pub fn create_draft(&self, user_id: u64, fields: &Fields, audience: &str) -> Result<DraftRef> {
        let mut body = json!({
            "draft_title": fields.title.clone().unwrap_or_default(),
            "draft_bylines": [{"id": user_id, "is_guest": false}],
            "audience": audience,
        });
        if let Some(subtitle) = &fields.subtitle {
            body["draft_subtitle"] = json!(subtitle);
        }
        if let Some(doc) = &fields.body {
            body["draft_body"] = json!(doc.to_string());
        }
        let value = self.publication_call(Method::Post, "/api/v1/drafts", "/publish/post", Some(body))?;
        self.draft_ref(&value)
    }

    /// PUT {pub}/api/v1/drafts/{id} with only the fields given.
    pub fn update_draft(&self, id: u64, fields: &Fields) -> Result<DraftRef> {
        let mut body = json!({});
        if let Some(title) = &fields.title {
            body["draft_title"] = json!(title);
        }
        if let Some(subtitle) = &fields.subtitle {
            body["draft_subtitle"] = json!(subtitle);
        }
        if let Some(doc) = &fields.body {
            body["draft_body"] = json!(doc.to_string());
        }
        let value = self.publication_call(
            Method::Put,
            &format!("/api/v1/drafts/{id}"),
            "/publish/post",
            Some(body),
        )?;
        self.draft_ref(&value).or_else(|_| Ok(self.edit_ref(id)))
    }

    /// GET {pub}/api/v1/drafts/{id}: the draft as stored, for comparing with what was sent.
    pub fn get_draft(&self, id: u64) -> Result<Value> {
        self.publication_call(Method::Get, &format!("/api/v1/drafts/{id}"), "/publish/post", None)
    }

    /// GET {pub}/api/v1/drafts/{id}/prepublish: Substack's own checks, as returned.
    pub fn prepublish(&self, id: u64) -> Result<Value> {
        self.publication_call(
            Method::Get,
            &format!("/api/v1/drafts/{id}/prepublish"),
            "/publish/post",
            None,
        )
    }

    /// POST {pub}/api/v1/drafts/{id}/publish. Makes the post public. `send_email`
    /// mails the whole list, and neither can be undone.
    pub fn publish(&self, id: u64, send_email: bool) -> Result<Value> {
        let body = json!({"send": send_email});
        self.publication_call(
            Method::Post,
            &format!("/api/v1/drafts/{id}/publish"),
            "/publish/post",
            Some(body),
        )
    }

    pub fn edit_url(&self, id: u64) -> String {
        format!("{}/publish/post/{id}", self.publication.origin)
    }

    fn edit_ref(&self, id: u64) -> DraftRef {
        DraftRef {
            id,
            edit_url: self.edit_url(id),
        }
    }

    fn draft_ref(&self, value: &Value) -> Result<DraftRef> {
        let id = value["id"]
            .as_u64()
            .ok_or_else(|| Error::Response("the draft has no numeric id".into()))?;
        Ok(self.edit_ref(id))
    }

    fn publication_call(&self, method: Method, path: &str, referer: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{path}", self.publication.origin);
        let referer = format!("{}{referer}", self.publication.origin);
        self.call(method, &url, &referer, body)
    }

    fn call(&self, method: Method, url: &str, referer: &str, body: Option<Value>) -> Result<Value> {
        // A mistyped custom domain is somebody else's server: it gets no
        // cookie until Substack itself has said the domain is this account's.
        let host = url.strip_prefix("https://").and_then(|rest| rest.split('/').next());
        let substack = host.is_some_and(|host| host == "substack.com" || host.ends_with(".substack.com"));
        if !substack && !self.confirmed.get() {
            return Err(Error::Config(format!(
                "{} is not a Substack host, and the account's profile has not been seen to list it; \
                 call profile() first, so the cookie is only sent to a domain that is yours",
                host.unwrap_or(url)
            )));
        }
        let request = self.request(method, url, referer, body);
        let response = self.transport.send(&request)?;
        read(&response)
    }

    /// The request for a call, as it will be sent.
    pub fn request(&self, method: Method, url: &str, referer: &str, body: Option<Value>) -> Request {
        let mut headers = vec![
            ("Cookie".to_string(), self.cookie.header()),
            ("User-Agent".to_string(), self.user_agent.clone()),
            ("Accept".to_string(), "application/json, text/plain, */*".to_string()),
            // Without one, Substack may bounce an API call to the custom domain.
            ("Referer".to_string(), referer.to_string()),
        ];
        if body.is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        Request {
            method,
            url: url.to_string(),
            headers,
            body: body.map(|b| b.to_string()),
        }
    }

    /// Whether `info` is this client's publication.
    pub(crate) fn is_mine(&self, info: &PublicationInfo) -> bool {
        info.origin == self.publication.origin
            || format!("https://{}.substack.com", info.subdomain) == self.publication.origin
    }
}

fn publication_info(p: &Value) -> Option<PublicationInfo> {
    let subdomain = p["subdomain"].as_str().filter(|s| !s.is_empty())?.to_string();
    let custom_domain = p["custom_domain"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let origin = match &custom_domain {
        Some(domain) => format!("https://{domain}"),
        None => format!("https://{subdomain}.substack.com"),
    };
    Some(PublicationInfo {
        id: p["id"].as_u64(),
        name: p["name"].as_str().unwrap_or(&subdomain).to_string(),
        subdomain,
        custom_domain,
        origin,
    })
}

/// Turns an answer into JSON, or the most useful error for why not.
fn read(response: &Response) -> Result<Value> {
    let status = response.status;
    if (300..400).contains(&status) {
        let to = response.header("location").unwrap_or("somewhere else").to_string();
        return Err(Error::Redirect { status, to });
    }
    if is_challenge(response) {
        return Err(Error::Challenge { status });
    }
    if status == 401 || status == 403 {
        return Err(Error::CookieRejected { status });
    }
    if !(200..300).contains(&status) {
        let body: String = response.body.trim().chars().take(300).collect();
        return Err(Error::Status { status, body });
    }
    if response.body.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&response.body).map_err(|e| {
        let head: String = response.body.trim().chars().take(80).collect();
        Error::Response(format!("not JSON ({e}): {head}"))
    })
}

fn is_challenge(response: &Response) -> bool {
    if response
        .header("cf-mitigated")
        .is_some_and(|v| v.eq_ignore_ascii_case("challenge"))
    {
        return true;
    }
    let body = response.body.trim_start();
    let html = body.starts_with('<');
    let marker = body.contains("Just a moment")
        || body.contains("cf-chl")
        || body.contains("challenge-platform")
        || body.contains("Attention Required! | Cloudflare");
    html && marker
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::Mock;

    fn client(mock: &Mock) -> Client<&Mock> {
        Client::with_transport(
            mock,
            Cookie::new("s%3Aabc.def").unwrap(),
            Publication::new("Demo").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn publication_forms() {
        for input in [
            "demo",
            "demo.substack.com",
            "https://demo.substack.com/",
            "HTTP://demo.substack.com/p/x?y",
        ] {
            assert_eq!(
                Publication::new(input).unwrap().origin(),
                "https://demo.substack.com",
                "{input}"
            );
        }
        assert_eq!(
            Publication::new("https://news.example.com/archive").unwrap().origin(),
            "https://news.example.com"
        );
        assert!(Publication::new("").is_err());
        assert!(Publication::new("a b").is_err());
        assert!(Publication::new("evil.com@x").is_err());
    }

    #[test]
    fn cookie_forms_and_redaction() {
        assert_eq!(Cookie::new(" substack.sid=s%3Aabc ").unwrap().0, "s%3Aabc");
        assert_eq!(
            Cookie::new("s%3Aabc").unwrap().header(),
            "substack.sid=s%3Aabc; connect.sid=s%3Aabc"
        );
        for bad in ["", "a b", "a;b", "a\r\nX: y", "\"a\""] {
            assert!(Cookie::new(bad).is_err(), "{bad:?}");
        }
        assert!(!format!("{:?}", Cookie::new("secret").unwrap()).contains("secret"));
        let mock = Mock::default();
        assert!(!format!("{:?}", client(&mock)).contains("abc"));
        let request = client(&mock).request(Method::Get, "https://x", "https://x/", None);
        assert!(!format!("{request:?}").contains("abc"));
    }

    #[test]
    fn profile_request_and_parse() {
        let mock = Mock::default().reply(
            200,
            r#"{"id":42,"publicationUsers":[
                {"publication":{"id":7,"name":"Demo","subdomain":"demo","custom_domain":null}},
                {"publication":{"id":8,"name":"News","subdomain":"news","custom_domain":"news.example.com"}}],
               "primaryPublication":{"id":7,"name":"Demo","subdomain":"demo"}}"#,
        );
        let profile = client(&mock).profile().unwrap();
        assert_eq!(profile.user_id, 42);
        assert_eq!(profile.publications.len(), 2);
        assert_eq!(profile.publications[1].origin, "https://news.example.com");
        let sent = mock.sent.borrow();
        assert_eq!(sent[0].method, Method::Get);
        assert_eq!(sent[0].url, "https://substack.com/api/v1/user/profile/self");
        assert_eq!(
            sent[0].header("cookie"),
            Some("substack.sid=s%3Aabc.def; connect.sid=s%3Aabc.def")
        );
        assert_eq!(sent[0].header("user-agent"), Some(USER_AGENT));
        assert_eq!(sent[0].header("referer"), Some("https://substack.com/"));
        assert_eq!(sent[0].header("content-type"), None);
        assert_eq!(sent[0].body, None);
    }

    #[test]
    fn upload_request_and_parse() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
        let mock = Mock::default().reply(
            200,
            r#"{"id":1,"url":"https://substack-post-media.s3.amazonaws.com/x.png","contentType":"image/png","bytes":11,"imageWidth":640,"imageHeight":480}"#,
        );
        let up = client(&mock).upload_image(&png).unwrap();
        assert_eq!(up.url, "https://substack-post-media.s3.amazonaws.com/x.png");
        assert_eq!((up.width, up.height), (Some(640), Some(480)));
        let sent = mock.sent.borrow();
        assert_eq!(sent[0].method, Method::Post);
        assert_eq!(sent[0].url, "https://demo.substack.com/api/v1/image");
        assert_eq!(sent[0].header("content-type"), Some("application/json"));
        assert_eq!(
            sent[0].header("referer"),
            Some("https://demo.substack.com/publish/post")
        );
        let body: Value = serde_json::from_str(sent[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["image"], "data:image/png;base64,iVBORw0KGgoBAgM=");
    }

    #[test]
    fn upload_rejects_non_images_before_sending() {
        let mock = Mock::default();
        assert!(matches!(client(&mock).upload_image(b"%PDF-1.4"), Err(Error::Image(_))));
        assert!(mock.sent.borrow().is_empty());
    }

    #[test]
    fn create_draft_request() {
        let mock = Mock::default().reply(200, r#"{"id":99,"draft_title":"T"}"#);
        let fields = Fields {
            title: Some("T".into()),
            subtitle: Some("S".into()),
            body: Some(json!({"type": "doc", "content": [{"type": "paragraph"}]})),
        };
        let draft = client(&mock).create_draft(42, &fields, "everyone").unwrap();
        assert_eq!(
            draft,
            DraftRef {
                id: 99,
                edit_url: "https://demo.substack.com/publish/post/99".into()
            }
        );
        let sent = mock.sent.borrow();
        assert_eq!(
            (sent[0].method, sent[0].url.as_str()),
            (Method::Post, "https://demo.substack.com/api/v1/drafts")
        );
        let body: Value = serde_json::from_str(sent[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["draft_title"], "T");
        assert_eq!(body["draft_subtitle"], "S");
        assert_eq!(body["draft_bylines"], json!([{"id": 42, "is_guest": false}]));
        assert_eq!(body["audience"], "everyone");
        // draft_body is a JSON string, not an object.
        let inner: Value = serde_json::from_str(body["draft_body"].as_str().unwrap()).unwrap();
        assert_eq!(inner["type"], "doc");
        assert_eq!(body.as_object().unwrap().len(), 5);
    }

    #[test]
    fn create_draft_omits_absent_fields() {
        let mock = Mock::default().reply(200, r#"{"id":1}"#);
        client(&mock)
            .create_draft(
                1,
                &Fields {
                    title: Some("T".into()),
                    ..Fields::default()
                },
                "everyone",
            )
            .unwrap();
        let body: Value = serde_json::from_str(mock.sent.borrow()[0].body.as_ref().unwrap()).unwrap();
        assert!(body.get("draft_subtitle").is_none() && body.get("draft_body").is_none());
    }

    #[test]
    fn update_draft_sends_only_given_fields() {
        let mock = Mock::default().reply(200, r#"{"id":5}"#);
        let fields = Fields {
            subtitle: Some("new".into()),
            ..Fields::default()
        };
        let draft = client(&mock).update_draft(5, &fields).unwrap();
        assert_eq!(draft.edit_url, "https://demo.substack.com/publish/post/5");
        let sent = mock.sent.borrow();
        assert_eq!(
            (sent[0].method, sent[0].url.as_str()),
            (Method::Put, "https://demo.substack.com/api/v1/drafts/5")
        );
        assert_eq!(sent[0].body.as_deref(), Some(r#"{"draft_subtitle":"new"}"#));
    }

    #[test]
    fn prepublish_and_publish_requests() {
        let mock = Mock::default().reply(200, r#"{"ok":true}"#).reply(200, "{}");
        let c = client(&mock);
        c.prepublish(5).unwrap();
        c.publish(5, false).unwrap();
        let sent = mock.sent.borrow();
        assert_eq!(
            (sent[0].method, sent[0].url.as_str()),
            (Method::Get, "https://demo.substack.com/api/v1/drafts/5/prepublish")
        );
        assert_eq!(
            (sent[1].method, sent[1].url.as_str()),
            (Method::Post, "https://demo.substack.com/api/v1/drafts/5/publish")
        );
        assert_eq!(sent[1].body.as_deref(), Some(r#"{"send":false}"#));
    }

    #[test]
    fn custom_domain_requests_go_to_that_domain() {
        let profile = |domain: &str| {
            format!(
                r#"{{"id":1,"publicationUsers":[{{"publication":{{"subdomain":"news","custom_domain":"{domain}"}}}}]}}"#
            )
        };
        let mock = Mock::default()
            .reply(200, &profile("news.example.com"))
            .reply(200, r#"{"id":3}"#);
        let c = Client::with_transport(
            &mock,
            Cookie::new("c").unwrap(),
            Publication::new("news.example.com").unwrap(),
        )
        .unwrap();
        // Not before the account's profile has named the domain.
        let early = c.create_draft(1, &Fields::default(), "everyone").unwrap_err();
        assert!(early.to_string().contains("news.example.com is not a Substack host"));
        assert!(mock.sent.borrow().is_empty());
        c.profile().unwrap();
        let draft = c.create_draft(1, &Fields::default(), "everyone").unwrap();
        assert_eq!(mock.sent.borrow()[1].url, "https://news.example.com/api/v1/drafts");
        assert_eq!(draft.edit_url, "https://news.example.com/publish/post/3");

        // A domain the profile does not list never sees the cookie.
        let mock = Mock::default().reply(200, &profile("news.example.com"));
        let c = Client::with_transport(
            &mock,
            Cookie::new("c").unwrap(),
            Publication::new("news.exampel.com").unwrap(),
        )
        .unwrap();
        c.profile().unwrap();
        assert!(c.create_draft(1, &Fields::default(), "everyone").is_err());
        assert_eq!(mock.sent.borrow().len(), 1);
        assert_eq!(
            mock.sent.borrow()[0].url,
            "https://substack.com/api/v1/user/profile/self"
        );
    }

    fn error_for(status: u16, headers: &[(&str, &str)], body: &str) -> Error {
        let response = Response {
            status,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.into(),
        };
        read(&response).unwrap_err()
    }

    #[test]
    fn errors_are_specific() {
        assert!(matches!(
            error_for(401, &[], "{}"),
            Error::CookieRejected { status: 401 }
        ));
        assert!(matches!(
            error_for(403, &[], r#"{"error":"Not authorized"}"#),
            Error::CookieRejected { status: 403 }
        ));
        assert!(matches!(
            error_for(403, &[], "<!DOCTYPE html><title>Just a moment...</title>"),
            Error::Challenge { status: 403 }
        ));
        assert!(matches!(
            error_for(403, &[("CF-Mitigated", "challenge")], ""),
            Error::Challenge { .. }
        ));
        assert!(matches!(
            error_for(
                200,
                &[],
                "<html><script src=\"/cdn-cgi/challenge-platform/x\"></script>"
            ),
            Error::Challenge { .. }
        ));
        match error_for(301, &[("Location", "https://news.example.com/api/v1/drafts")], "") {
            Error::Redirect { to, .. } => assert_eq!(to, "https://news.example.com/api/v1/drafts"),
            other => panic!("{other:?}"),
        }
        match error_for(400, &[], &"x".repeat(1000)) {
            Error::Status { status: 400, body } => assert_eq!(body.len(), 300),
            other => panic!("{other:?}"),
        }
        assert!(matches!(error_for(200, &[], "<html>hi</html>"), Error::Response(_)));
        assert!(
            error_for(401, &[], "")
                .to_string()
                .contains("the cookie is not accepted")
        );
    }

    #[test]
    fn missing_ids_are_response_errors() {
        let mock = Mock::default().reply(200, "{}").reply(200, r#"{"url":"nope"}"#);
        let c = client(&mock);
        assert!(matches!(
            c.create_draft(1, &Fields::default(), "everyone"),
            Err(Error::Response(_))
        ));
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert!(matches!(c.upload_image(&png), Err(Error::Response(_))));
    }
}
