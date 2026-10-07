//! Markdown analysis for a source-visible editor: every byte keeps its place,
//! and the parser only decides how each byte is styled.

use crate::critic::{self, Kind, Suggestion};
use pulldown_cmark::{CodeBlockKind, Event, MetadataBlockKind, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// Style bits; a byte can carry several.
pub mod style {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const STRIKE: u16 = 1 << 2;
    pub const CODE: u16 = 1 << 3;
    pub const LINK: u16 = 1 << 4;
    /// Syntax punctuation.
    pub const MARKER: u16 = 1 << 5;
    /// URLs, HTML, front matter.
    pub const MUTED: u16 = 1 << 6;
    pub const QUOTE: u16 = 1 << 7;
    pub const LIST: u16 = 1 << 8;
    pub const MATH: u16 = 1 << 9;
    /// Suggested insertion.
    pub const INS: u16 = 1 << 10;
    /// Suggested deletion.
    pub const DEL: u16 = 1 << 11;
    /// CriticMarkup comment, including a suggestion's identity.
    pub const COMMENT: u16 = 1 << 12;
}

/// Code token kinds, carried in `Span::syntax` for highlighted code.
pub mod syntax {
    pub const KEYWORD: u8 = 1;
    pub const STRING: u8 = 2;
    pub const NUMBER: u8 = 3;
    pub const CONSTANT: u8 = 4;
    pub const COMMENT: u8 = 5;
    pub const FUNCTION: u8 = 6;
    pub const TYPE: u8 = 7;
    pub const TAG: u8 = 8;
    pub const ATTRIBUTE: u8 = 9;
    pub const OPERATOR: u8 = 10;
    pub const PUNCTUATION: u8 = 11;
    pub const VARIABLE: u8 = 12;
    pub const COUNT: usize = 13;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Block {
    #[default]
    Text,
    Heading(u8),
    Code,
    Table,
    Frontmatter,
    Rule,
}

/// A styled byte range within one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: u16,
    /// A `syntax` kind for highlighted code, else 0.
    pub syntax: u8,
}

#[derive(Clone, Copy, Debug, Default)]
struct Line {
    block: Block,
    spans: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The last line of the image syntax; the picture is shown below it.
    pub line: usize,
    pub url: String,
}

/// Code with a known language: a fenced block, or front matter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeBlock {
    pub lang: String,
    /// `(line, byte where the code starts on it)`, in order.
    pub lines: Vec<(u32, u32)>,
}

#[derive(Default)]
pub struct Doc {
    lines: Vec<Line>,
    spans: Vec<Span>,
    /// What was analysed, kept to find what an edit changed.
    source: String,
    line_starts: Vec<usize>,
    /// Lines where a top-level block starts: safe places to resume parsing.
    tops: Vec<u32>,
    /// Lines holding link reference and footnote definitions. They decide
    /// how references read anywhere in the document.
    defs: Vec<Range<u32>>,
    /// The same definitions restated one per line, to resolve references
    /// when only part of the document is parsed. `None` if a label cannot be
    /// restated faithfully.
    def_text: Option<String>,
    comments: Vec<Range<usize>>,
    /// Lines that would open a metadata block if a closing line existed
    /// later in the document.
    loose: Vec<u32>,
    /// The last analysis reused everything outside the edited blocks.
    pub incremental: bool,
    pub images: Vec<Image>,
    pub suggestions: Vec<Suggestion>,
    /// Top-level fenced code blocks, as line ranges.
    pub fences: Vec<Range<usize>>,
    pub code_blocks: Vec<CodeBlock>,
    pub words: usize,
    pub chars: usize,
}

impl Doc {
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn block(&self, line: usize) -> Block {
        self.lines.get(line).map_or(Block::Text, |line| line.block)
    }
    pub fn spans(&self, line: usize) -> &[Span] {
        self.lines
            .get(line)
            .map_or(&[], |line| &self.spans[line.spans.0 as usize..line.spans.1 as usize])
    }
    /// Images shown below `line`.
    pub fn images_on(&self, line: usize) -> impl Iterator<Item = &Image> {
        let start = self.images.partition_point(|image| image.line < line);
        self.images[start..].iter().take_while(move |image| image.line == line)
    }
    /// The code block covering `line`, and the line's index within it.
    pub fn code_at(&self, line: usize) -> Option<(usize, usize)> {
        let line = line as u32;
        let after = self
            .code_blocks
            .partition_point(|block| block.lines.first().is_some_and(|first| first.0 <= line));
        let block = after.checked_sub(1)?;
        let at = self.code_blocks[block]
            .lines
            .binary_search_by_key(&line, |entry| entry.0)
            .ok()?;
        Some((block, at))
    }
    pub fn in_fence(&self, line: usize) -> bool {
        self.fences.iter().any(|fence| fence.contains(&line))
    }
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_WIKILINKS
}

struct Painter<'a> {
    text: &'a str,
    bytes: &'a [u8],
    styles: Vec<u16>,
    line_starts: Vec<usize>,
    blocks: Vec<Block>,
}

impl Painter<'_> {
    fn paint(&mut self, range: Range<usize>, style: u16) {
        let end = range.end.min(self.styles.len());
        for slot in &mut self.styles[range.start.min(end)..end] {
            *slot |= style;
        }
    }

    fn line_of(&self, byte: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1)
    }

    /// Lines touched by `range`, ignoring a trailing newline.
    fn lines(&self, range: &Range<usize>) -> Range<usize> {
        let mut end = range.end.max(range.start + 1);
        while end > range.start + 1 && matches!(self.bytes.get(end - 1), Some(b'\n')) {
            end -= 1;
        }
        self.line_of(range.start)..self.line_of(end - 1) + 1
    }

    fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.line_starts[line];
        let end = self.line_starts.get(line + 1).map_or(self.bytes.len(), |next| next - 1);
        start..end
    }

    fn set_block(&mut self, range: &Range<usize>, block: Block) {
        for line in self.lines(range) {
            self.blocks[line] = block;
        }
    }

    fn run(&self, at: usize, pred: impl Fn(u8) -> bool) -> usize {
        self.bytes[at.min(self.bytes.len())..]
            .iter()
            .take_while(|&&b| pred(b))
            .count()
    }

    fn delimited(&mut self, range: Range<usize>, style: u16, width: usize) {
        let width = width.min((range.end - range.start) / 2);
        self.paint(range.clone(), style);
        self.paint(range.start..range.start + width, style::MARKER);
        self.paint(range.end - width..range.end, style::MARKER);
    }

    fn quote_markers(&mut self, range: &Range<usize>) {
        for line in self.lines(range) {
            let line_range = self.line_range(line);
            let mut at = line_range.start.max(range.start.min(line_range.end));
            if line > self.line_of(range.start) {
                at = line_range.start;
            }
            loop {
                let ws = self.run(at, |b| b == b' ' || b == b'\t');
                if at + ws >= line_range.end || self.bytes[at + ws] != b'>' {
                    break;
                }
                let mut end = at + ws + 1;
                if self.bytes.get(end) == Some(&b' ') {
                    end += 1;
                }
                self.paint(at + ws..end, style::MARKER);
                at = end;
            }
        }
    }
}

