//! The YouTube Data API v3, one method per call.

use crate::error::{Error, Result, summary};
use crate::http::{Body, Method, Request, Response, Transport, Ureq};
use crate::keychain::Credentials;
use crate::oauth;
use serde_json::{Value, json};
use std::path::Path;

const API: &str = "https://www.googleapis.com/youtube/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status";
/// YouTube's own limit on a title.
const TITLE_CHARS: usize = 100;

/// Who sees an uploaded video.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Privacy {
    /// Anyone with the address, which is what an embed needs.
    #[default]
    Unlisted,
    Public,
    Private,
}

impl Privacy {
    pub const ALL: [Privacy; 3] = [Privacy::Unlisted, Privacy::Public, Privacy::Private];

    pub fn as_str(self) -> &'static str {
        match self {
            Privacy::Unlisted => "unlisted",
            Privacy::Public => "public",
            Privacy::Private => "private",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Privacy::Unlisted => "Unlisted",
            Privacy::Public => "Public",
            Privacy::Private => "Private",
        }
    }

    pub fn parse(text: &str) -> Option<Privacy> {
        Privacy::ALL.into_iter().find(|privacy| privacy.as_str() == text)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedVideo {
    pub id: String,
}

/// What became of a video that was uploaded before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoState {
    /// Still there, on this channel, and not rejected.
    Live,
    /// Deleted, rejected, failed, or not this channel's to reuse.
    Gone,
}

pub struct Client<T: Transport = Ureq> {
    transport: T,
    access_token: String,
}

impl Client<Ureq> {
    /// Signed in with what the keychain holds.
    pub fn from_keychain() -> Result<Self> {
        Self::signed_in(Ureq::default(), &crate::keychain::credentials()?)
    }
}

impl<T: Transport> Client<T> {
    /// Trades the refresh token for an access token, once.
    pub fn signed_in(transport: T, credentials: &Credentials) -> Result<Self> {
        let access_token = oauth::access_token(&transport, credentials)?;
        Ok(Client {
            transport,
            access_token,
        })
    }

    pub fn with_token(transport: T, access_token: &str) -> Self {
        Client {
            transport,
            access_token: access_token.to_string(),
        }
    }

    fn send(&self, method: Method, url: &str, headers: Vec<(String, String)>, body: Body) -> Result<Response> {
        let mut all = vec![("Authorization".to_string(), format!("Bearer {}", self.access_token))];
        all.extend(headers);
        let response = self.transport.send(&Request {
            method,
            url: url.to_string(),
            headers: all,
            body,
        })?;
        match response.status {
            200..=299 => Ok(response),
            401 => Err(Error::SignedOut(summary(&response.body))),
            status => Err(Error::Status {
                status,
                body: summary(&response.body),
            }),
        }
    }

    fn json(response: &Response) -> Result<Value> {
        serde_json::from_str(&response.body).map_err(|_| Error::Response("the answer is not JSON".into()))
    }

    /// GET channels?mine=true: the channel videos go to.
    pub fn channel(&self) -> Result<Channel> {
        let response = self.send(
            Method::Get,
            &format!("{API}/channels?part=snippet&mine=true"),
            Vec::new(),
            Body::None,
        )?;
        let value = Self::json(&response)?;
        let item = &value["items"][0];
        let id = item["id"].as_str().ok_or_else(|| {
            Error::Config("this Google account has no YouTube channel; make one on youtube.com first".into())
        })?;
        Ok(Channel {
            id: id.to_string(),
            title: item["snippet"]["title"].as_str().unwrap_or(id).to_string(),
        })
    }

    /// GET videos?id=: whether an earlier upload can be used again.
    pub fn video_state(&self, id: &str, channel: &str) -> Result<VideoState> {
        let url = format!("{API}/videos?part=snippet,status&id={}", oauth::encode(id));
        let value = Self::json(&self.send(Method::Get, &url, Vec::new(), Body::None)?)?;
        let item = &value["items"][0];
        let ours = item["snippet"]["channelId"].as_str() == Some(channel);
        let broken = matches!(
            item["status"]["uploadStatus"].as_str(),
            Some("deleted" | "failed" | "rejected")
        );
        Ok(match item.is_object() && ours && !broken {
            true => VideoState::Live,
            false => VideoState::Gone,
        })
    }

    /// The resumable upload: POST the details for a session address, then
    /// PUT the file there. The file is streamed from disk.
    pub fn upload(&self, path: &Path, title: &str, privacy: Privacy) -> Result<UploadedVideo> {
        let size = std::fs::metadata(path)
            .map_err(|err| Error::File(path.to_path_buf(), err))?
            .len();
        let mime = mime(path);
        let details = json!({
            "snippet": {"title": clean_title(title, path)},
            "status": {"privacyStatus": privacy.as_str(), "selfDeclaredMadeForKids": false},
        });
        let started = self.send(
            Method::Post,
            UPLOAD,
            vec![
                ("Content-Type".into(), "application/json; charset=UTF-8".into()),
                ("X-Upload-Content-Length".into(), size.to_string()),
                ("X-Upload-Content-Type".into(), mime.into()),
            ],
            Body::Bytes(details.to_string().into_bytes()),
        )?;
        let session = started
            .header("location")
            .filter(|url| url.starts_with("https://www.googleapis.com/"))
            .ok_or_else(|| Error::Response("YouTube gave no upload address".into()))?
            .to_string();
        let done = self.send(
            Method::Put,
            &session,
            vec![("Content-Type".into(), mime.into())],
            Body::File(path.to_path_buf()),
        )?;
        let value = Self::json(&done)?;
        let id = value["id"]
            .as_str()
            .ok_or_else(|| Error::Response("the upload answer has no video id".into()))?;
        Ok(UploadedVideo { id: id.to_string() })
    }
}

