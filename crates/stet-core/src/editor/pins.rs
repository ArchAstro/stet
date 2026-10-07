//! Pins: sections of the margin attached to places in the document.
//!
//! A margin section is a heading and what follows it. A line `@ …` right
//! under the heading pins the section: `@ # Heading` to a heading, anything
//! else to the first place those words appear. The anchor lives in the
//! margin, so the document carries no trace of it.
//!
//! While the document is being edited a pin follows its text: if the
//! anchored words change, the `@` line is rewritten to match.

use super::links::slug;
use super::{Editor, Mode};
use crate::markdown::Block;
use std::collections::HashMap;
use std::ops::Range;

/// Longer anchors are stored as their beginning and end.
const ANCHOR_MAX: usize = 96;
const ELLIPSIS: &str = " … ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// The section's heading text.
    pub title: String,
    /// Margin lines of the section, heading first.
    pub lines: Range<usize>,
    /// What follows `@`; empty for a section that is not pinned.
    pub anchor: String,
    /// The anchored chars of the document, if the anchor was found.
    pub at: Option<Range<usize>>,
}

impl Pin {
    pub fn pinned(&self) -> bool {
        self.at.is_some()
    }
}

#[derive(Default)]
pub(super) struct Pins {
    /// `(document revision, margin revision)` the list was computed for.
    key: Option<(u64, u64, u64)>,
    list: Vec<Pin>,
    /// Where each anchor was last found, to follow it through edits.
    seen: HashMap<String, (Range<usize>, u64)>,
}

struct Section {
    title: String,
    lines: Range<usize>,
    /// The `@` line and its text.
    anchor: Option<(usize, String)>,
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The anchor text for a stretch of document.
fn anchor_for(text: &str) -> String {
    let text = squash(text);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= ANCHOR_MAX {
        return text;
    }
    let head: String = chars[..56].iter().collect();
    let tail: String = chars[chars.len() - 32..].iter().collect();
    format!("{}{ELLIPSIS}{}", head.trim_end(), tail.trim_start())
}

/// Finds `needle` in `hay` ignoring case and runs of whitespace. Returns a
/// char range of `hay`.
fn find_loose(hay: &[char], needle: &str, from: usize) -> Option<Range<usize>> {
    let needle: Vec<char> = squash(needle).to_lowercase().chars().collect();
    if needle.is_empty() {
        return None;
    }
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    'start: for start in from..hay.len() {
        if hay[start].is_whitespace() {
            continue;
        }
        let (mut at, mut want) = (start, 0);
        while want < needle.len() {
            let Some(&c) = hay.get(at) else { continue 'start };
            if needle[want] == ' ' {
                if !c.is_whitespace() {
                    continue 'start;
                }
                while hay.get(at).is_some_and(|c| c.is_whitespace()) {
                    at += 1;
                }
            } else if lower(c) == needle[want] {
                at += 1;
            } else {
                continue 'start;
            }
            want += 1;
        }
        return Some(start..at);
    }
    None
}

impl Editor {
    /// The sections of the margin, which must be the buffer in place.
    fn margin_sections(&mut self) -> Vec<Section> {
        self.refresh();
        let mut out: Vec<Section> = Vec::new();
        let count = self.buf.line_count();
        for line in 0..count {
            let text = self.buf.line_text(line);
            if matches!(self.doc.block(line), Block::Heading(_)) && text.trim_start().starts_with('#') {
                if let Some(last) = out.last_mut() {
                    last.lines.end = line;
                }
                out.push(Section {
                    title: text.trim_start_matches(['#', ' ']).trim().to_string(),
                    lines: line..count,
                    anchor: None,
                });
            } else if let Some(section) = out.last_mut()
                && section.anchor.is_none()
                && let Some(anchor) = text.strip_prefix("@ ")
            {
                // Only directly under the heading, blank lines aside.
                let between = section.lines.start + 1..line;
                if between.into_iter().all(|line| self.buf.line_is_blank(line)) {
                    section.anchor = Some((line, anchor.trim().to_string()));
                }
            }
        }
        out
    }

