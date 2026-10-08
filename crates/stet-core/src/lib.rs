//! Platform-free editing core for stet: text buffer, vim and standard key
//! handling, markdown analysis, review suggestions, themes and settings.
//! Nothing here touches a window, GPU, font or OS API.

pub mod buffer;
pub mod config;
pub mod critic;
pub mod editor;
pub mod highlight;
pub mod html;
pub mod input;
pub mod markdown;
pub mod table;
pub mod theme;

pub use buffer::Buffer;
pub use config::Config;
pub use editor::{Clip, Clipboard, Editor, Effect, MemoryClipboard, Mode, Primary, ScrollTo};
pub use input::{Key, KeyEvent, Mods};
