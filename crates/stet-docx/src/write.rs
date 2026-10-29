//! Markdown to `.docx`. Suggestions become tracked changes (`w:ins`,
//! `w:del`), notes and margin comments become Word comments.
//!
//! CriticMarkup is taken out before the Markdown is parsed: each suggestion
//! and note is swapped for a private-use token, so the parser sees ordinary
//! text, and the token is expanded again as runs are made.

use crate::{Comment, FindPicture, Options};
use pulldown_cmark::{Alignment, Event, LinkType, Parser, Tag, TagEnd};
use std::io::Write as _;
use stet_core::critic::{self, Kind};

/// Opens and closes the token that stands for a suggestion or a note.
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

/// The text width of a Letter page with 1in margins, in twentieths of a point.
const TEXT_TWIPS: u32 = 9360;
/// The widest a picture is drawn: the text width, in EMU.
const MAX_EMU: u64 = 5_943_600;
const EMU_PER_PX: u64 = 9525;

/// What a token in the text stands for.
enum Mark {
    Suggestion {
        kind: Kind,
        old: String,
        new: String,
        author: String,
    },
    /// A standalone `{>>note<<}`.
    Note(String),
}

/// `markdown` with every suggestion and note swapped for a token.
fn tokenize(markdown: &str) -> (String, Vec<Mark>) {
    let parsed = critic::parse(markdown);
    let mut spans: Vec<(std::ops::Range<usize>, Mark)> = parsed
        .suggestions
        .into_iter()
        .map(|s| {
            let mark = Mark::Suggestion {
                kind: s.kind,
                old: s.old_text,
                new: s.new_text,
                author: s.author,
            };
            (s.bytes, mark)
        })
        .chain(parsed.comments.into_iter().map(|range| {
            let note = markdown[range.start + 3..range.end - 3].trim().to_string();
            (range, Mark::Note(note))
        }))
        .collect();
    spans.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(markdown.len());
    let mut marks = Vec::with_capacity(spans.len());
    let mut at = 0;
    for (range, mark) in spans {
        if range.start < at {
            continue;
        }
        out.push_str(&markdown[at..range.start]);
        out.push(OPEN);
        out.push_str(&marks.len().to_string());
        out.push(CLOSE);
        marks.push(mark);
        at = range.end;
    }
    out.push_str(&markdown[at..]);
    (out, marks)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Ins(u32),
    Del(u32),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Fmt {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    /// The relationship of the hyperlink the run is in.
    link: Option<String>,
}

#[derive(Clone, Debug)]
enum Seg {
    Text {
        text: String,
        fmt: Fmt,
        change: Option<Change>,
    },
    Break {
        fmt: Fmt,
        change: Option<Change>,
    },
    /// A picture's run.
    Raw(String),
    /// A note, until it is anchored to the word before it.
    Note(usize),
    CommentStart(usize),
    CommentEnd(usize),
}

impl Seg {
    /// Characters it shows, for finding words; deleted text shows none.
    fn shown(&self) -> Option<&str> {
        match self {
            Seg::Text { text, change, .. } if !matches!(change, Some(Change::Del(_))) => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Props {
    style: Option<&'static str>,
    num: Option<(u32, u32)>,
    indent: u32,
    rule: bool,
    align: Option<&'static str>,
}

#[derive(Default)]
struct Para {
    props: Props,
    segs: Vec<Seg>,
}

impl Para {
    fn text(&self) -> String {
        self.segs.iter().filter_map(Seg::shown).collect()
    }

    /// Splits segments so one begins at char `pos` of the shown text, and
    /// returns its index (where a mark placed before `pos` goes).
    fn before(&mut self, pos: usize) -> usize {
        self.boundary(pos, true)
    }

    /// Where a mark placed after char `pos - 1` goes.
    fn after(&mut self, pos: usize) -> usize {
        self.boundary(pos, false)
    }

    fn boundary(&mut self, pos: usize, start: bool) -> usize {
        let mut offset = 0;
        for i in 0..self.segs.len() {
            let Some(len) = self.segs[i].shown().map(|text| text.chars().count()) else {
                continue;
            };
            if start && pos == offset {
                return i;
            }
            if pos > offset && pos < offset + len {
                let Seg::Text { text, fmt, change } = self.segs[i].clone() else {
                    unreachable!()
                };
                let cut = text.char_indices().nth(pos - offset).map_or(text.len(), |(at, _)| at);
                self.segs[i] = Seg::Text {
                    text: text[..cut].to_string(),
                    fmt: fmt.clone(),
                    change,
                };
                self.segs.insert(
                    i + 1,
                    Seg::Text {
                        text: text[cut..].to_string(),
                        fmt,
                        change,
                    },
                );
                return i + 1;
            }
            offset += len;
            if !start && pos == offset {
                return i + 1;
            }
        }
        self.segs.len()
    }
}

/// The body: XML around paragraphs that are still being worked on.
enum Chunk {
    Xml(String),
    Para(usize),
}

struct Rel {
    id: String,
    kind: &'static str,
    target: String,
    external: bool,
}

struct ListCtx {
    num: u32,
}

struct CommentOut {
    author: String,
    text: String,
}

struct Writer<'a> {
    marks: Vec<Mark>,
    picture: FindPicture<'a>,
    body: Vec<Chunk>,
    paras: Vec<Para>,
    cur: Option<usize>,
    fmt: Fmt,
    partial: String,
    rels: Vec<Rel>,
    media: Vec<(String, Vec<u8>)>,
    /// `w:num` instances beyond the bullet one: the start of each ordered list.
    ordered: Vec<u64>,
    lists: Vec<ListCtx>,
    item_fresh: bool,
    quote: u32,
    heading: Option<u32>,
    code: Option<String>,
    table: Vec<Alignment>,
    cell: usize,
    head: bool,
    image: Option<(String, String)>,
    skip: bool,
    footnote: Option<String>,
    revision: u32,
    comments: Vec<CommentOut>,
    author: String,
    /// Who made each revision, by its `w:id`.
    rev_authors: Vec<String>,
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            OPEN | CLOSE | '\u{FFFE}' | '\u{FFFF}' => {}
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// A link Google Docs can follow: not a file beside the document.
fn external(dest: &str) -> bool {
    let lower = dest.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

/// A picture's size in pixels, from its header.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG") && bytes.len() >= 24 {
        let be = |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        return Some((be(16), be(20)));
    }
    if bytes.starts_with(b"GIF8") && bytes.len() >= 10 {
        let le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as u32;
        return Some((le(6), le(8)));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xFF {
                at += 1;
                continue;
            }
            let marker = bytes[at + 1];
            let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32;
                return Some((w, h));
            }
            at += 2 + len;
        }
    }
    None
}

fn initials(author: &str) -> String {
    author
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .take(3)
        .collect()
}

impl<'a> Writer<'a> {
    fn rel(&mut self, kind: &'static str, target: String, external: bool) -> String {
        let id = format!("rId{}", 10 + self.rels.len());
        self.rels.push(Rel {
            id: id.clone(),
            kind,
            target,
            external,
        });
        id
    }

    /// Properties for a paragraph opened where the writer is now.
    fn props(&mut self) -> Props {
        let depth = self.lists.len() as u32;
        let mut props = Props {
            indent: 720 * (self.quote + depth.saturating_sub(1)),
            ..Props::default()
        };
        if let Some(level) = self.heading {
            props.style = Some(
                ["Heading1", "Heading2", "Heading3", "Heading4", "Heading5", "Heading6"][(level as usize - 1).min(5)],
            );
            return props;
        }
        if self.code.is_some() {
            props.style = Some("Code");
        } else if self.quote > 0 {
            props.style = Some("Quote");
        }
        if !self.table.is_empty() {
            props.indent = 0;
            props.align = match self.table.get(self.cell) {
                Some(Alignment::Center) => Some("center"),
                Some(Alignment::Right) => Some("right"),
                _ => None,
            };
            return props;
        }
        if let Some(list) = self.lists.last() {
            if self.item_fresh && self.code.is_none() {
                props.num = Some((list.num, depth - 1));
                props.indent = 720 * self.quote;
                self.item_fresh = false;
            } else {
                props.indent += 720;
            }
        }
        props
    }

    fn ensure_para(&mut self) -> usize {
        if let Some(cur) = self.cur {
            return cur;
        }
        let props = self.props();
        self.paras.push(Para {
            props,
            segs: Vec::new(),
        });
        let index = self.paras.len() - 1;
        self.body.push(Chunk::Para(index));
        self.cur = Some(index);
        if let Some(label) = self.footnote.take() {
            self.push_plain(&format!("[^{label}]: "));
        }
        index
    }

    fn close_para(&mut self) {
        if self.cur.is_some() && !self.partial.is_empty() {
            let rest = std::mem::take(&mut self.partial);
            self.push_plain(&rest);
        }
        self.cur = None;
    }

    fn seg(&mut self, seg: Seg) {
        let para = self.ensure_para();
        self.paras[para].segs.push(seg);
    }

    /// Text with no tokens to expand.
    fn push_plain(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let fmt = self.fmt.clone();
        self.push_changed(text, fmt, None);
    }

    /// Text in a change; newlines are line breaks.
    fn push_changed(&mut self, text: &str, fmt: Fmt, change: Option<Change>) {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.seg(Seg::Break {
                    fmt: fmt.clone(),
                    change,
                });
            }
            if !line.is_empty() {
                self.seg(Seg::Text {
                    text: line.to_string(),
                    fmt: fmt.clone(),
                    change,
                });
            }
        }
    }

    /// Text from the parser, tokens expanded.
    fn push_text(&mut self, text: &str) {
        let mut text = std::mem::take(&mut self.partial) + text;
        while let Some(open) = text.find(OPEN) {
            let before = text[..open].to_string();
            self.push_plain(&before);
            let rest = &text[open + OPEN.len_utf8()..];
            let Some(close) = rest.find(CLOSE) else {
                self.partial = text[open..].to_string();
                return;
            };
            let index: Option<usize> = rest[..close].parse().ok();
            let after = rest[close + CLOSE.len_utf8()..].to_string();
            if let Some(index) = index {
                self.expand(index);
            }
            text = after;
        }
        self.push_plain(&text);
    }

    fn expand(&mut self, index: usize) {
        let fmt = self.fmt.clone();
        let (kind, old, new, author) = match self.marks.get(index) {
            Some(Mark::Suggestion { kind, old, new, author }) => (*kind, old.clone(), new.clone(), author.clone()),
            Some(Mark::Note(note)) => {
                let note = note.clone();
                let id = self.comments.len();
                self.comments.push(CommentOut {
                    author: self.author.clone(),
                    text: note,
                });
                self.seg(Seg::Note(id));
                return;
            }
            None => return,
        };
        if kind != Kind::Insert {
            self.revision += 1;
            let id = self.revision;
            self.revision_author(id, &author);
            self.push_changed(&old, fmt.clone(), Some(Change::Del(id)));
        }
        if kind != Kind::Delete {
            self.revision += 1;
            let id = self.revision;
            self.revision_author(id, &author);
            self.push_changed(&new, fmt, Some(Change::Ins(id)));
        }
    }

    fn revision_author(&mut self, id: u32, author: &str) {
        if self.rev_authors.len() <= id as usize {
            self.rev_authors.resize(id as usize + 1, String::new());
        }
        self.rev_authors[id as usize] = author.to_string();
    }

    fn event(&mut self, event: Event) {
        if self.skip {
            if matches!(event, Event::End(TagEnd::MetadataBlock(_))) {
                self.skip = false;
            }
            return;
        }
        if let Some((_, alt)) = &mut self.image {
            match event {
                Event::Text(text) | Event::Code(text) => alt.push_str(&text),
                Event::End(TagEnd::Image) => self.end_image(),
                _ => {}
            }
            return;
        }
        if let Some(code) = &mut self.code {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => self.end_code(),
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => {
                let saved = self.fmt.code;
                self.fmt.code = true;
                self.push_text(&text);
                self.fmt.code = saved;
            }
            Event::InlineMath(text) => self.push_text(&format!("${text}$")),
            Event::DisplayMath(text) => self.push_text(&format!("$${text}$$")),
            Event::Html(html) => {
                let text = strip_tags(&html);
                if !text.trim().is_empty() {
                    self.push_text(text.trim_end_matches('\n'));
                }
            }
            Event::InlineHtml(html) => {
                if html.trim_start_matches('<').to_ascii_lowercase().starts_with("br") {
                    let fmt = self.fmt.clone();
                    self.seg(Seg::Break { fmt, change: None });
                }
            }
            Event::FootnoteReference(label) => self.push_plain(&format!("[^{label}]")),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => {
                let fmt = self.fmt.clone();
                self.seg(Seg::Break { fmt, change: None });
            }
            Event::Rule => {
                self.close_para();
                let mut props = self.props();
                props.rule = true;
                self.paras.push(Para {
                    props,
                    segs: Vec::new(),
                });
                self.body.push(Chunk::Para(self.paras.len() - 1));
            }
            Event::TaskListMarker(done) => self.push_plain(if done { "☑ " } else { "☐ " }),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph if self.cur.is_some_and(|cur| !self.paras[cur].segs.is_empty()) => self.close_para(),
            Tag::Heading { level, .. } => {
                self.close_para();
                self.heading = Some(level as u32);
            }
            Tag::BlockQuote(_) => {
                self.close_para();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.close_para();
                self.code = Some(String::new());
            }
            Tag::HtmlBlock => self.close_para(),
            Tag::List(start) => {
                self.close_para();
                let num = match start {
                    Some(start) => {
                        self.ordered.push(start);
                        1 + self.ordered.len() as u32
                    }
                    None => 1,
                };
                self.lists.push(ListCtx { num });
            }
            Tag::Item => {
                self.close_para();
                self.item_fresh = true;
            }
            Tag::FootnoteDefinition(label) => {
                self.close_para();
                self.footnote = Some(label.to_string());
            }
            Tag::DefinitionList | Tag::DefinitionListTitle => self.close_para(),
            Tag::DefinitionListDefinition => {
                self.close_para();
                self.quote += 1;
            }
            Tag::Table(aligns) => {
                self.close_para();
                let width = TEXT_TWIPS / aligns.len().max(1) as u32;
                let grid: String = aligns
                    .iter()
                    .map(|_| format!(r#"<w:gridCol w:w="{width}"/>"#))
                    .collect();
                self.body.push(Chunk::Xml(format!(
                    r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="{TEXT_TWIPS}" w:type="dxa"/><w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/><w:left w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/><w:bottom w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/><w:right w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/><w:insideH w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/><w:insideV w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/></w:tblBorders><w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="0" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr><w:tblGrid>{grid}</w:tblGrid>"#
                )));
                self.table = aligns;
            }
            Tag::TableHead => {
                self.head = true;
                self.fmt.bold = true;
                self.cell = 0;
                self.body
                    .push(Chunk::Xml("<w:tr><w:trPr><w:tblHeader/></w:trPr>".to_string()));
            }
            Tag::TableRow => {
                self.cell = 0;
                self.body.push(Chunk::Xml("<w:tr>".to_string()));
            }
            Tag::TableCell => {
                let width = TEXT_TWIPS / self.table.len().max(1) as u32;
                self.body.push(Chunk::Xml(format!(
                    r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/></w:tcPr>"#
                )));
            }
            Tag::Emphasis => self.fmt.italic = true,
            Tag::Strong => self.fmt.bold = true,
            Tag::Strikethrough => self.fmt.strike = true,
            Tag::Link {
                link_type, dest_url, ..
            } if !matches!(link_type, LinkType::WikiLink { .. }) && external(&dest_url) => {
                let id = self.rel(
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
                    dest_url.to_string(),
                    true,
                );
                self.fmt.link = Some(id);
            }
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.to_string(), String::new())),
            Tag::MetadataBlock(_) => self.skip = true,
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock | TagEnd::DefinitionListTitle => self.close_para(),
            TagEnd::Heading(_) => {
                self.close_para();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.close_para();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::DefinitionListDefinition => {
                self.close_para();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.close_para();
                self.lists.pop();
                self.item_fresh = false;
            }
            TagEnd::Item | TagEnd::FootnoteDefinition => {
                self.close_para();
                self.footnote = None;
                self.item_fresh = false;
            }
            TagEnd::Table => {
                self.close_para();
                self.table.clear();
                self.body.push(Chunk::Xml("</w:tbl>".to_string()));
            }
            TagEnd::TableHead => {
                self.head = false;
                self.fmt.bold = false;
                self.body.push(Chunk::Xml("</w:tr>".to_string()));
            }
            TagEnd::TableRow => self.body.push(Chunk::Xml("</w:tr>".to_string())),
            TagEnd::TableCell => {
                // A cell holds at least one paragraph.
                self.ensure_para();
                self.close_para();
                self.cell += 1;
                self.body.push(Chunk::Xml("</w:tc>".to_string()));
            }
            TagEnd::Emphasis => self.fmt.italic = false,
            TagEnd::Strong => self.fmt.bold = self.head,
            TagEnd::Strikethrough => self.fmt.strike = false,
            TagEnd::Link => self.fmt.link = None,
            _ => {}
        }
    }

    fn end_code(&mut self) {
        let code = self.code.take().unwrap_or_default();
        let code = code.strip_suffix('\n').unwrap_or(&code).to_string();
        let saved = self.fmt.clone();
        self.fmt = Fmt {
            code: true,
            ..Fmt::default()
        };
        for line in code.split('\n') {
            // `props` reads `code` to choose the style.
            self.code = Some(String::new());
            self.ensure_para();
            self.code = None;
            self.push_text(line);
            self.close_para();
        }
        self.fmt = saved;
    }

    fn end_image(&mut self) {
        let Some((dest, alt)) = self.image.take() else { return };
        let Some((bytes, ext)) = (self.picture)(&dest) else {
            let shown = if alt.is_empty() { dest } else { alt };
            self.push_text(&shown);
            return;
        };
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let n = self.media.len() + 1;
        let name = format!("image{n}.{ext}");
        let (w, h) = dimensions(&bytes)
            .filter(|(w, h)| *w > 0 && *h > 0)
            .unwrap_or((400, 300));
        let (mut cx, mut cy) = (w as u64 * EMU_PER_PX, h as u64 * EMU_PER_PX);
        if cx > MAX_EMU {
            cy = cy * MAX_EMU / cx;
            cx = MAX_EMU;
        }
        self.media.push((name.clone(), bytes));
        let rid = self.rel(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image",
            format!("media/{name}"),
            false,
        );
        let alt = escape(&alt);
        self.seg(Seg::Raw(format!(
            r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="{n}" name="Picture {n}" descr="{alt}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="{n}" name="{name}" descr="{alt}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#
        )));
    }

    /// Anchors each note to the word before it.
    fn anchor_notes(&mut self) {
        for para in &mut self.paras {
            while let Some(at) = para.segs.iter().position(|seg| matches!(seg, Seg::Note(_))) {
                let Seg::Note(id) = para.segs[at] else { unreachable!() };
                let before: String = para.segs[..at].iter().filter_map(Seg::shown).collect();
                let end = before.chars().count();
                let trimmed = before.trim_end();
                let word_start = trimmed
                    .rfind(char::is_whitespace)
                    .map_or(0, |at| trimmed[..at + 1].chars().count());
                let word_start = if trimmed.is_empty() { end } else { word_start };
                para.segs[at] = Seg::CommentEnd(id);
                let start = para.before(word_start);
                para.segs.insert(start, Seg::CommentStart(id));
            }
        }
    }

    /// Anchors a margin comment around the first place its words appear,
    /// or at the start of the document.
    fn anchor(&mut self, id: usize, anchor: &str) {
        let anchor = anchor.split_whitespace().collect::<Vec<_>>().join(" ");
        if !anchor.is_empty() {
            let (head, tail) = match anchor.split_once(" … ") {
                Some((head, tail)) => (head.to_string(), Some(tail.to_string())),
                None => (anchor.clone(), None),
            };
            for para in &mut self.paras {
                let text = para.text();
                // Collapse whitespace, keeping a map back to char positions.
                let mut squeezed = String::new();
                let mut map = Vec::new();
                let mut space = false;
                for (i, c) in text.chars().enumerate() {
                    if c.is_whitespace() {
                        if !space && !squeezed.is_empty() {
                            squeezed.push(' ');
                            map.push(i);
                        }
                        space = true;
                    } else {
                        squeezed.push(c);
                        map.push(i);
                        space = false;
                    }
                }
                let Some(at) = squeezed.find(&head) else { continue };
                let mut end = at + head.len();
                if let Some(tail) = &tail {
                    match squeezed[end..].find(tail.as_str()) {
                        Some(found) => end += found + tail.len(),
                        None => continue,
                    }
                }
                let start_char = map[squeezed[..at].chars().count()];
                let end_char = map[squeezed[..end].chars().count() - 1] + 1;
                let close = para.after(end_char);
                para.segs.insert(close, Seg::CommentEnd(id));
                let open = para.before(start_char);
                para.segs.insert(open, Seg::CommentStart(id));
                return;
            }
        }
        if self.paras.is_empty() {
            self.paras.push(Para::default());
            self.body.insert(0, Chunk::Para(0));
        }
        let first = &mut self.paras[0];
        first.segs.insert(0, Seg::CommentEnd(id));
        first.segs.insert(0, Seg::CommentStart(id));
    }

    fn run(&self, fmt: &Fmt, inner: &str) -> String {
        let mut props = String::new();
        if fmt.link.is_some() {
            props.push_str(r#"<w:rStyle w:val="Hyperlink"/>"#);
        }
        if fmt.code {
            props.push_str(r#"<w:rFonts w:ascii="Courier New" w:hAnsi="Courier New" w:cs="Courier New"/>"#);
        }
        if fmt.bold {
            props.push_str("<w:b/>");
        }
        if fmt.italic {
            props.push_str("<w:i/>");
        }
        if fmt.strike {
            props.push_str("<w:strike/>");
        }
        format!("<w:r><w:rPr>{props}</w:rPr>{inner}</w:r>")
    }

    fn change_open(&self, change: Change, date: &str) -> String {
        let (tag, id) = match change {
            Change::Ins(id) => ("w:ins", id),
            Change::Del(id) => ("w:del", id),
        };
        let author = escape(self.rev_authors.get(id as usize).map_or("stet", String::as_str));
        format!(r#"<{tag} w:id="{id}" w:author="{author}"{date}>"#)
    }

    fn para_xml(&self, para: &Para, date: &str) -> String {
        let p = &para.props;
        let mut ppr = String::new();
        if let Some(style) = p.style {
            ppr.push_str(&format!(r#"<w:pStyle w:val="{style}"/>"#));
        }
        if let Some((num, level)) = p.num {
            ppr.push_str(&format!(
                r#"<w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr>"#
            ));
        }
        if p.rule {
            ppr.push_str(r#"<w:pBdr><w:bottom w:val="single" w:sz="6" w:space="1" w:color="BFBFBF"/></w:pBdr>"#);
        }
        if p.indent > 0 {
            ppr.push_str(&format!(r#"<w:ind w:left="{}"/>"#, p.indent));
        }
        if let Some(align) = p.align {
            ppr.push_str(&format!(r#"<w:jc w:val="{align}"/>"#));
        }
        let mut out = format!("<w:p><w:pPr>{ppr}</w:pPr>");
        let mut link: Option<&str> = None;
        let mut change: Option<Change> = None;
        let close_change = |out: &mut String, change: &mut Option<Change>| match change.take() {
            Some(Change::Ins(_)) => out.push_str("</w:ins>"),
            Some(Change::Del(_)) => out.push_str("</w:del>"),
            None => {}
        };
        for seg in &para.segs {
            let (want_link, want_change) = match seg {
                Seg::Text { fmt, change, .. } | Seg::Break { fmt, change } => (fmt.link.as_deref(), *change),
                _ => (None, None),
            };
            if want_link != link {
                close_change(&mut out, &mut change);
                if link.is_some() {
                    out.push_str("</w:hyperlink>");
                }
                if let Some(id) = want_link {
                    out.push_str(&format!(r#"<w:hyperlink r:id="{id}">"#));
                }
                link = want_link;
            }
            if want_change != change {
                close_change(&mut out, &mut change);
                if let Some(next) = want_change {
                    out.push_str(&self.change_open(next, date));
                }
                change = want_change;
            }
            match seg {
                Seg::Text { text, fmt, change } => {
                    let tag = if matches!(change, Some(Change::Del(_))) { "w:delText" } else { "w:t" };
                    let inner = format!(r#"<{tag} xml:space="preserve">{}</{tag}>"#, escape(text));
                    out.push_str(&self.run(fmt, &inner));
                }
                Seg::Break { fmt, .. } => out.push_str(&self.run(fmt, "<w:br/>")),
                Seg::Raw(xml) => out.push_str(xml),
                Seg::CommentStart(id) => out.push_str(&format!(r#"<w:commentRangeStart w:id="{id}"/>"#)),
                Seg::CommentEnd(id) | Seg::Note(id) => out.push_str(&format!(
                    r#"<w:commentRangeEnd w:id="{id}"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{id}"/></w:r>"#
                )),
            }
        }
        close_change(&mut out, &mut change);
        if link.is_some() {
            out.push_str("</w:hyperlink>");
        }
        out.push_str("</w:p>");
        out
    }
}

/// HTML with its tags taken out.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' if inside => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

/// Writes `markdown` (CriticMarkup included) as a `.docx`, with `comments`
/// from the margin anchored to their words.
pub fn write(markdown: &str, comments: &[Comment], options: &Options, picture: FindPicture) -> Result<Vec<u8>, String> {
    let (text, marks) = tokenize(markdown);
    let mut writer = Writer {
        marks,
        picture,
        body: Vec::new(),
        paras: Vec::new(),
        cur: None,
        fmt: Fmt::default(),
        partial: String::new(),
        rels: Vec::new(),
        media: Vec::new(),
        ordered: Vec::new(),
        lists: Vec::new(),
        item_fresh: false,
        quote: 0,
        heading: None,
        code: None,
        table: Vec::new(),
        cell: 0,
        head: false,
        image: None,
        skip: false,
        footnote: None,
        revision: 0,
        comments: Vec::new(),
        author: options.author.to_string(),
        rev_authors: Vec::new(),
    };
    for event in Parser::new_ext(&text, stet_core::markdown::options()) {
        writer.event(event);
    }
    writer.close_para();
    writer.anchor_notes();
    for comment in comments {
        let id = writer.comments.len();
        writer.comments.push(CommentOut {
            author: comment.author.clone(),
            text: comment.text.clone(),
        });
        writer.anchor(id, &comment.anchor);
    }
    if writer.paras.is_empty() {
        writer.paras.push(Para::default());
        writer.body.push(Chunk::Para(0));
    }
    let date = options
        .date
        .map(|date| format!(r#" w:date="{}""#, escape(date)))
        .unwrap_or_default();
    let mut body = String::new();
    for chunk in &writer.body {
        match chunk {
            Chunk::Xml(xml) => body.push_str(xml),
            Chunk::Para(index) => body.push_str(&writer.para_xml(&writer.paras[*index], &date)),
        }
    }
    package(&writer, &body, &date).map_err(|err| err.to_string())
}

const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#;

const HEADER: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;

fn package(writer: &Writer, body: &str, date: &str) -> zip::result::ZipResult<Vec<u8>> {
    use zip::write::SimpleFileOptions;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let has_comments = !writer.comments.is_empty();
    let put = |zip: &mut zip::ZipWriter<_>, name: &str, bytes: &[u8]| -> zip::result::ZipResult<()> {
        zip.start_file(name, options)?;
        zip.write_all(bytes)?;
        Ok(())
    };

    let comments_override = if has_comments {
        r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#
    } else {
        ""
    };
    let types = format!(
        r#"{HEADER}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="jpg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/><Default Extension="webp" ContentType="image/webp"/><Default Extension="svg" ContentType="image/svg+xml"/><Default Extension="bmp" ContentType="image/bmp"/><Default Extension="tif" ContentType="image/tiff"/><Default Extension="tiff" ContentType="image/tiff"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>{comments_override}</Types>"#
    );
    put(&mut zip, "[Content_Types].xml", types.as_bytes())?;
    put(
        &mut zip,
        "_rels/.rels",
        format!(
            r#"{HEADER}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#
        )
        .as_bytes(),
    )?;

    let document = format!(
        r#"{HEADER}<w:document {NS}><w:body>{body}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    put(&mut zip, "word/document.xml", document.as_bytes())?;
    put(&mut zip, "word/styles.xml", styles().as_bytes())?;
    put(&mut zip, "word/numbering.xml", numbering(&writer.ordered).as_bytes())?;

    let mut rels = String::from(
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#,
    );
    if has_comments {
        rels.push_str(r#"<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>"#);
        let mut xml = format!(r#"{HEADER}<w:comments {NS}>"#);
        for (id, comment) in writer.comments.iter().enumerate() {
            let author = if comment.author.trim().is_empty() {
                "stet"
            } else {
                comment.author.as_str()
            };
            xml.push_str(&format!(
                r#"<w:comment w:id="{id}" w:author="{}"{date} w:initials="{}">"#,
                escape(author),
                escape(&initials(author))
            ));
            let mut first = true;
            let lines: Vec<&str> = comment.text.lines().filter(|line| !line.trim().is_empty()).collect();
            for line in if lines.is_empty() { vec![""] } else { lines } {
                xml.push_str(r#"<w:p><w:pPr><w:pStyle w:val="CommentText"/></w:pPr>"#);
                if first {
                    xml.push_str(r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:annotationRef/></w:r>"#);
                    first = false;
                }
                xml.push_str(&format!(
                    r#"<w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
                    escape(line)
                ));
            }
            xml.push_str("</w:comment>");
        }
        xml.push_str("</w:comments>");
        put(&mut zip, "word/comments.xml", xml.as_bytes())?;
    }
    for rel in &writer.rels {
        let mode = if rel.external { r#" TargetMode="External""# } else { "" };
        rels.push_str(&format!(
            r#"<Relationship Id="{}" Type="{}" Target="{}"{mode}/>"#,
            rel.id,
            rel.kind,
            escape(&rel.target)
        ));
    }
    put(
        &mut zip,
        "word/_rels/document.xml.rels",
        format!(r#"{HEADER}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#)
            .as_bytes(),
    )?;
    for (name, bytes) in &writer.media {
        put(&mut zip, &format!("word/media/{name}"), bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

fn styles() -> String {
    let mut headings = String::new();
    for (level, size) in [(1, 40), (2, 32), (3, 28), (4, 24), (5, 22), (6, 22)] {
        headings.push_str(&format!(
            r#"<w:style w:type="paragraph" w:styleId="Heading{level}"><w:name w:val="heading {level}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="80"/><w:outlineLvl w:val="{}"/></w:pPr><w:rPr><w:b/><w:sz w:val="{size}"/></w:rPr></w:style>"#,
            level - 1
        ));
    }
    format!(
        r#"{HEADER}<w:styles {NS}><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:cs="Arial" w:eastAsia="Arial"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="en-US"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>{headings}<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:rPr><w:sz w:val="52"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="Quote"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:ind w:left="720"/></w:pPr><w:rPr><w:i/><w:color w:val="595959"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Code"><w:name w:val="Code"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New" w:cs="Courier New"/><w:sz w:val="20"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="CommentText"><w:name w:val="annotation text"/><w:basedOn w:val="Normal"/><w:rPr><w:sz w:val="20"/></w:rPr></w:style><w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:rPr><w:color w:val="1155CC"/><w:u w:val="single"/></w:rPr></w:style><w:style w:type="character" w:styleId="CommentReference"><w:name w:val="annotation reference"/><w:basedOn w:val="DefaultParagraphFont"/><w:rPr><w:sz w:val="16"/></w:rPr></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style><w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/></w:style></w:styles>"#
    )
}

/// One bulleted list definition (`w:num` 1) and one numbered, with a
/// `w:num` for each ordered list so each starts again.
fn numbering(ordered: &[u64]) -> String {
    let bullets = ["•", "◦", "▪"];
    let mut bullet_levels = String::new();
    let mut decimal_levels = String::new();
    for level in 0..9u32 {
        let left = 720 * (level + 1);
        bullet_levels.push_str(&format!(
            r#"<w:lvl w:ilvl="{level}"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="{}"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{left}" w:hanging="360"/></w:pPr></w:lvl>"#,
            bullets[level as usize % 3]
        ));
        let format = ["decimal", "lowerLetter", "lowerRoman"][level as usize % 3];
        decimal_levels.push_str(&format!(
            r#"<w:lvl w:ilvl="{level}"><w:start w:val="1"/><w:numFmt w:val="{format}"/><w:lvlText w:val="%{}."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{left}" w:hanging="360"/></w:pPr></w:lvl>"#,
            level + 1
        ));
    }
    let mut nums = String::from(r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#);
    for (i, start) in ordered.iter().enumerate() {
        let mut overrides = String::new();
        for level in 0..9 {
            overrides.push_str(&format!(
                r#"<w:lvlOverride w:ilvl="{level}"><w:startOverride w:val="{}"/></w:lvlOverride>"#,
                if level == 0 { *start } else { 1 }
            ));
        }
        nums.push_str(&format!(
            r#"<w:num w:numId="{}"><w:abstractNumId w:val="1"/>{overrides}</w:num>"#,
            i + 2
        ));
    }
    format!(
        r#"{HEADER}<w:numbering {NS}><w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/>{bullet_levels}</w:abstractNum><w:abstractNum w:abstractNumId="1"><w:multiLevelType w:val="hybridMultilevel"/>{decimal_levels}</w:abstractNum>{nums}</w:numbering>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn part(docx: &[u8], name: &str) -> Option<String> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(docx)).unwrap();
        let mut file = zip.by_name(name).ok()?;
        let mut out = String::new();
        file.read_to_string(&mut out).unwrap();
        Some(out)
    }

    fn well_formed(xml: &str) {
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut depth = 0i32;
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Start(_)) => depth += 1,
                Ok(quick_xml::events::Event::End(_)) => depth -= 1,
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(err) => panic!("bad XML: {err}\n{xml}"),
            }
            assert!(depth >= 0, "unbalanced XML");
        }
        assert_eq!(depth, 0, "unclosed elements");
    }

    fn options() -> Options<'static> {
        Options {
            author: "Calvin",
            date: Some("2026-10-29T00:00:00Z"),
        }
    }

    fn docx(markdown: &str, comments: &[Comment]) -> Vec<u8> {
        write(markdown, comments, &options(), &mut |_| None).unwrap()
    }

    #[test]
    fn suggestions_become_tracked_changes() {
        let md = "# Plan\n\nWe ship {++soon++}{>>id:s_aaaaaaaa by:Ann<<} and {--never--}{>>id:s_bbbbbbbb by:Bob<<} \
                  {~~fast~>slow~~}{>>id:s_cccccccc by:Ann<<}.";
        let out = docx(md, &[]);
        let doc = part(&out, "word/document.xml").unwrap();
        well_formed(&doc);
        assert!(doc.contains(r#"<w:pStyle w:val="Heading1"/>"#), "{doc}");
        assert!(doc.contains(r#"w:author="Ann" w:date="2026-10-29T00:00:00Z"><w:r><w:rPr></w:rPr><w:t xml:space="preserve">soon</w:t>"#), "{doc}");
        assert!(doc.contains(r#"<w:del w:id="2" w:author="Bob""#), "{doc}");
        assert!(doc.contains(r#"<w:delText xml:space="preserve">never</w:delText>"#));
        // A replacement is a deletion then an insertion, by the same author.
        let fast = doc.find(">fast</w:delText>").unwrap();
        let slow = doc.find(">slow</w:t>").unwrap();
        assert!(fast < slow);
        assert!(!doc.contains("{++") && !doc.contains("s_aaaaaaaa"));
        let styles = part(&out, "word/styles.xml").unwrap();
        well_formed(&styles);
        assert!(styles.contains(r#"<w:name w:val="heading 1"/>"#));
        assert!(part(&out, "word/comments.xml").is_none());
    }

    #[test]
    fn margin_comments_wrap_their_words_and_notes_the_word_before() {
        let comments = [
            Comment {
                author: "Ann Lee".into(),
                text: "Is this right?\nBob: yes".into(),
                anchor: "the  quick fox".into(),
            },
            Comment {
                author: "Ann Lee".into(),
                text: "Nowhere".into(),
                anchor: "not in the text".into(),
            },
        ];
        let out = docx("Intro.\n\nSee the **quick** fox run{>>check<<} home.", &comments);
        let doc = part(&out, "word/document.xml").unwrap();
        well_formed(&doc);
        // The note (id 0) covers "run"; the margin comment (id 1) covers
        // "the quick fox"; the unfound one (id 2) sits at the start.
        let start = |id: usize| doc.find(&format!(r#"<w:commentRangeStart w:id="{id}"/>"#)).unwrap();
        let end = |id: usize| doc.find(&format!(r#"<w:commentRangeEnd w:id="{id}"/>"#)).unwrap();
        let between = |a: usize, b: usize| {
            let mut text = String::new();
            let mut rest = &doc[a..b];
            while let Some(at) = rest.find(r#"<w:t xml:space="preserve">"#) {
                rest = &rest[at + 26..];
                let close = rest.find("</w:t>").unwrap();
                text.push_str(&rest[..close]);
                rest = &rest[close..];
            }
            text
        };
        assert_eq!(between(start(1), end(1)), "the quick fox");
        assert_eq!(between(start(0), end(0)), "run");
        assert!(start(2) < doc.find("Intro.").unwrap());
        let xml = part(&out, "word/comments.xml").unwrap();
        well_formed(&xml);
        assert!(
            xml.contains(r#"<w:comment w:id="1" w:author="Ann Lee" w:date="2026-10-29T00:00:00Z" w:initials="AL">"#),
            "{xml}"
        );
        assert!(xml.contains(">Is this right?</w:t>") && xml.contains(">Bob: yes</w:t>"));
        assert!(xml.contains(r#"w:author="Calvin""#) && xml.contains(">check</w:t>"));
        let types = part(&out, "[Content_Types].xml").unwrap();
        assert!(types.contains("/word/comments.xml"));
        let rels = part(&out, "word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains(r#"Target="comments.xml""#));
    }

    #[test]
    fn long_anchors_match_by_their_ends() {
        let comments = [Comment {
            author: "A".into(),
            text: "x".into(),
            anchor: "one two … nine ten".into(),
        }];
        let out = docx(
            "zero one two three four five six seven eight nine ten eleven",
            &comments,
        );
        let doc = part(&out, "word/document.xml").unwrap();
        let start = doc.find(r#"<w:commentRangeStart w:id="0"/>"#).unwrap();
        let end = doc.find(r#"<w:commentRangeEnd w:id="0"/>"#).unwrap();
        assert!(doc[..start].contains(">zero </w:t>"), "{doc}");
        assert!(
            doc[start..end].contains(">one two three four five six seven eight nine ten</w:t>"),
            "{doc}"
        );
    }

    #[test]
    fn lists_tables_links_code_and_quotes() {
        let md = "- a\n- b\n  1. one\n  2. two\n\n3. three\n4. four\n\n| H1 | H2 |\n|---|--:|\n| x | **y** |\n\n\
                  [site](https://example.com) and [local](notes.md)\n\n```rust\nfn  main() {}\n<tag>\n```\n\n> quoted\n\n---\n";
        let out = docx(md, &[]);
        let doc = part(&out, "word/document.xml").unwrap();
        well_formed(&doc);
        assert!(doc.contains(r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>"#));
        assert!(doc.contains(r#"<w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr>"#));
        assert!(doc.contains(r#"<w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr>"#));
        let numbering = part(&out, "word/numbering.xml").unwrap();
        well_formed(&numbering);
        assert!(numbering.contains(
            r#"<w:num w:numId="3"><w:abstractNumId w:val="1"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="3"/>"#
        ));
        assert!(doc.contains("<w:tbl>") && doc.contains("<w:tblHeader/>") && doc.contains(r#"<w:jc w:val="right"/>"#));
        assert!(
            doc.contains(r#"<w:rPr><w:b/></w:rPr><w:t xml:space="preserve">H1</w:t>"#),
            "{doc}"
        );
        assert!(doc.contains(r#"<w:hyperlink r:id="rId10">"#));
        assert!(!doc.contains("notes.md\""));
        let rels = part(&out, "word/_rels/document.xml.rels").unwrap();
        well_formed(&rels);
        assert!(rels.contains(r#"Target="https://example.com" TargetMode="External""#));
        assert!(doc.contains(r#"<w:pStyle w:val="Code"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New" w:cs="Courier New"/></w:rPr><w:t xml:space="preserve">fn  main() {}</w:t>"#), "{doc}");
        assert!(doc.contains(">&lt;tag&gt;</w:t>"));
        assert!(doc.contains(r#"<w:pStyle w:val="Quote"/>"#));
        assert!(doc.contains("<w:pBdr>"));
    }

    #[test]
    fn pictures_are_embedded_at_their_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(2000u32.to_be_bytes());
        png.extend(1000u32.to_be_bytes());
        png.extend([0; 16]);
        let bytes = png.clone();
        let out = write(
            "![A chart](assets/chart.png) and ![gone](missing.png)",
            &[],
            &options(),
            &mut |dest| (dest == "assets/chart.png").then(|| (bytes.clone(), "png".to_string())),
        )
        .unwrap();
        let doc = part(&out, "word/document.xml").unwrap();
        well_formed(&doc);
        assert!(doc.contains(r#"<a:blip r:embed="rId10"/>"#));
        // 2000px is wider than the page: scaled to 6.5in, aspect kept.
        assert!(doc.contains(r#"<wp:extent cx="5943600" cy="2971800"/>"#), "{doc}");
        assert!(doc.contains(r#"descr="A chart""#));
        assert!(doc.contains(">gone</w:t>"));
        let rels = part(&out, "word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains(r#"Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png""#));
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&out[..])).unwrap();
        let mut media = Vec::new();
        zip.by_name("word/media/image1.png")
            .unwrap()
            .read_to_end(&mut media)
            .unwrap();
        assert_eq!(media, png);
    }

    #[test]
    fn front_matter_and_html_are_left_out_and_text_is_escaped() {
        let out = docx(
            "---\ntitle: x\n---\n\nA & B <b>bold</b> \u{1}\n\n<div>block</div>\n",
            &[],
        );
        let doc = part(&out, "word/document.xml").unwrap();
        well_formed(&doc);
        assert!(!doc.contains("title: x"));
        assert!(doc.contains("A &amp; B "));
        assert!(doc.contains(">block</w:t>"));
        assert!(!doc.contains("<div>") && !doc.contains('\u{1}'));
    }

    #[test]
    fn an_empty_document_is_still_a_document() {
        let out = docx("", &[]);
        let doc = part(&out, "word/document.xml").unwrap();
        well_formed(&doc);
        assert!(doc.contains("<w:p>"));
    }
}