    /// Where `anchor` is in the document, which must be the buffer in place.
    fn resolve_anchor(&mut self, anchor: &str) -> Option<Range<usize>> {
        if let Some(heading) = anchor.strip_prefix('#') {
            self.refresh();
            let wanted = slug(heading.trim_start_matches('#'));
            let line = (0..self.buf.line_count()).find(|&line| {
                matches!(self.doc.block(line), Block::Heading(_))
                    && slug(self.buf.line_text(line).trim_start_matches(['#', ' '])) == wanted
            })?;
            return Some(self.buf.line_start(line)..self.buf.line_end(line));
        }
        let hay: Vec<char> = self.buf.text().chars().collect();
        match anchor.split_once(ELLIPSIS) {
            Some((head, tail)) => {
                let start = find_loose(&hay, head, 0)?;
                let end = find_loose(&hay, tail, start.end)?;
                Some(start.start..end.end)
            }
            None => find_loose(&hay, anchor, 0),
        }
    }

    fn pane_revisions(&self) -> Option<(u64, u64, u64)> {
        let (document, margin) = if self.margin_active {
            (self.margin_parent.as_ref()?, None)
        } else {
            (self.margins.get(self.path.as_ref()?)?, Some(()))
        };
        // `document` above is whichever buffer is parked; sort out which is which.
        let (doc_rev, margin_rev) = match margin {
            None => (document.buf.revision(), self.buf.revision()),
            Some(()) => (self.buf.revision(), document.buf.revision()),
        };
        Some((self.doc_id, doc_rev, margin_rev))
    }

    /// Every section of the margin, with where it is pinned. Cheap when
    /// neither pane changed since the last call.
    pub fn pins(&mut self) -> Vec<Pin> {
        if self.margin_owner().is_none() {
            return Vec::new();
        }
        let key = self.pane_revisions();
        if key.is_some() && self.pin_state.key == key {
            return self.pin_state.list.clone();
        }
        let list = if self.margin_active {
            self.with_other_pane(|document| document.compute_pins())
                .unwrap_or_default()
        } else {
            self.compute_pins()
        };
        // Following a pin may have rewritten its `@` line.
        self.pin_state.key = self.pane_revisions();
        self.pin_state.list = list.clone();
        list
    }

    /// With the document in place.
    fn compute_pins(&mut self) -> Vec<Pin> {
        let sections = self
            .with_other_pane(|margin| margin.margin_sections())
            .unwrap_or_default();
        let revision = self.buf.revision();
        let mut seen = std::mem::take(&mut self.pin_state.seen);
        let mut rewrites: Vec<(usize, String)> = Vec::new();
        let mut list = Vec::with_capacity(sections.len());
        for section in sections {
            let (mut anchor, mut at) = (String::new(), None);
            if let Some((line, text)) = section.anchor {
                at = self.resolve_anchor(&text);
                anchor = text;
                if at.is_none()
                    && let Some((range, then)) = seen.get(&anchor).cloned()
                {
                    // The words were edited: stay with the text they became.
                    let moved =
                        self.buf.rebase_pos(range.start, then, false)..self.buf.rebase_pos(range.end, then, true);
                    let now = anchor_for(&self.buf.slice(moved.clone()));
                    if !now.is_empty() && self.resolve_anchor(&now).is_some() {
                        seen.remove(&anchor);
                        rewrites.push((line, now.clone()));
                        at = self.resolve_anchor(&now);
                        anchor = now;
                    }
                }
                if let Some(range) = &at {
                    seen.insert(anchor.clone(), (range.clone(), revision));
                }
            }
            list.push(Pin {
                title: section.title,
                lines: section.lines,
                anchor,
                at,
            });
        }
        self.pin_state.seen = seen;
        if !rewrites.is_empty() {
            self.with_other_pane(|margin| {
                for (line, anchor) in rewrites {
                    let range = margin.buf.line_start(line)..margin.buf.line_end(line);
                    margin.raw_edit(range, &format!("@ {anchor}"));
                }
            });
        }
        list
    }

    /// What the document's cursor is on, as an anchor: the selection, the
    /// heading on this line, or the line's opening words.
    fn anchor_here(&mut self) -> Option<String> {
        self.refresh();
        if let Some(selection) = self.selection() {
            let text = anchor_for(&self.buf.slice(selection));
            return (!text.is_empty()).then_some(text);
        }
        let line = self.buf.line_of(self.cursor);
        let text = self.buf.line_text(line);
        if matches!(self.doc.block(line), Block::Heading(_)) && text.trim_start().starts_with('#') {
            return Some(format!("# {}", text.trim_start_matches(['#', ' ']).trim()));
        }
        let words: Vec<&str> = text.split_whitespace().take(12).collect();
        (!words.is_empty()).then(|| words.join(" "))
    }

