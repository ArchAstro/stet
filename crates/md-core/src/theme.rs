//! Themes. The built-in set mirrors ArchDev's catalog: every theme fills the
//! same Catppuccin token roles (14 accents, 12 neutrals), and the editor reads
//! semantic colors derived from those roles.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(hex: &str) -> Option<Rgb> {
        let hex = hex.trim().strip_prefix('#')?;
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(channel(0)?, channel(2)?, channel(4)?))
    }

    /// `weight` of `self` over `other`, like CSS `color-mix(in srgb, …)`.
    pub fn mix(self, other: Rgb, weight: f32) -> Rgb {
        let mix = |a: u8, b: u8| (a as f32 * weight + b as f32 * (1.0 - weight)).round() as u8;
        Rgb(mix(self.0, other.0), mix(self.1, other.1), mix(self.2, other.2))
    }

    pub fn luminance(self) -> f32 {
        let linear = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.0) + 0.7152 * linear(self.1) + 0.0722 * linear(self.2)
    }

    pub fn contrast(self, other: Rgb) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

macro_rules! palette {
    ($($name:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
        pub struct Palette { $(pub $name: Rgb,)* }

        pub const TOKENS: &[&str] = &[$(stringify!($name),)*];

        impl Palette {
            const fn from_hex(values: [u32; 26]) -> Palette {
                let mut i = 0;
                $(
                    let $name = Rgb((values[i] >> 16) as u8, (values[i] >> 8) as u8, values[i] as u8);
                    i += 1;
                )*
                let _ = i;
                Palette { $($name,)* }
            }

            pub fn get(&self, token: &str) -> Option<Rgb> {
                match token { $(stringify!($name) => Some(self.$name),)* _ => None }
            }

            fn set(&mut self, token: &str, value: Rgb) -> bool {
                match token { $(stringify!($name) => { self.$name = value; true })* _ => false }
            }
        }
    };
}

palette!(
    rosewater, flamingo, pink, mauve, red, maroon, peach, yellow, green, teal, sky, sapphire, blue, lavender, text,
    subtext1, subtext0, overlay2, overlay1, overlay0, surface2, surface1, surface0, base, mantle, crust,
);

/// `(name, label, palette)` in ArchDev catalog order.
#[rustfmt::skip]
const BUILTIN: &[(&str, &str, Palette)] = &[
    ("latte", "Latte", Palette::from_hex([
        0xdc8a78, 0xdd7878, 0xea76cb, 0x8839ef, 0xd20f39, 0xe64553, 0xfe640b, 0xdf8e1d, 0x40a02b, 0x179299, 0x04a5e5, 0x209fb5, 0x1e66f5, 0x7287fd,
        0x4c4f69, 0x5c5f77, 0x6c6f85, 0x7c7f93, 0x8c8fa1, 0x9ca0b0, 0xacb0be, 0xbcc0cc, 0xccd0da, 0xeff1f5, 0xe6e9ef, 0xdce0e8])),
    ("frappe", "Frappé", Palette::from_hex([
        0xf2d5cf, 0xeebebe, 0xf4b8e4, 0xca9ee6, 0xe78284, 0xea999c, 0xef9f76, 0xe5c890, 0xa6d189, 0x81c8be, 0x99d1db, 0x85c1dc, 0x8caaee, 0xbabbf1,
        0xc6d0f5, 0xb5bfe2, 0xa5adce, 0x949cbb, 0x838ba7, 0x737994, 0x626880, 0x51576d, 0x414559, 0x303446, 0x292c3c, 0x232634])),
    ("macchiato", "Macchiato", Palette::from_hex([
        0xf4dbd6, 0xf0c6c6, 0xf5bde6, 0xc6a0f6, 0xed8796, 0xee99a0, 0xf5a97f, 0xeed49f, 0xa6da95, 0x8bd5ca, 0x91d7e3, 0x7dc4e4, 0x8aadf4, 0xb7bdf8,
        0xcad3f5, 0xb8c0e0, 0xa5adcb, 0x939ab7, 0x8087a2, 0x6e738d, 0x5b6078, 0x494d64, 0x363a4f, 0x24273a, 0x1e2030, 0x181926])),
    ("mocha", "Mocha", Palette::from_hex([
        0xf5e0dc, 0xf2cdcd, 0xf5c2e7, 0xcba6f7, 0xf38ba8, 0xeba0ac, 0xfab387, 0xf9e2af, 0xa6e3a1, 0x94e2d5, 0x89dceb, 0x74c7ec, 0x89b4fa, 0xb4befe,
        0xcdd6f4, 0xbac2de, 0xa6adc8, 0x9399b2, 0x7f849c, 0x6c7086, 0x585b70, 0x45475a, 0x313244, 0x1e1e2e, 0x181825, 0x11111b])),
    ("gruvbox-light", "Gruvbox Light", Palette::from_hex([
        0xd65d0e, 0xcc241d, 0xb16286, 0x8f3f71, 0x9d0006, 0xcc241d, 0xaf3a03, 0xb57614, 0x79740e, 0x427b58, 0x458588, 0x076678, 0x076678, 0xb16286,
        0x3c3836, 0x504945, 0x665c54, 0x7c6f64, 0x928374, 0xa89984, 0xbdae93, 0xd5c4a1, 0xebdbb2, 0xfbf1c7, 0xf2e5bc, 0xebdbb2])),
    ("everforest", "Everforest Dark", Palette::from_hex([
        0xe69875, 0xe67e80, 0xd699b6, 0xd699b6, 0xe67e80, 0xe67e80, 0xe69875, 0xdbbc7f, 0xa7c080, 0x83c092, 0x7fbbb3, 0x7fbbb3, 0x7fbbb3, 0xd699b6,
        0xd3c6aa, 0x9da9a0, 0x859289, 0x7a8478, 0x56635f, 0x4f585e, 0x3d484d, 0x343f44, 0x2d353b, 0x272e33, 0x232a2e, 0x1e2326])),
    ("everforest-light", "Everforest Light", Palette::from_hex([
        0xf57d26, 0xf85552, 0xdf69ba, 0xa54e8a, 0xf85552, 0xe66868, 0xf57d26, 0xdfa000, 0x8da101, 0x35a77c, 0x3a94c5, 0x3a94c5, 0x3a94c5, 0xdf69ba,
        0x5c6a72, 0x5c6a72, 0x829181, 0x939f91, 0xa6b0a0, 0xbec5b2, 0xe8e5d5, 0xedeada, 0xf2efdf, 0xfffbef, 0xf8f5e4, 0xf2efdf])),
    ("dracula", "Dracula", Palette::from_hex([
        0xffffff, 0xff6e6e, 0xff79c6, 0xbd93f9, 0xff6e6e, 0xff6e6e, 0xffb86c, 0xf1fa8c, 0x50fa7b, 0x8be9fd, 0xa4ffff, 0x8be9fd, 0x8be9fd, 0xd6acff,
        0xf8f8f2, 0xb5bccf, 0x8f9abb, 0x6272a4, 0x535c7f, 0x4b5263, 0x44475a, 0x343746, 0x2f3241, 0x282a36, 0x21222c, 0x191a21])),
    ("solaris-light", "Solaris Light", Palette::from_hex([
        0xd4590c, 0xcf222e, 0xd63384, 0x6f42c1, 0xcf222e, 0xd63384, 0xd4590c, 0xbf8700, 0x2e8b57, 0x0891b2, 0x0e9cb8, 0x0891b2, 0x0066cc, 0x8250df,
        0x24292e, 0x3d4752, 0x5a6370, 0x6e7781, 0x8b949e, 0xb0b8c4, 0xb0b8c4, 0xd0d5dc, 0xe0e4ea, 0xf5f7fa, 0xedf0f5, 0xe4e8ee])),
    ("solaris-dark", "Solaris Dark", Palette::from_hex([
        0xe0d0c0, 0xd48a7a, 0xd48a7a, 0xc4a0e8, 0xd4856a, 0xd48a7a, 0xd4956a, 0xa8b065, 0x7ab89e, 0x7ab89e, 0x90c8b8, 0x7ab89e, 0xc4a0e8, 0xd4b8f0,
        0xd4cfc8, 0xb8b0a4, 0x9a9086, 0x7a7068, 0x6b6358, 0x5a5248, 0x3d362e, 0x33291e, 0x282218, 0x1a1612, 0x161210, 0x110d09])),
    ("paper", "Paper White", Palette::from_hex([
        0x9a4f2b, 0xb8503f, 0xbf3989, 0x8250df, 0xcf222e, 0xa40e26, 0x953800, 0x8a6300, 0x1a7f37, 0x1b7c83, 0x0e7490, 0x0550ae, 0x0969da, 0x6639ba,
        0x1f2328, 0x59636e, 0x636c76, 0x818b98, 0x8c959f, 0x9198a1, 0xafb8c1, 0xd1d9e0, 0xe6eaef, 0xffffff, 0xfbfcfd, 0xf6f8fa])),
    ("ristretto", "Omarchy Ristretto", Palette::from_hex([
        0xfb9a77, 0xff8297, 0xbebffd, 0xa8a9eb, 0xfd6883, 0xfd6883, 0xfb9a77, 0xf9cc6c, 0xadda78, 0x85dacc, 0x9bf1e1, 0x85dacc, 0xf38d70, 0xbebffd,
        0xe6d9db, 0xc3b7b8, 0xaba0a1, 0x72696a, 0x5e585a, 0x4f4b4d, 0x403e41, 0x3d2f2a, 0x352a28, 0x2c2525, 0x211b1b, 0x181414])),
    ("nord", "Nord", Palette::from_hex([
        0xeceff4, 0xd08770, 0xb48ead, 0x81a1c1, 0xbf616a, 0xbf616a, 0xd08770, 0xebcb8b, 0xa3be8c, 0x8fbcbb, 0x88c0d0, 0x81a1c1, 0x5e81ac, 0xb48ead,
        0xd8dee9, 0xb9c1ce, 0x9aa5b5, 0x7b8799, 0x657187, 0x596579, 0x4c566a, 0x434c5e, 0x3b4252, 0x2e3440, 0x292e39, 0x242933])),
];

pub const DEFAULT_THEME: &str = "paper";

/// Semantic colors the editor reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorColors {
    pub background: Rgb,
    pub text: Rgb,
    /// Syntax punctuation: `#`, `**`, list bullets' neighbours, fences.
    pub marker: Rgb,
    /// Secondary text: URLs, HTML, front matter.
    pub muted: Rgb,
    pub heading: Rgb,
    pub link: Rgb,
    pub code: Rgb,
    pub code_background: Rgb,
    pub quote: Rgb,
    pub list_marker: Rgb,
    pub math: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub search: Rgb,
    pub insert: Rgb,
    pub insert_background: Rgb,
    pub delete: Rgb,
    pub delete_background: Rgb,
    pub comment: Rgb,
    pub status: Rgb,
    pub error: Rgb,
    pub rule: Rgb,
    /// Side panel and pop-up surfaces.
    pub panel: Rgb,
    /// The highlighted row on a panel.
    pub panel_active: Rgb,
    /// Code token colors, indexed by `markdown::syntax` kind.
    pub syntax: [Rgb; crate::markdown::syntax::COUNT],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub label: String,
    pub palette: Palette,
    pub colors: EditorColors,
}

