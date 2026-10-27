//! Suggestion mode: edits are written as CriticMarkup instead of applied, and
//! resolved later by replacing the whole markup span.

use super::{EditPos, Editor};
use crate::critic::{Kind, Suggestion, compose_checked, is_valid_author};
use std::ops::Range;

const UNREPRESENTABLE: &str = "that text cannot be written inside a suggestion";

impl Editor {
    pub(super) fn suggest_edit(&mut self, range: Range<usize>, text: &str) -> EditPos {
        let unchanged = EditPos {
            start: range.start,
            end: range.end,
        };
        match self.try_suggest(range, text) {
            Ok(pos) => pos,
            Err(why) => {
                self.error(why);
                unchanged
            }
        }
    }

    pub(super) fn try_suggest(&mut self, range: Range<usize>, text: &str) -> Result<EditPos, &'static str> {
        self.refresh();
        let author = self.config.author.clone();
        if !is_valid_author(&author) {
            return Err("set a display name first: :author <name>");
        }
        let lines = self.buf.line_of(range.start)..=self.buf.line_of(range.end);
        if lines.into_iter().any(|line| self.doc.in_fence(line)) {
            return Err("suggestions cannot be recorded inside a code fence");
        }
        let suggestions = self.doc.suggestions.clone();
        let author = author.as_str();
        let own = |kind: Kind| suggestions.iter().filter(move |s| s.author == author && s.kind == kind);
        let typed = text.chars().count();

        // Inside our own suggested text: a plain edit that reshapes the suggestion.
        let inside = suggestions.iter().find(|s| {
            let new = s.new_chars();
            s.author == author && s.kind != Kind::Delete && new.start <= range.start && range.end <= new.end
        });
        if let Some(s) = inside {
            let offset = s.new_chars().start;
            let mut new_text: Vec<char> = s.new_text.chars().collect();
            new_text.splice(range.start - offset..range.end - offset, text.chars());
            let new_text: String = new_text.into_iter().collect();
            if s.kind == Kind::Insert && new_text.is_empty() {
                let pos = self.raw_edit(s.span.clone(), "");
                return Ok(EditPos {
                    start: pos.start,
                    end: pos.start,
                });
            }
            compose_checked(s.kind, &s.old_text, &new_text, &s.id, &s.author).ok_or(UNREPRESENTABLE)?;
            return Ok(self.raw_edit(range, text));
        }

        let overlaps = |s: &Suggestion| {
            if range.is_empty() {
                s.span.start < range.start && range.start < s.span.end
            } else {
                s.span.start < range.end && range.start < s.span.end
            }
        };
        if suggestions.iter().any(overlaps) {
            return Err("resolve the suggestion here first (gsa accepts, gsr rejects)");
        }

