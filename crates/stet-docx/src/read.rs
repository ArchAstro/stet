//! A `.docx` as Markdown: the body's paragraphs, lists, tables, links and
//! pictures, tracked changes as CriticMarkup suggestions, and comments with
//! the words they are about.

use crate::{Comment, Imported, KeepPicture};
use quick_xml::events::Event;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use stet_core::critic::{Kind, compose_checked, is_valid_author};

const FALLBACK_AUTHOR: &str = "Google Docs";

const MONOSPACE: &[&str] = &[
    "courier",
    "consolas",
    "mono",
    "menlo",
    "monaco",
    "source code",
    "inconsolata",
    "lucida console",
    "fira code",
];

// --- A small element tree ----------------------------------------------------------

#[derive(Default)]
struct Node {
    /// The qualified name, `w:p`.
    name: String,
    attrs: Vec<(String, String)>,
    kids: Vec<Kid>,
}

enum Kid {
    Node(Node),
    Text(String),
}

fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

impl Node {
    fn is(&self, name: &str) -> bool {
        local(&self.name) == name
    }

    /// An attribute by qualified name (`r:id`), or by local name (`val`).
    fn attr(&self, name: &str) -> Option<&str> {
        let qualified = name.contains(':');
        self.attrs
            .iter()
            .find(|(key, _)| if qualified { key == name } else { local(key) == name })
            .map(|(_, value)| value.as_str())
    }

    fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.kids.iter().filter_map(|kid| match kid {
            Kid::Node(node) => Some(node),
            Kid::Text(_) => None,
        })
    }

    fn child(&self, name: &str) -> Option<&Node> {
        self.nodes().find(|node| node.is(name))
    }

    /// The first descendant named `name`, depth first.
    fn find(&self, name: &str) -> Option<&Node> {
        self.nodes()
            .find_map(|node| if node.is(name) { Some(node) } else { node.find(name) })
    }

    /// The `val` of child `name`.
    fn val(&self, name: &str) -> Option<&str> {
        self.child(name)?.attr("val")
    }

    /// All text inside, wherever it is.
    fn text(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        for kid in &self.kids {
            match kid {
                Kid::Text(text) => out.push_str(text),
                Kid::Node(node) => node.collect_text(out),
            }
        }
    }
}

