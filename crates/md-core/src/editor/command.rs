//! The `:` command line and `/` search prompt.

use super::{CmdKind, CmdLine, Editor, Effect, Matcher, Mode, PaletteKind};
use crate::critic::is_valid_author;
use crate::input::{Key, KeyEvent};
use std::path::Path;

impl Editor {
    pub(super) fn cmdline_key(&mut self, event: KeyEvent) {
        let Some(cmdline) = &mut self.cmdline else { return };
        match event.key {
            Key::Esc => self.cmdline = None,
            Key::Char('c' | '[') if event.mods.ctrl => self.cmdline = None,
            Key::Char('u') if event.mods.ctrl => cmdline.text.clear(),
            Key::Char('w') if event.mods.ctrl => {
                let keep = cmdline.text.trim_end().rfind(' ').map_or(0, |at| at + 1);
                cmdline.text.truncate(keep);
            }
            Key::Enter => {
                let CmdLine { kind, text } = self.cmdline.take().unwrap();
                match kind {
                    CmdKind::Command => self.run_command(&text),
                    CmdKind::SearchForward => self.start_search(&text, true),
                    CmdKind::SearchBackward => self.start_search(&text, false),
                    CmdKind::Agent => self.agent_send(&text),
                }
            }
            Key::Backspace
                if cmdline.text.pop().is_none() => {
                    self.cmdline = None;
                }
            Key::Tab => self.complete(),
            Key::Char(c) if event.plain_char().is_some() => cmdline.text.push(c),
            _ => {}
        }
        self.preview_search();
    }

    /// Highlights matches while a search is being typed.
    pub(super) fn preview_search(&mut self) {
        self.vim.preview = match &self.cmdline {
            Some(cmdline) if matches!(cmdline.kind, CmdKind::SearchForward | CmdKind::SearchBackward) => Matcher::new(&cmdline.text, self.config.regex_search),
            _ => None,
        };
    }

    /// Completes theme and font names, cycling on repeated Tab.
    fn complete(&mut self) {
        let Some(cmdline) = self.cmdline.as_mut().filter(|cmdline| cmdline.kind == CmdKind::Command) else {
            return;
        };
        let Some((command, typed)) = cmdline.text.split_once(' ') else { return };
        let names: Vec<&str> = match command {
            "theme" | "colorscheme" | "colo" => self.themes.names().collect(),
            "font" | "monofont" | "uifont" => self.font_families.iter().map(String::as_str).collect(),
            _ => return,
        };
        let typed_lower = typed.to_lowercase();
        let next = match names.iter().position(|name| *name == typed) {
            Some(at) => names[(at + 1) % names.len()],
            None => match names.iter().find(|name| name.to_lowercase().starts_with(&typed_lower)) {
                Some(name) => name,
                None => return,
            },
        };
        cmdline.text = format!("{command} {next}");
    }

