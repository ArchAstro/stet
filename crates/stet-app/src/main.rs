//! stet — a minimal markdown writer. This crate is the shell: it owns the
//! window, GPU and OS integration, and drives the platform-free `stet-core`.

mod app;
mod ctl;
mod gpu;
mod images;
#[cfg(target_os = "macos")]
mod macos;
mod motion;
mod platform;
#[cfg(feature = "substack")]
mod publish;
mod retouch;
mod session;
mod text;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use stet_core::{Config, Editor};

const USAGE: &str = "stet [options] [file]

  --theme <name>          start with this theme
  --novim                 standard (non-modal) keys
  --screenshot <out.png>  render one frame off-screen and exit
  --size <WxH>            screenshot size in points (default 1100x760)
  --scale <factor>        screenshot pixel density (default 2)
  --keys <notation>       keys to replay before the screenshot, e.g. 'ggvj' or ':theme nord<CR>'
  --bench                 print parse and layout timings for the file and exit
  --fonts                 list the installed font families and exit
  -n, --new-window        open a separate window even if stet is already running
  -f, --foreground        stay attached to the terminal until the window closes (also --wait)
  --drive                 read scripted input from stdin (keys, click, move, drag, scroll, resize, drop, quit)
  -V, --version           print the version
  -h, --help              this text

stet ctl ...                control the running stet from another program (stet ctl --help)";

#[derive(Default)]
pub struct Args {
    pub file: Option<PathBuf>,
    pub theme: Option<String>,
    pub novim: bool,
    pub screenshot: Option<PathBuf>,
    pub size: (u32, u32),
    pub scale: f32,
    pub keys: String,
    pub bench: bool,
    pub fonts: bool,
    pub drive: bool,
    pub foreground: bool,
    pub new_window: bool,
}

impl Args {
    /// Off-screen runs leave no trace and ignore remembered state.
    fn headless(&self) -> bool {
        self.bench || self.fonts || self.screenshot.is_some()
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        size: (1100, 760),
        scale: 2.0,
        ..Args::default()
    };
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        let mut value = |name: &str| raw.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "-V" | "--version" => {
                println!("stet {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--theme" => args.theme = Some(value("--theme")?),
            "--novim" => args.novim = true,
            "--bench" => args.bench = true,
            "--fonts" => args.fonts = true,
            "--drive" => args.drive = true,
            "-f" | "--foreground" | "--wait" => args.foreground = true,
            "-n" | "--new-window" => args.new_window = true,
            "--screenshot" => args.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--keys" => args.keys = value("--keys")?,
            "--scale" => {
                args.scale = value("--scale")?.parse().map_err(|_| "--scale expects a number")?;
            }
            "--size" => {
                let size = value("--size")?;
                let parsed = size
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                args.size = parsed.ok_or("--size expects WxH")?;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => args.file = Some(PathBuf::from(arg)),
        }
    }
    Ok(args)
}

/// Builds the editor from `config.toml`, user themes and the command line.
/// Problems are collected rather than fatal: a typo in the config must never
/// keep someone from their document.
pub fn build_editor(args: &Args) -> (Editor, Vec<PathBuf>) {
    let mut problems = Vec::new();
    let dir = platform::config_dir();
    let mut config = Config::default();
    if let Some(source) = dir
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join("config.toml")).ok())
    {
        match Config::from_toml(&source) {
            Ok(parsed) => config = parsed,
            Err(err) => problems.push(format!("config.toml: {err}")),
        }
    }
    let state = platform::State::load();
    let remembered = !args.headless() && !platform::State::older_than_config();
    if remembered {
        if let Some(theme) = &state.theme {
            config.theme = theme.clone();
        }
        // The remembered family goes first; the configured ones stay as fallbacks.
        for (family, list) in [
            (&state.prose_font, &mut config.prose_font),
            (&state.mono_font, &mut config.mono_font),
        ] {
            if let Some(family) = family {
                list.retain(|name| name != family);
                list.insert(0, family.clone());
            }
        }
    }
    if args.novim {
        config.vim = false;
    }
    let mut editor = Editor::new(config, Box::new(platform::SystemClipboard::default()));
    if !args.headless() {
        editor.recovery_dir = dir.as_ref().map(|dir| dir.join("recovery"));
        editor.recover_in_background(true);
    }
    let themes = dir.as_ref().and_then(|dir| std::fs::read_dir(dir.join("themes")).ok());
    for entry in themes.into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let loaded = std::fs::read_to_string(&path)
                .map_err(|err| err.to_string())
                .and_then(|source| editor.themes.load_toml(&name, &source));
            if let Err(err) = loaded {
                problems.push(format!("themes/{name}.toml: {err}"));
            }
        }
    }
    let wanted = args.theme.clone().unwrap_or(editor.config.theme.clone());
    match editor.themes.get(&wanted).cloned() {
        Some(theme) => editor.theme = theme,
        None => problems.push(format!("no theme named `{wanted}`")),
    }
    if let Some(file) = &args.file {
        let file = std::path::absolute(file).unwrap_or(file.clone());
        if let Err(err) = editor.open(&file) {
            problems.push(err);
        }
    }
    if !problems.is_empty() {
        editor.message = Some(stet_core::editor::Message {
            text: problems.join("; "),
            error: true,
        });
    }
    // Fonts dropped next to the config, or shipped next to the binary.
    let mut font_dirs: Vec<PathBuf> = dir.iter().map(|dir| dir.join("fonts")).collect();
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("fonts")))
    {
        font_dirs.push(exe);
    }
    (editor, font_dirs)
}

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// With `STET_TIMING` set, prints how long after launch a startup step ended.
pub fn timing(step: &str) {
    if std::env::var_os("STET_TIMING").is_some() {
        let start = START.get_or_init(std::time::Instant::now);
        eprintln!("stet: {:>8.2?}  {step}", start.elapsed());
    }
}