fn parse(xml: &[u8]) -> Result<Node, String> {
    let xml = std::str::from_utf8(xml).map_err(|err| err.to_string())?;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut stack = vec![Node::default()];
    let element = |start: &quick_xml::events::BytesStart| -> Result<Node, String> {
        let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
        let mut attrs = Vec::new();
        for attr in start.attributes().with_checks(false) {
            let attr = attr.map_err(|err| err.to_string())?;
            let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|err| err.to_string())?;
            attrs.push((key, value.into_owned()));
        }
        Ok(Node {
            name,
            attrs,
            kids: Vec::new(),
        })
    };
    let push_text = |stack: &mut Vec<Node>, text: &str| {
        let top = stack.last_mut().expect("root");
        match top.kids.last_mut() {
            Some(Kid::Text(last)) => last.push_str(text),
            _ => top.kids.push(Kid::Text(text.to_string())),
        }
    };
    loop {
        match reader.read_event().map_err(|err| err.to_string())? {
            Event::Start(start) => stack.push(element(&start)?),
            Event::Empty(start) => {
                let node = element(&start)?;
                stack.last_mut().expect("root").kids.push(Kid::Node(node));
            }
            Event::End(_) if stack.len() > 1 => {
                let node = stack.pop().expect("open element");
                stack.last_mut().expect("root").kids.push(Kid::Node(node));
            }
            Event::Text(text) => push_text(&mut stack, &text.decode().map_err(|err| err.to_string())?),
            Event::CData(data) => push_text(&mut stack, &data.decode().map_err(|err| err.to_string())?),
            Event::GeneralRef(entity) => {
                let name = entity.decode().map_err(|err| err.to_string())?;
                let resolved = match entity.resolve_char_ref().map_err(|err| err.to_string())? {
                    Some(c) => c.to_string(),
                    None => quick_xml::escape::resolve_predefined_entity(&name)
                        .unwrap_or_default()
                        .to_string(),
                };
                push_text(&mut stack, &resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    while stack.len() > 1 {
        let node = stack.pop().expect("open element");
        stack.last_mut().expect("root").kids.push(Kid::Node(node));
    }
    Ok(stack.pop().expect("root"))
}

// --- Inline text -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Format {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
}

enum Span {
    Text(String, Format),
    /// Markdown already: a picture, a link, a suggestion.
    Raw(String),
}

/// Backslashes before what would otherwise be read as markup.
fn escape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (at, &c) in chars.iter().enumerate() {
        let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
        let escaped = match c {
            '\\' | '*' | '`' | '[' | ']' => true,
            // snake_case stays as it is: `_` inside a word is never emphasis.
            '_' => !(word(at.checked_sub(1).and_then(|i| chars.get(i))) && word(chars.get(at + 1))),
            _ => false,
        };
        if escaped {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `code` between enough backticks.
fn code_span(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

/// Spans as Markdown, runs of one format merged so emphasis is not split.
fn render(spans: &[Span]) -> String {
    let mut merged: Vec<Span> = Vec::new();
    for span in spans {
        match (merged.last_mut(), span) {
            (Some(Span::Text(text, last)), Span::Text(more, format)) if last == format => text.push_str(more),
            (_, Span::Text(text, format)) => merged.push(Span::Text(text.clone(), *format)),
            (_, Span::Raw(raw)) => merged.push(Span::Raw(raw.clone())),
        }
    }
    let mut out = String::new();
    for span in &merged {
        match span {
            Span::Raw(raw) => out.push_str(raw),
            Span::Text(text, format) => {
                let core = text.trim();
                if core.is_empty() || *format == Format::default() {
                    out.push_str(&if format.code { text.clone() } else { escape(text) });
                    continue;
                }
                let lead = &text[..text.len() - text.trim_start().len()];
                let trail = &text[text.trim_end().len()..];
                let body = if format.code { code_span(core) } else { escape(core) };
                // Right after a closing `*`, another `*` would read as one run.
                let under = out.ends_with('*');
                let mut open = String::new();
                if format.bold {
                    open.push_str(if under { "__" } else { "**" });
                }
                if format.italic {
                    open.push(if under { '_' } else { '*' });
                }
                if format.strike {
                    open.push_str("~~");
                }
                let close: String = open.chars().rev().collect();
                out.push_str(lead);
                out.push_str(&open);
                out.push_str(&body);
                out.push_str(&close);
                out.push_str(trail);
            }
        }
    }
    out
}

/// Escapes what would make a paragraph's first characters a heading, list
/// item or quote.
fn escape_line_start(line: &str) -> String {
    if line.starts_with(['#', '>']) || line.starts_with("- ") || line.starts_with("+ ") {
        return format!("\\{line}");
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") ")) {
        return format!("{}\\{}", &line[..digits], &line[digits..]);
    }
    line.to_string()
}

fn link(text: &str, url: &str) -> String {
    let url = if url.contains([' ', '(', ')', '<', '>']) {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    } else {
        url.to_string()
    };
    format!("[{text}]({url})")
}

// --- Blocks ------------------------------------------------------------------------

enum Block {
    Para(String),
    Item { indent: usize, line: String },
    Code(String),
    Table(String),
}

fn join(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut at = 0;
    while at < blocks.len() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        match &blocks[at] {
            Block::Para(text) | Block::Table(text) => {
                out.push_str(text);
                at += 1;
            }
            Block::Item { .. } => {
                let mut lines = Vec::new();
                while let Some(Block::Item { indent, line }) = blocks.get(at) {
                    lines.push(format!("{}{line}", " ".repeat(*indent)));
                    at += 1;
                }
                out.push_str(&lines.join("\n"));
            }
            Block::Code(_) => {
                let mut lines = Vec::new();
                while let Some(Block::Code(line)) = blocks.get(at) {
                    lines.push(line.as_str());
                    at += 1;
                }
                let code = lines.join("\n");
                let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                let fence = "`".repeat(longest.max(2) + 1);
                out.push_str(&format!("{fence}\n{code}\n{fence}"));
            }
        }
    }
    out
}

// --- The document ------------------------------------------------------------------

struct Reader<'k, 'a> {
    files: HashMap<String, Vec<u8>>,
    /// Relationship ID to the part or address it names.
    rels: HashMap<String, String>,
    /// Style ID to its name, lowercased.
    styles: HashMap<String, String>,
    /// List and level to whether it counts (rather than bullets).
    ordered: HashMap<(String, String), bool>,
    keep: &'k mut (dyn FnMut(&[u8], &str) -> Option<String> + 'a),
    ids: HashSet<String>,
    /// Comments open at this point, with the words seen since each began.
    open: Vec<String>,
    anchors: HashMap<String, String>,
    /// Comment IDs in the order they first appear.
    order: Vec<String>,
    /// Indents of the list levels above the current one.
    levels: Vec<usize>,
}

/// The ID a suggestion gets: `s_` and eight base-36 digits, from its place
/// and text, so the same document reads the same way twice.
fn suggestion_id(seed: &str, taken: &mut HashSet<String>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3);
    }
    loop {
        let mut n = hash % 36u64.pow(8);
        let mut digits = [b'0'; 8];
        for digit in digits.iter_mut().rev() {
            *digit = b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize];
            n /= 36;
        }
        let id = format!("s_{}", std::str::from_utf8(&digits).expect("ascii"));
        if taken.insert(id.clone()) {
            return id;
        }
        hash = hash.wrapping_mul(0x100_0000_01b3).wrapping_add(1);
    }
}