/// A title YouTube takes: no angle brackets, at most 100 characters, never empty.
fn clean_title(title: &str, path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Video");
    let title = match title.trim() {
        "" => stem,
        title => title,
    };
    let clean: String = title
        .chars()
        .filter(|c| !matches!(c, '<' | '>'))
        .take(TITLE_CHARS)
        .collect();
    match clean.trim() {
        "" => "Video".to_string(),
        clean => clean.to_string(),
    }
}

fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::Mock;

    fn client(mock: &Mock) -> Client<&Mock> {
        Client::with_token(mock, "ya29.token")
    }

    #[test]
    fn the_channel_is_read() {
        let mock = Mock::default().reply(
            200,
            r#"{"items":[{"id":"UC1","snippet":{"title":"The Building Machine"}}]}"#,
        );
        let channel = client(&mock).channel().unwrap();
        assert_eq!(
            channel,
            Channel {
                id: "UC1".into(),
                title: "The Building Machine".into()
            }
        );
        let sent = mock.sent.borrow();
        assert_eq!(
            sent[0].url,
            "https://www.googleapis.com/youtube/v3/channels?part=snippet&mine=true"
        );
        assert_eq!(sent[0].header("authorization"), Some("Bearer ya29.token"));
    }

    #[test]
    fn an_account_without_a_channel_says_so() {
        let mock = Mock::default().reply(200, r#"{"items":[]}"#);
        assert!(
            client(&mock)
                .channel()
                .unwrap_err()
                .to_string()
                .contains("no YouTube channel")
        );
    }

    #[test]
    fn video_state_checks_owner_and_status() {
        let live = r#"{"items":[{"snippet":{"channelId":"UC1"},"status":{"uploadStatus":"processed"}}]}"#;
        let other = r#"{"items":[{"snippet":{"channelId":"UC2"},"status":{"uploadStatus":"processed"}}]}"#;
        let rejected = r#"{"items":[{"snippet":{"channelId":"UC1"},"status":{"uploadStatus":"rejected"}}]}"#;
        let mock = Mock::default()
            .reply(200, live)
            .reply(200, other)
            .reply(200, rejected)
            .reply(200, r#"{"items":[]}"#);
        let c = client(&mock);
        assert_eq!(c.video_state("abc", "UC1").unwrap(), VideoState::Live);
        assert_eq!(c.video_state("abc", "UC1").unwrap(), VideoState::Gone);
        assert_eq!(c.video_state("abc", "UC1").unwrap(), VideoState::Gone);
        assert_eq!(c.video_state("abc", "UC1").unwrap(), VideoState::Gone);
        assert!(
            mock.sent.borrow()[0]
                .url
                .ends_with("/videos?part=snippet,status&id=abc")
        );
    }

    #[test]
    fn upload_is_resumable_and_streams_the_file() {
        let dir = std::env::temp_dir().join(format!("stet-youtube-client-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let session = "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&upload_id=X";
        let mock = Mock::default()
            .reply_with(200, &[("Location", session)], "")
            .reply(200, r#"{"id":"vid123"}"#);
        let uploaded = client(&mock).upload(&path, "A <b>post</b>", Privacy::Unlisted).unwrap();
        assert_eq!(uploaded.id, "vid123");
        let sent = mock.sent.borrow();
        assert_eq!(sent[0].method, Method::Post);
        assert_eq!(sent[0].url, UPLOAD);
        assert_eq!(sent[0].header("x-upload-content-length"), Some("10"));
        assert_eq!(sent[0].header("x-upload-content-type"), Some("video/mp4"));
        let details: Value = serde_json::from_str(sent[0].text().unwrap()).unwrap();
        assert_eq!(details["snippet"]["title"], "A bpost/b");
        assert_eq!(details["status"]["privacyStatus"], "unlisted");
        assert_eq!(sent[1].method, Method::Put);
        assert_eq!(sent[1].url, session);
        assert_eq!(sent[1].body, Body::File(path.clone()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_upload_address_elsewhere_is_refused() {
        let dir = std::env::temp_dir().join(format!("stet-youtube-elsewhere-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.mp4");
        std::fs::write(&path, b"x").unwrap();
        let mock = Mock::default().reply_with(200, &[("Location", "https://evil.example/u")], "");
        assert!(client(&mock).upload(&path, "t", Privacy::Unlisted).is_err());
        assert_eq!(mock.sent.borrow().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn titles_are_cleaned() {
        let path = Path::new("/x/clip.mov");
        assert_eq!(clean_title("  ", path), "clip");
        assert_eq!(clean_title("<>", path), "Video");
        assert_eq!(clean_title(&"a".repeat(150), path).len(), 100);
    }

    #[test]
    fn a_401_means_signed_out() {
        let mock = Mock::default().reply(401, r#"{"error":{"message":"Invalid Credentials"}}"#);
        let err = client(&mock).channel().unwrap_err();
        assert!(matches!(err, Error::SignedOut(_)), "{err}");
    }
}