impl Theme {
    pub fn new(name: &str, label: &str, palette: Palette) -> Theme {
        Theme {
            name: name.to_string(),
            label: label.to_string(),
            palette,
            colors: derive(&palette),
        }
    }

    pub fn is_light(&self) -> bool {
        self.palette.base.luminance() > 0.5
    }
}

/// Nudges an accent toward the text color until it reads on the background.
fn readable(accent: Rgb, background: Rgb, text: Rgb, floor: f32) -> Rgb {
    (0..=10)
        .map(|step| accent.mix(text, 1.0 - step as f32 / 10.0))
        .find(|color| color.contrast(background) >= floor)
        .unwrap_or(text)
}

fn derive(p: &Palette) -> EditorColors {
    let accent = |color: Rgb| readable(color, p.base, p.text, 3.0);
    EditorColors {
        background: p.base,
        text: p.text,
        marker: p.overlay0,
        muted: readable(p.overlay1, p.base, p.text, 3.0),
        heading: p.text,
        link: accent(p.sky),
        code: accent(p.mauve),
        code_background: p.mantle,
        quote: p.subtext0,
        list_marker: accent(p.mauve),
        math: accent(p.peach),
        cursor: accent(p.blue),
        selection: [0.22, 0.18, 0.14, 0.1]
            .map(|weight| p.blue.mix(p.base, weight))
            .into_iter()
            .find(|tint| p.text.contrast(*tint) >= 4.5)
            .unwrap_or(p.surface0),
        search: p.yellow.mix(p.base, 0.4),
        insert: accent(p.green),
        insert_background: p.green.mix(p.base, 0.14),
        delete: accent(p.red),
        delete_background: p.red.mix(p.base, 0.12),
        comment: p.overlay0,
        status: readable(p.overlay1, p.base, p.text, 3.0),
        error: accent(p.red),
        rule: p.surface1,
        panel: p.mantle,
        panel_active: p.surface0,
        syntax: {
            use crate::markdown::syntax::*;
            let code = |color: Rgb| readable(color, p.mantle, p.text, 3.5);
            let mut colors = [p.text; COUNT];
            colors[KEYWORD as usize] = code(p.mauve);
            colors[STRING as usize] = code(p.green);
            colors[NUMBER as usize] = code(p.peach);
            colors[CONSTANT as usize] = code(p.peach);
            colors[COMMENT as usize] = readable(p.overlay1, p.mantle, p.text, 2.6);
            colors[FUNCTION as usize] = code(p.blue);
            colors[TYPE as usize] = code(p.yellow);
            colors[TAG as usize] = code(p.red);
            colors[ATTRIBUTE as usize] = code(p.teal);
            colors[OPERATOR as usize] = code(p.sky);
            colors[PUNCTUATION as usize] = readable(p.overlay2, p.mantle, p.text, 3.5);
            colors[VARIABLE as usize] = code(p.maroon);
            colors
        },
    }
}