    /// From the document: starts a new margin note pinned to the selection
    /// or the line under the cursor, and puts the keyboard in it. From the
    /// margin: pins the section under the cursor to where the document's
    /// cursor is.
    pub fn pin_here(&mut self) {
        if self.margin_active {
            let Some(Some(anchor)) = self.with_other_pane(|document| document.anchor_here()) else {
                return self.error("the document's cursor is on an empty line");
            };
            let line = self.buf.line_of(self.cursor);
            let Some(section) = self
                .margin_sections()
                .into_iter()
                .find(|section| section.lines.contains(&line))
            else {
                return self.error("put the cursor in a section (under a heading) to pin it");
            };
            match section.anchor {
                Some((line, _)) => {
                    let range = self.buf.line_start(line)..self.buf.line_end(line);
                    self.raw_edit(range, &format!("@ {anchor}"));
                }
                None => {
                    let at = self.buf.line_end(section.lines.start);
                    self.raw_edit(at..at, &format!("\n@ {anchor}"));
                }
            }
            self.close_group();
            return self.info("pinned to the document's cursor");
        }
        let Some(anchor) = self.anchor_here() else {
            return self.error("nothing here to pin a note to");
        };
        let title: String = anchor
            .trim_start_matches(['#', ' '])
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        let title = title.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string();
        self.anchor = None;
        if self.mode.is_visual() {
            self.mode = Mode::Normal;
        }
        self.focus_margin();
        if !self.margin_active {
            return;
        }
        let end = self.buf.len();
        let lead = match self.buf.slice(end.saturating_sub(2)..end).as_str() {
            _ if end == 0 => "",
            "\n\n" => "",
            tail if tail.ends_with('\n') => "\n",
            _ => "\n\n",
        };
        let pos = self.raw_edit(end..end, &format!("{lead}## {title}\n@ {anchor}\n\n"));
        self.cursor = pos.end;
        self.close_group();
        // Ready to type the note.
        self.mode = Mode::Insert;
        self.open_group();
    }

    /// Removes the pin of the margin section under the cursor.
    pub fn unpin(&mut self) {
        if !self.margin_active {
            return self.error("unpin from the margin, with the cursor in the section");
        }
        let line = self.buf.line_of(self.cursor);
        let section = self
            .margin_sections()
            .into_iter()
            .find(|section| section.lines.contains(&line));
        match section.and_then(|section| section.anchor) {
            Some((line, _)) => {
                let end = (self.buf.line_end(line) + 1).min(self.buf.len());
                self.cursor = self.raw_edit(self.buf.line_start(line)..end, "").start;
                self.close_group();
                self.clamp_cursor();
            }
            None => self.error("this section is not pinned"),
        }
    }

    /// Crosses to the other side of a pin: from anchored text to its note,
    /// from a note to the text it is pinned to.
    pub fn follow_pin(&mut self) {
        let pins = self.pins();
        if self.margin_active {
            let line = self.buf.line_of(self.cursor);
            match pins
                .iter()
                .find(|pin| pin.lines.contains(&line))
                .and_then(|pin| pin.at.clone())
            {
                Some(at) => {
                    self.focus_document();
                    self.cursor = at.start.min(self.buf.len());
                    self.clamp_cursor();
                }
                None => self.error("this section is not pinned anywhere"),
            }
            return;
        }
        let cursor = self.cursor;
        let line = self.buf.line_of(cursor);
        let on_line = |at: &Range<usize>| {
            self.buf.line_of(at.start) <= line && line <= self.buf.line_of(at.end.max(at.start + 1) - 1)
        };
        let found = pins
            .iter()
            .filter(|pin| pin.at.is_some())
            .position(|pin| pin.at.as_ref().is_some_and(|at| at.contains(&cursor)))
            .or_else(|| pins.iter().position(|pin| pin.at.as_ref().is_some_and(on_line)));
        match found {
            Some(index) => self.open_note(index),
            None => self.error("no note is pinned here"),
        }
    }

    /// Puts the keyboard in the margin at pin `index` (as `pins` lists them).
    pub fn open_note(&mut self, index: usize) {
        let Some(pin) = self.pins().get(index).cloned() else {
            return;
        };
        self.focus_margin();
        if self.margin_active {
            self.cursor = self.buf.line_start(pin.lines.start.min(self.buf.line_count() - 1));
            self.clamp_cursor();
        }
    }
}