fn author_of(node: &Node) -> String {
    let author = node.attr("w:author").unwrap_or("").trim();
    if is_valid_author(author) {
        author.to_string()
    } else {
        FALLBACK_AUTHOR.to_string()
    }
}

/// Where a relationship target lives in the package.
fn part_path(target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(absolute) = target.strip_prefix('/') {
        absolute.split('/').collect()
    } else {
        "word".split('/').chain(target.split('/')).collect()
    };
    let mut out: Vec<&str> = Vec::new();
    for part in parts.drain(..) {
        match part {
            ".." => {
                out.pop();
            }
            "." | "" => {}
            part => out.push(part),
        }
    }
    out.join("/")
}

impl Reader<'_, '_> {
    fn new_text(&mut self, text: &str) {
        for id in &self.open {
            self.anchors.entry(id.clone()).or_default().push_str(text);
        }
    }

    fn format(&self, run: &Node) -> Format {
        let Some(props) = run.child("rPr") else {
            return Format::default();
        };
        let on = |name: &str| {
            props
                .child(name)
                .is_some_and(|node| !matches!(node.attr("val"), Some("0" | "false" | "off" | "none")))
        };
        let font = props
            .child("rFonts")
            .and_then(|fonts| fonts.attr("ascii").or(fonts.attr("hAnsi")))
            .unwrap_or("")
            .to_ascii_lowercase();
        let style = props
            .val("rStyle")
            .and_then(|id| self.styles.get(id))
            .map_or("", String::as_str);
        Format {
            bold: on("b"),
            italic: on("i"),
            strike: on("strike") || on("dstrike"),
            code: MONOSPACE.iter().any(|mono| font.contains(mono)) || style.contains("code"),
        }
    }

    /// A picture in a run, as Markdown, once `keep` has saved it.
    fn picture(&mut self, node: &Node) -> Option<String> {
        let id = node
            .find("blip")
            .and_then(|blip| blip.attr("r:embed"))
            .or_else(|| node.find("imagedata").and_then(|data| data.attr("r:id")))?;
        let path = part_path(self.rels.get(id)?);
        let bytes = self.files.get(&path)?;
        let ext = path.rsplit_once('.').map_or("png", |(_, ext)| ext).to_ascii_lowercase();
        let dest = (self.keep)(bytes, &ext)?;
        let alt = node
            .find("docPr")
            .and_then(|pr| pr.attr("descr").filter(|alt| !alt.is_empty()).or(pr.attr("title")))
            .unwrap_or("");
        let alt = alt.replace(['\n', '\r'], " ").replace('[', "\\[").replace(']', "\\]");
        Some(format!("![{alt}]({})", dest.replace(' ', "%20")))
    }

    /// One run's text and pictures.
    fn run(&mut self, run: &Node, spans: &mut Vec<Span>) {
        let format = self.format(run);
        for node in run.nodes() {
            let text = match local(&node.name) {
                "t" => node.text(),
                "tab" | "ptab" => " ".to_string(),
                "br" | "cr" => "\n".to_string(),
                "noBreakHyphen" => "-".to_string(),
                "drawing" | "pict" | "object" => {
                    if let Some(picture) = self.picture(node) {
                        spans.push(Span::Raw(picture));
                    }
                    continue;
                }
                "commentReference" => {
                    self.seen(node);
                    continue;
                }
                _ => continue,
            };
            self.new_text(&text);
            spans.push(Span::Text(text, format));
        }
    }

    fn seen(&mut self, node: &Node) {
        if let Some(id) = node.attr("w:id").or(node.attr("id"))
            && !self.order.iter().any(|seen| seen == id)
        {
            self.order.push(id.to_string());
        }
    }

    /// The plain text a tracked change holds: inserted or deleted.
    fn change_text(node: &Node, out: &mut String) {
        for kid in node.nodes() {
            match local(&kid.name) {
                "t" | "delText" => out.push_str(&kid.text()),
                "tab" | "br" | "cr" => out.push(' '),
                "noBreakHyphen" => out.push('-'),
                "rPr" | "pPr" | "instrText" | "delInstrText" => {}
                _ => Self::change_text(kid, out),
            }
        }
    }

