//! Cursor motions and text objects, as pure functions over the buffer.

use crate::buffer::Buffer;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn class_at(buf: &Buffer, pos: usize, big: bool) -> Class {
    buf.char_at(pos).map_or(Class::Space, |c| class(c, big))
}

fn is_empty_line_at(buf: &Buffer, pos: usize) -> bool {
    buf.char_at(pos) == Some('\n') && (pos == 0 || buf.char_at(pos - 1) == Some('\n'))
}

pub fn word_forward(buf: &Buffer, pos: usize, big: bool) -> usize {
    let len = buf.len();
    let mut at = pos;
    let start = class_at(buf, at, big);
    if start != Class::Space {
        while at < len && class_at(buf, at, big) == start {
            at += 1;
        }
    }
    while at < len && class_at(buf, at, big) == Class::Space {
        if at > pos && is_empty_line_at(buf, at) {
            break;
        }
        at += 1;
    }
    at
}

pub fn word_backward(buf: &Buffer, pos: usize, big: bool) -> usize {
    let mut at = pos;
    if at == 0 {
        return 0;
    }
    at -= 1;
    while at > 0 && class_at(buf, at, big) == Class::Space && !is_empty_line_at(buf, at) {
        at -= 1;
    }
    let start = class_at(buf, at, big);
    if start != Class::Space {
        while at > 0 && class_at(buf, at - 1, big) == start {
            at -= 1;
        }
    }
    at
}

/// Inclusive end of the current or next word.
pub fn word_end(buf: &Buffer, pos: usize, big: bool) -> usize {
    let len = buf.len();
    let mut at = pos + 1;
    while at < len && class_at(buf, at, big) == Class::Space {
        at += 1;
    }
    if at >= len {
        return len.saturating_sub(1);
    }
    let start = class_at(buf, at, big);
    while at + 1 < len && class_at(buf, at + 1, big) == start {
        at += 1;
    }
    at
}

/// Inclusive end of the previous word (`ge`).
pub fn word_end_backward(buf: &Buffer, pos: usize, big: bool) -> usize {
    let mut at = pos;
    let start = class_at(buf, at, big);
    if start != Class::Space {
        while at > 0 && class_at(buf, at, big) == start {
            at -= 1;
        }
    }
    while at > 0 && class_at(buf, at, big) == Class::Space {
        at -= 1;
    }
    at
}

pub fn first_non_blank(buf: &Buffer, line: usize) -> usize {
    let (start, end) = (buf.line_start(line), buf.line_end(line));
    (start..end)
        .find(|&at| !buf.char_at(at).is_some_and(char::is_whitespace))
        .unwrap_or(end)
}

pub fn paragraph_forward(buf: &Buffer, pos: usize) -> usize {
    let last = buf.line_count() - 1;
    let mut line = buf.line_of(pos);
    while line < last && buf.line_is_blank(line) {
        line += 1;
    }
    while line < last && !buf.line_is_blank(line) {
        line += 1;
    }
    if buf.line_is_blank(line) {
        buf.line_start(line)
    } else {
        buf.line_end(line)
    }
}

pub fn paragraph_backward(buf: &Buffer, pos: usize) -> usize {
    let mut line = buf.line_of(pos);
    while line > 0 && buf.line_is_blank(line) {
        line -= 1;
    }
    while line > 0 && !buf.line_is_blank(line) {
        line -= 1;
    }
    buf.line_start(line)
}

/// `f`/`t`/`F`/`T` within the cursor line. Returns the landing position.
pub fn find_char(buf: &Buffer, pos: usize, ch: char, forward: bool, till: bool, count: usize) -> Option<usize> {
    let line = buf.line_of(pos);
    let (start, end) = (buf.line_start(line), buf.line_end(line));
    let mut at = pos;
    for _ in 0..count.max(1) {
        at = if forward {
            (at + 1..end).find(|&i| buf.char_at(i) == Some(ch))?
        } else {
            (start..at).rev().find(|&i| buf.char_at(i) == Some(ch))?
        };
    }
    Some(match (till, forward) {
        (false, _) => at,
        (true, true) => at - 1,
        (true, false) => at + 1,
    })
}

const PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

fn pair_of(c: char) -> Option<(char, char, bool)> {
    PAIRS.iter().find_map(|&(open, close)| {
        if c == open {
            Some((open, close, true))
        } else if c == close {
            Some((open, close, false))
        } else {
            None
        }
    })
}

