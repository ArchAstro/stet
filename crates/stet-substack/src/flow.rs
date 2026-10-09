//! The whole trip for one Markdown file: convert, upload its pictures, create
//! the draft. Nothing here publishes.

use crate::client::{Client, DraftRef, Fields};
use crate::convert::{self, Document, Options};
use crate::error::{Error, Result};
use crate::http::Transport;
use crate::image;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DraftOptions {
    pub convert: Options,
    /// `everyone`, `only_paid`, `founding` or `only_free`.
    pub audience: String,
}

impl Default for DraftOptions {
    fn default() -> Self {
        DraftOptions {
            convert: Options::default(),
            audience: "everyone".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub draft: DraftRef,
    pub uploaded_images: usize,
    /// What the conversion approximated or dropped.
    pub warnings: Vec<String>,
}

/// Reads `path`, resolves its pictures beside it, and makes a draft of it.
pub fn create_draft_from_file<T: Transport>(
    client: &Client<T>,
    path: &Path,
    options: &DraftOptions,
) -> Result<Created> {
    let markdown = std::fs::read_to_string(path).map_err(|e| Error::File(path.to_path_buf(), e))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let fallback = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled");
    create_draft_from_markdown(client, &markdown, dir, fallback, options)
}

/// Request order: GET the profile, POST each distinct local image, POST the draft.
/// Everything that can fail locally (a missing or unsupported picture) fails first.
pub fn create_draft_from_markdown<T: Transport>(
    client: &Client<T>,
    markdown: &str,
    dir: &Path,
    fallback_title: &str,
    options: &DraftOptions,
) -> Result<Created> {
    let mut document = convert::convert(markdown, &options.convert);
    let pictures = read_pictures(&document, dir)?;

    let profile = client.profile()?;
    if !profile.publications.is_empty() && !profile.publications.iter().any(|p| client.is_mine(p)) {
        let names: Vec<&str> = profile.publications.iter().map(|p| p.origin.as_str()).collect();
        return Err(Error::Config(format!(
            "{} is not one of this account's publications: {}",
            client.publication().origin(),
            names.join(", ")
        )));
    }
    for (src, bytes) in &pictures {
        let uploaded = client.upload_image(bytes)?;
        document.set_image(src, &uploaded.into());
    }

    let mut fields = Fields::from(&document);
    fields.title = fields.title.or_else(|| Some(fallback_title.to_string()));
    let draft = client.create_draft(profile.user_id, &fields, &options.audience)?;
    Ok(Created {
        draft,
        uploaded_images: pictures.len(),
        warnings: document.warnings,
    })
}

fn read_pictures(document: &Document, dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for src in document.local_images() {
        let path = resolve(&src, dir);
        let bytes = std::fs::read(&path).map_err(|e| Error::File(path.clone(), e))?;
        image::data_uri(&bytes).map_err(|e| Error::Image(format!("{}: {e}", path.display())))?;
        out.push((src, bytes));
    }
    Ok(out)
}

/// A Markdown image address as a path: `file://` dropped, `%20` and friends
/// decoded, relative to the document unless absolute.
pub fn resolve(src: &str, dir: &Path) -> PathBuf {
    let src = src.strip_prefix("file://").unwrap_or(src);
    let decoded = percent_decode(src.split(['?', '#']).next().unwrap_or(src));
    let path = PathBuf::from(decoded);
    if path.is_absolute() { path } else { dir.join(path) }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Cookie, Publication};
    use crate::http::mock::Mock;
    use serde_json::Value;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const PROFILE: &str = r#"{"id":42,"publicationUsers":[{"publication":{"id":7,"name":"Demo","subdomain":"demo"}}]}"#;
    const UPLOAD: &str =
        r#"{"url":"https://substack-post-media.s3.amazonaws.com/x.png","imageWidth":10,"imageHeight":20}"#;

    fn client(mock: &Mock) -> Client<&Mock> {
        Client::with_transport(mock, Cookie::new("c").unwrap(), Publication::new("demo").unwrap()).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stet-substack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_paths() {
        let dir = Path::new("/docs");
        assert_eq!(resolve("a/b%20c.png", dir), Path::new("/docs/a/b c.png"));
        assert_eq!(resolve("/abs/x.png", dir), Path::new("/abs/x.png"));
        assert_eq!(resolve("file:///abs/x.png?raw=1", dir), Path::new("/abs/x.png"));
        assert_eq!(resolve("100%.png", dir), Path::new("/docs/100%.png"));
    }

    #[test]
    fn whole_flow_in_order() {
        let dir = scratch("flow");
        std::fs::write(dir.join("my pic.png"), PNG).unwrap();
        let md = "# Title\n\nHello ![x](my%20pic.png) and ![y](my%20pic.png)\n\n![remote](https://e.example/r.png)\n";
        let mock = Mock::default()
            .reply(200, PROFILE)
            .reply(200, UPLOAD)
            .reply(200, r#"{"id":77}"#);
        let created =
            create_draft_from_markdown(&client(&mock), md, &dir, "fallback", &DraftOptions::default()).unwrap();
        assert_eq!(created.draft.id, 77);
        assert_eq!(created.draft.edit_url, "https://demo.substack.com/publish/post/77");
        assert_eq!(created.uploaded_images, 1);

        let sent = mock.sent.borrow();
        let steps: Vec<(&str, &str)> = sent.iter().map(|r| (r.method.as_str(), r.url.as_str())).collect();
        assert_eq!(
            steps,
            [
                ("GET", "https://substack.com/api/v1/user/profile/self"),
                ("POST", "https://demo.substack.com/api/v1/image"),
                ("POST", "https://demo.substack.com/api/v1/drafts"),
            ]
        );
        let body: Value = serde_json::from_str(sent[2].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["draft_title"], "Title");
        assert_eq!(body["draft_bylines"][0]["id"], 42);
        let doc: Value = serde_json::from_str(body["draft_body"].as_str().unwrap()).unwrap();
        let text = doc.to_string();
        assert!(!text.contains("my%20pic.png"), "{text}");
        assert_eq!(text.matches("substack-post-media.s3.amazonaws.com/x.png").count(), 2);
        assert!(text.contains("https://e.example/r.png"));
        assert!(!text.contains("publish"), "a draft never publishes");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_picture_fails_before_any_request() {
        let dir = scratch("missing");
        let mock = Mock::default();
        let err = create_draft_from_markdown(&client(&mock), "![](nope.png)", &dir, "t", &DraftOptions::default())
            .unwrap_err();
        assert!(matches!(err, Error::File(..)), "{err}");
        assert!(mock.sent.borrow().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn non_image_fails_before_any_request() {
        let dir = scratch("notimage");
        std::fs::write(dir.join("x.png"), b"hello").unwrap();
        let mock = Mock::default();
        let err =
            create_draft_from_markdown(&client(&mock), "![](x.png)", &dir, "t", &DraftOptions::default()).unwrap_err();
        assert!(matches!(err, Error::Image(_)), "{err}");
        assert!(mock.sent.borrow().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn untitled_documents_take_the_file_name() {
        let mock = Mock::default().reply(200, PROFILE).reply(200, r#"{"id":1}"#);
        create_draft_from_markdown(
            &client(&mock),
            "text",
            Path::new("."),
            "notes",
            &DraftOptions::default(),
        )
        .unwrap();
        let body: Value = serde_json::from_str(mock.sent.borrow()[1].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["draft_title"], "notes");
    }

    #[test]
    fn someone_elses_publication_is_refused() {
        let mock = Mock::default().reply(200, PROFILE);
        let other =
            Client::with_transport(&mock, Cookie::new("c").unwrap(), Publication::new("other").unwrap()).unwrap();
        let err = create_draft_from_markdown(&other, "x", Path::new("."), "t", &DraftOptions::default()).unwrap_err();
        assert!(err.to_string().contains("https://demo.substack.com"), "{err}");
        assert_eq!(mock.sent.borrow().len(), 1);
    }

    #[test]
    fn a_rejected_cookie_stops_the_flow() {
        let mock = Mock::default().reply(401, "{}");
        let err =
            create_draft_from_markdown(&client(&mock), "x", Path::new("."), "t", &DraftOptions::default()).unwrap_err();
        assert!(matches!(err, Error::CookieRejected { status: 401 }));
    }
}