/// The block-level pass over one stretch of text.
struct Pass {
    styles: Vec<u16>,
    line_starts: Vec<usize>,
    blocks: Vec<Block>,
    images: Vec<Image>,
    code_blocks: Vec<CodeBlock>,
    tops: Vec<u32>,
    defs: Vec<Range<u32>>,
    def_text: Option<String>,
}

impl Pass {
    /// Drops the first `lines` lines (`bytes` bytes) and renumbers the rest.
    fn trim_front(&mut self, lines: usize, bytes: usize) {
        let line = lines as u32;
        self.styles.drain(..bytes);
        self.line_starts.drain(..lines);
        self.line_starts.iter_mut().for_each(|start| *start -= bytes);
        self.blocks.drain(..lines);
        self.images.retain(|image| image.line >= lines);
        self.images.iter_mut().for_each(|image| image.line -= lines);
        self.code_blocks.retain(|block| block.lines[0].0 >= line);
        self.code_blocks
            .iter_mut()
            .flat_map(|block| &mut block.lines)
            .for_each(|entry| entry.0 -= line);
        self.tops.retain(|&top| top >= line);
        self.tops.iter_mut().for_each(|top| *top -= line);
        self.defs.retain(|def| def.start >= line);
        self.defs
            .iter_mut()
            .for_each(|def| *def = def.start - line..def.end - line);
    }
}

