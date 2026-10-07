//! Rope-backed text buffer with grouped undo. Positions are char indices.

use ropey::Rope;
use std::ops::Range;

#[derive(Clone, Debug)]
struct Edit {
    at: usize,
    removed: String,
    inserted: String,
}

#[derive(Clone, Debug, Default)]
struct Txn {
    edits: Vec<Edit>,
    cursor_before: usize,
    cursor_after: usize,
    state_before: u64,
    state_after: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

pub struct Buffer {
    rope: Rope,
    undo: Vec<Txn>,
    redo: Vec<Txn>,
    open: Option<Txn>,
    depth: u32,
    /// Bumps on every change; cache key for anything derived from the text.
    revision: u64,
    /// Identity of the current text state; undo restores earlier identities.
    state: u64,
    next_state: u64,
    saved_state: u64,
    pub line_ending: LineEnding,
    /// Every change since revision `ops_base`, one per revision, so a position
    /// read at an earlier revision can be carried forward to now.
    ops: Vec<Op>,
    ops_base: u64,
}

/// One change: `removed` chars at `at` became `inserted` chars.
#[derive(Clone, Copy, Debug)]
struct Op {
    at: usize,
    removed: usize,
    inserted: usize,
}

/// Where a range read at an earlier revision is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rebased {
    At(Range<usize>),
    /// Text inside the range changed in the meantime.
    Conflict,
    /// The revision is too old (or not from this buffer) to follow.
    Unknown,
}

const OP_LOG: usize = 8192;

impl Default for Buffer {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl Buffer {
    /// Line endings are normalized to `\n` in memory and restored by `to_file_string`.
    pub fn from_text(text: &str) -> Self {
        let line_ending = if text.contains("\r\n") {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        };
        let rope = if text.contains('\r') {
            Rope::from_str(&text.replace("\r\n", "\n").replace('\r', "\n"))
        } else {
            Rope::from_str(text)
        };
        Self {
            rope,
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            depth: 0,
            revision: 0,
            state: 0,
            next_state: 1,
            saved_state: 0,
            line_ending,
            ops: Vec::new(),
            ops_base: 0,
        }
    }

