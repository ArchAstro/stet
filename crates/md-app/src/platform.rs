//! The few things that differ per OS: clipboard, config location, dialogs.

use md_core::Clipboard;
use std::path::PathBuf;

/// The system clipboard. Opened lazily: some platforms block on first use.
#[derive(Default)]
pub struct SystemClipboard(Option<arboard::Clipboard>);

impl SystemClipboard {
    fn handle(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.0.is_none() {
            self.0 = arboard::Clipboard::new().ok();
        }
        self.0.as_mut()
    }
}

impl Clipboard for SystemClipboard {
    fn get(&mut self) -> Option<String> {
        self.handle()?.get_text().ok()
    }
    fn set(&mut self, text: &str) {
        if let Some(clipboard) = self.handle() {
            let _ = clipboard.set_text(text.to_string());
        }
    }
}

/// `~/.config/md` on Linux and macOS, `%APPDATA%\md` on Windows.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MD_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let dirs = directories::BaseDirs::new()?;
    Some(if cfg!(windows) {
        dirs.config_dir().join("md")
    } else {
        dirs.home_dir().join(".config").join("md")
    })
}

pub fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Markdown", &["md", "markdown", "mdown", "txt"])
        .add_filter("All files", &["*"])
        .pick_file()
}

pub fn pick_save(name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_file_name(if name == "Untitled" { "Untitled.md" } else { name })
        .save_file()
}

#[derive(PartialEq, Eq)]
pub enum Unsaved {
    Save,
    Discard,
    Cancel,
}

pub fn confirm_unsaved(name: &str) -> Unsaved {
    let answer = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title("Unsaved changes")
        .set_description(format!("Save changes to {name} before closing?"))
        .set_buttons(rfd::MessageButtons::YesNoCancel)
        .show();
    match answer {
        rfd::MessageDialogResult::Yes => Unsaved::Save,
        rfd::MessageDialogResult::No => Unsaved::Discard,
        _ => Unsaved::Cancel,
    }
}

/// Downloaded images are kept here between runs.
pub fn cache_dir() -> Option<PathBuf> {
    if std::env::var_os("MD_CONFIG_DIR").is_some() {
        return config_dir().map(|dir| dir.join("cache"));
    }
    Some(directories::BaseDirs::new()?.cache_dir().join("md"))
}

/// Choices made while running (as opposed to written in `config.toml`),
/// remembered for the next launch.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct State {
    pub theme: Option<String>,
    pub zoom: Option<f32>,
    pub prose_font: Option<String>,
    pub mono_font: Option<String>,
    /// Window size in points.
    pub window: Option<(f32, f32)>,
}

impl State {
    fn path() -> Option<PathBuf> {
        Some(config_dir()?.join("state.toml"))
    }

    pub fn load() -> State {
        Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|source| toml::from_str(&source).ok())
            .unwrap_or_default()
    }

    /// True if `config.toml` was edited after the state was written: the
    /// file is then the newer word on anything both set.
    pub fn older_than_config() -> bool {
        let modified = |name: &str| {
            std::fs::metadata(config_dir()?.join(name))
                .and_then(|meta| meta.modified())
                .ok()
        };
        match (modified("state.toml"), modified("config.toml")) {
            (Some(state), Some(config)) => state < config,
            _ => false,
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let (Some(dir), Ok(source)) = (path.parent(), toml::to_string(self)) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(path, source);
        }
    }
}

/// Where a running md listens for files to open.
pub fn socket_path() -> Option<PathBuf> {
    let socket = config_dir()?.join("md.sock");
    // Socket addresses are short (about 100 bytes); a deep config folder
    // gets a stand-in under the temp folder instead.
    if socket.as_os_str().len() < 96 {
        return Some(socket);
    }
    let hash = socket
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
        });
    Some(std::env::temp_dir().join(format!("md-{hash:016x}.sock")))
}

/// Sends one request line to the md that is already running and returns
/// its reply. `None` if none is running.
#[cfg(unix)]
pub fn request(line: &str, patient: bool) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path()?).ok()?;
    // `patient` waits as long as it takes (an assistant waiting for a message).
    let _ = stream.set_read_timeout((!patient).then(|| std::time::Duration::from_secs(10)));
    stream.write_all(line.replace('\n', " ").as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).ok()?;
    (!reply.is_empty()).then_some(reply)
}

#[cfg(not(unix))]
pub fn request(_: &str, _: bool) -> Option<String> {
    None
}

/// Answers requests from later `md` launches and from `md ctl`, one line in
/// and one line out, calling `answer` for each with a way to ask whether
/// the caller is still there. False if another md is already listening.
#[cfg(unix)]
pub fn listen(answer: impl Fn(String, &dyn Fn() -> bool) -> String + Send + Sync + 'static) -> bool {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let Some(socket) = socket_path() else { return false };
    if UnixStream::connect(&socket).is_ok() {
        return false;
    }
    // Whatever is there was left by an md that is gone.
    let _ = std::fs::remove_file(&socket);
    if let Some(dir) = socket.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(listener) = UnixListener::bind(&socket) else {
        return false;
    };
    // Only this user may connect.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
    }
    let answer = std::sync::Arc::new(answer);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let answer = answer.clone();
            // One thread per caller: a slow one cannot hold up the rest.
            std::thread::spawn(move || {
                use std::io::Read;
                let mut line = String::new();
                let (Ok(mut reply_to), Ok(probe)) = (stream.try_clone(), stream.try_clone()) else {
                    return;
                };
                if BufReader::new(stream).read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    // The caller sends nothing more, so a read that ends means it hung up.
                    let _ = probe.set_nonblocking(true);
                    let alive = move || !matches!((&probe).read(&mut [0u8; 1]), Ok(0));
                    let reply = answer(line, &alive);
                    let _ = reply_to.set_nonblocking(false);
                    let _ = reply_to.write_all(reply.replace('\n', " ").as_bytes());
                    let _ = reply_to.write_all(b"\n");
                }
            });
        }
    });
    true
}

#[cfg(not(unix))]
pub fn listen(_: impl Fn(String, &dyn Fn() -> bool) -> String + Send + Sync + 'static) -> bool {
    false
}

/// Removes the listening socket if this process owns it.
pub fn stop_listening(owned: bool) {
    if let (true, Some(socket)) = (owned, socket_path()) {
        let _ = std::fs::remove_file(socket);
    }
}