/// A user theme file: any palette token, plus optional semantic overrides.
///
/// ```toml
/// label = "My Theme"
/// base = "mocha"          # theme to inherit unset tokens from
/// [palette]
/// blue = "#00aaff"
/// [editor]
/// cursor = "#ff0000"
/// ```
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    label: Option<String>,
    base: Option<String>,
    #[serde(default)]
    palette: BTreeMap<String, String>,
    #[serde(default)]
    editor: BTreeMap<String, String>,
}

pub struct Themes {
    themes: Vec<Theme>,
}

impl Default for Themes {
    fn default() -> Self {
        Themes {
            themes: BUILTIN
                .iter()
                .map(|(name, label, palette)| Theme::new(name, label, *palette))
                .collect(),
        }
    }
}

impl Themes {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.themes.iter().map(|theme| theme.name.as_str())
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.iter().find(|theme| theme.name == name)
    }

    pub fn default_theme(&self) -> &Theme {
        self.get(DEFAULT_THEME).unwrap_or(&self.themes[0])
    }

    /// The theme after `name` in catalog order, wrapping.
    pub fn next(&self, name: &str, step: isize) -> &Theme {
        let at = self.themes.iter().position(|theme| theme.name == name).unwrap_or(0) as isize;
        &self.themes[(at + step).rem_euclid(self.themes.len() as isize) as usize]
    }

    /// Adds or replaces a theme from TOML source.
    pub fn load_toml(&mut self, name: &str, source: &str) -> Result<(), String> {
        let file: ThemeFile = toml::from_str(source).map_err(|err| err.to_string())?;
        let base = file.base.as_deref().unwrap_or(DEFAULT_THEME);
        let mut palette = self
            .get(base)
            .ok_or_else(|| format!("unknown base theme `{base}`"))?
            .palette;
        let color = |key: &str, value: &str| {
            Rgb::parse(value).ok_or_else(|| format!("`{key}`: expected #rrggbb, got `{value}`"))
        };
        for (token, value) in &file.palette {
            if !palette.set(token, color(token, value)?) {
                return Err(format!("unknown palette token `{token}`"));
            }
        }
        let mut theme = Theme::new(name, file.label.as_deref().unwrap_or(name), palette);
        for (role, value) in &file.editor {
            let value = color(role, value)?;
            let c = &mut theme.colors;
            let slot = match role.as_str() {
                "background" => &mut c.background,
                "text" => &mut c.text,
                "marker" => &mut c.marker,
                "muted" => &mut c.muted,
                "heading" => &mut c.heading,
                "link" => &mut c.link,
                "code" => &mut c.code,
                "code_background" => &mut c.code_background,
                "quote" => &mut c.quote,
                "list_marker" => &mut c.list_marker,
                "math" => &mut c.math,
                "cursor" => &mut c.cursor,
                "selection" => &mut c.selection,
                "search" => &mut c.search,
                "insert" => &mut c.insert,
                "insert_background" => &mut c.insert_background,
                "delete" => &mut c.delete,
                "delete_background" => &mut c.delete_background,
                "comment" => &mut c.comment,
                "status" => &mut c.status,
                "error" => &mut c.error,
                "rule" => &mut c.rule,
                "panel" => &mut c.panel,
                "panel_active" => &mut c.panel_active,
                "syntax_keyword" => &mut c.syntax[crate::markdown::syntax::KEYWORD as usize],
                "syntax_string" => &mut c.syntax[crate::markdown::syntax::STRING as usize],
                "syntax_number" => &mut c.syntax[crate::markdown::syntax::NUMBER as usize],
                "syntax_constant" => &mut c.syntax[crate::markdown::syntax::CONSTANT as usize],
                "syntax_comment" => &mut c.syntax[crate::markdown::syntax::COMMENT as usize],
                "syntax_function" => &mut c.syntax[crate::markdown::syntax::FUNCTION as usize],
                "syntax_type" => &mut c.syntax[crate::markdown::syntax::TYPE as usize],
                "syntax_tag" => &mut c.syntax[crate::markdown::syntax::TAG as usize],
                "syntax_attribute" => &mut c.syntax[crate::markdown::syntax::ATTRIBUTE as usize],
                "syntax_operator" => &mut c.syntax[crate::markdown::syntax::OPERATOR as usize],
                "syntax_punctuation" => &mut c.syntax[crate::markdown::syntax::PUNCTUATION as usize],
                "syntax_variable" => &mut c.syntax[crate::markdown::syntax::VARIABLE as usize],
                _ => return Err(format!("unknown editor role `{role}`")),
            };
            *slot = value;
        }
        match self.themes.iter_mut().find(|existing| existing.name == name) {
            Some(existing) => *existing = theme,
            None => self.themes.push(theme),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_archdev_order_and_values() {
        let themes = Themes::default();
        assert_eq!(
            themes.names().collect::<Vec<_>>(),
            [
                "latte",
                "frappe",
                "macchiato",
                "mocha",
                "gruvbox-light",
                "everforest",
                "everforest-light",
                "dracula",
                "solaris-light",
                "solaris-dark",
                "paper",
                "ristretto",
                "nord",
            ]
        );
        let mocha = themes.get("mocha").unwrap();
        assert_eq!(mocha.palette.rosewater, Rgb::parse("#f5e0dc").unwrap());
        assert_eq!(mocha.palette.crust, Rgb::parse("#11111b").unwrap());
        assert_eq!(mocha.palette.get("mauve"), Rgb::parse("#cba6f7"));
        assert!(!mocha.is_light());
        assert!(themes.get("paper").unwrap().is_light());
        assert_eq!(TOKENS.len(), 26);
    }

    #[test]
    fn text_roles_stay_readable_in_every_theme() {
        for theme in &Themes::default().themes {
            let c = &theme.colors;
            assert!(c.text.contrast(c.background) >= 4.5, "{} text", theme.name);
            for (role, color) in [
                ("link", c.link),
                ("code", c.code),
                ("insert", c.insert),
                ("delete", c.delete),
                ("cursor", c.cursor),
                ("muted", c.muted),
                ("list_marker", c.list_marker),
            ] {
                assert!(color.contrast(c.background) >= 3.0, "{} {role}", theme.name);
            }
            assert!(c.text.contrast(c.selection) >= 4.5, "{} selection", theme.name);
        }
    }

    #[test]
    fn user_themes_inherit_and_override() {
        let mut themes = Themes::default();
        themes
            .load_toml(
                "mine",
                "label = \"Mine\"\nbase = \"nord\"\n[palette]\nblue = \"#010203\"\n[editor]\ncursor = \"#ff0000\"\n",
            )
            .unwrap();
        let mine = themes.get("mine").unwrap();
        assert_eq!(mine.label, "Mine");
        assert_eq!(mine.palette.blue, Rgb(1, 2, 3));
        assert_eq!(mine.palette.base, themes.get("nord").unwrap().palette.base);
        assert_eq!(mine.colors.cursor, Rgb(255, 0, 0));
        assert!(themes.load_toml("bad", "[palette]\nnope = \"#000000\"").is_err());
        assert!(themes.load_toml("bad", "[palette]\nblue = \"blue\"").is_err());
        assert_eq!(themes.next("mine", 1).name, "latte");
        assert_eq!(themes.next("latte", -1).name, "mine");
    }
}
