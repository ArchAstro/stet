//! Platform-neutral key input.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Command on macOS, the Windows/Super key elsewhere.
    pub sup: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    pub fn new(key: Key) -> Self {
        KeyEvent { key, mods: Mods::default() }
    }

    pub fn ctrl(c: char) -> Self {
        KeyEvent { key: Key::Char(c), mods: Mods { ctrl: true, ..Mods::default() } }
    }

    /// The typed character, when no command modifier is held.
    pub fn plain_char(&self) -> Option<char> {
        match self.key {
            Key::Char(c) if !self.mods.ctrl && !self.mods.sup => Some(c),
            _ => None,
        }
    }

    pub fn is_ctrl(&self, c: char) -> bool {
        self.mods.ctrl && !self.mods.sup && self.key == Key::Char(c)
    }
}

/// Parses vim-style key notation: `dw`, `<Esc>`, `<C-r>`, `<D-s>` (Command),
/// `<S-Tab>`, `<M-b>` (Alt), `<lt>` for a literal `<`.
pub fn parse_keys(notation: &str) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    let mut rest = notation;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(close) = rest.find('>')
                && let Some(event) = parse_named(&rest[1..close]) {
                    out.push(event);
                    rest = &rest[close + 1..];
                    continue;
                }
        out.push(KeyEvent::new(Key::Char(c)));
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn parse_named(name: &str) -> Option<KeyEvent> {
    let mut mods = Mods::default();
    let mut name = name;
    while name.len() > 2 && name.as_bytes()[1] == b'-' {
        match name.as_bytes()[0].to_ascii_uppercase() {
            b'C' => mods.ctrl = true,
            b'S' => mods.shift = true,
            b'M' | b'A' => mods.alt = true,
            b'D' => mods.sup = true,
            _ => return None,
        }
        name = &name[2..];
    }
    let key = match name.to_ascii_lowercase().as_str() {
        "esc" => Key::Esc,
        "cr" | "enter" => Key::Enter,
        "bs" => Key::Backspace,
        "del" => Key::Delete,
        "tab" => Key::Tab,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Char(' '),
        "lt" => Key::Char('<'),
        _ => {
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    };
    Some(KeyEvent { key, mods })
}