/// Runs pulldown over `text`.
fn pass(text: &str) -> Pass {
    let bytes = text.as_bytes();
    let mut line_starts = vec![0];
    line_starts.extend(memchr::memchr_iter(b'\n', bytes).map(|at| at + 1));
    let mut p = Painter {
        text,
        bytes,
        styles: vec![0; bytes.len()],
        blocks: vec![Block::Text; line_starts.len()],
        line_starts,
    };
    let mut images = Vec::new();
    // Open links and images: (range start, end of the last child seen, is image).
    let mut links: Vec<(usize, usize, bool)> = Vec::new();
    let mut code_blocks: Vec<CodeBlock> = Vec::new();
    let mut in_code = false;
    let mut tops: Vec<u32> = Vec::new();
    let mut depth = 0usize;
    let mut defs: Vec<Range<usize>> = Vec::new();
    // `(label, destination)`; a footnote has no destination.
    let mut labels: Vec<(String, Option<String>)> = Vec::new();
    let parser = Parser::new_ext(text, options());
    for (label, def) in parser.reference_definitions().iter() {
        defs.push(def.span.clone());
        labels.push((label.to_string(), Some(def.dest.to_string())));
    }

    for (event, range) in parser.into_offset_iter() {
        match &event {
            Event::Start(tag) => {
                if depth == 0 {
                    tops.push(p.line_of(range.start) as u32);
                }
                depth += 1;
                if let Tag::FootnoteDefinition(label) = tag {
                    defs.push(range.clone());
                    labels.push((label.to_string(), None));
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ if depth == 0 => tops.push(p.line_of(range.start) as u32),
            _ => {}
        }
        if let Some(open) = links.last_mut()
            && !matches!(event, Event::End(TagEnd::Link | TagEnd::Image))
        {
            open.1 = open.1.max(range.end);
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    p.set_block(&range, Block::Heading(level as u8));
                    let indent = p.run(range.start, |b| b == b' ');
                    let hashes = p.run(range.start + indent, |b| b == b'#');
                    if hashes > 0 {
                        let at = range.start + indent + hashes;
                        let space = p.run(at, |b| b == b' ' || b == b'\t');
                        p.paint(range.start..at + space, style::MARKER);
                    } else {
                        let lines = p.lines(&range);
                        if lines.len() > 1 {
                            let underline = p.line_range(lines.end - 1);
                            p.paint(underline, style::MARKER);
                        }
                    }
                }
                Tag::BlockQuote(_) => {
                    p.paint(range.clone(), style::QUOTE);
                    p.quote_markers(&range);
                }
                Tag::CodeBlock(kind) => {
                    if let CodeBlockKind::Fenced(info) = &kind {
                        let lang = info
                            .split([' ', ',', '\t', '{'])
                            .next()
                            .unwrap_or("")
                            .trim_start_matches('.');
                        if !lang.is_empty() {
                            code_blocks.push(CodeBlock {
                                lang: lang.to_ascii_lowercase(),
                                lines: Vec::new(),
                            });
                            in_code = true;
                        }
                    }
                    p.set_block(&range, Block::Code);
                    if matches!(kind, CodeBlockKind::Fenced(_)) {
                        let lines = p.lines(&range);
                        let first = p.line_range(lines.start);
                        p.paint(range.start.max(first.start)..first.end, style::MARKER);
                        if lines.len() > 1 {
                            let last = p.line_range(lines.end - 1);
                            let fence = p.text[last.clone()].trim_start_matches([' ', '\t', '>']).trim_end();
                            let closes = fence.len() >= 3
                                && (fence.bytes().all(|b| b == b'`') || fence.bytes().all(|b| b == b'~'));
                            if closes {
                                p.paint(last, style::MARKER);
                            }
                        }
                    }
                }
                Tag::Item => {
                    let start = range.start + p.run(range.start, |b| b == b' ' || b == b'\t');
                    let bullet = p.run(start, |b| matches!(b, b'-' | b'*' | b'+')).min(1);
                    let digits = p.run(start, |b| b.is_ascii_digit());
                    let width = if bullet == 1 { 1 } else { digits + 1 };
                    p.paint(start..start + width, style::LIST);
                }
                Tag::DefinitionListDefinition => p.paint(range.start..range.start + 1, style::LIST),
                Tag::FootnoteDefinition(_) => {
                    let label = p.text[range.clone()].find("]:").map_or(0, |at| at + 2);
                    p.paint(range.start..range.start + label, style::LINK);
                }
                Tag::Table(_) => {
                    p.set_block(&range, Block::Table);
                    let lines = p.lines(&range);
                    for line in lines.clone() {
                        let line_range = p.line_range(line);
                        if line == lines.start + 1 {
                            p.paint(line_range, style::MARKER);
                            continue;
                        }
                        for at in line_range {
                            if bytes[at] == b'|' && (at == 0 || bytes[at - 1] != b'\\') {
                                p.paint(at..at + 1, style::MARKER);
                            }
                        }
                    }
                }
                Tag::TableHead => p.paint(range, style::BOLD),
                Tag::Emphasis => p.delimited(range, style::ITALIC, 1),
                Tag::Strong => p.delimited(range, style::BOLD, 2),
                Tag::Strikethrough => {
                    let width = p.run(range.start, |b| b == b'~').min(2);
                    p.delimited(range, style::STRIKE, width);
                }
                Tag::Link { .. } => links.push((range.start, range.start + 1, false)),
                Tag::Image { dest_url, .. } => {
                    links.push((range.start, range.start + 2, true));
                    images.push(Image {
                        line: p.lines(&range).end - 1,
                        url: dest_url.to_string(),
                    });
                }
                Tag::HtmlBlock => p.paint(range, style::MUTED),
                Tag::MetadataBlock(kind) => {
                    let lang = if kind == MetadataBlockKind::YamlStyle {
                        "yaml"
                    } else {
                        "toml"
                    };
                    code_blocks.push(CodeBlock {
                        lang: lang.to_string(),
                        lines: Vec::new(),
                    });
                    in_code = true;
                    p.set_block(&range, Block::Frontmatter);
                    p.paint(range, style::MUTED);
                }
                _ => {}
            },
            Event::End(TagEnd::CodeBlock | TagEnd::MetadataBlock(_)) => in_code = false,
            Event::Text(_) if in_code => {
                let block = code_blocks.last_mut().unwrap();
                for line in p.lines(&range) {
                    let start = p.line_starts[line];
                    block
                        .lines
                        .push((line as u32, range.start.max(start).saturating_sub(start) as u32));
                }
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                let Some((start, inner_end, image)) = links.pop() else {
                    continue;
                };
                let open = if image { 2 } else { 1 };
                let source = &p.text[range.clone()];
                if source.starts_with('<') {
                    p.delimited(range, style::LINK, 1);
                } else if source.starts_with("[[") {
                    p.delimited(range, style::LINK, 2);
                } else {
                    let inner_end = inner_end.clamp((start + open).min(range.end), range.end);
                    p.paint(start..start + open, style::MARKER);
                    p.paint(start + open..inner_end, style::LINK);
                    p.paint(inner_end..range.end, style::MUTED);
                }
            }
            Event::Code(_) => {
                let ticks = p.run(range.start, |b| b == b'`');
                p.delimited(range, style::CODE, ticks);
            }
            Event::InlineMath(_) => p.delimited(range, style::MATH, 1),
            Event::DisplayMath(_) => p.delimited(range, style::MATH, 2),
            Event::Html(_) | Event::InlineHtml(_) => p.paint(range, style::MUTED),
            Event::FootnoteReference(_) => p.paint(range, style::LINK),
            Event::TaskListMarker(_) => p.paint(range, style::LIST),
            Event::Rule => {
                p.set_block(&range, Block::Rule);
                p.paint(range, style::MARKER);
            }
            _ => {}
        }
    }

    code_blocks.retain(|block| !block.lines.is_empty());
    images.sort_by_key(|image: &Image| image.line);
    tops.dedup();
    let mut defs: Vec<Range<u32>> = defs
        .iter()
        .map(|def| p.lines(def))
        .map(|lines| lines.start as u32..lines.end as u32)
        .collect();
    defs.sort_by_key(|def| (def.start, def.end));
    defs.dedup();
    labels.sort();
    labels.dedup();
    let simple = |(label, dest): &(String, Option<String>)| {
        !label.trim().is_empty()
            && !label.starts_with('^')
            && !label.contains(['[', ']', '\\', '\n', '\r'])
            && !dest
                .as_ref()
                .is_some_and(|dest| dest.contains(['<', '>', '\\', '\n', '\r']))
    };
    let restate = |(label, dest): &(String, Option<String>)| match dest {
        Some(dest) => format!("[{label}]: <{dest}>\n\n"),
        None => format!("[^{label}]: x\n\n"),
    };
    let def_text = labels.iter().all(simple).then(|| labels.iter().map(restate).collect());
    Pass {
        styles: p.styles,
        line_starts: p.line_starts,
        blocks: p.blocks,
        images,
        code_blocks,
        tops,
        defs,
        def_text,
    }
}

/// Paints the suggestions and comments that lie inside `window` (document
/// byte offsets) onto its styles.
fn paint_critic(styles: &mut [u16], window: Range<usize>, suggestions: &[Suggestion], comments: &[Range<usize>]) {
    let base = window.start;
    let inside = |range: &Range<usize>| range.start >= window.start && range.end <= window.end;
    let first = suggestions.partition_point(|suggestion| suggestion.bytes.start < window.start);
    for suggestion in suggestions[first..]
        .iter()
        .take_while(|suggestion| inside(&suggestion.bytes))
    {
        let span = suggestion.bytes.clone();
        let edit_end = suggestion.meta_bytes.start;
        // Markdown's own reading of these bytes (`~~` as strikethrough) does not apply.
        for slot in &mut styles[span.start - base..span.end - base] {
            *slot &= !(style::STRIKE | style::MARKER);
        }
        let mut paint = |range: Range<usize>, style: u16| {
            for slot in &mut styles[range.start - base..range.end - base] {
                *slot |= style;
            }
        };
        paint(span.start..span.start + 3, style::MARKER);
        paint(edit_end - 3..edit_end, style::MARKER);
        paint(suggestion.old_bytes.clone(), style::DEL);
        paint(suggestion.new_bytes.clone(), style::INS);
        if suggestion.kind == Kind::Replace {
            paint(suggestion.old_bytes.end..suggestion.new_bytes.start, style::MARKER);
        }
        styles[suggestion.meta_bytes.start - base..suggestion.meta_bytes.end - base].fill(style::COMMENT);
    }
    let first = comments.partition_point(|comment| comment.start < window.start);
    for comment in comments[first..].iter().take_while(|comment| inside(comment)) {
        styles[comment.start - base..comment.end - base].fill(style::COMMENT);
    }
}

