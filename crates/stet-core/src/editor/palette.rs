//! The pop-up menu: one searchable list used for help (every command with
//! its shortcut, markdown syntax, vim keys), opening files, backlinks, and
//! picking themes and fonts.

use super::{Editor, Mode, Primary};
use crate::input::{Key, KeyEvent};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    Help,
    Files,
    Backlinks,
    /// Completing a `[[` wiki link.
    Notes,
    Themes,
    Fonts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Run an editor command.
    Command(String),
    /// Insert markdown at the cursor. Block snippets start on their own line.
    Insert {
        text: String,
        block: bool,
    },
    Open {
        path: PathBuf,
        line: Option<usize>,
    },
    /// Reference rows do nothing.
    None,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    /// Right-hand text: a shortcut, syntax or path.
    pub detail: String,
    pub section: String,
    pub act: Act,
}

pub struct Palette {
    pub kind: PaletteKind,
    pub placeholder: String,
    pub query: String,
    pub items: Vec<Item>,
    /// Indices into `items` matching the query, best first.
    pub matches: Vec<usize>,
    pub selected: usize,
    /// Command that undoes a live preview when the menu is dismissed.
    revert: Option<String>,
}

impl Palette {
    fn new(kind: PaletteKind, placeholder: &str, items: Vec<Item>) -> Palette {
        let mut palette = Palette {
            kind,
            placeholder: placeholder.to_string(),
            query: String::new(),
            matches: Vec::new(),
            items,
            selected: 0,
            revert: None,
        };
        palette.filter();
        palette
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut ranked: Vec<(u8, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let label = item.label.to_lowercase();
                let rest = format!("{} {}", item.detail, item.section).to_lowercase();
                if !words.iter().all(|word| label.contains(word) || rest.contains(word)) {
                    return None;
                }
                let rank = match () {
                    _ if words.is_empty() => 0,
                    _ if label.starts_with(&query) => 0,
                    _ if words.iter().all(|word| label.contains(word)) => 1,
                    _ => 2,
                };
                Some((rank, index))
            })
            .collect();
        ranked.sort();
        self.matches = ranked.into_iter().map(|(_, index)| index).collect();
        self.selected = 0;
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.matches.get(self.selected).map(|&index| &self.items[index])
    }
}

fn item(section: &str, label: &str, detail: impl Into<String>, act: Act) -> Item {
    Item {
        label: label.to_string(),
        detail: detail.into(),
        section: section.to_string(),
        act,
    }
}

const MARKDOWN: &[(&str, &str, &str, bool)] = &[
    ("Heading", "# Title", "# ", true),
    ("Subheading", "## Section", "## ", true),
    ("Bold", "**bold**", "**bold**", false),
    ("Italic", "*italic*", "*italic*", false),
    ("Strikethrough", "~~struck~~", "~~struck~~", false),
    ("Inline code", "`code`", "`code`", false),
    ("Code block", "```lang … ```", "```\n\n```", true),
    ("Link", "[text](url)", "[text](url)", false),
    ("Link to a note", "[[Note name]]", "[[", false),
    ("Image", "![alt](picture.png)", "![alt](picture.png)", false),
    ("Bulleted list", "- item", "- ", true),
    ("Numbered list", "1. item", "1. ", true),
    ("Task", "- [ ] to do", "- [ ] ", true),
    ("Quote", "> quoted", "> ", true),
    (
        "Table",
        "| a | b |",
        "| Column | Column |\n| ------ | ------ |\n|        |        |",
        true,
    ),
    ("Footnote", "text[^1]  …  [^1]: note", "[^1]", false),
    ("Math", "$e^{i\\pi}$  or  $$ … $$", "$x$", false),
    ("Horizontal rule", "---", "---", true),
    ("Front matter", "--- title: … ---", "---\ntitle: \n---", true),
    ("Suggest an insertion", "{++added++}", "", false),
    ("Suggest a deletion", "{--removed--}", "", false),
    ("Suggest a replacement", "{~~old~>new~~}", "", false),
];