    pub fn run_command(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let last = self.buf.line_count() - 1;
        if let Ok(number) = line.parse::<usize>() {
            self.cursor = self.buf.line_start(number.saturating_sub(1).min(last));
            return;
        }
        if line == "$" {
            self.cursor = self.buf.line_start(last);
            return;
        }
        for (prefix, whole) in [("%s", true), ("s", false)] {
            let rest = line.strip_prefix(prefix).unwrap_or("");
            if rest.chars().next().is_some_and(|c| !c.is_alphanumeric() && !c.is_whitespace() && c != '!') {
                return self.substitute(rest, whole);
            }
        }
        let (name, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let arg = arg.trim();
        let force = name.ends_with('!');
        let name = name.trim_end_matches('!');
        let unsaved = "No write since last change (add ! to override)";
        match name {
            "w" | "write" => {
                let path = (!arg.is_empty()).then(|| Path::new(arg));
                if let Err(err) = self.save(path, force) {
                    self.error(err);
                }
            }
            "q" | "quit" | "bd" | "bdelete" | "tabclose" | "tabc" => {
                self.close_tab(force);
            }
            "close" => self.effects.push(Effect::Close),
            "qa" | "qall" | "quitall" => {
                let clean = force || self.dirty_tabs().is_empty();
                self.effects.push(if clean { Effect::Quit } else { Effect::CloseAll });
            }
            "wa" | "wall" => {
                if let Err(err) = self.save_all() {
                    self.error(err);
                }
            }
            "wqa" | "wqall" | "xa" | "xall" => match self.save_all() {
                Ok(()) => self.effects.push(Effect::Quit),
                Err(err) => self.error(err),
            },
            "wq" | "x" | "xit" => {
                if self.path.is_none() && arg.is_empty() {
                    self.effects.push(Effect::SaveAsDialog);
                    return;
                }
                let path = (!arg.is_empty()).then(|| Path::new(arg));
                match self.save(path, force) {
                    Ok(()) => {
                        self.close_tab(true);
                    }
                    Err(err) => self.error(err),
                }
            }
            "saveas" => self.effects.push(Effect::SaveAsDialog),
            "open" => self.effects.push(Effect::OpenDialog),
            "tabnew" | "tabe" | "tabedit" => {
                if arg.is_empty() {
                    self.new_tab();
                } else if let Err(err) = self.open_in_tab(Path::new(arg)) {
                    self.error(err);
                }
            }
            "tabnext" | "tabn" | "bn" | "bnext" => self.cycle_tab(1),
            "tabprevious" | "tabp" | "tabprev" | "bp" | "bprevious" => self.cycle_tab(-1),
            "files" | "find" if name == "files" || !arg.is_empty() => self.open_palette(PaletteKind::Files),
            "find" => self.cmdline = Some(CmdLine { kind: CmdKind::SearchForward, text: String::new() }),
            "sidebar" | "browse" | "tree" | "Ex" | "Explore" => self.toggle_sidebar(),
            "backlinks" => self.open_palette(PaletteKind::Backlinks),
            "agent" | "ask" if arg.is_empty() => self.agent_prompt(),
            "agent" | "ask" => {
                if self.agent.is_none() {
                    return self.error("no assistant is connected (one connects with `md ctl wait`)");
                }
                self.agent_send(arg);
            }
            "actions" | "context" => self.open_context_menu(super::MenuAt::Cursor),
            "nextsuggestion" | "prevsuggestion" => {
                self.refresh();
                let starts: Vec<usize> = self.doc().suggestions.iter().map(|suggestion| suggestion.span.start).collect();
                let cursor = self.cursor;
                let found = if name == "nextsuggestion" {
                    starts.iter().find(|&&start| start > cursor).or(starts.first())
                } else {
                    starts.iter().rfind(|&&start| start < cursor).or(starts.last())
                };
                match found {
                    Some(&start) => self.cursor = start,
                    None => self.info("no suggestions"),
                }
            }
            "follow" => self.follow_link(),
            "back" => self.jump(true),
            "forward" => self.jump(false),
            "recover" => self.apply_recovery(true),
            "bold" => self.wrap_selection("**", "**"),
            "italic" => self.wrap_selection("*", "*"),
            "link" => self.wrap_selection("[", "]()"),
            "task" => self.toggle_task(),
            "fullscreen" => self.effects.push(Effect::ToggleFullscreen),
            "zoom" => self.effects.push(Effect::Zoom(match arg {
                "in" => 1,
                "out" => -1,
                _ => 0,
            })),
            "font" | "monofont" | "uifont" => {
                let list = match name {
                    "font" => &mut self.config.prose_font,
                    "monofont" => &mut self.config.mono_font,
                    _ => &mut self.config.ui_font,
                };
                if arg.is_empty() {
                    return if name == "monofont" { self.open_mono_fonts() } else { self.open_palette(PaletteKind::Fonts) };
                }
                list.retain(|family| !family.eq_ignore_ascii_case(arg));
                list.insert(0, arg.to_string());
                self.effects.push(Effect::FontChanged);
                self.info(format!("{name}: {arg}"));
            }
            "e" | "edit" => {
                if self.buf.is_dirty() && !force {
                    return self.error(unsaved);
                }
                let path = if arg.is_empty() { self.path.clone() } else { Some(Path::new(arg).to_path_buf()) };
                match path {
                    None => self.error("No file name"),
                    Some(path) => {
                        // Forcing discards the unsaved text, and its recovery copy with it.
                        self.remove_recovery();
                        if let Err(err) = self.open(&path) {
                            self.error(err);
                        }
                    }
                }
            }
            "enew" | "new" => {
                if self.buf.is_dirty() && !force {
                    return self.error(unsaved);
                }
                self.remove_recovery();
                self.blank_document();
            }
            "theme" | "colorscheme" | "colo" => {
                if arg.is_empty() {
                    return self.open_palette(PaletteKind::Themes);
                }
                match self.themes.get(arg).cloned() {
                    Some(theme) => {
                        self.info(theme.label.clone());
                        self.theme = theme;
                        self.config.theme = arg.to_string();
                        self.effects.push(Effect::ThemeChanged);
                    }
                    None => self.error(format!("no theme named `{arg}`")),
                }
            }
            "vim" | "novim" => {
                self.config.vim = name == "vim";
                self.anchor = None;
                self.close_group();
                self.mode = if self.config.vim { Mode::Normal } else { Mode::Insert };
            }
            "suggest" | "sug" => {
                self.suggesting = match arg {
                    "on" => true,
                    "off" => false,
                    _ => !self.suggesting,
                };
                self.info(if self.suggesting {
                    format!("suggesting as {}", self.config.author)
                } else {
                    "editing".to_string()
                });
            }
            "accept" | "reject" => self.resolve_at_cursor(name == "accept"),
            "acceptall" | "rejectall" => self.resolve_all(name == "acceptall"),
            "author" => {
                if arg.is_empty() {
                    self.info(self.config.author.clone());
                } else if is_valid_author(arg) {
                    self.config.author = arg.to_string();
                } else {
                    self.error("that name cannot be written into suggestion markup");
                }
            }
            "focus" => self.config.focus = !self.config.focus,
            "typewriter" => self.config.typewriter = !self.config.typewriter,
            "noh" | "nohlsearch" => self.vim.highlight = false,
            "u" | "undo" => self.undo(1),
            "red" | "redo" => self.redo(1),
            "h" | "help" | "commands" | "menu" => self.open_palette(PaletteKind::Help),
            _ => self.error(format!("Not an editor command: {name}")),
        }
    }

    /// `:s/pattern/replacement/[g]`: a regex with `\1` and `&` in the
    /// replacement, case-smart like `/`.
    fn substitute(&mut self, spec: &str, whole: bool) {
        let delimiter = spec.chars().next().unwrap();
        let mut parts = spec[delimiter.len_utf8()..].split(delimiter);
        let (pattern, replacement, flags) = (
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        );
        let Some(matcher) = Matcher::new(pattern, self.config.regex_search) else {
            return self.error("empty pattern");
        };
        let current = self.buf.line_of(self.cursor);
        let lines = if whole { 0..self.buf.line_count() } else { current..current + 1 };
        let mut count = 0;
        for line in lines.rev() {
            let start = self.buf.line_start(line);
            let mut matches = matcher.replacements(&self.buf.line_text(line), Some(replacement));
            if !flags.contains('g') {
                matches.truncate(1);
            }
            for (found, replacement) in matches.into_iter().rev() {
                self.cursor = self.edit(start + found.start..start + found.end, &replacement).start;
                count += 1;
            }
        }
        if count == 0 {
            self.error(format!("Pattern not found: {pattern}"));
        } else {
            self.info(format!("{count} substitution{}", if count == 1 { "" } else { "s" }));
        }
    }
}