/// Run-length encodes the styles of lines `0..count` into spans.
fn emit(text: &str, pass: &Pass, count: usize, lines: &mut Vec<Line>, spans: &mut Vec<Span>) {
    for line in 0..count {
        let start = pass.line_starts[line];
        let end = pass.line_starts.get(line + 1).map_or(text.len(), |next| next - 1);
        let first = spans.len() as u32;
        let mut at = start;
        while at < end {
            let current = pass.styles[at];
            let mut run = at + 1;
            while run < end && pass.styles[run] == current {
                run += 1;
            }
            if current != 0 {
                spans.push(Span {
                    start: (at - start) as u32,
                    end: (run - start) as u32,
                    style: current,
                    syntax: 0,
                });
            }
            at = run;
        }
        lines.push(Line {
            block: pass.blocks[line],
            spans: (first, spans.len() as u32),
        });
    }
}

/// True if the line ends with one of `ends`. Deliberately loose: whatever
/// container markers come before (quote, list, definition) do not matter.
fn delimiter_line(line: &str, ends: &[&str]) -> bool {
    let line = line.trim_end();
    ends.iter().any(|end| line.ends_with(end))
}

/// Lines among the first `count` that pulldown would turn into a metadata
/// block given a closing line anywhere after them. Its search for that line
/// has no bound, so these are the one place where text far away decides how
/// a block reads.
fn loose_openers(text: &str, pass: &Pass, count: usize) -> Vec<u32> {
    let line = |index: usize| {
        let start = pass.line_starts[index];
        let end = pass.line_starts.get(index + 1).map_or(text.len(), |next| next - 1);
        &text[start..end.max(start)]
    };
    let candidate = |index: usize| {
        delimiter_line(line(index), &["---", "+++"])
            && index + 1 < pass.line_starts.len()
            && line(index + 1).bytes().any(|b| !b.is_ascii_whitespace())
    };
    (0..count)
        .filter(|&index| match pass.blocks[index] {
            Block::Rule | Block::Text => candidate(index),
            // A metadata block that ends at its own closing line is settled;
            // one cut short by its container still hangs on a line elsewhere.
            Block::Frontmatter if index == 0 || pass.blocks[index - 1] != Block::Frontmatter => {
                let last = (index..pass.blocks.len())
                    .take_while(|&at| pass.blocks[at] == Block::Frontmatter)
                    .last()
                    .unwrap_or(index);
                candidate(index) && !(last > index && delimiter_line(line(last), &["---", "+++", "..."]))
            }
            _ => false,
        })
        .map(|index| index as u32)
        .collect()
}

/// Lines covered by a byte range, ignoring trailing newlines.
fn fence_lines(line_starts: &[usize], text: &str, fence: &Range<usize>) -> Range<usize> {
    let bytes = text.as_bytes();
    let line_of = |byte: usize| line_starts.partition_point(|&start| start <= byte).saturating_sub(1);
    let mut end = fence.end.max(fence.start + 1);
    while end > fence.start + 1 && matches!(bytes.get(end - 1), Some(b'\n')) {
        end -= 1;
    }
    line_of(fence.start)..line_of(end - 1) + 1
}

pub fn parse(text: &str) -> Doc {
    let critic = critic::parse(text);
    let mut pass = pass(text);
    paint_critic(&mut pass.styles, 0..text.len(), &critic.suggestions, &critic.comments);
    let mut doc = Doc {
        lines: Vec::with_capacity(pass.blocks.len()),
        fences: critic
            .fences
            .iter()
            .map(|fence| fence_lines(&pass.line_starts, text, fence))
            .collect(),
        suggestions: critic.suggestions,
        comments: critic.comments,
        words: text.split_whitespace().count(),
        chars: count_chars(text),
        source: text.to_string(),
        ..Doc::default()
    };
    emit(text, &pass, pass.blocks.len(), &mut doc.lines, &mut doc.spans);
    doc.loose = loose_openers(text, &pass, pass.blocks.len());
    doc.images = pass.images;
    doc.code_blocks = pass.code_blocks;
    doc.tops = pass.tops;
    doc.defs = pass.defs;
    doc.def_text = pass.def_text;
    doc.line_starts = pass.line_starts;
    doc
}

/// Documents smaller than this are simply re-analysed whole.
pub const INCREMENTAL_FROM: usize = 16 << 10;

/// Analyses `text`, reusing `old` (the analysis of the text before an edit)
/// for everything outside the blocks the edit touched.
pub fn update(old: Doc, text: &str) -> Doc {
    if text.len() < INCREMENTAL_FROM {
        return parse(text);
    }
    reanalyse(old, text)
}

/// `update` without the size floor.
pub fn reanalyse(old: Doc, text: &str) -> Doc {
    match plan(&old, text) {
        Some(change) => change.apply(old, text),
        None => parse(text),
    }
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let limit = a.len().min(b.len());
    let mut at = 0;
    while at + 256 <= limit && a[at..at + 256] == b[at..at + 256] {
        at += 256;
    }
    while at < limit && a[at] == b[at] {
        at += 1;
    }
    at
}

fn common_suffix(a: &[u8], b: &[u8], limit: usize) -> usize {
    let mut len = 0;
    while len + 256 <= limit && a[a.len() - len - 256..a.len() - len] == b[b.len() - len - 256..b.len() - len] {
        len += 256;
    }
    while len < limit && a[a.len() - len - 1] == b[b.len() - len - 1] {
        len += 1;
    }
    len
}

/// Code points in `text`: every byte that does not continue a character.
fn count_chars(text: &str) -> usize {
    text.as_bytes().iter().filter(|&&byte| (byte as i8) >= -0x40).count()
}