const VIM: &[(&str, &str)] = &[
    ("Move", "h j k l   w b e   0 ^ $   gg G   { }   ( )   %"),
    ("Find on the line", "f F t T   ; ,"),
    ("Search", "/pattern  ?pattern   n N   * #"),
    ("Insert", "i a I A o O   (3ix repeats)"),
    ("Operators", "d c y  > <  gu gU g~   + motion or object"),
    ("Text objects", "iw aw  ip ap  is as  i\" a\"  i( a(  i[ i{ i<"),
    ("Lines", "dd cc yy  >> <<  J gJ  x X  r ~  D C Y"),
    ("Paste, undo, repeat", "p P   u Ctrl-r   ."),
    ("Visual", "v  V  Ctrl-v (block: I A c d r)   o"),
    ("Registers", "\"a…\"z   \"0   \"+   \"_"),
    ("Macros", "qa … q   @a   @@"),
    ("Marks", "ma   `a  'a"),
    ("Scroll", "Ctrl-d Ctrl-u   Ctrl-f Ctrl-b   zz zt zb"),
    ("Links", "gf or Enter follows   Ctrl-o back   Ctrl-i forward"),
    ("Tabs", "]t  [t   :tabnew  :q"),
    ("Tasks and suggestions", "gt toggles a task   ]s [s   gsa gsr"),
    (
        "Margin",
        "Ctrl-w m opens it   Ctrl-w w switches pane   Ctrl-w y copies over   Ctrl-w d moves over   Ctrl-w a pins a note   Ctrl-w g jumps across",
    ),
    (
        "Menus and the assistant",
        "K or gm: actions here   ga: message the assistant",
    ),
    ("Replace", ":s/old/new/g   :%s/old/new/g   (regex, \\1, &)"),
];

impl Editor {
    /// A shortcut as the platform writes it: `⇧⌘S` or `Ctrl+Shift+S`.
    pub fn chord(&self, shift: bool, key: &str) -> String {
        match (self.primary, shift) {
            (Primary::Super, false) => format!("⌘{key}"),
            (Primary::Super, true) => format!("⇧⌘{key}"),
            (Primary::Ctrl, false) => format!("Ctrl+{key}"),
            (Primary::Ctrl, true) => format!("Ctrl+Shift+{key}"),
        }
    }

    fn help_items(&self) -> Vec<Item> {
        let command =
            |label: &str, detail: String, line: &str| item("Commands", label, detail, Act::Command(line.to_string()));
        let chord = |shift: bool, key: &str| self.chord(shift, key);
        let mut items = vec![
            command("Go to file…", chord(false, "P"), "files"),
            command("Show or hide the file browser", chord(false, "\\"), "sidebar"),
            command("New tab", chord(false, "T"), "tabnew"),
            command("Open…", chord(false, "O"), "open"),
            command("Save", chord(false, "S"), "w"),
            command("Save as…", chord(true, "S"), "saveas"),
            command("Close tab", chord(false, "W"), "close"),
            command("Next tab", format!("{}  ]t", chord(true, "]")), "tabnext"),
            command("Previous tab", format!("{}  [t", chord(true, "[")), "tabprevious"),
            command(
                "Follow the link under the cursor",
                format!("{}  gf", chord(false, "↵")),
                "follow",
            ),
            command("Back", format!("{}  Ctrl-o", chord(false, "[")), "back"),
            command("Forward", format!("{}  Ctrl-i", chord(false, "]")), "forward"),
            command("Notes linking here…", chord(true, "L"), "backlinks"),
            command(
                "Show or hide the margin (research notes)",
                format!("{}  Ctrl-w m", chord(true, "M")),
                "margin",
            ),
            command(
                "Switch between document and margin",
                format!("{}  Ctrl-w w", chord(true, "O")),
                "pane",
            ),
            command(
                "Copy selection or paragraph to the other pane",
                format!("{}  Ctrl-w y", chord(true, "↵")),
                "send",
            ),
            command(
                "Move selection or paragraph to the other pane",
                "Ctrl-w d".to_string(),
                "move",
            ),
            command("Pin a margin note to this text", "Ctrl-w a".to_string(), "pin"),
            command(
                "Jump between a note and what it is pinned to",
                "Ctrl-w g".to_string(),
                "note",
            ),
            command(
                "Unpin the margin section under the cursor",
                ":unpin".to_string(),
                "unpin",
            ),
            command(
                "Notes beside their pins, or the margin top to bottom",
                ":pins".to_string(),
                "pins",
            ),
            command(
                "Message the connected assistant…",
                format!("{}  ga", chord(true, "A")),
                "agent",
            ),
            command(
                "Actions for what is under the cursor…",
                format!("{}  K", chord(false, ".")),
                "actions",
            ),
            command("Find", format!("{}  /", chord(false, "F")), "find"),
            command("Choose a theme…", chord(true, "T"), "theme"),
            command("Choose the writing font…", ":font".to_string(), "font"),
            command("Choose the code font…", ":monofont".to_string(), "monofont"),
            command("Focus mode", chord(true, "D"), "focus"),
            command("Typewriter scrolling", ":typewriter".to_string(), "typewriter"),
            command("Suggestion mode", chord(true, "E"), "suggest"),
            command("Accept the suggestion", format!("{}  gsa", chord(true, "Y")), "accept"),
            command("Reject the suggestion", format!("{}  gsr", chord(true, "N")), "reject"),
            command("Accept all suggestions", ":acceptall".to_string(), "acceptall"),
            command("Reject all suggestions", ":rejectall".to_string(), "rejectall"),
            command("Paste as plain text", chord(true, "V"), "pasteplain"),
            command("Retouch the picture on this line", ":image".to_string(), "image"),
            command("Publish to Substack…", ":publish".to_string(), "publish"),
            command("Tidy the table", ":table".to_string(), "table"),
            command("Add a table row", ":table row".to_string(), "table row"),
            command("Add a table column", ":table column".to_string(), "table column"),
            command("Bold", chord(false, "B"), "bold"),
            command("Italic", chord(false, "I"), "italic"),
            command("Insert a link", chord(false, "K"), "link"),
            command("Toggle the task on this line", "gt".to_string(), "task"),
            command("Zoom in", chord(false, "+"), "zoom in"),
            command("Zoom out", chord(false, "-"), "zoom out"),
            command("Actual size", chord(false, "0"), "zoom reset"),
            command("Full screen", format!("{}  F11", chord(true, "F")), "fullscreen"),
            command(
                if self.config.vim {
                    "Turn vim keys off"
                } else {
                    "Turn vim keys on"
                },
                if self.config.vim { ":novim" } else { ":vim" }.to_string(),
                if self.config.vim { "novim" } else { "vim" },
            ),
            command("Quit", chord(false, "Q"), "qa"),
        ];
        items.extend(MARKDOWN.iter().map(|(label, syntax, text, block)| {
            let act = if text.is_empty() {
                Act::None
            } else {
                Act::Insert {
                    text: text.to_string(),
                    block: *block,
                }
            };
            item("Markdown", label, *syntax, act)
        }));
        if self.config.vim {
            items.extend(
                VIM.iter()
                    .map(|(label, keys)| item("Vim keys", label, *keys, Act::None)),
            );
        }
        items
    }

