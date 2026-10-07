//! Images referenced by the document, loaded and decoded off the UI thread.
//! Local files, `data:` URIs and (optionally) `http(s)` URLs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Larger images are downscaled before upload.
const MAX_SIDE: u32 = 4096;
/// Downloads stop here.
const MAX_BYTES: u64 = 32 << 20;

pub struct Decoded {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

enum State {
    Loading,
    Failed,
    Ready { id: u64, width: u32, height: u32 },
}

/// Where an image comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    File(PathBuf),
    Url(String),
    /// A `data:` URI, whole.
    Data(String),
}

pub struct Images {
    states: HashMap<Source, State>,
    next_id: u64,
    sender: Sender<(Source, Option<Decoded>)>,
    receiver: Receiver<(Source, Option<Decoded>)>,
    /// Wakes the event loop when a decode finishes.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Downloads are kept here; `None` fetches every time.
    cache: Option<PathBuf>,
    /// Decode on the calling thread (screenshots).
    pub blocking: bool,
}

/// Resolves a markdown image destination. Remote URLs resolve only when
/// `remote` allows contacting servers.
pub fn resolve(url: &str, document: Option<&Path>, remote: bool) -> Option<Source> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return remote.then(|| Source::Url(url.to_string()));
    }
    if url.starts_with("data:") {
        return Some(Source::Data(url.to_string()));
    }
    let path = percent_decode(url.strip_prefix("file://").unwrap_or(url));
    // `file:///C:/pictures/a.png` leaves `/C:/pictures/a.png`.
    let drive = |path: &str| path.len() > 2 && path.as_bytes()[1].is_ascii_alphabetic() && path.as_bytes()[2] == b':';
    let path = if cfg!(windows) && path.starts_with('/') && drive(&path) {
        path[1..].to_string()
    } else {
        path
    };
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Some(Source::File(path));
    }
    Some(Source::File(document?.parent()?.join(path)))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes.get(at + 1..at + 3).and_then(|hex| std::str::from_utf8(hex).ok());
        match (bytes[at], hex.and_then(|hex| u8::from_str_radix(hex, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        bits = bits << 6 | value as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

#[cfg(feature = "remote-images")]
fn fetch(url: &str) -> Option<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    let mut response = agent.get(url).header("User-Agent", "md").call().ok()?;
    response.body_mut().with_config().limit(MAX_BYTES).read_to_vec().ok()
}

#[cfg(not(feature = "remote-images"))]
fn fetch(_: &str) -> Option<Vec<u8>> {
    None
}

fn load(source: &Source, cache: Option<&Path>) -> Option<Vec<u8>> {
    match source {
        Source::File(path) => {
            if std::fs::metadata(path).ok()?.len() > MAX_BYTES * 4 {
                return None;
            }
            std::fs::read(path).ok()
        }
        Source::Data(uri) => {
            let (header, payload) = uri.split_once(',')?;
            if header.ends_with(";base64") {
                base64_decode(payload)
            } else {
                Some(percent_decode(payload).into_bytes())
            }
        }
        Source::Url(url) => {
            let cached = cache.map(|dir| {
                let hash = url.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
                    (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
                });
                dir.join("images").join(format!("{hash:016x}"))
            });
            if let Some(bytes) = cached.as_ref().and_then(|file| std::fs::read(file).ok()) {
                return Some(bytes);
            }
            let bytes = fetch(url)?;
            if let Some(file) = cached {
                let _ = std::fs::create_dir_all(file.parent()?);
                let _ = std::fs::write(file, &bytes);
            }
            Some(bytes)
        }
    }
}

fn decode(bytes: &[u8], id: u64) -> Option<Decoded> {
    let image = image::load_from_memory(bytes).ok()?;
    let image = if image.width().max(image.height()) > MAX_SIDE {
        image.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Some(Decoded {
        id,
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

impl Images {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>, cache: Option<PathBuf>) -> Images {
        let (sender, receiver) = channel();
        Images {
            states: HashMap::new(),
            next_id: 1,
            sender,
            receiver,
            wake,
            cache,
            blocking: false,
        }
    }

    /// `(id, width, height)` once decoded; starts loading on first request.
    pub fn get(&mut self, source: &Source) -> Option<(u64, u32, u32)> {
        match self.states.get(source) {
            Some(State::Ready { id, width, height }) => return Some((*id, *width, *height)),
            Some(_) => return None,
            None => {}
        }
        let id = self.next_id;
        self.next_id += 1;
        self.states.insert(source.clone(), State::Loading);
        let (sender, wake, owned, cache) = (
            self.sender.clone(),
            self.wake.clone(),
            source.clone(),
            self.cache.clone(),
        );
        let work = move || {
            let decoded = load(&owned, cache.as_deref()).and_then(|bytes| decode(&bytes, id));
            let _ = sender.send((owned, decoded));
            wake();
        };
        if self.blocking {
            work();
        } else {
            std::thread::spawn(work);
        }
        None
    }

    /// Finished decodes, ready for upload. Empty when nothing changed.
    pub fn poll(&mut self) -> Vec<Decoded> {
        let mut ready = Vec::new();
        while let Ok((source, decoded)) = self.receiver.try_recv() {
            let state = match &decoded {
                Some(image) => State::Ready {
                    id: image.id,
                    width: image.width,
                    height: image.height,
                },
                None => State::Failed,
            };
            self.states.insert(source, state);
            ready.extend(decoded);
        }
        ready
    }

    /// Forget failures so edited or newly created files are retried.
    pub fn retry_failed(&mut self) {
        self.states.retain(|_, state| !matches!(state, State::Failed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_destinations() {
        let doc = Path::new("/notes/a.md");
        let file = |path: &str| Some(Source::File(PathBuf::from(path)));
        assert_eq!(resolve("img/x%20y.png", Some(doc), true), file("/notes/img/x y.png"));
        // What counts as an absolute path is the platform's call.
        if cfg!(windows) {
            assert_eq!(resolve("C:/abs.png", None, true), file("C:/abs.png"));
            assert_eq!(resolve("file:///C:/abs.png", Some(doc), true), file("C:/abs.png"));
        } else {
            assert_eq!(resolve("/abs.png", None, true), file("/abs.png"));
            assert_eq!(resolve("file:///abs.png", Some(doc), true), file("/abs.png"));
        }
        assert_eq!(
            resolve("https://example.com/x.png", Some(doc), true),
            Some(Source::Url("https://example.com/x.png".into()))
        );
        assert_eq!(resolve("https://example.com/x.png", Some(doc), false), None);
        assert_eq!(resolve("rel.png", None, true), None);
    }

    #[test]
    fn data_uris_decode() {
        assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        let pixel = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let bytes = load(&Source::Data(pixel.into()), None).unwrap();
        let image = decode(&bytes, 7).unwrap();
        assert_eq!((image.id, image.width, image.height), (7, 1, 1));
    }
}
