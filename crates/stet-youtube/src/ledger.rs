//! What has been uploaded already, so the same video is never uploaded
//! twice: each file is known by the SHA-256 of its bytes (not its name or
//! place, which change), and maps to the video it became on which channel.
//! A small JSON file, written whole and swapped into place.

use crate::client::{Channel, Client, Privacy, VideoState};
use crate::error::{Error, Result};
use crate::http::Transport;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One upload at a time in this process, so two publishes of the same post
/// cannot both miss the ledger and both upload.
static UPLOADING: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub video: String,
    pub channel: String,
    /// The file's name when it was uploaded, for whoever reads the ledger.
    pub file: String,
}

#[derive(Debug, Default)]
pub struct Ledger {
    path: PathBuf,
    entries: Map<String, Value>,
}

impl Ledger {
    /// The ledger at `path`; empty if there is none yet.
    pub fn load(path: &Path) -> Result<Ledger> {
        let entries = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(mut root)) => match root.remove("videos") {
                    Some(Value::Object(videos)) => videos,
                    _ => Map::new(),
                },
                // Unreadable is not empty: starting over would upload everything again.
                _ => {
                    return Err(Error::Config(format!(
                        "{} is not readable as JSON; fix or remove it",
                        path.display()
                    )));
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(err) => return Err(Error::File(path.to_path_buf(), err)),
        };
        Ok(Ledger {
            path: path.to_path_buf(),
            entries,
        })
    }

    /// The video this file became on `channel`, if it was uploaded there.
    pub fn get(&self, digest: &str, channel: &str) -> Option<Entry> {
        let entry = self.entries.get(digest)?;
        let video = entry["video"].as_str()?;
        (entry["channel"].as_str() == Some(channel)).then(|| Entry {
            video: video.to_string(),
            channel: channel.to_string(),
            file: entry["file"].as_str().unwrap_or("").to_string(),
        })
    }

    /// Writes down an upload, and saves at once.
    pub fn record(&mut self, digest: &str, entry: &Entry) -> Result<()> {
        self.entries.insert(
            digest.to_string(),
            json!({"video": entry.video, "channel": entry.channel, "file": entry.file}),
        );
        self.save()
    }

    pub fn forget(&mut self, digest: &str) -> Result<()> {
        if self.entries.remove(digest).is_some() {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let failed = |err| Error::File(self.path.clone(), err);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        let text = serde_json::to_string_pretty(&json!({"videos": self.entries})).expect("JSON from JSON");
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, text + "\n").map_err(failed)?;
        std::fs::rename(&temporary, &self.path).map_err(failed)
    }
}

/// The SHA-256 of a file's bytes, read in pieces.
pub fn file_digest(path: &Path) -> Result<String> {
    let failed = |err| Error::File(path.to_path_buf(), err);
    let mut file = std::fs::File::open(path).map_err(failed)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buf).map_err(failed)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Uploaded before and still there.
    Reused(String),
    Uploaded(String),
}

impl Outcome {
    pub fn video(&self) -> &str {
        match self {
            Outcome::Reused(id) | Outcome::Uploaded(id) => id,
        }
    }
}