fn main() {
    START.get_or_init(std::time::Instant::now);
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().is_some_and(|first| first == "ctl") {
        std::process::exit(ctl::cli(&raw[1..]));
    }
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            if err.is_empty() {
                println!("{USAGE}");
                return;
            }
            eprintln!("stet: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.bench {
        return app::bench(&args);
    }
    if args.fonts {
        let (editor, font_dirs) = build_editor(&args);
        let fonts = text::Fonts::new(&editor.config, &font_dirs);
        let [prose, mono, ui] = fonts.names();
        println!("in use: writing \"{prose}\", code \"{mono}\", interface \"{ui}\"\n");
        for family in fonts.families() {
            println!("{family}");
        }
        return;
    }
    if args.screenshot.is_some() {
        if let Err(err) = app::screenshot(&args) {
            eprintln!("stet: {err}");
            std::process::exit(1);
        }
        return;
    }
    // An stet that is already running takes the file as a tab: no new process,
    // no new window, nothing to wait for.
    let own_process = args.foreground || args.drive || args.new_window;
    let files: Vec<String> = args
        .file
        .iter()
        .map(|file| {
            std::path::absolute(file)
                .unwrap_or(file.clone())
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let open = serde_json::json!({ "cmd": "open", "files": files }).to_string();
    if !own_process && platform::request(&open, false).is_some() {
        return;
    }
    if !own_process && detach() {
        return;
    }
    if let Err(err) = app::run(args) {
        eprintln!("stet: {err}");
        std::process::exit(1);
    }
}

/// Started from a terminal, the window runs as its own process and the
/// shell gets its prompt back at once. True if that process was started.
fn detach() -> bool {
    use std::io::IsTerminal;
    use std::process::{Command, Stdio};
    const MARK: &str = "STET_DETACHED";
    let from_terminal = std::io::stdin().is_terminal() || std::io::stderr().is_terminal();
    if !from_terminal || std::env::var_os(MARK).is_some() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let mut command = Command::new(exe);
    command
        .args(std::env::args_os().skip(1))
        .env(MARK, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own process group: Ctrl-C in the terminal is not for the window.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().is_ok()
}

/// A wake callback that does nothing (screenshots decode synchronously).
pub fn no_wake() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}