/// `%`: the bracket matching the first bracket at or after the cursor.
pub fn match_pair(buf: &Buffer, pos: usize) -> Option<usize> {
    let end = buf.line_end(buf.line_of(pos));
    let (at, (open, close, forward)) = (pos..end).find_map(|at| {
        let c = buf.char_at(at)?;
        (c != '<' && c != '>')
            .then(|| pair_of(c))
            .flatten()
            .map(|pair| (at, pair))
    })?;
    // Walk away from the bracket; `enter` deepens, `leave` closes.
    let (enter, leave) = if forward { (open, close) } else { (close, open) };
    let mut depth = 0usize;
    let mut step = |i: usize| {
        let c = buf.char_at(i)?;
        if c == enter {
            depth += 1;
        } else if c == leave {
            depth = depth.saturating_sub(1);
            return (depth == 0).then_some(i);
        }
        None
    };
    if forward {
        (at..buf.len()).find_map(&mut step)
    } else {
        (0..=at).rev().find_map(&mut step)
    }
}

fn is_sentence_start(buf: &Buffer, at: usize) -> bool {
    if buf.char_at(at).is_none_or(char::is_whitespace) {
        return false;
    }
    if at == 0 {
        return true;
    }
    let mut back = at;
    let mut newlines = 0;
    while back > 0 && buf.char_at(back - 1).is_some_and(char::is_whitespace) {
        back -= 1;
        newlines += usize::from(buf.char_at(back) == Some('\n'));
    }
    if back == at {
        return false;
    }
    if back == 0 || newlines >= 2 {
        return true;
    }
    while back > 0
        && buf
            .char_at(back - 1)
            .is_some_and(|c| matches!(c, ')' | ']' | '"' | '\'' | '*' | '_' | '”' | '’'))
    {
        back -= 1;
    }
    back > 0 && buf.char_at(back - 1).is_some_and(|c| matches!(c, '.' | '!' | '?'))
}

pub fn sentence_forward(buf: &Buffer, pos: usize) -> usize {
    (pos + 1..buf.len())
        .find(|&at| is_sentence_start(buf, at))
        .unwrap_or(buf.len())
}

pub fn sentence_backward(buf: &Buffer, pos: usize) -> usize {
    (0..pos).rev().find(|&at| is_sentence_start(buf, at)).unwrap_or(0)
}

pub fn next_grapheme(buf: &Buffer, pos: usize) -> usize {
    let len = buf.len();
    if pos >= len {
        return len;
    }
    let ascii = |at: usize| buf.char_at(at).is_none_or(|c| c.is_ascii());
    if buf.char_at(pos) == Some('\n') || (ascii(pos) && ascii(pos + 1)) {
        return pos + 1;
    }
    let line = buf.line_of(pos);
    let start = buf.line_start(line);
    let mut col = 0;
    for grapheme in buf.line_text(line).graphemes(true) {
        col += grapheme.chars().count();
        if start + col > pos {
            break;
        }
    }
    start + col
}

pub fn prev_grapheme(buf: &Buffer, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let line = buf.line_of(pos);
    let start = buf.line_start(line);
    let ascii = |at: usize| buf.char_at(at).is_none_or(|c| c.is_ascii());
    if pos == start || (ascii(pos - 1) && ascii(pos)) {
        return pos - 1;
    }
    let mut col = 0;
    for grapheme in buf.line_text(line).graphemes(true) {
        let next = col + grapheme.chars().count();
        if start + next >= pos {
            break;
        }
        col = next;
    }
    start + col
}

/// A text object's range, and whether it covers whole lines.
pub struct Object {
    pub range: Range<usize>,
    pub linewise: bool,
}