    pub fn to_file_string(&self) -> String {
        let text = self.text();
        match self.line_ending {
            LineEnding::Lf => text,
            LineEnding::CrLf => text.replace('\n', "\r\n"),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn is_dirty(&self) -> bool {
        self.state != self.saved_state
    }
    pub fn mark_saved(&mut self) {
        self.saved_state = self.state;
    }

    pub fn len(&self) -> usize {
        self.rope.len_chars()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }
    pub fn text(&self) -> String {
        String::from(&self.rope)
    }
    pub fn slice(&self, range: Range<usize>) -> String {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        self.rope.slice(start..end).to_string()
    }
    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.rope.get_char(pos)
    }
    pub fn byte_to_char(&self, byte: usize) -> usize {
        self.rope.byte_to_char(byte.min(self.rope.len_bytes()))
    }
    pub fn char_to_byte(&self, pos: usize) -> usize {
        self.rope.char_to_byte(pos.min(self.len()))
    }

    /// A trailing newline yields a final empty line, which is addressable.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }
    pub fn line_of(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.len()))
    }
    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.line_count()))
    }
    /// Char index of the line's newline (or buffer end for the last line).
    pub fn line_end(&self, line: usize) -> usize {
        if line + 1 >= self.line_count() {
            self.len()
        } else {
            self.rope.line_to_char(line + 1) - 1
        }
    }
    pub fn line_len(&self, line: usize) -> usize {
        self.line_end(line) - self.line_start(line)
    }
    /// Line text without its newline.
    pub fn line_text(&self, line: usize) -> String {
        self.slice(self.line_start(line)..self.line_end(line))
    }
    pub fn line_is_blank(&self, line: usize) -> bool {
        let (start, end) = (self.line_start(line), self.line_end(line));
        self.rope.slice(start..end).chars().all(char::is_whitespace)
    }

    /// Opens an undo group; nested calls join the outer group.
    pub fn begin(&mut self, cursor: usize) {
        if self.depth == 0 && self.open.is_none() {
            self.open = Some(Txn {
                cursor_before: cursor,
                cursor_after: cursor,
                state_before: self.state,
                ..Txn::default()
            });
        }
        self.depth += 1;
    }

    pub fn commit(&mut self, cursor: usize) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth > 0 {
            return;
        }
        if let Some(mut txn) = self.open.take()
            && !txn.edits.is_empty() {
                txn.cursor_after = cursor;
                txn.state_after = self.state;
                self.undo.push(txn);
            }
    }

    pub fn in_group(&self) -> bool {
        self.depth > 0
    }

    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let end = range.end.min(self.len());
        let start = range.start.min(end);
        if start == end && text.is_empty() {
            return;
        }
        let standalone = self.open.is_none();
        if standalone {
            self.begin(start);
        }
        let removed = self.rope.slice(start..end).to_string();
        self.apply(start, removed.chars().count(), text);
        self.state = self.next_state;
        self.next_state += 1;
        self.redo.clear();
        if let Some(txn) = &mut self.open {
            txn.edits.push(Edit {
                at: start,
                removed,
                inserted: text.to_string(),
            });
        }
        if standalone {
            self.commit(start + text.chars().count());
        }
    }

    fn apply(&mut self, at: usize, remove: usize, insert: &str) {
        if remove > 0 {
            self.rope.remove(at..at + remove);
        }
        if !insert.is_empty() {
            self.rope.insert(at, insert);
        }
        self.revision += 1;
        if self.ops.len() >= OP_LOG {
            self.ops.drain(..OP_LOG / 2);
            self.ops_base += (OP_LOG / 2) as u64;
        }
        self.ops.push(Op { at, removed: remove, inserted: insert.chars().count() });
    }

    /// Carries `range`, as it was at `revision`, through every change since.
    /// Text typed at its edges stays outside it; a change inside it is a
    /// conflict. This is the transform half of operational transformation,
    /// with this buffer as the single authority.
    pub fn rebase(&self, range: Range<usize>, revision: u64) -> Rebased {
        if revision > self.revision || revision < self.ops_base {
            return Rebased::Unknown;
        }
        let (mut start, mut end) = (range.start, range.end.max(range.start));
        for op in &self.ops[(revision - self.ops_base) as usize..] {
            let (at, gone) = (op.at, op.at + op.removed);
            let delta = op.inserted as isize - op.removed as isize;
            let shift = |pos: usize| (pos as isize + delta) as usize;
            if gone <= start && (at < start || op.removed > 0 || start < end) {
                // Entirely before the range (an insertion exactly at the
                // start of a non-empty range counts as before it).
                (start, end) = (shift(start), shift(end));
            } else if gone <= start {
                // An insertion exactly at an empty range: the range stays put.
            } else if at >= end {
                // Entirely after.
            } else {
                return Rebased::Conflict;
            }
        }
        Rebased::At(start..end)
    }

    /// Carries one position forward; inside deleted text it lands where the
    /// deletion was. `stick_left` keeps it before text inserted exactly there.
    pub fn rebase_pos(&self, pos: usize, revision: u64, stick_left: bool) -> usize {
        if revision > self.revision || revision < self.ops_base {
            return pos.min(self.len());
        }
        let mut pos = pos;
        for op in &self.ops[(revision - self.ops_base) as usize..] {
            let gone = op.at + op.removed;
            if pos > gone || (pos == gone && (op.removed > 0 || !stick_left) && pos > op.at) || (pos == op.at && op.removed == 0 && !stick_left) {
                pos = (pos as isize + op.inserted as isize - op.removed as isize) as usize;
            } else if pos > op.at {
                pos = op.at + op.inserted;
            }
        }
        pos.min(self.len())
    }

    /// Returns the cursor position to restore.
    pub fn undo(&mut self) -> Option<usize> {
        self.close_open();
        let txn = self.undo.pop()?;
        for edit in txn.edits.iter().rev() {
            self.apply(edit.at, edit.inserted.chars().count(), &edit.removed);
        }
        self.state = txn.state_before;
        let cursor = txn.cursor_before;
        self.redo.push(txn);
        Some(cursor.min(self.len()))
    }

    pub fn redo(&mut self) -> Option<usize> {
        self.close_open();
        let txn = self.redo.pop()?;
        for edit in &txn.edits {
            self.apply(edit.at, edit.removed.chars().count(), &edit.inserted);
        }
        self.state = txn.state_after;
        let cursor = txn.cursor_after;
        self.undo.push(txn);
        Some(cursor.min(self.len()))
    }

    fn close_open(&mut self) {
        if self.open.is_some() {
            self.depth = 1;
            let cursor = self.open.as_ref().map_or(0, |txn| txn.cursor_after);
            self.commit(cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_positions() {
        let buf = Buffer::from_text("ab\n\ncd\n");
        assert_eq!(buf.line_count(), 4);
        assert_eq!(buf.line_text(0), "ab");
        assert_eq!(buf.line_text(1), "");
        assert_eq!(buf.line_end(2), 6);
        assert_eq!(buf.line_len(3), 0);
        assert_eq!(buf.line_of(4), 2);
    }

    #[test]
    fn grouped_undo_restores_text_cursor_and_dirty_state() {
        let mut buf = Buffer::from_text("hello");
        buf.begin(5);
        buf.replace(5..5, " world");
        buf.replace(0..1, "H");
        buf.commit(11);
        assert_eq!(buf.text(), "Hello world");
        assert!(buf.is_dirty());
        assert_eq!(buf.undo(), Some(5));
        assert_eq!(buf.text(), "hello");
        assert!(!buf.is_dirty());
        assert_eq!(buf.redo(), Some(11));
        assert_eq!(buf.text(), "Hello world");
        assert_eq!(buf.undo(), Some(5));
        buf.replace(0..0, "x");
        assert_eq!(buf.redo(), None);
    }

    #[test]
    fn crlf_round_trips() {
        let buf = Buffer::from_text("a\r\nb\r\n");
        assert_eq!(buf.text(), "a\nb\n");
        assert_eq!(buf.to_file_string(), "a\r\nb\r\n");
    }

    #[test]
    fn unicode_positions_are_chars() {
        let mut buf = Buffer::from_text("😀é\nx");
        buf.replace(1..2, "");
        assert_eq!(buf.text(), "😀\nx");
        assert_eq!(buf.line_end(0), 1);
    }

    #[test]
    fn ranges_follow_later_edits() {
        let mut buf = Buffer::from_text("one two three");
        let revision = buf.revision();
        let two = 4..7;
        buf.replace(0..0, ">> ");
        assert_eq!(buf.rebase(two.clone(), revision), Rebased::At(7..10));
        buf.replace(13..13, "!");
        assert_eq!(buf.rebase(two.clone(), revision), Rebased::At(7..10));
        // Typing right at either edge stays outside the range.
        buf.replace(7..7, "[");
        buf.replace(11..11, "]");
        assert_eq!(buf.rebase(two.clone(), revision), Rebased::At(8..11));
        assert_eq!(buf.slice(8..11), "two");
        // An insertion point keeps its place when text lands exactly on it.
        let here = buf.revision();
        buf.replace(8..8, "x");
        assert_eq!(buf.rebase(8..8, here), Rebased::At(8..8));
        assert_eq!(buf.rebase_pos(8, here, true), 8);
        assert_eq!(buf.rebase_pos(8, here, false), 9);
        // A change inside the range is a conflict; undo is just another change.
        buf.replace(10..11, "W");
        assert_eq!(buf.rebase(two.clone(), revision), Rebased::Conflict);
        buf.undo();
        assert_eq!(buf.rebase(two, revision), Rebased::Conflict);
        assert_eq!(buf.rebase(0..1, buf.revision() + 1), Rebased::Unknown);
        assert_eq!(buf.rebase_pos(5, revision, true), 10);
    }
}
