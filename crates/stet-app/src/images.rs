//! Images referenced by the document, loaded and decoded off the UI thread.
//! Local files, `data:` URIs and (optionally) `http(s)` URLs. A video is
//! shown as one frame of itself with a play button on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Larger images are downscaled before upload.
const MAX_SIDE: u32 = 4096;
/// Larger sources are not decoded at all.
const MAX_DECODE: u32 = 16384;
/// Downloads stop here.
const MAX_BYTES: u64 = 32 << 20;
/// A texture not drawn for this long is freed.
const KEEP: Duration = Duration::from_secs(60);
/// Over this many bytes of textures, the longest unseen go first...
const BUDGET: u64 = 512 << 20;
/// ...but nothing drawn within this long, so scrolling never reloads.
const RECENT: Duration = Duration::from_secs(2);
/// The longer side of a video's frame.
const POSTER_SIDE: u32 = 1280;

pub struct Decoded {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

enum State {
    Loading,
    Failed,
    /// Decoded and uploaded; `used` is when it was last drawn.
    Ready {
        id: u64,
        width: u32,
        height: u32,
        used: Instant,
    },
    /// Its texture was freed. The size stays, so the layout holds still.
    Evicted {
        id: u64,
        width: u32,
        height: u32,
    },
    /// Drawn again after eviction: decoding anew, under a new id.
    Reloading {
        id: u64,
        width: u32,
        height: u32,
    },
}

/// Where an image comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    File(PathBuf),
    Url(String),
    /// A `data:` URI, whole.
    Data(String),
}

impl Source {
    /// What to hand the system's player, if this is a video.
    pub fn video(&self) -> Option<String> {
        let (path, open) = match self {
            Source::File(path) => (path.clone(), path.to_string_lossy().into_owned()),
            Source::Url(url) => (PathBuf::from(url.split(['?', '#']).next()?), url.clone()),
            Source::Data(_) => return None,
        };
        stet_core::editor::is_video(&path).then_some(open)
    }
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
    let mut response = agent.get(url).header("User-Agent", "stet").call().ok()?;
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
    // Absurd sizes (a tiny file that inflates to gigabytes) are refused.
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE);
    limits.max_image_height = Some(MAX_DECODE);
    limits.max_alloc = Some(1 << 30);
    reader.limits(limits);
    let image = reader.decode().ok()?;
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