pub fn text_object(buf: &Buffer, pos: usize, around: bool, ch: char) -> Option<Object> {
    let chars = |range: Range<usize>| Some(Object { range, linewise: false });
    match ch {
        'w' | 'W' => {
            let big = ch == 'W';
            let line = buf.line_of(pos);
            let (line_start, line_end) = (buf.line_start(line), buf.line_end(line));
            if line_start == line_end {
                return None;
            }
            let pos = pos.min(line_end - 1);
            let kind = class_at(buf, pos, big);
            let (mut start, mut end) = (pos, pos + 1);
            while start > line_start && class_at(buf, start - 1, big) == kind {
                start -= 1;
            }
            while end < line_end && class_at(buf, end, big) == kind {
                end += 1;
            }
            if around {
                let before = end;
                while end < line_end && class_at(buf, end, big) == Class::Space && kind != Class::Space {
                    end += 1;
                }
                if end == before {
                    while start > line_start && class_at(buf, start - 1, big) == Class::Space {
                        start -= 1;
                    }
                }
            }
            chars(start..end)
        }
        'p' => {
            let last = buf.line_count() - 1;
            let line = buf.line_of(pos);
            let blank = buf.line_is_blank(line);
            let (mut first, mut end) = (line, line);
            while first > 0 && buf.line_is_blank(first - 1) == blank {
                first -= 1;
            }
            while end < last && buf.line_is_blank(end + 1) == blank {
                end += 1;
            }
            if around {
                while end < last && buf.line_is_blank(end + 1) != blank {
                    end += 1;
                }
            }
            Some(Object {
                range: first..end,
                linewise: true,
            })
        }
        's' => {
            let start = if is_sentence_start(buf, pos) {
                pos
            } else {
                sentence_backward(buf, pos)
            };
            let mut end = sentence_forward(buf, pos);
            if !around {
                while end > start + 1 && buf.char_at(end - 1).is_some_and(char::is_whitespace) {
                    end -= 1;
                }
            } else {
                // Never swallow the paragraph break.
                while end > start + 1 && buf.char_at(end - 1) == Some('\n') {
                    end -= 1;
                }
            }
            chars(start..end)
        }
        '"' | '\'' | '`' => {
            let line = buf.line_of(pos);
            let (line_start, line_end) = (buf.line_start(line), buf.line_end(line));
            let quotes: Vec<usize> = (line_start..line_end)
                .filter(|&at| buf.char_at(at) == Some(ch))
                .collect();
            let (open, close) = quotes
                .chunks_exact(2)
                .map(|pair| (pair[0], pair[1]))
                .find(|&(_, close)| close >= pos)?;
            if around {
                chars(open..close + 1)
            } else {
                chars(open + 1..close)
            }
        }
        _ => {
            let (open, close) = match ch {
                'b' => ('(', ')'),
                'B' => ('{', '}'),
                _ => pair_of(ch).map(|(open, close, _)| (open, close))?,
            };
            let mut depth = 0usize;
            let start = (0..=pos.min(buf.len().saturating_sub(1))).rev().find(|&at| {
                let c = buf.char_at(at);
                if c == Some(close) && at != pos {
                    depth += 1;
                } else if c == Some(open) {
                    if depth == 0 {
                        return true;
                    }
                    depth -= 1;
                }
                false
            })?;
            let mut depth = 0usize;
            let end = (start..buf.len()).find(|&at| {
                let c = buf.char_at(at);
                if c == Some(open) {
                    depth += 1;
                } else if c == Some(close) {
                    depth -= 1;
                    return depth == 0;
                }
                false
            })?;
            if around {
                chars(start..end + 1)
            } else {
                chars(start + 1..end)
            }
        }
    }
}

/// A compiled search pattern: a regex with smart case, or the literal text
/// when regexes are off or the pattern does not compile.
pub struct Matcher {
    regex: regex::Regex,
    pattern: String,
    literal: bool,
}

impl Matcher {
    pub fn new(pattern: &str, regex: bool) -> Option<Matcher> {
        if pattern.is_empty() {
            return None;
        }
        let insensitive = !pattern.chars().any(char::is_uppercase);
        let build = |source: &str| {
            regex::RegexBuilder::new(source)
                .case_insensitive(insensitive)
                .multi_line(true)
                .size_limit(1 << 20)
                .build()
                .ok()
        };
        let compiled = regex.then(|| build(pattern)).flatten();
        let literal = compiled.is_none();
        let regex = compiled.or_else(|| build(&regex::escape(pattern)))?;
        Some(Matcher {
            regex,
            pattern: pattern.to_string(),
            literal,
        })
    }

    /// Matches `word` only as a whole word (`*` and `#`).
    pub fn word(word: &str) -> Option<Matcher> {
        let source = format!(r"\b{}\b", regex::escape(word));
        let regex = regex::Regex::new(&source).ok()?;
        Some(Matcher {
            regex,
            pattern: word.to_string(),
            literal: false,
        })
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Non-empty matches as char ranges, in order.
    pub fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        self.replacements(text, None)
            .into_iter()
            .map(|(range, _)| range)
            .collect()
    }

    /// Matches as char ranges, each with `replacement` expanded for it
    /// (`\1`…`\9` and `&` refer to the match when the pattern is a regex).
    pub fn replacements(&self, text: &str, replacement: Option<&str>) -> Vec<(Range<usize>, String)> {
        let template = replacement.map(|replacement| self.template(replacement));
        let mut out = Vec::new();
        let (mut byte, mut chars) = (0, 0);
        for captures in self.regex.captures_iter(text) {
            let found = captures.get(0).unwrap();
            if found.is_empty() {
                continue;
            }
            chars += text[byte..found.start()].chars().count();
            let start = chars;
            chars += found.as_str().chars().count();
            byte = found.end();
            let mut expanded = String::new();
            if let Some(template) = &template {
                captures.expand(template, &mut expanded);
            }
            out.push((start..chars, expanded));
        }
        out
    }

    /// Vim-style replacement text as a `regex` expansion template.
    fn template(&self, replacement: &str) -> String {
        let mut out = String::new();
        let mut chars = replacement.chars();
        while let Some(c) = chars.next() {
            match c {
                '$' => out.push_str("$$"),
                '&' if !self.literal => out.push_str("${0}"),
                '\\' if !self.literal => match chars.next() {
                    Some(digit @ '0'..='9') => out.push_str(&format!("${{{digit}}}")),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                },
                other => out.push(other),
            }
        }
        out
    }
}