    /// The runs of a paragraph, or of something inside one (a link, a
    /// content control), as spans.
    fn inline(&mut self, parent: &Node, spans: &mut Vec<Span>) {
        let kids: Vec<&Node> = parent.nodes().collect();
        let mut at = 0;
        while at < kids.len() {
            let node = kids[at];
            at += 1;
            match local(&node.name) {
                "r" => self.run(node, spans),
                "ins" | "del" | "moveFrom" | "moveTo" => {
                    // Consecutive changes by one author are one suggestion:
                    // deletions, then the insertions that replace them.
                    let author = author_of(node);
                    let same =
                        |node: &Node, names: &[&str]| names.contains(&local(&node.name)) && author_of(node) == author;
                    let (mut old, mut new) = (String::new(), String::new());
                    at -= 1;
                    while at < kids.len() && same(kids[at], &["del", "moveFrom"]) {
                        Self::change_text(kids[at], &mut old);
                        at += 1;
                    }
                    while at < kids.len() && same(kids[at], &["ins", "moveTo"]) {
                        Self::change_text(kids[at], &mut new);
                        // Comments and pictures inside an insertion still count.
                        for inner in kids[at].nodes() {
                            match local(&inner.name) {
                                "commentRangeStart" => self.open_comment(inner),
                                "commentRangeEnd" => self.close_comment(inner),
                                _ => {}
                            }
                        }
                        at += 1;
                    }
                    self.suggestion(&old, &new, &author, spans);
                }
                "hyperlink" => {
                    let mut inner = Vec::new();
                    self.inline(node, &mut inner);
                    let text = render(&inner);
                    match node.attr("r:id").and_then(|id| self.rels.get(id)) {
                        Some(url) if !text.trim().is_empty() => spans.push(Span::Raw(link(&text, url))),
                        _ => spans.push(Span::Raw(text)),
                    }
                }
                "commentRangeStart" => self.open_comment(node),
                "commentRangeEnd" => self.close_comment(node),
                "sdt" => {
                    if let Some(content) = node.child("sdtContent") {
                        self.inline(content, spans);
                    }
                }
                "smartTag" | "fldSimple" | "customXml" | "dir" | "bdo" => self.inline(node, spans),
                _ => {}
            }
        }
    }

    fn open_comment(&mut self, node: &Node) {
        if let Some(id) = node.attr("w:id").or(node.attr("id")) {
            self.seen(node);
            self.open.push(id.to_string());
            self.anchors.entry(id.to_string()).or_default();
        }
    }

    fn close_comment(&mut self, node: &Node) {
        if let Some(id) = node.attr("w:id").or(node.attr("id")) {
            self.open.retain(|open| open != id);
        }
    }

    fn suggestion(&mut self, old: &str, new: &str, author: &str, spans: &mut Vec<Span>) {
        let kind = match (old.is_empty(), new.is_empty()) {
            (true, true) => return,
            (true, false) => Kind::Insert,
            (false, true) => Kind::Delete,
            (false, false) => Kind::Replace,
        };
        self.new_text(new);
        let (old_md, new_md) = (escape(old), escape(new));
        let seed = format!("{}|{author}|{old}|{new}", self.ids.len());
        let id = suggestion_id(&seed, &mut self.ids);
        match compose_checked(kind, &old_md, &new_md, &id, author) {
            Some(markup) => spans.push(Span::Raw(markup)),
            // Text the markup cannot hold: take the change as made.
            None => spans.push(Span::Text(new.to_string(), Format::default())),
        }
    }

    fn heading_level(&self, style: &str) -> Option<usize> {
        let name = self
            .styles
            .get(style)
            .cloned()
            .unwrap_or_else(|| style.to_ascii_lowercase());
        let name = name.replace(' ', "");
        if name == "title" {
            return Some(1);
        }
        let level: usize = name.strip_prefix("heading")?.parse().ok()?;
        (1..=6).contains(&level).then_some(level)
    }

    fn paragraph(&mut self, p: &Node, blocks: &mut Vec<Block>) {
        let mut spans = Vec::new();
        self.inline(p, &mut spans);
        // A paragraph's end is a space in an anchor that runs past it.
        self.new_text(" ");
        let props = p.child("pPr");
        let style = props.and_then(|props| props.val("pStyle")).unwrap_or("");
        let style_name = self.styles.get(style).cloned().unwrap_or_default();
        let numbered = props.and_then(|props| props.child("numPr")).and_then(|num| {
            let id = num.val("numId")?;
            (id != "0").then(|| (id.to_string(), num.val("ilvl").unwrap_or("0").to_string()))
        });
        let text = render(&spans).trim().to_string();
        if text.is_empty() {
            return;
        }
        let plain_start = matches!(spans.iter().find(|span| !matches!(span, Span::Text(t, _) if t.trim().is_empty())),
            Some(Span::Text(_, format)) if *format == Format::default());
        let line = if plain_start {
            escape_line_start(&text)
        } else {
            text.clone()
        };
        if let Some(level) = self.heading_level(style) {
            self.levels.clear();
            blocks.push(Block::Para(format!("{} {text}", "#".repeat(level))));
        } else if let Some((id, level)) = numbered {
            let ordered = self.ordered.get(&(id, level.clone())).copied().unwrap_or(false);
            let level: usize = level.parse().unwrap_or(0);
            self.levels.truncate(level);
            while self.levels.len() < level {
                self.levels.push(2);
            }
            let indent: usize = self.levels.iter().sum();
            let marker = if ordered { "1. " } else { "- " };
            self.levels.push(marker.len());
            blocks.push(Block::Item {
                indent,
                line: format!("{marker}{text}"),
            });
        } else if spans.iter().all(|span| match span {
            Span::Text(text, format) => format.code || text.trim().is_empty(),
            Span::Raw(_) => false,
        }) {
            self.levels.clear();
            let code: String = spans
                .iter()
                .map(|span| match span {
                    Span::Text(text, _) => text.as_str(),
                    Span::Raw(raw) => raw.as_str(),
                })
                .collect();
            blocks.push(Block::Code(code.trim_end().to_string()));
        } else if style_name.contains("quote") {
            self.levels.clear();
            blocks.push(Block::Para(format!("> {text}")));
        } else if style_name == "subtitle" {
            self.levels.clear();
            blocks.push(Block::Para(format!("*{text}*")));
        } else {
            self.levels.clear();
            blocks.push(Block::Para(line));
        }
    }

