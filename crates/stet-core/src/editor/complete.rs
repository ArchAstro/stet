//! Completion and hints for the `:` command line: Tab opens a list of what
//! the word under the cursor can become, command names first and then each
//! command's arguments.

use super::{CmdKind, Editor};
use std::path::{Path, PathBuf};

/// What a command's argument can be, for completing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Values {
    None,
    Path,
    Themes,
    Fonts,
    Words(&'static [&'static str]),
}

/// A command the `:` line knows.
#[derive(Debug, PartialEq, Eq)]
pub struct Spec {
    /// The name it is listed by, then the other names it answers to.
    pub names: &'static [&'static str],
    /// How its argument is written: `<required>`, `[optional]`, or empty.
    pub args: &'static str,
    pub about: &'static str,
    pub values: Values,
}

const ON_OFF: &[&str] = &["on", "off"];

const fn spec(names: &'static [&'static str], args: &'static str, about: &'static str, values: Values) -> Spec {
    Spec {
        names,
        args,
        about,
        values,
    }
}

/// Every command `run_command` takes, in the order the list shows them.
pub const COMMANDS: &[Spec] = &[
    spec(
        &["w", "write"],
        "[path]",
        "Save, or save to a path (! overwrites)",
        Values::Path,
    ),
    spec(&["wq", "x", "xit"], "[path]", "Save and close the tab", Values::Path),
    spec(
        &["q", "quit", "bd", "bdelete", "tabclose", "tabc"],
        "",
        "Close the tab (! discards)",
        Values::None,
    ),
    spec(
        &["close"],
        "",
        "Close the tab, asking about unsaved changes",
        Values::None,
    ),
    spec(&["qa", "qall", "quitall"], "", "Quit (! discards)", Values::None),
    spec(&["wa", "wall"], "", "Save every tab", Values::None),
    spec(
        &["wqa", "wqall", "xa", "xall"],
        "",
        "Save every tab and quit",
        Values::None,
    ),
    spec(&["saveas"], "", "Save as…", Values::None),
    spec(&["open"], "", "Open a file…", Values::None),
    spec(
        &["e", "edit"],
        "[path]",
        "Open a file here, or reload this one (! discards)",
        Values::Path,
    ),
    spec(&["enew", "new"], "", "A blank document here", Values::None),
    spec(
        &["tabnew", "tabe", "tabedit"],
        "[path]",
        "A new tab, or a file in one",
        Values::Path,
    ),
    spec(&["tabnext", "tabn", "bn", "bnext"], "", "Next tab", Values::None),
    spec(
        &["tabprevious", "tabp", "tabprev", "bp", "bprevious"],
        "",
        "Previous tab",
        Values::None,
    ),
    spec(&["files"], "", "Go to file…", Values::None),
    spec(&["find"], "[text]", "Search, or go to file with a name", Values::None),
    spec(
        &["sidebar", "browse", "tree", "Ex", "Explore"],
        "",
        "Show or hide the file browser",
        Values::None,
    ),
    spec(&["backlinks"], "", "Notes linking here…", Values::None),
    spec(&["follow"], "", "Follow the link under the cursor", Values::None),
    spec(&["back"], "", "Back to where a link was followed from", Values::None),
    spec(&["forward"], "", "Forward again", Values::None),
    spec(
        &["s"],
        "/old/new/[g]",
        "Replace on this line (regex, \\1, &)",
        Values::None,
    ),
    spec(&["%s"], "/old/new/[g]", "Replace in the whole document", Values::None),
    spec(
        &["noh", "nohlsearch"],
        "",
        "Stop highlighting search matches",
        Values::None,
    ),
    spec(&["undo", "u"], "", "Undo", Values::None),
    spec(&["redo", "red"], "", "Redo", Values::None),
    spec(&["bold"], "", "Make the selection bold", Values::None),
    spec(&["italic"], "", "Make the selection italic", Values::None),
    spec(&["link"], "", "Make the selection a link", Values::None),
    spec(&["task"], "", "Toggle the task on this line", Values::None),
    spec(
        &["table", "tableformat", "tf"],
        "[columns rows]",
        "Tidy the table here, or make one",
        Values::None,
    ),
    spec(&["pasteplain"], "", "Paste without formatting", Values::None),
    spec(&["image", "retouch"], "", "Retouch the picture here", Values::None),
    spec(
        &["suggest", "sug"],
        "[on|off]",
        "Suggest changes instead of making them",
        Values::Words(ON_OFF),
    ),
    spec(&["accept"], "", "Accept the suggestion here", Values::None),
    spec(&["reject"], "", "Reject the suggestion here", Values::None),
    spec(&["acceptall"], "", "Accept every suggestion", Values::None),
    spec(&["rejectall"], "", "Reject every suggestion", Values::None),
    spec(&["nextsuggestion"], "", "Next suggestion", Values::None),
    spec(&["prevsuggestion"], "", "Previous suggestion", Values::None),
    spec(&["author"], "[name]", "Who suggestions are written by", Values::None),
    spec(&["margin", "scratch"], "", "Show or hide the margin", Values::None),
    spec(
        &["pane", "other"],
        "",
        "Switch between document and margin",
        Values::None,
    ),
    spec(&["send", "copyover"], "", "Copy to the other pane", Values::None),
    spec(&["move", "moveover"], "", "Move to the other pane", Values::None),
    spec(&["pin", "annotate"], "", "Pin a margin note to this text", Values::None),
    spec(&["unpin"], "", "Unpin the margin section here", Values::None),
    spec(&["note", "gopin"], "", "Jump between a note and its pin", Values::None),
    spec(&["pins"], "", "Notes beside their pins, or top to bottom", Values::None),
    spec(&["attach"], "<path>", "Attach a file to the margin", Values::Path),
    spec(
        &["notebook", "nb"],
        "[on|off]",
        "Run code cells in this document",
        Values::Words(ON_OFF),
    ),
    spec(
        &["run"],
        "[all|stale|above|below|N]",
        "Run the cell here, or others",
        Values::Words(&["all", "stale", "above", "below"]),
    ),
    spec(&["interrupt", "stop"], "", "Interrupt the kernel", Values::None),
    spec(&["restart"], "", "Restart the kernel", Values::None),
    spec(&["clear"], "", "Clear this cell's output", Values::None),
    spec(&["clearall"], "", "Clear every output", Values::None),
    spec(&["agent", "ask"], "[message]", "Message the assistant", Values::None),
    spec(&["actions", "context"], "", "Actions for the text here", Values::None),
    spec(&["publish", "substack"], "", "Publish to Substack", Values::None),
    spec(&["youtube"], "", "Add a YouTube video", Values::None),
    spec(
        &["theme", "colorscheme", "colo"],
        "[name]",
        "Change the colour theme",
        Values::Themes,
    ),
    spec(&["font"], "[family]", "The prose font", Values::Fonts),
    spec(&["monofont"], "[family]", "The code font", Values::Fonts),
    spec(&["uifont"], "[family]", "The interface font", Values::Fonts),
    spec(
        &["zoom"],
        "[in|out|reset]",
        "Zoom the text",
        Values::Words(&["in", "out", "reset"]),
    ),
    spec(&["fullscreen"], "", "Toggle full screen", Values::None),
    spec(&["focus"], "", "Dim all but the paragraph here", Values::None),
    spec(&["typewriter"], "", "Keep the cursor line centred", Values::None),
    spec(&["vim"], "", "Vim keys", Values::None),
    spec(&["novim"], "", "Plain keys", Values::None),
    spec(&["recover"], "", "Restore the unsaved copy of this file", Values::None),
    spec(
        &["help", "h", "commands", "menu"],
        "",
        "Search commands, shortcuts and markdown",
        Values::None,
    ),
];