/// A verified partial re-analysis, ready to be spliced into the old one.
struct Change {
    /// Old lines `window_line..resume_line` are replaced.
    window_line: usize,
    resume_line: usize,
    /// The changed bytes: `old[edit.0..edit.1]` became `text[edit.0..edit.2]`.
    edit: (usize, usize, usize),
    lines: Vec<Line>,
    spans: Vec<Span>,
    line_starts: Vec<usize>,
    tops: Vec<u32>,
    images: Vec<Image>,
    code_blocks: Vec<CodeBlock>,
    critic: critic::Parsed,
    words: usize,
    chars: usize,
}

impl Change {
    fn apply(mut self, mut doc: Doc, text: &str) -> Doc {
        let (window_line, resume_line) = (self.window_line, self.resume_line);
        let kept = self.lines.len();
        let line_shift = (window_line + kept) as isize - resume_line as isize;
        let delta = self.edit.2 as isize - self.edit.1 as isize;

        let spans_before = doc
            .lines
            .get(window_line)
            .map_or(doc.spans.len(), |line| line.spans.0 as usize);
        let spans_after = doc
            .lines
            .get(resume_line)
            .map_or(doc.spans.len(), |line| line.spans.0 as usize);
        for line in &mut self.lines {
            line.spans = (line.spans.0 + spans_before as u32, line.spans.1 + spans_before as u32);
        }
        let span_shift = self.spans.len() as isize - (spans_after - spans_before) as isize;
        doc.spans.splice(spans_before..spans_after, self.spans);
        doc.lines.splice(window_line..resume_line, self.lines);
        if span_shift != 0 {
            for line in &mut doc.lines[window_line + kept..] {
                line.spans = (
                    (line.spans.0 as isize + span_shift) as u32,
                    (line.spans.1 as isize + span_shift) as u32,
                );
            }
        }
        doc.line_starts.splice(window_line..resume_line, self.line_starts);
        if delta != 0 {
            for start in &mut doc.line_starts[window_line + kept..] {
                *start = (*start as isize + delta) as usize;
            }
        }

        let moved = |line: u32| (line as isize + line_shift) as u32;
        let (window, resume) = (window_line as u32, resume_line as u32);
        let range =
            |list: &[u32]| list.partition_point(|&line| line < window)..list.partition_point(|&line| line < resume);
        let at = range(&doc.tops);
        let tail = at.start + self.tops.len();
        doc.tops.splice(at, self.tops);
        let at = range(&doc.loose);
        let loose_tail = at.start;
        doc.loose.drain(at);
        let defs_tail = doc.defs.partition_point(|def| def.start < window);
        let at = doc.images.partition_point(|image| image.line < window_line)
            ..doc.images.partition_point(|image| image.line < resume_line);
        let images_tail = at.start + self.images.len();
        doc.images.splice(at, self.images);
        let first = |block: &CodeBlock| block.lines[0].0;
        let at = doc.code_blocks.partition_point(|block| first(block) < window)
            ..doc.code_blocks.partition_point(|block| first(block) < resume);
        let code_tail = at.start + self.code_blocks.len();
        doc.code_blocks.splice(at, self.code_blocks);
        if line_shift != 0 {
            doc.tops[tail..].iter_mut().for_each(|top| *top = moved(*top));
            doc.loose[loose_tail..].iter_mut().for_each(|line| *line = moved(*line));
            doc.defs[defs_tail..]
                .iter_mut()
                .for_each(|def| *def = moved(def.start)..moved(def.end));
            doc.images[images_tail..]
                .iter_mut()
                .for_each(|image| image.line = moved(image.line as u32) as usize);
            doc.code_blocks[code_tail..]
                .iter_mut()
                .flat_map(|block| &mut block.lines)
                .for_each(|entry| entry.0 = moved(entry.0));
        }

        doc.source
            .replace_range(self.edit.0..self.edit.1, &text[self.edit.0..self.edit.2]);
        doc.fences = self
            .critic
            .fences
            .iter()
            .map(|fence| fence_lines(&doc.line_starts, text, fence))
            .collect();
        doc.suggestions = self.critic.suggestions;
        doc.comments = self.critic.comments;
        doc.words = self.words;
        doc.chars = self.chars;
        doc.incremental = true;
        doc
    }
}