    fn file_items(&self) -> Vec<Item> {
        let root = self.workspace_root();
        self.workspace_notes()
            .into_iter()
            .map(|path| {
                let relative = path.strip_prefix(&root).unwrap_or(&path);
                let folder = relative
                    .parent()
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                item("", &name, folder, Act::Open { path, line: None })
            })
            .collect()
    }

    pub fn open_palette(&mut self, kind: PaletteKind) {
        self.close_group();
        self.cmdline = None;
        self.sidebar.focused = false;
        self.vim.clear_pending();
        let mut revert = None;
        let (placeholder, items) = match kind {
            PaletteKind::Help => ("Search commands, shortcuts and markdown", self.help_items()),
            PaletteKind::Files => ("Go to file", self.file_items()),
            PaletteKind::Notes => {
                let mut items = self.file_items();
                for item in &mut items {
                    item.act = Act::Insert {
                        text: format!("{}]]", item.label),
                        block: false,
                    };
                }
                ("Link to note", items)
            }
            PaletteKind::Backlinks => {
                let root = self.workspace_root();
                let items = self
                    .backlinks()
                    .into_iter()
                    .map(|link| {
                        let shown = link
                            .path
                            .strip_prefix(&root)
                            .unwrap_or(&link.path)
                            .to_string_lossy()
                            .into_owned();
                        item(
                            "",
                            &link.text,
                            shown,
                            Act::Open {
                                path: link.path,
                                line: Some(link.line),
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                if items.is_empty() {
                    return self.info("no other note links here");
                }
                ("Notes linking here", items)
            }
            PaletteKind::Themes => {
                revert = Some(format!("theme {}", self.theme.name));
                let items = self
                    .themes
                    .names()
                    .map(|name| {
                        let label = self.themes.get(name).map_or(name, |theme| theme.label.as_str());
                        item("", label, name, Act::Command(format!("theme {name}")))
                    })
                    .collect();
                ("Theme", items)
            }
            PaletteKind::Fonts => {
                let current = self.config.prose_font.first().cloned().unwrap_or_default();
                revert = Some(format!("font {current}"));
                let generic = ["system-ui", "serif", "sans-serif", "monospace"].map(str::to_string);
                let items = generic
                    .iter()
                    .chain(&self.font_families)
                    .map(|family| item("", family, "", Act::Command(format!("font {family}"))))
                    .collect();
                ("Writing font", items)
            }
        };
        let mut palette = Palette::new(kind, placeholder, items);
        palette.revert = revert;
        // Start on the current choice so arrowing previews its neighbours.
        let current = match kind {
            PaletteKind::Themes => Some(format!("theme {}", self.theme.name)),
            PaletteKind::Fonts => self.config.prose_font.first().map(|family| format!("font {family}")),
            _ => None,
        };
        if let Some(current) = current {
            let at = palette
                .matches
                .iter()
                .position(|&index| palette.items[index].act == Act::Command(current.clone()));
            palette.selected = at.unwrap_or(0);
        }
        self.palette = Some(palette);
    }

    /// The code-font picker reuses the font list.
    pub(super) fn open_mono_fonts(&mut self) {
        self.open_palette(PaletteKind::Fonts);
        let current = self.config.mono_font.first().cloned().unwrap_or_default();
        if let Some(palette) = &mut self.palette {
            palette.placeholder = "Code font".to_string();
            palette.revert = Some(format!("monofont {current}"));
            for item in &mut palette.items {
                item.act = Act::Command(format!("monofont {}", item.label));
            }
            palette.selected = palette.items.iter().position(|item| item.label == current).unwrap_or(0);
        }
    }

    pub fn close_palette(&mut self) {
        if let Some(revert) = self.palette.take().and_then(|palette| palette.revert) {
            self.run_command(&revert);
            self.message = None;
        }
    }

    fn palette_move(&mut self, delta: isize) {
        let Some(palette) = &mut self.palette else { return };
        let count = palette.matches.len() as isize;
        if count == 0 {
            return;
        }
        palette.selected = (palette.selected as isize + delta).rem_euclid(count) as usize;
        self.palette_preview();
    }

    /// Themes and fonts apply as the selection moves.
    fn palette_preview(&mut self) {
        let Some(palette) = &self.palette else { return };
        if !matches!(palette.kind, PaletteKind::Themes | PaletteKind::Fonts) {
            return;
        }
        if let Some(Act::Command(line)) = palette.selected_item().map(|item| item.act.clone()) {
            let palette = self.palette.take();
            self.run_command(&line);
            self.message = None;
            self.palette = palette;
        }
    }

    pub(super) fn palette_key(&mut self, event: KeyEvent) {
        let ctrl = event.mods.ctrl;
        match event.key {
            Key::Esc => self.close_palette(),
            Key::Char('c' | '[' | 'g') if ctrl => self.close_palette(),
            Key::Enter => self.palette_run(),
            Key::Down | Key::Tab if !event.mods.shift => self.palette_move(1),
            Key::Up | Key::Tab => self.palette_move(-1),
            Key::Char('n' | 'j') if ctrl => self.palette_move(1),
            Key::Char('p' | 'k') if ctrl => self.palette_move(-1),
            Key::PageDown => self.palette_move(8),
            Key::PageUp => self.palette_move(-8),
            _ => {
                let Some(palette) = &mut self.palette else { return };
                match event.key {
                    Key::Backspace => {
                        palette.query.pop();
                    }
                    Key::Char('u') if ctrl => palette.query.clear(),
                    Key::Char(c) if event.plain_char().is_some() => palette.query.push(c),
                    _ => return,
                }
                palette.filter();
                self.palette_preview();
            }
        }
    }

    pub(super) fn palette_text(&mut self, text: &str) {
        if let Some(palette) = &mut self.palette {
            palette.query.push_str(text.lines().next().unwrap_or(""));
            palette.filter();
            self.palette_preview();
        }
    }

    /// Selects and runs the `index`th visible row (a click).
    pub fn palette_click(&mut self, index: usize) {
        if let Some(palette) = &mut self.palette
            && index < palette.matches.len()
        {
            palette.selected = index;
            self.palette_run();
        }
    }

    fn palette_run(&mut self) {
        let Some(palette) = self.palette.take() else { return };
        let Some(item) = palette.selected_item() else { return };
        match item.act.clone() {
            Act::None => {}
            Act::Command(line) => self.run_command(&line),
            Act::Open { path, line } => {
                self.push_jump();
                match self.open_path(&path) {
                    Ok(()) => {
                        if let Some(line) = line {
                            self.cursor = self.buf.line_start(line.min(self.buf.line_count() - 1));
                        }
                    }
                    Err(err) => self.error(err),
                }
            }
            Act::Insert { text, block } => {
                let line = self.buf.line_of(self.cursor);
                if self.mode != Mode::Insert {
                    self.anchor = None;
                    self.mode = Mode::Insert;
                    // Normal mode sits on a character; type after it at a line's end.
                    if palette.kind == PaletteKind::Notes || self.cursor + 1 >= self.buf.line_end(line) {
                        self.cursor = (self.cursor + 1).min(self.buf.line_end(line));
                    }
                }
                let prefix = if block && !self.buf.line_is_blank(line) {
                    self.cursor = self.buf.line_end(line);
                    "\n"
                } else {
                    ""
                };
                self.type_text(&format!("{prefix}{text}"));
            }
        }
        self.clamp_cursor();
    }
}
