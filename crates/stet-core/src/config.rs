//! User settings, read from `config.toml`.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: String,
    /// Modal vim keys. When false the editor behaves like a standard text field.
    pub vim: bool,
    /// Display name written into suggestions.
    pub author: String,
    /// Points.
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Text column width in characters.
    pub line_width: usize,
    /// Font families in preference order; the first one installed wins.
    pub prose_font: Vec<String>,
    pub mono_font: Vec<String>,
    /// Bare `j`/`k` move by wrapped display line (like `gj`/`gk`).
    pub visual_line_motion: bool,
    /// Dim everything but the paragraph under the cursor.
    pub focus: bool,
    /// Keep the cursor line vertically centered.
    pub typewriter: bool,
    /// Spaces per indent step.
    pub indent: usize,
    /// Yank, delete and put use the system clipboard by default.
    pub system_clipboard: bool,
    /// Show images below the line that references them.
    pub images: bool,
    /// Fetch `http(s)` images. Off keeps a document from contacting servers.
    pub remote_images: bool,
    /// Font for the file browser, menu and tabs.
    pub ui_font: Vec<String>,
    /// `/`, `?` and `:s` patterns are regular expressions.
    pub regex_search: bool,
    /// Color code blocks by language.
    pub highlight: bool,
    /// Typing `[[` offers the notes that can be linked.
    pub link_completion: bool,
    /// Show the file browser at startup.
    pub sidebar: bool,
    /// While writing, show margin notes beside the text they are pinned to
    /// (false lists the margin from top to bottom at all times).
    pub pinned_notes: bool,
    /// Pasting turns what other programs copy (a web page's HTML, a
    /// picture, files) into Markdown. Off pastes the plain text.
    pub smart_paste: bool,
    /// Copying offers an HTML rendering beside the Markdown, so it arrives
    /// formatted in programs that take rich text.
    pub copy_html: bool,
    /// Folder beside the document where pasted pictures are kept. Empty
    /// keeps them next to the document itself.
    pub image_dir: String,
    /// Draw tables as a grid, with long cells wrapped inside their column
    /// (false shows the source as it is).
    pub table_grid: bool,
    /// Your own key bindings; they win over the built-in ones.
    pub keys: Keys,
}

/// Key bindings by mode. Each entry maps a key sequence in vim notation
/// (`"<Space>w"`, `"jk"`, `"<D-j>"`) to an editor command (`":w"`), to other
/// keys (`"gj"`, `"<Esc>"`), or to `"<Nop>"` to switch a built-in off.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    pub normal: std::collections::BTreeMap<String, String>,
    pub insert: std::collections::BTreeMap<String, String>,
    pub visual: std::collections::BTreeMap<String, String>,
    /// Every mode, shortcuts included.
    pub all: std::collections::BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        let fonts = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
        Config {
            theme: crate::theme::DEFAULT_THEME.to_string(),
            vim: true,
            author: "Me".to_string(),
            font_size: 17.0,
            line_height: 1.55,
            line_width: 72,
            prose_font: fonts(&[
                "iA Writer Quattro S",
                "iA Writer Quattro V",
                "iA Writer Duo S",
                "SF Mono",
                "Menlo",
                "Cascadia Mono",
                "Consolas",
                "DejaVu Sans Mono",
                "Liberation Mono",
            ]),
            mono_font: fonts(&[
                "iA Writer Mono S",
                "iA Writer Mono V",
                "SF Mono",
                "Menlo",
                "Cascadia Mono",
                "Consolas",
                "DejaVu Sans Mono",
                "Liberation Mono",
            ]),
            visual_line_motion: true,
            focus: false,
            typewriter: false,
            indent: 4,
            system_clipboard: true,
            images: true,
            remote_images: true,
            ui_font: fonts(&["system-ui"]),
            regex_search: true,
            highlight: true,
            link_completion: true,
            sidebar: false,
            pinned_notes: true,
            smart_paste: true,
            copy_html: true,
            image_dir: "assets".to_string(),
            table_grid: true,
            keys: Keys::default(),
        }
    }
}

impl Config {
    pub fn from_toml(source: &str) -> Result<Config, String> {
        let mut config: Config = toml::from_str(source).map_err(|err| err.to_string())?;
        config.font_size = config.font_size.clamp(6.0, 96.0);
        config.line_height = config.line_height.clamp(1.0, 3.0);
        config.line_width = config.line_width.clamp(20, 400);
        config.indent = config.indent.clamp(1, 16);
        if !crate::critic::is_valid_author(&config.author) {
            return Err(format!(
                "author `{}` cannot be written into suggestion markup",
                config.author
            ));
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_files_keep_defaults_and_unknown_keys_fail() {
        let config = Config::from_toml("theme = \"nord\"\nvim = false\nfont_size = 500").unwrap();
        assert_eq!(config.theme, "nord");
        assert!(!config.vim);
        assert_eq!(config.font_size, 96.0);
        assert_eq!(config.line_width, 72);
        assert!(Config::from_toml("nope = 1").is_err());
        assert!(Config::from_toml("author = \"a<<}\"").is_err());
    }
}