fn plan(old: &Doc, text: &str) -> Option<Change> {
    // STET_TRACE_PARSE=1 prints which check sent an edit to a full analysis.
    fn why<T>(check: u32) -> Option<T> {
        if std::env::var_os("STET_TRACE_PARSE").is_some() {
            eprintln!("stet: full analysis (check {check})");
        }
        None
    }
    if old.lines.is_empty() || old.source.is_empty() {
        return why(1);
    }
    let (before, after) = (old.source.as_bytes(), text.as_bytes());
    // Snap both ends outward to character boundaries.
    let mut prefix = common_prefix(before, after);
    while !text.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = common_suffix(before, after, before.len().min(after.len()) - prefix);
    while !text.is_char_boundary(after.len() - suffix) {
        suffix -= 1;
    }
    let old_end = before.len() - suffix;
    let delta = after.len() as isize - before.len() as isize;
    let start_of = |line: u32| old.line_starts[line as usize];

    // Back up two blocks: an edit can join its block to the one before, and
    // a definition typed under a paragraph joins both to a list before that.
    let edited = old.tops.partition_point(|&top| start_of(top) <= prefix);
    // Then on to a block that follows a blank line, where nothing before it
    // (a definition, a paragraph) can still be open.
    let blank = |line: usize| {
        before[old.line_starts[line]..old.line_starts[line + 1]]
            .iter()
            .all(u8::is_ascii_whitespace)
    };
    let mut back = edited.saturating_sub(3);
    while back > 0 && !blank(old.tops[back] as usize - 1) {
        back -= 1;
    }
    let window_line = if back > 0 { old.tops[back] as usize } else { 0 };
    let window_start = old.line_starts[window_line];
    // Resume at the first block that starts after the edit; parse one block
    // further so that block is seen whole.
    let resume = old.tops[edited.min(old.tops.len())..]
        .iter()
        .position(|&top| start_of(top) >= old_end)
        .map(|at| at + edited);
    let resume_line = resume.map(|at| old.tops[at] as usize);
    let window_end_old = match resume.and_then(|at| old.tops.get(at + 2)) {
        Some(&top) => start_of(top),
        None => before.len(),
    };
    let shift = |old_offset: usize| (old_offset as isize + delta) as usize;
    let window_text = &text[window_start..shift(window_end_old)];
    // Definitions elsewhere decide how references in the window read, so the
    // window is parsed with them in front, closed off by a rule. Editing the
    // definitions themselves restyles the whole document.
    let resume_at = resume_line.unwrap_or(old.lines.len());
    if old
        .defs
        .iter()
        .any(|def| (def.start as usize) < resume_at && def.end as usize > window_line)
    {
        return why(2);
    }
    let mut pass = if old.defs.is_empty() {
        pass(window_text)
    } else {
        let mut combined = old.def_text.clone()?;
        combined.push_str("***\n\n");
        let (bytes, lines) = (combined.len(), memchr::memchr_iter(b'\n', combined.as_bytes()).count());
        combined.push_str(window_text);
        let mut pass = pass(&combined);
        pass.trim_front(lines, bytes);
        pass
    };
    // The window's line holding the resume point, and proof that parsing
    // arrives there, and at the block after it, at a clean block boundary.
    let kept_lines = match resume {
        Some(at) => {
            let line = pass
                .line_starts
                .binary_search(&(shift(start_of(old.tops[at])) - window_start))
                .ok()
                .or_else(|| why(101))?;
            pass.tops.binary_search(&(line as u32)).ok().or_else(|| why(102))?;
            if let Some(&next) = old.tops.get(at + 1) {
                let next_line = pass
                    .line_starts
                    .binary_search(&(shift(start_of(next)) - window_start))
                    .ok()
                    .or_else(|| why(103))?;
                pass.tops.binary_search(&(next_line as u32)).ok().or_else(|| why(104))?;
            }
            line
        }
        None => pass.blocks.len(),
    };
    if pass.defs.iter().any(|def| (def.start as usize) < kept_lines) {
        return why(3);
    }
    // Metadata blocks look ahead without bound. A candidate in the window may
    // have its closing line beyond it, and a closing line typed here may
    // complete a candidate far above.
    let loose = loose_openers(window_text, &pass, kept_lines);
    if !loose.is_empty() {
        return why(4);
    }
    if old.loose.first().is_some_and(|&line| (line as usize) < window_line) {
        let closes = |line: usize| {
            let start = pass.line_starts[line];
            let end = pass
                .line_starts
                .get(line + 1)
                .map_or(window_text.len(), |next| next - 1);
            delimiter_line(&window_text[start..end.max(start)], &["---", "+++", "..."])
        };
        let closed = |line: usize| {
            let start = old.line_starts[line];
            let end = old.line_starts.get(line + 1).map_or(before.len(), |next| next - 1);
            delimiter_line(&old.source[start..end.max(start)], &["---", "+++", "..."])
        };
        if (0..kept_lines).any(closes) || (window_line..resume_at).any(closed) {
            return why(5);
        }
    }
    let resume_old = resume_line.map_or(before.len(), |line| old.line_starts[line]);
    let window_new = window_start..shift(resume_old);
    let window_old = window_start..resume_old;

    // Anything that could be a definition, even a shadowed duplicate, can
    // change which definition wins.
    let (was, now) = (&old.source[window_old.clone()], &text[window_new.clone()]);
    if memchr::memmem::find(was.as_bytes(), b"]:").is_some() || memchr::memmem::find(now.as_bytes(), b"]:").is_some() {
        return why(6);
    }

    // Suggestions are found by their own whole-document scan. They may only
    // differ inside the window; anything else restyles text we are keeping.
    let critic = critic::parse(text);
    let outside = |list: &[Range<usize>], window: &Range<usize>, moved: isize| -> Option<Vec<Range<usize>>> {
        let mut out = Vec::new();
        for range in list {
            if range.end <= window.start {
                out.push(range.clone());
            } else if range.start >= window.end {
                out.push((range.start as isize + moved) as usize..(range.end as isize + moved) as usize);
            } else if !(range.start >= window.start && range.end <= window.end) {
                return why(7);
            }
        }
        Some(out)
    };
    let ranges = |suggestions: &[Suggestion]| {
        suggestions
            .iter()
            .map(|suggestion| suggestion.bytes.clone())
            .collect::<Vec<_>>()
    };
    if outside(&ranges(&old.suggestions), &window_old, delta)? != outside(&ranges(&critic.suggestions), &window_new, 0)?
        || outside(&old.comments, &window_old, delta)? != outside(&critic.comments, &window_new, 0)?
    {
        return why(8);
    }
    paint_critic(
        &mut pass.styles,
        window_start..window_start + window_text.len(),
        &critic.suggestions,
        &critic.comments,
    );

    let resume_line = resume_line.unwrap_or(old.lines.len());
    let (mut lines, mut spans) = (Vec::with_capacity(kept_lines), Vec::new());
    emit(window_text, &pass, kept_lines, &mut lines, &mut spans);
    pass.line_starts.truncate(kept_lines);
    pass.line_starts.iter_mut().for_each(|start| *start += window_start);
    pass.tops.retain(|&top| (top as usize) < kept_lines);
    pass.tops.iter_mut().for_each(|top| *top += window_line as u32);
    pass.images.retain(|image| image.line < kept_lines);
    pass.images.iter_mut().for_each(|image| image.line += window_line);
    pass.code_blocks
        .retain(|block| (block.lines[0].0 as usize) < kept_lines);
    for block in &mut pass.code_blocks {
        block.lines.retain(|entry| (entry.0 as usize) < kept_lines);
        block.lines.iter_mut().for_each(|entry| entry.0 += window_line as u32);
    }
    Some(Change {
        window_line,
        resume_line,
        edit: (prefix, old_end, shift(old_end)),
        lines,
        spans,
        line_starts: pass.line_starts,
        tops: pass.tops,
        images: pass.images,
        code_blocks: pass.code_blocks,
        critic,
        words: old.words + now.split_whitespace().count() - was.split_whitespace().count(),
        chars: old.chars + count_chars(now) - count_chars(was),
    })
}

#[cfg(test)]
mod tests {
    use super::style::*;
    use super::*;

    /// `(text, style)` for each styled span of `line`.
    fn styled(source: &str, line: usize) -> Vec<(String, u16)> {
        let doc = parse(source);
        let text = source.split('\n').nth(line).unwrap();
        doc.spans(line)
            .iter()
            .map(|span| (text[span.start as usize..span.end as usize].to_string(), span.style))
            .collect()
    }