    fn table(&mut self, table: &Node, blocks: &mut Vec<Block>) {
        self.levels.clear();
        let mut rows: Vec<Vec<String>> = Vec::new();
        for row in table.nodes().filter(|node| node.is("tr")) {
            let mut cells = Vec::new();
            for cell in row.nodes().filter(|node| node.is("tc")) {
                let mut parts = Vec::new();
                for p in cell.nodes().filter(|node| node.is("p")) {
                    let mut spans = Vec::new();
                    self.inline(p, &mut spans);
                    self.new_text(" ");
                    let text = render(&spans).trim().replace('\n', " ");
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                cells.push(parts.join(" ").replace('|', "\\|"));
            }
            rows.push(cells);
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return;
        }
        let line = |cells: &[String]| {
            let mut cells = cells.to_vec();
            cells.resize(width, String::new());
            format!("| {} |", cells.join(" | "))
        };
        // A header row is bold already; Word files often say so twice.
        for cell in &mut rows[0] {
            if let Some(inner) = cell.strip_prefix("**").and_then(|cell| cell.strip_suffix("**"))
                && !inner.is_empty()
                && !inner.contains("**")
            {
                *cell = inner.to_string();
            }
        }
        let mut lines = vec![line(&rows[0]), line(&vec!["---".to_string(); width])];
        lines.extend(rows[1..].iter().map(|row| line(row)));
        blocks.push(Block::Table(lines.join("\n")));
    }

    fn body(&mut self, parent: &Node, blocks: &mut Vec<Block>) {
        for node in parent.nodes() {
            match local(&node.name) {
                "p" => self.paragraph(node, blocks),
                "tbl" => self.table(node, blocks),
                "sdt" => {
                    if let Some(content) = node.child("sdtContent") {
                        self.body(content, blocks);
                    }
                }
                "customXml" => self.body(node, blocks),
                _ => {}
            }
        }
    }
}

fn styles(xml: &Node) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(root) = xml.child("styles") {
        for style in root.nodes().filter(|node| node.is("style")) {
            if let (Some(id), Some(name)) = (style.attr("styleId"), style.val("name")) {
                out.insert(id.to_string(), name.to_ascii_lowercase());
            }
        }
    }
    out
}

fn numbering(xml: &Node) -> HashMap<(String, String), bool> {
    let mut out = HashMap::new();
    let Some(root) = xml.child("numbering") else {
        return out;
    };
    let mut abstracts: HashMap<&str, Vec<(String, bool)>> = HashMap::new();
    for abs in root.nodes().filter(|node| node.is("abstractNum")) {
        let Some(id) = abs.attr("abstractNumId") else { continue };
        let levels = abs
            .nodes()
            .filter(|node| node.is("lvl"))
            .map(|lvl| {
                let format = lvl.val("numFmt").unwrap_or("bullet");
                (
                    lvl.attr("ilvl").unwrap_or("0").to_string(),
                    !matches!(format, "bullet" | "none"),
                )
            })
            .collect();
        abstracts.insert(id, levels);
    }
    for num in root.nodes().filter(|node| node.is("num")) {
        let (Some(id), Some(abs)) = (num.attr("numId"), num.val("abstractNumId")) else {
            continue;
        };
        for (level, ordered) in abstracts.get(abs).into_iter().flatten() {
            out.insert((id.to_string(), level.clone()), *ordered);
        }
    }
    out
}

fn relationships(xml: &Node) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(root) = xml.child("Relationships") {
        for rel in root.nodes().filter(|node| node.is("Relationship")) {
            if let (Some(id), Some(target)) = (rel.attr("Id"), rel.attr("Target")) {
                out.insert(id.to_string(), target.to_string());
            }
        }
    }
    out
}

