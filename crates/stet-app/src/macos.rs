//! What makes stet a Mac app rather than a program with a window: files opened
//! from Finder or the Dock arrive as tabs, Quit asks about unsaved work, and
//! the menu bar offers every command.

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::sel;
use objc2_foundation::{NSArray, NSURL};
use std::path::PathBuf;
use std::sync::OnceLock;
use stet_core::input::parse_keys;
use stet_core::{Editor, Key, KeyEvent};

/// What the application delegate reports to the event loop.
pub enum AppEvent {
    /// Finder, the Dock or `open` asked for these files.
    Open(Vec<PathBuf>),
    /// The system asked the app to quit (Dock menu, log out).
    Quit,
}

static SINK: OnceLock<Box<dyn Fn(AppEvent) + Send + Sync>> = OnceLock::new();

// The windowing library owns the application delegate and gives it only the
// two methods it needs itself. These are the two stet needs, added to that
// same class when the app starts.

extern "C" fn open_urls(_this: &AnyObject, _cmd: Sel, _application: &AnyObject, urls: &NSArray<NSURL>) {
    let files: Vec<PathBuf> = urls
        .iter()
        .filter_map(|url| unsafe { url.path() })
        .map(|path| PathBuf::from(path.to_string()))
        .collect();
    if let (Some(sink), false) = (SINK.get(), files.is_empty()) {
        sink(AppEvent::Open(files));
    }
}

/// `NSTerminateCancel`: quitting goes through the editor so unsaved
/// documents are asked about; the editor then ends the event loop itself.
extern "C" fn should_terminate(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) -> usize {
    match SINK.get() {
        Some(sink) => {
            sink(AppEvent::Quit);
            0
        }
        None => 1,
    }
}

/// Teaches the application delegate to open files and to ask before
/// quitting. Call after the event loop exists.
pub fn install(sink: impl Fn(AppEvent) + Send + Sync + 'static) {
    let Some(class) = AnyClass::get("WinitApplicationDelegate") else {
        return;
    };
    if SINK.set(Box::new(sink)).is_err() {
        return;
    }
    let class = class as *const AnyClass as *mut objc2::ffi::objc_class;
    // SAFETY: each function matches the signature its type encoding states
    // (`v@:@@` and `Q@:@`), and the class exists for the life of the process.
    unsafe {
        let open: unsafe extern "C" fn() =
            std::mem::transmute(open_urls as extern "C" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>));
        objc2::ffi::class_addMethod(
            class,
            sel!(application:openURLs:).as_ptr(),
            Some(open),
            c"v@:@@".as_ptr(),
        );
        let quit: unsafe extern "C" fn() =
            std::mem::transmute(should_terminate as extern "C" fn(&AnyObject, Sel, &AnyObject) -> usize);
        objc2::ffi::class_addMethod(
            class,
            sel!(applicationShouldTerminate:).as_ptr(),
            Some(quit),
            c"Q@:@".as_ptr(),
        );
    }
}

/// What a menu item does: the same keys the shortcut sends, or a command.
const MENUS: &[(&str, &[(&str, &str)])] = &[
    (
        "File",
        &[
            ("New Tab", "<D-t>"),
            ("Open…", "<D-o>"),
            ("Go to File…", "<D-p>"),
            ("", ""),
            ("Save", "<D-s>"),
            ("Save As…", "<D-S-s>"),
            ("", ""),
            ("Close Tab", "<D-w>"),
        ],
    ),
    (
        "Edit",
        &[
            ("Undo", "<D-z>"),
            ("Redo", "<D-S-z>"),
            ("", ""),
            ("Cut", "<D-x>"),
            ("Copy", "<D-c>"),
            ("Paste", "<D-v>"),
            ("Paste as Plain Text", "<D-S-v>"),
            ("Select All", "<D-a>"),
            ("", ""),
            ("Find…", "<D-f>"),
            ("Find Next", "<D-g>"),
            ("Find Previous", "<D-S-g>"),
            ("", ""),
            ("Bold", "<D-b>"),
            ("Italic", "<D-i>"),
            ("Link", "<D-k>"),
        ],
    ),
    (
        "View",
        &[
            ("File Browser", "<D-\\>"),
            ("Theme…", "<D-S-t>"),
            ("Writing Font…", ":font"),
            ("Code Font…", ":monofont"),
            ("", ""),
            ("Focus Mode", "<D-S-d>"),
            ("Typewriter Scrolling", ":typewriter"),
            ("", ""),
            ("Zoom In", "<D-=>"),
            ("Zoom Out", "<D-->"),
            ("Actual Size", "<D-0>"),
            ("", ""),
            ("Enter Full Screen", "<D-S-f>"),
        ],
    ),
    (
        "Go",
        &[
            ("Follow Link", "<D-CR>"),
            ("Back", "<D-[>"),
            ("Forward", "<D-]>"),
            ("", ""),
            ("Notes Linking Here…", "<D-S-l>"),
            ("", ""),
            ("Next Tab", "<D-S-]>"),
            ("Previous Tab", "<D-S-[>"),
        ],
    ),
    (
        "Review",
        &[
            ("Suggest Edits", "<D-S-e>"),
            ("", ""),
            ("Accept Suggestion", "<D-S-y>"),
            ("Reject Suggestion", "<D-S-n>"),
            ("Next Suggestion", ":nextsuggestion"),
            ("Previous Suggestion", ":prevsuggestion"),
            ("", ""),
            ("Accept All", ":acceptall"),
            ("Reject All", ":rejectall"),
            ("", ""),
            ("Message Assistant…", "<D-S-a>"),
        ],
    ),
];