/// One frame of a video file, as an image file's bytes: from Quick Look
/// on macOS, else from `ffmpeg` if there is one.
fn frame(video: &Path, id: u64) -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};
    if cfg!(target_os = "macos") {
        // Quick Look writes `<file name>.png` into a folder of our choosing.
        let folder = std::env::temp_dir().join(format!("stet-poster-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&folder).ok()?;
        let _ = Command::new("/usr/bin/qlmanage")
            .args(["-t", "-s", &POSTER_SIDE.to_string(), "-o"])
            .arg(&folder)
            .arg(video)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let png = folder.join(format!("{}.png", video.file_name()?.to_string_lossy()));
        let bytes = std::fs::read(png).ok();
        let _ = std::fs::remove_dir_all(folder);
        if bytes.is_some() {
            return bytes;
        }
    }
    let scale = format!("scale={POSTER_SIDE}:{POSTER_SIDE}:force_original_aspect_ratio=decrease");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", &scale, "-f", "image2pipe", "-c:v", "png", "-"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    (output.status.success() && !output.stdout.is_empty()).then_some(output.stdout)
}

/// What stands for a video in the text: a frame of it, or a blank screen
/// when there is none to be had, under a play button. A file that is not
/// there shows nothing, as a missing picture does.
fn poster(source: &Source, id: u64) -> Option<Decoded> {
    let picture = match source {
        Source::File(path) => {
            if !path.is_file() {
                return None;
            }
            frame(path, id).and_then(|bytes| image::load_from_memory(&bytes).ok())
        }
        _ => None,
    };
    let mut rgba = match picture {
        Some(picture) if picture.width().max(picture.height()) > POSTER_SIDE => picture
            .resize(POSTER_SIDE, POSTER_SIDE, image::imageops::FilterType::Triangle)
            .into_rgba8(),
        Some(picture) => picture.into_rgba8(),
        None => image::RgbaImage::from_pixel(POSTER_SIDE, POSTER_SIDE * 9 / 16, image::Rgba([24, 24, 27, 255])),
    };
    play_button(&mut rgba);
    Some(Decoded {
        id,
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// A dark disc with a white triangle, in the middle of the picture.
fn play_button(picture: &mut image::RgbaImage) {
    let (width, height) = (picture.width() as f32, picture.height() as f32);
    let (cx, cy) = (width / 2.0, height / 2.0);
    let radius = (width.min(height) * 0.11).max(8.0);
    // The triangle, clockwise, nudged right so it looks centred.
    let corners = [
        (cx - radius * 0.28, cy - radius * 0.42),
        (cx + radius * 0.46, cy),
        (cx - radius * 0.28, cy + radius * 0.42),
    ];
    let reach = radius.ceil() as u32 + 1;
    let (left, top) = ((cx as u32).saturating_sub(reach), (cy as u32).saturating_sub(reach));
    for y in top..(cy as u32 + reach).min(picture.height()) {
        for x in left..(cx as u32 + reach).min(picture.width()) {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let disc = (radius - (px - cx).hypot(py - cy) + 0.5).clamp(0.0, 1.0);
            // How far inside the nearest edge the point is.
            let inside = (0..3)
                .map(|at| {
                    let ((ax, ay), (bx, by)) = (corners[at], corners[(at + 1) % 3]);
                    ((bx - ax) * (py - ay) - (by - ay) * (px - ax)) / (bx - ax).hypot(by - ay)
                })
                .fold(f32::MAX, f32::min);
            let triangle = (inside + 0.5).clamp(0.0, 1.0);
            let pixel = picture.get_pixel_mut(x, y);
            for channel in &mut pixel.0[..3] {
                let dimmed = *channel as f32 * (1.0 - disc * 0.6);
                *channel = (dimmed + (255.0 - dimmed) * triangle * 0.95).round() as u8;
            }
        }
    }
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
    /// Asking does not keep a texture alive, drawing it (`touch`) does.
    /// After eviction the id is dead: it draws nothing until `touch` has
    /// brought the picture back, but its size is still right.
    pub fn get(&mut self, source: &Source) -> Option<(u64, u32, u32)> {
        match self.states.get(source) {
            Some(
                State::Ready { id, width, height, .. }
                | State::Evicted { id, width, height }
                | State::Reloading { id, width, height },
            ) => return Some((*id, *width, *height)),
            Some(_) => return None,
            None => {}
        }
        self.states.insert(source.clone(), State::Loading);
        self.start(source);
        None
    }

    /// Decodes `source` under a fresh id, off the UI thread.
    fn start(&mut self, source: &Source) {
        let id = self.next_id;
        self.next_id += 1;
        let (sender, wake, owned, cache) = (
            self.sender.clone(),
            self.wake.clone(),
            source.clone(),
            self.cache.clone(),
        );
        let work = move || {
            let decoded = match owned.video() {
                // Never read whole: a video is far larger than a picture.
                Some(_) => poster(&owned, id),
                None => load(&owned, cache.as_deref()).and_then(|bytes| decode(&bytes, id)),
            };
            let _ = sender.send((owned, decoded));
            wake();
        };
        if self.blocking {
            work();
        } else {
            std::thread::spawn(work);
        }
    }

    /// `source` was drawn at `now`: keeps its texture, or brings it back.
    pub fn touch(&mut self, source: &Source, now: Instant) {
        match self.states.get_mut(source) {
            Some(State::Ready { used, .. }) => *used = now,
            Some(&mut State::Evicted { id, width, height }) => {
                self.states
                    .insert(source.clone(), State::Reloading { id, width, height });
                self.start(source);
            }
            _ => {}
        }
    }

    /// Frees pictures not drawn for a minute, then the longest unseen
    /// while textures exceed the budget. Returns the ids to free.
    pub fn evict(&mut self, now: Instant) -> Vec<u64> {
        let mut freed = Vec::new();
        let mut bytes = 0;
        let mut resident = Vec::new();
        for (source, state) in &self.states {
            if let State::Ready {
                width, height, used, ..
            } = state
            {
                bytes += *width as u64 * *height as u64 * 4;
                let idle = now.saturating_duration_since(*used);
                if idle >= KEEP {
                    freed.push(source.clone());
                } else if idle >= RECENT {
                    resident.push((*used, source.clone()));
                }
            }
        }
        for source in &freed {
            if let Some(State::Ready { width, height, .. }) = self.states.get(source) {
                bytes -= *width as u64 * *height as u64 * 4;
            }
        }
        resident.sort_by_key(|(used, _)| *used);
        for (_, source) in resident {
            if bytes <= BUDGET {
                break;
            }
            if let Some(State::Ready { width, height, .. }) = self.states.get(&source) {
                bytes -= *width as u64 * *height as u64 * 4;
            }
            freed.push(source);
        }
        freed
            .into_iter()
            .filter_map(|source| match self.states.get(&source) {
                Some(&State::Ready { id, width, height, .. }) => {
                    self.states.insert(source, State::Evicted { id, width, height });
                    Some(id)
                }
                _ => None,
            })
            .collect()
    }

    /// The file behind a picture, read now.
    pub fn bytes(&self, source: &Source) -> Option<Vec<u8>> {
        load(source, self.cache.as_deref())
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
                    used: Instant::now(),
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
    fn videos_are_shown_as_a_frame_to_click() {
        let url = Source::Url("https://example.com/talk.MP4?t=10".into());
        assert_eq!(url.video().as_deref(), Some("https://example.com/talk.MP4?t=10"));
        assert_eq!(Source::Url("https://example.com/mp4.png".into()).video(), None);
        assert_eq!(
            Source::File("/notes/demo.mov".into()).video().as_deref(),
            Some("/notes/demo.mov")
        );
        assert_eq!(Source::File("/notes/demo.png".into()).video(), None);

        // Nothing is fetched for a remote video: a blank screen stands for it.
        let poster = poster(&url, 3).unwrap();
        assert_eq!((poster.id, poster.width, poster.height), (3, 1280, 720));
        let pixel = |x: u32, y: u32| {
            let at = ((y * poster.width + x) * 4) as usize;
            [poster.rgba[at], poster.rgba[at + 1], poster.rgba[at + 2]]
        };
        assert_eq!(pixel(10, 10), [24, 24, 27]);
        // The button: white in the middle, a darker disc around it.
        assert!(pixel(640, 360).iter().all(|&channel| channel > 230));
        assert!(pixel(640 - 60, 360)[0] < 24);
        // A video that is not there shows nothing, as a missing picture does.
        assert!(super::poster(&Source::File("/no/such/clip.mp4".into()), 4).is_none());
    }

    fn ready(images: &mut Images, name: &str, id: u64, side: u32, used: Instant) -> Source {
        let source = Source::File(name.into());
        images.states.insert(
            source.clone(),
            State::Ready {
                id,
                width: side,
                height: side,
                used,
            },
        );
        source
    }

    #[test]
    fn pictures_unseen_for_a_while_are_evicted_and_return() {
        let mut images = Images::new(Arc::new(|| {}), None);
        images.blocking = true;
        let start = Instant::now();
        let old = ready(&mut images, "/no/old.png", 1, 100, start);
        let new = ready(&mut images, "/no/new.png", 2, 100, start + KEEP);
        assert_eq!(images.evict(start + KEEP), vec![1]);
        // Nothing twice, and the size stays so the layout holds still.
        assert!(images.evict(start + KEEP).is_empty());
        assert_eq!(images.get(&old), Some((1, 100, 100)));
        // Asking again does not bring it back, drawing does: under a new id.
        assert!(images.poll().is_empty());
        images.touch(&new, start + KEEP);
        images.touch(&old, start + KEEP);
        // The file is not there: the reload fails, once, and does not loop.
        assert!(images.poll().is_empty());
        assert_eq!(images.get(&old), None);
        images.touch(&old, start + KEEP);
        assert!(images.poll().is_empty());
        assert!(matches!(images.states[&new], State::Ready { id: 2, .. }));
    }

    #[test]
    fn a_budget_frees_the_longest_unseen_but_never_the_recent() {
        let mut images = Images::new(Arc::new(|| {}), None);
        let start = Instant::now();
        // 4096² is 64 MiB: ten of them is over the budget.
        for at in 0..10u64 {
            ready(
                &mut images,
                &format!("/no/{at}.png"),
                at + 1,
                4096,
                start + Duration::from_secs(at),
            );
        }
        let now = start + Duration::from_secs(9) + RECENT;
        let mut freed = images.evict(now);
        freed.sort();
        // Two come off; the one drawn last (id 10) and its neighbour are safe.
        assert_eq!(freed, vec![1, 2]);
        let all = start + Duration::from_secs(9);
        assert!(images.evict(all).is_empty());
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
