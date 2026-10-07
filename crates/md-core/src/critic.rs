//! Review suggestions stored as CriticMarkup in the document text.
//!
//! Follows the ArchDev plan-collab convention (`SUGGESTION_MARKUP.md`):
//!
//! ```text
//! suggestion = edit identity
//! edit       = "{++" text "++}" | "{--" text "--}" | "{~~" text "~>" text "~~}"
//! identity   = "{>>id:" id " by:" author "<<}"
//! id         = "s_" eight-lowercase-base36-characters
//! ```
//!
//! Malformed markup is ordinary text. Markup inside top-level fenced code
//! blocks is ignored. Spans are half-open and measured in Unicode code points.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Insert,
    Delete,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub id: String,
    pub kind: Kind,
    pub author: String,
    /// Edit plus identity comment, in code points.
    pub span: Range<usize>,
    pub old_text: String,
    pub new_text: String,
    pub bytes: Range<usize>,
    pub old_bytes: Range<usize>,
    pub new_bytes: Range<usize>,
    /// The identity comment.
    pub meta_bytes: Range<usize>,
}

impl Suggestion {
    /// Code-point range of the old text inside the document.
    pub fn old_chars(&self) -> Range<usize> {
        let start = self.span.start + 3;
        match self.kind {
            Kind::Insert => start..start,
            _ => start..start + self.old_text.chars().count(),
        }
    }

    /// Code-point range of the new text inside the document.
    pub fn new_chars(&self) -> Range<usize> {
        let start = match self.kind {
            Kind::Insert => self.span.start + 3,
            Kind::Delete => self.old_chars().end,
            Kind::Replace => self.old_chars().end + 2,
        };
        start..start + self.new_text.chars().count()
    }

    /// Replacement that accepts the suggestion: `(span, new text)`.
    pub fn accept(&self) -> (Range<usize>, &str) {
        (self.span.clone(), &self.new_text)
    }