        let old = self.buf.slice(range.clone());
        let old_len = old.chars().count();
        if range.is_empty() {
            // Typing right after our own deletion turns it into a replacement.
            if let Some(s) = own(Kind::Delete).find(|s| s.span.end == range.start) {
                let markup = compose_checked(Kind::Replace, &s.old_text, text, &s.id, author).ok_or(UNREPRESENTABLE)?;
                let start = self.raw_edit(s.span.clone(), &markup).start + 3 + s.old_text.chars().count() + 2;
                return Ok(EditPos {
                    start,
                    end: start + typed,
                });
            }
            let id = self.next_id();
            let markup = compose_checked(Kind::Insert, "", text, &id, author).ok_or(UNREPRESENTABLE)?;
            let start = self.raw_edit(range, &markup).start + 3;
            return Ok(EditPos {
                start,
                end: start + typed,
            });
        }
        if text.is_empty() {
            // Repeated backspace or forward-delete grows one deletion.
            let before = own(Kind::Delete).find(|s| s.span.start == range.end);
            let after = own(Kind::Delete).find(|s| s.span.end == range.start);
            let (span, merged, id) = match (before, after) {
                (Some(s), _) => (range.start..s.span.end, format!("{old}{}", s.old_text), s.id.clone()),
                (_, Some(s)) => (s.span.start..range.end, format!("{}{old}", s.old_text), s.id.clone()),
                _ => (range, old, self.next_id()),
            };
            let markup = compose_checked(Kind::Delete, &merged, "", &id, author).ok_or(UNREPRESENTABLE)?;
            return Ok(self.raw_edit(span, &markup));
        }
        let id = self.next_id();
        let markup = compose_checked(Kind::Replace, &old, text, &id, author).ok_or(UNREPRESENTABLE)?;
        let start = self.raw_edit(range, &markup).start + 3 + old_len + 2;
        Ok(EditPos {
            start,
            end: start + typed,
        })
    }

    /// Deletes whole lines as a suggestion. The lines may run into our own
    /// suggestions: an insertion there is retracted, and a deletion grows to
    /// take the lines with it.
    pub(super) fn suggest_delete_lines(&mut self, range: Range<usize>) -> EditPos {
        self.refresh();
        let suggestions = self.doc.suggestions.clone();
        let touched = |range: &Range<usize>| {
            let mut spans: Vec<&Suggestion> = suggestions
                .iter()
                .filter(|s| s.span.start < range.end && range.start < s.span.end)
                .collect();
            spans.sort_by_key(|s| s.span.start);
            spans
        };
        if touched(&range).is_empty() {
            return self.suggest_edit(range, "");
        }
        let unchanged = EditPos {
            start: range.start,
            end: range.end,
        };
        let author = self.config.author.clone();
        if !is_valid_author(&author) {
            self.error("set a display name first: :author <name>");
            return unchanged;
        }
        // Take in whole every suggestion the lines run into, and the rest of
        // the line the last one ends on.
        let mut range = range;
        loop {
            let spans = touched(&range);
            if spans.iter().any(|s| s.author != author) {
                self.error("resolve the suggestion here first (gsa accepts, gsr rejects)");
                return unchanged;
            }
            let start = spans.iter().map(|s| s.span.start).fold(range.start, usize::min);
            let mut end = spans.iter().map(|s| s.span.end).fold(range.end, usize::max);
            if end > range.end {
                let line = self.buf.line_of(end);
                end = if line + 1 < self.buf.line_count() {
                    self.buf.line_start(line + 1)
                } else {
                    self.buf.len()
                };
            }
            if (start, end) == (range.start, range.end) {
                break;
            }
            range = start..end;
        }
        let lines = self.buf.line_of(range.start)..=self.buf.line_of(range.end);
        if lines.into_iter().any(|line| self.doc.in_fence(line)) {
            self.error("suggestions cannot be recorded inside a code fence");
            return unchanged;
        }
        let spans = touched(&range);
        let mut deleted = String::new();
        let mut at = range.start;
        let mut inserted_line = false;
        for s in &spans {
            deleted.push_str(&self.buf.slice(at..s.span.start));
            deleted.push_str(&s.old_text);
            inserted_line |= s.kind != Kind::Delete && s.new_text.contains('\n');
            at = s.span.end;
        }
        deleted.push_str(&self.buf.slice(at..range.end));
        let id = match spans.iter().find(|s| s.kind == Kind::Delete) {
            Some(s) => s.id.clone(),
            None => self.next_id(),
        };
        // A line we inserted goes on its own; the break it followed stays.
        let text = if deleted.is_empty() || (inserted_line && deleted == "\n") {
            deleted
        } else {
            match compose_checked(Kind::Delete, &deleted, "", &id, &author) {
                Some(markup) => markup,
                None => {
                    self.error(UNREPRESENTABLE);
                    return unchanged;
                }
            }
        };
        self.raw_edit(range, &text)
    }

    /// Accepts or rejects the suggestion under the cursor, or the next one on
    /// the cursor line.
    pub(super) fn resolve_at_cursor(&mut self, accept: bool) {
        self.refresh();
        let cursor = self.cursor;
        let line = self.buf.line_of(cursor);
        let (start, end) = (self.buf.line_start(line), self.buf.line_end(line));
        let on_line = |s: &&Suggestion| s.span.start <= end && start < s.span.end;
        let suggestions = &self.doc.suggestions;
        let found = suggestions
            .iter()
            .find(|s| s.span.contains(&cursor))
            .or_else(|| suggestions.iter().filter(on_line).find(|s| s.span.start >= cursor))
            .or_else(|| suggestions.iter().rfind(on_line))
            .cloned();
        let Some(suggestion) = found else {
            return self.error("no suggestion here");
        };
        let (span, text) = if accept {
            suggestion.accept()
        } else {
            suggestion.reject()
        };
        self.cursor = self.raw_edit(span, text).start;
        self.close_group();
        self.info(format!(
            "{} {}'s suggestion",
            if accept { "accepted" } else { "rejected" },
            suggestion.author
        ));
    }

    pub(super) fn resolve_all(&mut self, accept: bool) {
        self.refresh();
        let suggestions = self.doc.suggestions.clone();
        for suggestion in suggestions.iter().rev() {
            let (span, text) = if accept {
                suggestion.accept()
            } else {
                suggestion.reject()
            };
            self.raw_edit(span, text);
        }
        self.cursor = self.cursor.min(self.buf.len());
        self.close_group();
        let verb = if accept { "accepted" } else { "rejected" };
        self.info(format!("{} suggestions {verb}", suggestions.len()));
    }
}