/// Comments by ID, replies folded into the comment they answer.
fn comments(
    files: &HashMap<String, Vec<u8>>,
    order: &[String],
    anchors: &HashMap<String, String>,
) -> Result<Vec<Comment>, String> {
    let Some(xml) = files.get("word/comments.xml") else {
        return Ok(Vec::new());
    };
    let root = parse(xml)?;
    let Some(list) = root.child("comments") else {
        return Ok(Vec::new());
    };
    // Each comment, and the paraId of its last paragraph (how replies point).
    let mut all: Vec<(String, Comment, Option<String>)> = Vec::new();
    for comment in list.nodes().filter(|node| node.is("comment")) {
        let Some(id) = comment.attr("w:id").or(comment.attr("id")) else {
            continue;
        };
        let paragraphs: Vec<&Node> = comment.nodes().filter(|node| node.is("p")).collect();
        let text = paragraphs
            .iter()
            .map(|p| {
                let mut out = String::new();
                Reader::change_text(p, &mut out);
                out.trim().to_string()
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let para = paragraphs.last().and_then(|p| p.attr("w14:paraId")).map(str::to_string);
        let author = comment.attr("w:author").unwrap_or("").trim().to_string();
        all.push((
            id.to_string(),
            Comment {
                author,
                text,
                anchor: anchors
                    .get(id)
                    .map(|a| a.split_whitespace().collect::<Vec<_>>().join(" "))
                    .unwrap_or_default(),
            },
            para,
        ));
    }
    let mut parent_of: HashMap<String, String> = HashMap::new();
    if let Some(xml) = files.get("word/commentsExtended.xml") {
        let root = parse(xml)?;
        if let Some(list) = root.child("commentsEx") {
            for ex in list.nodes().filter(|node| node.is("commentEx")) {
                if let (Some(para), Some(parent)) = (ex.attr("paraId"), ex.attr("paraIdParent")) {
                    parent_of.insert(para.to_string(), parent.to_string());
                }
            }
        }
    }
    let by_para: HashMap<String, usize> = all
        .iter()
        .enumerate()
        .filter_map(|(at, (_, _, para))| Some((para.clone()?, at)))
        .collect();
    let mut replies: Vec<(usize, usize)> = Vec::new();
    for (at, (_, _, para)) in all.iter().enumerate() {
        if let Some(parent) = para
            .as_ref()
            .and_then(|para| parent_of.get(para))
            .and_then(|p| by_para.get(p))
            && *parent != at
        {
            replies.push((*parent, at));
        }
    }
    let folded: HashSet<usize> = replies.iter().map(|(_, reply)| *reply).collect();
    for (parent, reply) in replies {
        let (author, text) = (all[reply].1.author.clone(), all[reply].1.text.clone());
        let line = if author.is_empty() {
            text
        } else {
            format!("{author}: {text}")
        };
        all[parent].1.text.push_str(&format!("\n\n{line}"));
    }
    let position = |id: &str| order.iter().position(|seen| seen == id).unwrap_or(usize::MAX);
    let mut kept: Vec<(usize, usize, Comment)> = all
        .into_iter()
        .enumerate()
        .filter(|(at, _)| !folded.contains(at))
        .map(|(at, (id, comment, _))| (position(&id), at, comment))
        .collect();
    kept.sort_by_key(|(position, at, _)| (*position, *at));
    Ok(kept.into_iter().map(|(_, _, comment)| comment).collect())
}

/// Reads a `.docx`. `keep` saves each picture and says where it went.
pub fn read(docx: &[u8], keep: KeepPicture) -> Result<Imported, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(docx)).map_err(|err| format!("not a .docx: {err}"))?;
    let mut files = HashMap::new();
    for at in 0..archive.len() {
        let mut file = archive.by_index(at).map_err(|err| err.to_string())?;
        let name = file.name().map_err(|err| err.to_string())?.into_owned();
        let wanted = name.starts_with("word/")
            && (name.ends_with(".xml") || name.ends_with(".rels") || name.starts_with("word/media/"));
        if wanted && file.is_file() {
            let mut bytes = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut bytes).map_err(|err| format!("{name}: {err}"))?;
            files.insert(name, bytes);
        }
    }
    let document = parse(
        files
            .get("word/document.xml")
            .ok_or("not a .docx: no word/document.xml")?,
    )?;
    let part = |name: &str| files.get(name).map(|xml| parse(xml)).transpose();
    let rels = part("word/_rels/document.xml.rels")?
        .map(|xml| relationships(&xml))
        .unwrap_or_default();
    let styles = part("word/styles.xml")?.map(|xml| styles(&xml)).unwrap_or_default();
    let ordered = part("word/numbering.xml")?
        .map(|xml| numbering(&xml))
        .unwrap_or_default();
    let mut reader = Reader {
        files,
        rels,
        styles,
        ordered,
        keep,
        ids: HashSet::new(),
        open: Vec::new(),
        anchors: HashMap::new(),
        order: Vec::new(),
        levels: Vec::new(),
    };
    let body = document
        .child("document")
        .and_then(|doc| doc.child("body"))
        .ok_or("not a .docx: the document has no body")?;
    let mut blocks = Vec::new();
    reader.body(body, &mut blocks);
    let mut markdown = join(&blocks);
    if !markdown.is_empty() {
        markdown.push('\n');
    }
    let comments = comments(&reader.files, &reader.order, &reader.anchors)?;
    Ok(Imported { markdown, comments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use stet_core::critic::parse_suggestions;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#;

    fn docx(body: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}<w:sectPr/></w:body></w:document>"#
        );
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        for (name, bytes) in extra {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn markdown(body: &str, extra: &[(&str, &[u8])]) -> Imported {
        read(&docx(body, extra), &mut |_, _| None).unwrap()
    }

    const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
        <w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/></w:style>
        <w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/></w:style></w:styles>"#;

    const RELS: &str = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/a?b=1&amp;c=2" TargetMode="External"/>
        <Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#;

    fn p(inner: &str) -> String {
        format!("<w:p>{inner}</w:p>")
    }

    fn r(text: &str) -> String {
        format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#)
    }

    #[test]
    fn headings_emphasis_and_links() {
        let body = [
            r#"<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Plan</w:t></w:r></w:p>"#.to_string(),
            r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Why</w:t></w:r></w:p>"#.to_string(),
            p(&[
                r("Some "),
                r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">bold </w:t></w:r>"#.to_string(),
                r#"<w:r><w:rPr><w:b w:val="1"/></w:rPr><w:t>words</w:t></w:r>"#.to_string(),
                r(", "),
                r#"<w:r><w:rPr><w:i/></w:rPr><w:t>slant</w:t></w:r>"#.to_string(),
                r(", "),
                r#"<w:r><w:rPr><w:b w:val="0"/><w:rFonts w:ascii="Courier New"/></w:rPr><w:t>x*y</w:t></w:r>"#
                    .to_string(),
                r(" and a "),
                r#"<w:hyperlink r:id="rId5"><w:r><w:rPr><w:u/></w:rPr><w:t>link</w:t></w:r></w:hyperlink>"#.to_string(),
                r(". 2*3 snake_case _x_"),
            ]
            .concat()),
            p(&r("# not a heading")),
            p(""),
        ]
        .concat();
        let got = markdown(
            &body,
            &[
                ("word/styles.xml", STYLES.as_bytes()),
                ("word/_rels/document.xml.rels", RELS.as_bytes()),
            ],
        );
        assert_eq!(
            got.markdown,
            "# Plan\n\n## Why\n\nSome **bold words**, *slant*, `x*y` and a [link](https://example.com/a?b=1&c=2). 2\\*3 snake_case \\_x\\_\n\n\\# not a heading\n"
        );
    }

    #[test]
    fn lists_nest_and_tables_become_pipes() {
        let numbering = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
        let item = |level: u8, text: &str| {
            format!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="1"/></w:numPr></w:pPr>{}</w:p>"#,
                r(text)
            )
        };
        let cell = |text: &str| format!("<w:tc>{}</w:tc>", p(&r(text)));
        let body = [
            item(0, "one"),
            item(1, "first"),
            item(1, "second"),
            item(0, "two"),
            p(&r("after")),
            format!(
                "<w:tbl><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
                cell("Name"),
                cell("Score"),
                cell("a|b"),
                cell("3")
            ),
        ]
        .concat();
        let got = markdown(&body, &[("word/numbering.xml", numbering.as_bytes())]);
        assert_eq!(
            got.markdown,
            "- one\n  1. first\n  1. second\n- two\n\nafter\n\n| Name | Score |\n| --- | --- |\n| a\\|b | 3 |\n"
        );
    }

    #[test]
    fn tracked_changes_become_suggestions() {
        let ins = |author: &str, text: &str| {
            format!(
                r#"<w:ins w:id="1" w:author="{author}" w:date="2026-01-01T00:00:00Z">{}</w:ins>"#,
                r(text)
            )
        };
        let del = |author: &str, text: &str| {
            format!(
                r#"<w:del w:id="2" w:author="{author}"><w:r><w:delText xml:space="preserve">{text}</w:delText></w:r></w:del>"#
            )
        };
        let body = [
            p(&[r("The "), del("Ann Lee", "quick"), ins("Ann Lee", "slow"), r(" fox")].concat()),
            p(&[r("Add "), ins("Bo", "more"), r(" and drop "), del("Bo", "this"), r(".")].concat()),
            p(&[r("Mixed "), del("Ann Lee", "a"), ins("Bo", "b")].concat()),
        ]
        .concat();
        let got = markdown(&body, &[]);
        let suggestions = parse_suggestions(&got.markdown);
        let summary: Vec<(Kind, &str, &str, &str)> = suggestions
            .iter()
            .map(|s| (s.kind, s.author.as_str(), s.old_text.as_str(), s.new_text.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (Kind::Replace, "Ann Lee", "quick", "slow"),
                (Kind::Insert, "Bo", "", "more"),
                (Kind::Delete, "Bo", "this", ""),
                (Kind::Delete, "Ann Lee", "a", ""),
                (Kind::Insert, "Bo", "", "b"),
            ]
        );
        let ids: HashSet<&str> = suggestions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), 5);
        assert!(
            got.markdown.starts_with("The {~~quick~>slow~~}{>>id:s_"),
            "{}",
            got.markdown
        );
        // The same document reads the same way twice.
        assert_eq!(markdown(&body, &[]).markdown, got.markdown);
    }

    #[test]
    fn comments_keep_their_words_and_replies() {
        let comments = r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">
            <w:comment w:id="0" w:author="Ann"><w:p w14:paraId="AAAA0001"><w:r><w:t>Is this right?</w:t></w:r></w:p></w:comment>
            <w:comment w:id="1" w:author="Bo"><w:p w14:paraId="AAAA0002"><w:r><w:t>Yes.</w:t></w:r></w:p></w:comment>
            <w:comment w:id="2" w:author="Cy"><w:p w14:paraId="AAAA0003"><w:r><w:t>Later one</w:t></w:r></w:p></w:comment></w:comments>"#;
        let extended = r#"<w15:commentsEx xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml">
            <w15:commentEx w15:paraId="AAAA0001" w15:done="0"/><w15:commentEx w15:paraId="AAAA0002" w15:paraIdParent="AAAA0001" w15:done="0"/></w15:commentsEx>"#;
        let body = [
            p(&[
                r("Start "),
                r#"<w:commentRangeStart w:id="2"/>"#.to_string(),
                r("late"),
                r#"<w:commentRangeEnd w:id="2"/><w:r><w:commentReference w:id="2"/></w:r>"#.to_string(),
            ]
            .concat()),
            p(&[
                r#"<w:commentRangeStart w:id="0"/><w:commentRangeStart w:id="1"/>"#.to_string(),
                r("the  claim"),
                r#"<w:commentRangeEnd w:id="0"/><w:commentRangeEnd w:id="1"/>"#.to_string(),
                r(" stands"),
            ]
            .concat()),
        ]
        .concat();
        let got = markdown(
            &body,
            &[
                ("word/comments.xml", comments.as_bytes()),
                ("word/commentsExtended.xml", extended.as_bytes()),
            ],
        );
        assert_eq!(got.markdown, "Start late\n\nthe  claim stands\n");
        assert_eq!(
            got.comments,
            [
                Comment {
                    author: "Cy".into(),
                    text: "Later one".into(),
                    anchor: "late".into()
                },
                Comment {
                    author: "Ann".into(),
                    text: "Is this right?\n\nBo: Yes.".into(),
                    anchor: "the claim".into()
                },
            ]
        );
    }

    #[test]
    fn pictures_go_to_keep() {
        let drawing = r#"<w:r><w:drawing><wp:inline><wp:docPr id="1" name="Picture 1" descr="A chart"/><a:graphic><a:graphicData><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="rId6"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#;
        let bytes = docx(
            &p(drawing),
            &[
                ("word/_rels/document.xml.rels", RELS.as_bytes()),
                ("word/media/image1.png", b"PNGDATA"),
            ],
        );
        let mut kept = Vec::new();
        let got = read(&bytes, &mut |bytes, ext| {
            kept.push((bytes.to_vec(), ext.to_string()));
            Some("assets/doc 1.png".into())
        })
        .unwrap();
        assert_eq!(kept, [(b"PNGDATA".to_vec(), "png".to_string())]);
        assert_eq!(got.markdown, "![A chart](assets/doc%201.png)\n");
    }

    #[test]
    fn monospace_paragraphs_are_a_code_block() {
        let code = |text: &str| {
            p(&format!(
                r#"<w:r><w:rPr><w:rFonts w:ascii="Consolas" w:hAnsi="Consolas"/></w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#
            ))
        };
        let body = [p(&r("Run:")), code("let x = 1;"), code("  x * 2"), p(&r("Done."))].concat();
        assert_eq!(
            markdown(&body, &[]).markdown,
            "Run:\n\n```\nlet x = 1;\n  x * 2\n```\n\nDone.\n"
        );
    }

    #[test]
    fn not_a_docx_says_so() {
        assert!(read(b"nope", &mut |_, _| None).unwrap_err().contains("not a .docx"));
    }
}