/// The video for this file on `channel`: the one it became before if that
/// is still there, or else a new upload, written down before returning.
pub fn upload_once<T: Transport>(
    client: &Client<T>,
    channel: &Channel,
    ledger: &Path,
    path: &Path,
    title: &str,
    privacy: Privacy,
) -> Result<Outcome> {
    let _one_at_a_time = UPLOADING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let digest = file_digest(path)?;
    // Read under the lock: an upload that just finished elsewhere is in it.
    let mut known = Ledger::load(ledger)?;
    if let Some(entry) = known.get(&digest, &channel.id) {
        match client.video_state(&entry.video, &channel.id)? {
            VideoState::Live => return Ok(Outcome::Reused(entry.video)),
            VideoState::Gone => known.forget(&digest)?,
        }
    }
    let uploaded = client.upload(path, title, privacy)?;
    let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    known.record(
        &digest,
        &Entry {
            video: uploaded.id.clone(),
            channel: channel.id.clone(),
            file,
        },
    )?;
    Ok(Outcome::Uploaded(uploaded.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::Mock;

    const SESSION: &str = "https://www.googleapis.com/upload/youtube/v3/videos?upload_id=1";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stet-youtube-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn channel() -> Channel {
        Channel {
            id: "UC1".into(),
            title: "Mine".into(),
        }
    }

    fn live(channel: &str) -> String {
        format!(r#"{{"items":[{{"snippet":{{"channelId":"{channel}"}},"status":{{"uploadStatus":"processed"}}}}]}}"#)
    }

    #[test]
    fn digests_are_sha256() {
        let dir = scratch("digest");
        let path = dir.join("a.mp4");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_digest(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_same_video_is_uploaded_once() {
        let dir = scratch("once");
        let ledger = dir.join("youtube.json");
        let video = dir.join("demo.mp4");
        std::fs::write(&video, b"frames").unwrap();

        let first = Mock::default()
            .reply_with(200, &[("Location", SESSION)], "")
            .reply(200, r#"{"id":"v1"}"#);
        let client = Client::with_token(&first, "t");
        let made = upload_once(&client, &channel(), &ledger, &video, "Post", Privacy::Unlisted).unwrap();
        assert_eq!(made, Outcome::Uploaded("v1".into()));

        // Published again, even from a copy under another name: reused, no upload.
        let copy = dir.join("renamed.mp4");
        std::fs::copy(&video, &copy).unwrap();
        let again = Mock::default().reply(200, &live("UC1"));
        let client = Client::with_token(&again, "t");
        let reused = upload_once(&client, &channel(), &ledger, &copy, "Post", Privacy::Unlisted).unwrap();
        assert_eq!(reused, Outcome::Reused("v1".into()));
        let steps = again.steps();
        assert_eq!(steps.len(), 1);
        assert!(steps[0].1.contains("/youtube/v3/videos?part=snippet,status&id=v1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_deleted_video_is_uploaded_again() {
        let dir = scratch("deleted");
        let ledger = dir.join("youtube.json");
        let video = dir.join("demo.mp4");
        std::fs::write(&video, b"frames").unwrap();
        let digest = file_digest(&video).unwrap();
        let entry = Entry {
            video: "old".into(),
            channel: "UC1".into(),
            file: "demo.mp4".into(),
        };
        Ledger::load(&ledger).unwrap().record(&digest, &entry).unwrap();

        let mock = Mock::default()
            .reply(200, r#"{"items":[]}"#)
            .reply_with(200, &[("Location", SESSION)], "")
            .reply(200, r#"{"id":"new"}"#);
        let client = Client::with_token(&mock, "t");
        let made = upload_once(&client, &channel(), &ledger, &video, "Post", Privacy::Unlisted).unwrap();
        assert_eq!(made, Outcome::Uploaded("new".into()));
        assert_eq!(Ledger::load(&ledger).unwrap().get(&digest, "UC1").unwrap().video, "new");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn another_channel_gets_its_own_upload() {
        let dir = scratch("channel");
        let ledger = dir.join("youtube.json");
        let video = dir.join("demo.mp4");
        std::fs::write(&video, b"frames").unwrap();
        let digest = file_digest(&video).unwrap();
        let entry = Entry {
            video: "theirs".into(),
            channel: "UC-other".into(),
            file: "demo.mp4".into(),
        };
        Ledger::load(&ledger).unwrap().record(&digest, &entry).unwrap();
        let mock = Mock::default()
            .reply_with(200, &[("Location", SESSION)], "")
            .reply(200, r#"{"id":"mine"}"#);
        let client = Client::with_token(&mock, "t");
        let made = upload_once(&client, &channel(), &ledger, &video, "Post", Privacy::Unlisted).unwrap();
        assert_eq!(made, Outcome::Uploaded("mine".into()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_upload_records_nothing() {
        let dir = scratch("failed");
        let ledger = dir.join("youtube.json");
        let video = dir.join("demo.mp4");
        std::fs::write(&video, b"frames").unwrap();
        let mock = Mock::default().reply(403, r#"{"error":{"message":"quotaExceeded"}}"#);
        let client = Client::with_token(&mock, "t");
        let err = upload_once(&client, &channel(), &ledger, &video, "Post", Privacy::Unlisted).unwrap_err();
        assert!(err.to_string().contains("quotaExceeded"), "{err}");
        assert!(!ledger.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreadable_ledger_stops_rather_than_starting_over() {
        let dir = scratch("broken");
        let ledger = dir.join("youtube.json");
        std::fs::write(&ledger, "{nope").unwrap();
        assert!(Ledger::load(&ledger).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