/// The command a typed name means, `!` and all.
pub fn spec_for(name: &str) -> Option<&'static Spec> {
    let name = name.trim_end_matches('!');
    COMMANDS.iter().find(|spec| spec.names.contains(&name))
}

/// One thing the word being completed can become.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What replaces the word.
    pub text: String,
    /// How its argument is written, for a command.
    pub args: &'static str,
    pub about: String,
}

/// The list Tab opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Byte where the word being completed starts.
    pub start: usize,
    pub items: Vec<Candidate>,
    pub selected: Option<usize>,
    /// The command line as this list last left it; once it is changed
    /// some other way the list no longer applies.
    pub line: String,
    /// True while the word is a command's name.
    pub naming: bool,
}

/// The most a list holds.
const MOST: usize = 200;

impl Editor {
    /// The command being typed on the `:` line, if it is one Stet knows.
    pub fn cmdline_spec(&self) -> Option<&'static Spec> {
        let cmdline = self
            .cmdline
            .as_ref()
            .filter(|cmdline| cmdline.kind == CmdKind::Command)?;
        let text = cmdline.text.trim_start();
        let name = text.split(char::is_whitespace).next()?;
        // `:s/a/b/` and `:%s/a/b/` run straight into their pattern.
        let name = match name {
            _ if name.starts_with("%s") && !name[2..].starts_with(char::is_alphanumeric) => "%s",
            _ if name.starts_with('s') && name[1..].starts_with(|c: char| !c.is_alphanumeric() && c != '!') => "s",
            name => name,
        };
        spec_for(name)
    }

    /// The list open under the `:` line, if Tab opened one and it still
    /// matches what is typed.
    pub fn completion(&self) -> Option<&Completion> {
        let cmdline = self.cmdline.as_ref()?;
        self.completion
            .as_ref()
            .filter(|completion| completion.line == cmdline.text && !completion.items.is_empty())
    }

    /// What the last word on the `:` line can become.
    fn candidates(&self, text: &str) -> (usize, Vec<Candidate>, bool) {
        let lead = text.len() - text.trim_start().len();
        let Some((name, _)) = text[lead..].split_once(char::is_whitespace) else {
            return (lead, command_names(&text[lead..]), true);
        };
        let start = text.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        let typed = &text[start..];
        let Some(spec) = spec_for(name) else {
            return (start, Vec::new(), false);
        };
        let word = |text: &str| Candidate {
            text: text.to_string(),
            args: "",
            about: String::new(),
        };
        let matching = |names: Vec<&str>| -> Vec<Candidate> {
            let lower = typed.to_lowercase();
            let mut found: Vec<Candidate> = names
                .iter()
                .filter(|name| name.to_lowercase().starts_with(&lower))
                .map(|name| word(name))
                .collect();
            // Then anything with the typed text inside it.
            found.extend(
                names
                    .iter()
                    .filter(|name| {
                        let name = name.to_lowercase();
                        !lower.is_empty() && !name.starts_with(&lower) && name.contains(&lower)
                    })
                    .map(|name| word(name)),
            );
            found.truncate(MOST);
            found
        };
        // A path or a font family is the whole rest of the line, spaces and all.
        let rest = text[lead + name.len()..].trim_start();
        let rest_start = text.len() - rest.len();
        match spec.values {
            Values::None => (start, Vec::new(), false),
            Values::Words(words) => (start, matching(words.to_vec()), false),
            Values::Themes => (start, matching(self.themes.names().collect()), false),
            Values::Path => (rest_start, self.paths(rest), false),
            Values::Fonts => {
                let lower = rest.to_lowercase();
                let families = self
                    .font_families
                    .iter()
                    .filter(|family| family.to_lowercase().starts_with(&lower));
                (
                    rest_start,
                    families.take(MOST).map(|family| word(family)).collect(),
                    false,
                )
            }
        }
    }

    /// Files and folders that complete `typed`, relative to the document's
    /// folder.
    fn paths(&self, typed: &str) -> Vec<Candidate> {
        let (folder, prefix) = match typed.rfind('/') {
            Some(at) => (&typed[..=at], &typed[at + 1..]),
            None => ("", typed),
        };
        let base = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let dir: PathBuf = match folder {
            "" => base,
            _ if folder.starts_with("~/") => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(&folder[2..]),
                None => return Vec::new(),
            },
            _ => base.join(folder),
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let lower = prefix.to_lowercase();
        let mut found: Vec<(bool, String)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let hidden = name.starts_with('.') && !prefix.starts_with('.');
                (!hidden && name.to_lowercase().starts_with(&lower))
                    .then(|| (entry.file_type().is_ok_and(|kind| kind.is_dir()), name))
            })
            .collect();
        // Folders first, then files, each alphabetically.
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        found
            .into_iter()
            .take(MOST)
            .map(|(dir, name)| Candidate {
                text: format!("{folder}{name}{}", if dir { "/" } else { "" }),
                args: "",
                about: if dir { "folder".to_string() } else { String::new() },
            })
            .collect()
    }

    /// Tab: open the list, or step to its next entry (`back` for the one
    /// before), writing it into the line.
    pub(super) fn complete(&mut self, back: bool) {
        let Some(cmdline) = self.cmdline.as_ref().filter(|cmdline| cmdline.kind == CmdKind::Command) else {
            return;
        };
        let text = cmdline.text.clone();
        if self.completion().is_none() {
            let (start, items, naming) = self.candidates(&text);
            if items.is_empty() {
                self.completion = None;
                return;
            }
            // One answer is simply taken.
            if items.len() == 1 {
                self.completion = None;
                let item = &items[0];
                let mut line = format!("{}{}", &text[..start], item.text);
                if naming && !item.args.is_empty() {
                    line.push(' ');
                }
                self.cmdline.as_mut().unwrap().text = line;
                return;
            }
            self.completion = Some(Completion {
                start,
                items,
                selected: None,
                line: text,
                naming,
            });
        }
        let completion = self.completion.as_mut().unwrap();
        let count = completion.items.len();
        completion.selected = Some(match (completion.selected, back) {
            (None, false) => 0,
            (None, true) => count - 1,
            (Some(at), false) => (at + 1) % count,
            (Some(at), true) => (at + count - 1) % count,
        });
        let item = &completion.items[completion.selected.unwrap()];
        let line = format!("{}{}", &completion.line[..completion.start], item.text);
        completion.line = line.clone();
        self.cmdline.as_mut().unwrap().text = line;
    }

    /// Keeps an open list matching the line as it is typed.
    pub(super) fn refilter(&mut self, was_open: bool) {
        let Some(cmdline) = self.cmdline.as_ref().filter(|cmdline| cmdline.kind == CmdKind::Command) else {
            self.completion = None;
            return;
        };
        if !was_open {
            return;
        }
        let text = cmdline.text.clone();
        let (start, items, naming) = self.candidates(&text);
        self.completion = Some(Completion {
            start,
            items,
            selected: None,
            line: text,
            naming,
        });
    }

    /// Enter with the list open: settle on the chosen entry. True if the
    /// line wants more before it runs: a folder to look inside, or a
    /// command's argument.
    pub(super) fn accept_completion(&mut self) -> bool {
        let Some(completion) = self.completion().cloned() else {
            return false;
        };
        self.completion = None;
        let Some(item) = completion.selected.map(|at| &completion.items[at]) else {
            return false;
        };
        let cmdline = self.cmdline.as_mut().unwrap();
        if completion.naming && item.args.starts_with('<') {
            cmdline.text.push(' ');
            return true;
        }
        if item.text.ends_with('/') {
            self.refilter(true);
            return true;
        }
        false
    }
}

/// Commands whose names start with what is typed, then those with it inside.
fn command_names(typed: &str) -> Vec<Candidate> {
    let typed = typed.trim_end_matches('!');
    let entry = |spec: &Spec, name: &str| Candidate {
        text: name.to_string(),
        args: spec.args,
        about: spec.about.to_string(),
    };
    let mut found = Vec::new();
    for spec in COMMANDS {
        if let Some(name) = spec.names.iter().find(|name| name.starts_with(typed)) {
            // Listed by its main name when that is what was being typed.
            let name = if spec.names[0].starts_with(typed) {
                spec.names[0]
            } else {
                name
            };
            found.push(entry(spec, name));
        }
    }
    if !typed.is_empty() {
        for spec in COMMANDS {
            if !spec.names.iter().any(|name| name.starts_with(typed)) && spec.names[0].contains(typed) {
                found.push(entry(spec, spec.names[0]));
            }
        }
    }
    // What was typed exactly comes first.
    if let Some(at) = found.iter().position(|item| item.text == typed) {
        let exact = found.remove(at);
        found.insert(0, exact);
    }
    found
}