    fn s(text: &str, style: u16) -> (String, u16) {
        (text.to_string(), style)
    }

    #[test]
    fn inline_emphasis_marks_delimiters() {
        assert_eq!(
            styled("a **b** *c* ~~d~~ `e`", 0),
            vec![
                s("**", BOLD | MARKER),
                s("b", BOLD),
                s("**", BOLD | MARKER),
                s("*", ITALIC | MARKER),
                s("c", ITALIC),
                s("*", ITALIC | MARKER),
                s("~~", STRIKE | MARKER),
                s("d", STRIKE),
                s("~~", STRIKE | MARKER),
                s("`", CODE | MARKER),
                s("e", CODE),
                s("`", CODE | MARKER),
            ]
        );
    }

    #[test]
    fn nested_emphasis_combines() {
        assert_eq!(
            styled("***x***", 0),
            vec![
                s("*", ITALIC | MARKER),
                s("**", ITALIC | BOLD | MARKER),
                s("x", ITALIC | BOLD),
                s("**", ITALIC | BOLD | MARKER),
                s("*", ITALIC | MARKER),
            ]
        );
    }

    #[test]
    fn headings_atx_and_setext() {
        let doc = parse("## Title\n\nSetext\n===\n\nbody");
        assert_eq!(doc.block(0), Block::Heading(2));
        assert_eq!(doc.block(2), Block::Heading(1));
        assert_eq!(doc.block(3), Block::Heading(1));
        assert_eq!(doc.block(5), Block::Text);
        assert_eq!(styled("## Title", 0), vec![s("## ", MARKER)]);
        assert_eq!(styled("Setext\n===", 1), vec![s("===", MARKER)]);
    }

    #[test]
    fn links_images_and_autolinks() {
        assert_eq!(
            styled("[text](http://x \"t\") <http://y>", 0),
            vec![
                s("[", MARKER),
                s("text", LINK),
                s("](http://x \"t\")", MUTED),
                s("<", LINK | MARKER),
                s("http://y", LINK),
                s(">", LINK | MARKER),
            ]
        );
        let doc = parse("para\n\n![alt](img.png)\nmore ![b](two.jpg)\n");
        assert_eq!(
            doc.images,
            vec![
                Image {
                    line: 2,
                    url: "img.png".into()
                },
                Image {
                    line: 3,
                    url: "two.jpg".into()
                },
            ]
        );
        assert_eq!(doc.images_on(3).count(), 1);
        assert_eq!(
            styled("![alt](i.png)", 0),
            vec![s("![", MARKER), s("alt", LINK), s("](i.png)", MUTED)]
        );
    }

    #[test]
    fn lists_tasks_and_quotes() {
        assert_eq!(styled("- [x] done", 0), vec![s("-", LIST), s("[x]", LIST)]);
        assert_eq!(styled("12. item", 0), vec![s("12.", LIST)]);
        assert_eq!(styled("- a\n    1. nested", 1), vec![s("1.", LIST)]);
        assert_eq!(styled("> quoted", 0), vec![s("> ", QUOTE | MARKER), s("quoted", QUOTE)]);
        assert_eq!(styled("> a\n> > b", 1), vec![s("> > ", QUOTE | MARKER), s("b", QUOTE)]);
    }

    #[test]
    fn code_blocks_tables_rules_frontmatter() {
        let source = "---\ntitle: x\n---\n\n```rust\nfn x() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n";
        let doc = parse(source);
        assert_eq!(doc.block(0), Block::Frontmatter);
        assert_eq!(doc.block(2), Block::Frontmatter);
        assert_eq!(
            (doc.block(4), doc.block(5), doc.block(6)),
            (Block::Code, Block::Code, Block::Code)
        );
        assert_eq!(styled(source, 4), vec![s("```rust", MARKER)]);
        assert_eq!(styled(source, 5), vec![]);
        assert_eq!(styled(source, 6), vec![s("```", MARKER)]);
        assert_eq!((doc.block(8), doc.block(10)), (Block::Table, Block::Table));
        assert_eq!(styled(source, 9), vec![s("|---|---|", MARKER)]);
        assert_eq!(styled(source, 10)[0], s("|", MARKER));
        assert_eq!(doc.block(12), Block::Rule);
        assert_eq!(doc.fences, vec![4..7]);
        assert!(doc.in_fence(5) && !doc.in_fence(7));
    }

    #[test]
    fn math_and_footnotes() {
        assert_eq!(
            styled("$x^2$ and[^n]\n\n[^n]: note", 0),
            vec![
                s("$", MATH | MARKER),
                s("x^2", MATH),
                s("$", MATH | MARKER),
                s("[^n]", LINK)
            ]
        );
    }

    #[test]
    fn suggestions_override_strikethrough() {
        let source = "{~~old~>new~~}{>>id:s_0123abcd by:C<<} {++in++}{>>id:s_0123abce by:C<<}";
        assert_eq!(
            styled(source, 0),
            vec![
                s("{~~", MARKER),
                s("old", DEL),
                s("~>", MARKER),
                s("new", INS),
                s("~~}", MARKER),
                s("{>>id:s_0123abcd by:C<<}", COMMENT),
                s("{++", MARKER),
                s("in", INS),
                s("++}", MARKER),
                s("{>>id:s_0123abce by:C<<}", COMMENT),
            ]
        );
        assert_eq!(parse(source).suggestions.len(), 2);
    }

    #[test]
    fn counts_and_unicode() {
        let doc = parse("héllo wörld\n\n**😀** end");
        assert_eq!(doc.words, 4);
        assert_eq!(doc.line_count(), 3);
        assert_eq!(styled("héllo **wörld**", 0)[1], s("wörld", BOLD));
    }