const HELP: &[(&str, &str)] = &[("All Commands…", "<D-/>"), ("Actions Here…", "<D-.>")];

fn accelerator(event: &KeyEvent) -> Option<Accelerator> {
    let code = match event.key {
        Key::Enter => Code::Enter,
        Key::Char(c) => match c.to_ascii_lowercase() {
            'a' => Code::KeyA,
            'b' => Code::KeyB,
            'c' => Code::KeyC,
            'd' => Code::KeyD,
            'e' => Code::KeyE,
            'f' => Code::KeyF,
            'g' => Code::KeyG,
            'i' => Code::KeyI,
            'k' => Code::KeyK,
            'l' => Code::KeyL,
            'n' => Code::KeyN,
            'o' => Code::KeyO,
            'p' => Code::KeyP,
            'q' => Code::KeyQ,
            's' => Code::KeyS,
            't' => Code::KeyT,
            'v' => Code::KeyV,
            'w' => Code::KeyW,
            'x' => Code::KeyX,
            'y' => Code::KeyY,
            'z' => Code::KeyZ,
            '0' => Code::Digit0,
            '=' => Code::Equal,
            '-' => Code::Minus,
            '/' => Code::Slash,
            '.' => Code::Period,
            '\\' => Code::Backslash,
            '[' => Code::BracketLeft,
            ']' => Code::BracketRight,
            _ => return None,
        },
        _ => return None,
    };
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::META, event.mods.sup);
    mods.set(Modifiers::SHIFT, event.mods.shift);
    mods.set(Modifiers::CONTROL, event.mods.ctrl);
    mods.set(Modifiers::ALT, event.mods.alt);
    Some(Accelerator::new(mods, code))
}

fn item(editor: &Editor, label: &str, action: &str) -> MenuItem {
    // A shortcut the user rebound stays out of the menu, so theirs wins.
    let shortcut = parse_keys(action)
        .first()
        .copied()
        .filter(|event| !action.starts_with(':') && !editor.has_binding(event))
        .and_then(|event| accelerator(&event));
    MenuItem::with_id(action, label, true, shortcut)
}

/// Builds the menu bar and installs it. `chosen` receives the action of
/// each item picked: key notation, or a `:command`. Keep the returned menu
/// alive for as long as the app runs.
pub fn menu_bar(editor: &Editor, chosen: impl Fn(String) + Send + Sync + 'static) -> Option<Menu> {
    let menu = Menu::new();
    let about = AboutMetadata {
        name: Some("Stet".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: Some("© ArchAstro. MIT license.".into()),
        ..Default::default()
    };
    let app = Submenu::new("Stet", true);
    app.append_items(&[
        &PredefinedMenuItem::about(Some("About Stet"), Some(about)),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &item(editor, "Quit Stet", "<D-q>"),
    ])
    .ok()?;
    menu.append(&app).ok()?;
    let fill = |submenu: &Submenu, entries: &[(&str, &str)]| {
        for (label, action) in entries {
            let added = if label.is_empty() {
                submenu.append(&PredefinedMenuItem::separator())
            } else {
                submenu.append(&item(editor, label, action))
            };
            added.ok()?;
        }
        Some(())
    };
    for (title, entries) in MENUS {
        let submenu = Submenu::new(*title, true);
        fill(&submenu, entries)?;
        menu.append(&submenu).ok()?;
    }
    let window = Submenu::new("Window", true);
    window
        .append_items(&[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(None)])
        .ok()?;
    menu.append(&window).ok()?;
    let help = Submenu::new("Help", true);
    fill(&help, HELP)?;
    menu.append(&help).ok()?;
    menu.init_for_nsapp();
    window.set_as_windows_menu_for_nsapp();
    help.set_as_help_menu_for_nsapp();
    muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| chosen(event.id.0)));
    Some(menu)
}