    /// Replacement that rejects the suggestion: `(span, old text)`.
    pub fn reject(&self) -> (Range<usize>, &str) {
        (self.span.clone(), &self.old_text)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub suggestions: Vec<Suggestion>,
    /// Standalone `{>>comment<<}` byte ranges (not suggestions).
    pub comments: Vec<Range<usize>>,
    /// Top-level fenced code blocks, as byte ranges.
    pub fences: Vec<Range<usize>>,
}

pub fn parse_suggestions(doc: &str) -> Vec<Suggestion> {
    parse(doc).suggestions
}

pub fn parse(doc: &str) -> Parsed {
    let mut parsed = Parsed {
        fences: fences(doc),
        ..Parsed::default()
    };
    let mut region_start = 0;
    let fences = parsed.fences.clone();
    for fence in fences.iter().chain(std::iter::once(&(doc.len()..doc.len()))) {
        parse_region(doc, region_start..fence.start, &mut parsed);
        region_start = fence.end;
    }
    // Code-point spans: one forward pass over the document.
    let mut chars = 0;
    let mut at = 0;
    for suggestion in &mut parsed.suggestions {
        let count = |text: &str| text.as_bytes().iter().filter(|&&byte| (byte as i8) >= -0x40).count();
        chars += count(&doc[at..suggestion.bytes.start]);
        let len = count(&doc[suggestion.bytes.clone()]);
        suggestion.span = chars..chars + len;
        chars += len;
        at = suggestion.bytes.end;
    }
    parsed
}

pub fn compose(kind: Kind, old: &str, new: &str, id: &str, author: &str) -> String {
    let edit = match kind {
        Kind::Insert => format!("{{++{new}++}}"),
        Kind::Delete => format!("{{--{old}--}}"),
        Kind::Replace => format!("{{~~{old}~>{new}~~}}"),
    };
    format!("{edit}{{>>id:{id} by:{author}<<}}")
}

/// Composes markup and verifies it parses back to exactly this suggestion.
/// `None` means the text cannot be represented (it contains reserved tokens).
pub fn compose_checked(kind: Kind, old: &str, new: &str, id: &str, author: &str) -> Option<String> {
    let markup = compose(kind, old, new, id, author);
    let parsed = parse_suggestions(&markup);
    let ok = parsed.len() == 1
        && parsed[0].bytes == (0..markup.len())
        && parsed[0].kind == kind
        && parsed[0].old_text == old
        && parsed[0].new_text == new
        && parsed[0].author == author;
    ok.then_some(markup)
}

pub fn is_valid_author(author: &str) -> bool {
    !author.trim().is_empty()
        && !author.contains(['\n', '\r'])
        && next_token(author.as_bytes(), 0, author.len()).is_none()
}

fn fences(doc: &str) -> Vec<Range<usize>> {
    let bytes = doc.as_bytes();
    let mut out = Vec::new();
    let mut open: Option<(usize, u8, usize)> = None;
    // Only a line holding three backticks or tildes can open or close a
    // fence, so visit just those lines.
    let backticks = memchr::memmem::find_iter(bytes, b"```");
    let tildes = memchr::memmem::find_iter(bytes, b"~~~");
    let mut hits: Vec<usize> = backticks.chain(tildes).collect();
    hits.sort_unstable();
    let mut done = 0;
    for hit in hits {
        if hit < done {
            continue;
        }
        let at = memchr::memrchr2(b'\n', b'\r', &bytes[..hit]).map_or(0, |found| found + 1);
        let end = memchr::memchr2(b'\n', b'\r', &bytes[hit..]).map_or(bytes.len(), |found| hit + found);
        let mut next = end;
        if next < bytes.len() {
            next += if bytes[next] == b'\r' && bytes.get(next + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
        }
        done = next.max(hit + 1);
        let line = &bytes[at..end];
        let indent = line.iter().take_while(|&&b| b == b' ').count();
        let marker = line.get(indent).copied().filter(|b| matches!(b, b'`' | b'~'));
        if let (Some(ch), true) = (marker, indent <= 3) {
            let run = line[indent..].iter().take_while(|&&b| b == ch).count();
            let rest = &line[indent + run..];
            match open {
                None if run >= 3 && !(ch == b'`' && rest.contains(&b'`')) => {
                    open = Some((at, ch, run));
                }
                Some((start, open_ch, open_run))
                    if ch == open_ch
                        && run >= open_run
                        && rest.iter().all(|b| b.is_ascii_whitespace()) =>
                {
                    out.push(start..next);
                    open = None;
                }
                _ => {}
            }
        }
    }
    if let Some((start, ..)) = open {
        out.push(start..bytes.len());
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Construct {
    Insert,
    Delete,
    Replace,
    Comment,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Token {
    Open(Construct),
    Close(Construct),
    Separator,
}

impl Token {
    fn len(self) -> usize {
        if self == Token::Separator { 2 } else { 3 }
    }
}

fn token_at(bytes: &[u8], at: usize, end: usize) -> Option<Token> {
    let get = |offset: usize| (at + offset < end).then(|| bytes[at + offset]);
    let (a, b) = (get(0)?, get(1)?);
    if a == b'~' && b == b'>' {
        return Some(Token::Separator);
    }
    let c = get(2)?;
    Some(match (a, b, c) {
        (b'{', b'+', b'+') => Token::Open(Construct::Insert),
        (b'{', b'-', b'-') => Token::Open(Construct::Delete),
        (b'{', b'~', b'~') => Token::Open(Construct::Replace),
        (b'{', b'>', b'>') => Token::Open(Construct::Comment),
        (b'+', b'+', b'}') => Token::Close(Construct::Insert),
        (b'-', b'-', b'}') => Token::Close(Construct::Delete),
        (b'~', b'~', b'}') => Token::Close(Construct::Replace),
        (b'<', b'<', b'}') => Token::Close(Construct::Comment),
        _ => return None,
    })
}

fn next_token(bytes: &[u8], from: usize, end: usize) -> Option<(usize, Token)> {
    (from..end).find_map(|at| {
        matches!(bytes[at], b'{' | b'+' | b'-' | b'~' | b'<')
            .then(|| token_at(bytes, at, end))
            .flatten()
            .map(|token| (at, token))
    })
}

struct Consumed {
    end: usize,
    /// Closed by its own delimiter with no reserved tokens inside.
    valid: bool,
    separator: Option<usize>,
}

/// Consumes the construct opening at `start`. Nested constructs are consumed
/// as a unit; an unterminated construct consumes the rest of the region.
fn consume(bytes: &[u8], start: usize, kind: Construct, end: usize) -> Consumed {
    let mut stack = vec![kind];
    let mut valid = true;
    let mut separators = Vec::new();
    let mut at = start + 3;
    while let Some((pos, token)) = next_token(bytes, at, end) {
        at = pos + token.len();
        match token {
            Token::Open(nested) => {
                valid = false;
                stack.push(nested);
            }
            Token::Close(closed) if Some(&closed) == stack.last() => {
                stack.pop();
                if stack.is_empty() {
                    let separator_ok = match kind {
                        Construct::Replace => separators.len() == 1,
                        _ => separators.is_empty(),
                    };
                    return Consumed {
                        end: at,
                        valid: valid && separator_ok,
                        separator: separators.first().copied(),
                    };
                }
            }
            Token::Close(_) => valid = false,
            Token::Separator if stack.len() == 1 => separators.push(pos),
            Token::Separator => {}
        }
    }
    Consumed {
        end,
        valid: false,
        separator: None,
    }
}

fn identity(body: &str) -> Option<(&str, &str)> {
    let rest = body.strip_prefix("id:")?;
    let id = rest.get(..10)?;
    let tail = id.strip_prefix("s_")?;
    if !tail.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()) {
        return None;
    }
    let author = rest[10..].strip_prefix(" by:")?;
    (!author.trim().is_empty() && !author.contains(['\n', '\r'])).then_some((id, author))
}

fn parse_region(doc: &str, region: Range<usize>, parsed: &mut Parsed) {
    let bytes = doc.as_bytes();
    let end = region.end;
    let mut at = region.start;
    // Outside a construct only an opening token matters, and each starts with `{`.
    while let Some(pos) = memchr::memchr(b'{', &bytes[at.min(end)..end]).map(|found| at + found) {
        let Some(Token::Open(kind)) = token_at(bytes, pos, end) else {
            at = pos + 1;
            continue;
        };
        let edit = consume(bytes, pos, kind, end);
        at = edit.end;
        if kind == Construct::Comment {
            if edit.valid {
                parsed.comments.push(pos..edit.end);
            }
            continue;
        }
        if !edit.valid || token_at(bytes, edit.end, end) != Some(Token::Open(Construct::Comment)) {
            continue;
        }
        let comment = consume(bytes, edit.end, Construct::Comment, end);
        if !comment.valid {
            continue;
        }
        let Some((id, author)) = identity(&doc[edit.end + 3..comment.end - 3]) else {
            continue;
        };
        let inner = pos + 3..edit.end - 3;
        let (kind, old_bytes, new_bytes) = match kind {
            Construct::Insert => (Kind::Insert, inner.start..inner.start, inner),
            Construct::Delete => (Kind::Delete, inner.clone(), inner.end..inner.end),
            _ => {
                let separator = edit.separator.unwrap_or(inner.end);
                (Kind::Replace, inner.start..separator, separator + 2..inner.end)
            }
        };
        parsed.suggestions.push(Suggestion {
            id: id.to_string(),
            kind,
            author: author.to_string(),
            span: 0..0,
            old_text: doc[old_bytes.clone()].to_string(),
            new_text: doc[new_bytes.clone()].to_string(),
            bytes: pos..comment.end,
            old_bytes,
            new_bytes,
            meta_bytes: edit.end..comment.end,
        });
        at = comment.end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(doc: &str, (span, text): (Range<usize>, &str)) -> String {
        let chars: Vec<char> = doc.chars().collect();
        let mut out: String = chars[..span.start].iter().collect();
        out.push_str(text);
        out.extend(&chars[span.end..]);
        out
    }

    #[test]
    fn parses_insert_delete_replace() {
        let doc = "a {++new++}{>>id:s_0123abcd by:Calvin<<} b {--old--}{>>id:s_aaaaaaa1 by:Agent Smith<<} c {~~x~>y~~}{>>id:s_zzzzzzzz by:Z<<}";
        let s = parse_suggestions(doc);
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].kind, s[0].new_text.as_str(), s[0].old_text.as_str()), (Kind::Insert, "new", ""));
        assert_eq!(s[0].span, 2..40);
        assert_eq!(s[0].author, "Calvin");
        assert_eq!(s[0].id, "s_0123abcd");
        assert_eq!((s[1].kind, s[1].old_text.as_str(), s[1].author.as_str()), (Kind::Delete, "old", "Agent Smith"));
        assert_eq!((s[2].kind, s[2].old_text.as_str(), s[2].new_text.as_str()), (Kind::Replace, "x", "y"));
        assert_eq!(&doc[s[2].old_bytes.clone()], "x");
        assert_eq!(&doc[s[2].new_bytes.clone()], "y");
    }

    #[test]
    fn accept_and_reject_replace_the_whole_span() {
        let doc = "say {~~old wording~>new wording~~}{>>id:s_0123abcd by:Calvin<<}!";
        let s = &parse_suggestions(doc)[0];
        assert_eq!(apply(doc, s.accept()), "say new wording!");
        assert_eq!(apply(doc, s.reject()), "say old wording!");
    }

    #[test]
    fn spans_are_code_points() {
        let doc = "😀 {++🚀++}{>>id:s_0123abcd by:Agent<<}";
        let s = &parse_suggestions(doc)[0];
        assert_eq!(s.span.start, 2);
        assert_eq!(apply(doc, s.accept()), "😀 🚀");
        assert_eq!(apply(doc, s.reject()), "😀 ");
        assert_eq!(s.new_chars(), 5..6);
    }

    #[test]
    fn empty_old_and_new_text_are_allowed() {
        let doc = "{~~~>~~}{>>id:s_00000000 by:a<<}{++++}{>>id:s_00000001 by:a<<}";
        let s = parse_suggestions(doc);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].old_text.as_str(), s[0].new_text.as_str()), ("", ""));
        assert_eq!(s[1].span.start, s[0].span.end);
    }

    #[test]
    fn malformed_markup_is_plain_text() {
        for doc in [
            "{++no identity++}",
            "{++gap++} {>>id:s_0123abcd by:A<<}",
            "{++bad id++}{>>id:s_0123ABCD by:A<<}",
            "{++short id++}{>>id:s_0123 by:A<<}",
            "{++blank author++}{>>id:s_0123abcd by:  <<}",
            "{++two\nline author++}{>>id:s_0123abcd by:A\nB<<}",
            "{++mismatched--}{>>id:s_0123abcd by:A<<}",
            "{~~no separator~~}{>>id:s_0123abcd by:A<<}",
            "{~~a~>b~>c~~}{>>id:s_0123abcd by:A<<}",
            "{++outer {++inner++}{>>id:s_0123abcd by:A<<} ++}{>>id:s_0123abce by:A<<}",
            "{>>id:s_0123abcd by:A<<}",
            "{++sep ~> inside++}{>>id:s_0123abcd by:A<<}",
            "{++unterminated {>>id:s_0123abcd by:A<<}",
        ] {
            assert_eq!(parse_suggestions(doc), vec![], "{doc}");
        }
    }

    #[test]
    fn unterminated_construct_consumes_the_rest_of_the_region() {
        let doc = "{++open\n{++x++}{>>id:s_0123abcd by:A<<}";
        assert_eq!(parse_suggestions(doc), vec![]);
    }

    #[test]
    fn fenced_code_is_ignored_and_bounds_regions() {
        let inner = "{++x++}{>>id:s_0123abcd by:A<<}";
        let doc = format!("```\n{inner}\n```\n{inner}\n~~~~ rust\n{inner}\n~~~\n{inner}\n");
        let s = parse_suggestions(&doc);
        assert_eq!(s.len(), 1, "only the suggestion between the fences");
        assert_eq!(parse(&doc).fences.len(), 2);
        let unclosed = format!("{inner}\n```\n{inner}");
        assert_eq!(parse_suggestions(&unclosed).len(), 1);
        let cr_only = format!("```\r{inner}\r```\r{inner}");
        assert_eq!(parse_suggestions(&cr_only).len(), 1);
        let unterminated = format!("{{++open\n```\n```\n{inner}");
        assert_eq!(parse_suggestions(&unterminated).len(), 1);
    }

    #[test]
    fn backtick_fence_info_cannot_contain_backticks() {
        let inner = "{++x++}{>>id:s_0123abcd by:A<<}";
        let doc = format!("``` a`b\n{inner}\n");
        assert_eq!(parse_suggestions(&doc).len(), 1);
    }

    #[test]
    fn compose_round_trips_and_rejects_reserved_text() {
        let markup = compose_checked(Kind::Replace, "a", "b", "s_0123abcd", "Calvin").unwrap();
        assert_eq!(markup, "{~~a~>b~~}{>>id:s_0123abcd by:Calvin<<}");
        assert!(compose_checked(Kind::Insert, "", "a+", "s_0123abcd", "C").is_some());
        assert!(compose_checked(Kind::Insert, "", "a++}", "s_0123abcd", "C").is_none());
        assert!(compose_checked(Kind::Delete, "a{", "", "s_0123abcd", "C").is_none());
        assert!(compose_checked(Kind::Replace, "a~>b", "c", "s_0123abcd", "C").is_none());
        assert!(!is_valid_author(" "));
        assert!(!is_valid_author("a<<}"));
        assert!(is_valid_author("Calvin F."));
    }

    #[test]
    fn standalone_comments_are_reported_separately() {
        let parsed = parse("note {>>just a comment<<} here");
        assert!(parsed.suggestions.is_empty());
        assert_eq!(parsed.comments, vec![5..25]);
    }
}