    fn same(a: &Doc, b: &Doc, context: &str) {
        assert_eq!(a.lines.len(), b.lines.len(), "line count: {context}");
        for line in 0..a.lines.len() {
            assert_eq!(a.block(line), b.block(line), "block of line {line}: {context}");
            assert_eq!(a.spans(line), b.spans(line), "spans of line {line}: {context}");
        }
        assert_eq!(a.images, b.images, "images: {context}");
        assert_eq!(a.code_blocks, b.code_blocks, "code blocks: {context}");
        assert_eq!(a.suggestions, b.suggestions, "suggestions: {context}");
        assert_eq!(a.fences, b.fences, "fences: {context}");
        assert_eq!((a.words, a.chars), (b.words, b.chars), "counts: {context}");
        assert_eq!(a.line_starts, b.line_starts, "line starts: {context}");
        assert_eq!(a.tops, b.tops, "block starts: {context}");
        assert_eq!(a.defs, b.defs, "definitions: {context}");
        assert_eq!(a.def_text, b.def_text, "restated definitions: {context}");
        assert_eq!(a.loose, b.loose, "loose openers: {context}");
    }

    #[test]
    fn an_edit_reanalyses_only_its_blocks() {
        let source = "# One\n\npara one\nstill one\n\n- a\n- b\n\n```rust\nfn x() {}\n```\n\npara two {++in++}{>>id:s_0123abcd by:C<<}\n\n![i](p.png)\n\nlast\n";
        let doc = parse(source);
        let edited = source.replace("still one", "still *one*");
        let next = reanalyse(doc, &edited);
        assert!(next.incremental);
        same(&next, &parse(&edited), "inline edit");
        // An unclosed fence swallows what follows: no clean boundary, so everything is redone.
        let fenced = edited.replace("para one", "```\npara one");
        let next = reanalyse(next, &fenced);
        assert!(!next.incremental);
        same(&next, &parse(&fenced), "opened fence");
        // A new definition restyles references anywhere, so everything is redone.
        let linked = format!("{edited}\n[ref]: http://x\n\n[^n]: note\n");
        let next = reanalyse(parse(&edited), &linked);
        assert!(!next.incremental);
        // With definitions in place, a reference typed far from them resolves.
        let referring = linked.replace("# One", "# One [ref] and[^n]");
        let next = reanalyse(next, &referring);
        assert!(next.incremental);
        assert_eq!(
            next.spans(0)
                .iter()
                .filter(|span| span.style & style::LINK != 0)
                .count(),
            2
        );
        same(&next, &parse(&referring), "reference far from its definition");
    }

    #[test]
    fn incremental_analysis_always_matches_a_full_one() {
        const BLOCKS: &[&str] = &[
            "# Heading",
            "## Sub *heading*",
            "Setext\n===",
            "plain paragraph with **bold** and `code`",
            "two line\nparagraph [link](http://x) end",
            "- item\n- item two\n    1. nested\n    2. more",
            "- [ ] task\n- [x] done",
            "> quote\n> more quote",
            "> - quoted list\n> ```py\n> x = 1\n> ```",
            "```rust\nfn main() {}\n```",
            "~~~\nplain fence\n~~~",
            "    indented code",
            "| a | b |\n|---|---|\n| 1 | 2 |",
            "---",
            "***",
            "<div>\nhtml block\n</div>",
            "![img](pic.png)",
            "text ![inline](a.png) more ![two](b.jpg)",
            "$$\nx^2\n$$",
            "inline $math$ here",
            "{++added++}{>>id:s_0123abcd by:A<<} text",
            "a {--gone--}{>>id:s_0123abce by:A<<} b",
            "{~~old~>new~~}{>>id:s_0123abcf by:B<<}",
            "{>>a comment<<} and text",
            "term\n: definition",
            "1. one\n2. two\n\n   continued",
            "héllo wörld 😀 ünïcode",
            "[[wiki link]] and <http://auto.link>",
            "~~struck~~ text",
            "",
            "[ref]: http://example.com \"title\"",
            "see [ref] and [other][ref] and[^n]",
            "[^n]: a footnote\n    continued",
        ];
        const SCRAPS: &[&str] = &[
            "x",
            " ",
            "\n",
            "\n\n",
            "#",
            "# ",
            ">",
            "> ",
            "- ",
            "```",
            "```\n",
            "~~~\n",
            "*",
            "**",
            "`",
            "|",
            "=",
            "===\n",
            "---\n",
            "{++",
            "++}",
            "{--",
            "--}",
            "{>>",
            "<<}",
            "{>>id:s_0123abcd by:Z<<}",
            "~>",
            "<div>",
            "[",
            "](",
            "![a](b.png)",
            "    ",
            "\t",
            "é",
            "😀",
            "$",
            "$$\n",
            "1. ",
            "word word",
            "\n# New heading\n",
            "\n- new item\n",
            "[ref]",
            "[^n]",
            "\n[ref]: http://y\n",
            "\n[^n]: note\n",
            "]:",
            "\n```js\nlet a = 1;\n```\n",
        ];
        let mut seed = std::env::var("STET_FUZZ_SEED")
            .ok()
            .and_then(|seed| seed.parse().ok())
            .unwrap_or(0x2545_f491_4f6c_dd1du64);
        let mut random = |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound.max(1) as u64) as usize
        };
        let mut taken = 0;
        let mut rounds = 0;
        // STET_FUZZ=5000 runs a long soak.
        let documents = std::env::var("STET_FUZZ")
            .ok()
            .and_then(|count| count.parse().ok())
            .unwrap_or(60);
        for document in 0..documents {
            let mut text = String::new();
            for _ in 0..3 + random(30) {
                text.push_str(BLOCKS[random(BLOCKS.len())]);
                text.push_str(if random(5) == 0 { "\n" } else { "\n\n" });
            }
            if document % 3 == 0 {
                text.insert_str(0, "---\ntitle: x\n---\n\n");
            }
            let mut doc = parse(&text);
            for _ in 0..60 {
                let boundaries: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
                let at = boundaries[random(boundaries.len())];
                let end = boundaries[(boundaries.partition_point(|&b| b < at) + random(6) * random(2) * random(8))
                    .min(boundaries.len() - 1)];
                let insert = match random(4) {
                    0 => "",
                    1 => BLOCKS[random(BLOCKS.len())],
                    _ => SCRAPS[random(SCRAPS.len())],
                };
                text.replace_range(at..end.max(at), insert);
                doc = reanalyse(doc, &text);
                taken += doc.incremental as usize;
                rounds += 1;
                same(
                    &doc,
                    &parse(&text),
                    &format!("document {document} after editing at {at}: {text:?}"),
                );
            }
        }
        assert!(
            taken * 4 > rounds,
            "the incremental path ran only {taken} of {rounds} times"
        );
    }
}
