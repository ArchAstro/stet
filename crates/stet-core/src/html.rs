//! HTML to Markdown, for what other programs put on the clipboard: a browser
//! selection, a spreadsheet range, a page of a word processor.

use crate::table;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::rc::Rc;

pub struct Markdown {
    pub text: String,
    /// The HTML carried structure plain text would lose: emphasis, links,
    /// headings, lists, tables, code, images. Without any, the clipboard's
    /// own plain text is the better thing to paste.
    pub rich: bool,
}

/// Converts `html`. `image` is asked about every image address and may
/// return another to use in its place (a file it saved a `data:` image to).
pub fn to_markdown(html: &str, image: &mut dyn FnMut(&str) -> Option<String>) -> Markdown {
    // Windows hands the fragment over behind a header of offsets.
    let html = match html.find('<') {
        Some(at) if html.starts_with("Version:") => &html[at..],
        _ => html,
    };
    let dom = html5ever::parse_document(RcDom::default(), Default::default()).one(html);
    let mut converter = Converter {
        image,
        rich: false,
        table: false,
        header: false,
    };
    let blocks = converter.container(&dom.document, &Style::default());
    let mut text = join(&blocks);
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    Markdown {
        text: text.trim_matches('\n').to_string(),
        rich: converter.rich,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    href: String,
}

#[derive(Clone, Debug, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    link: Option<Rc<Link>>,
}

impl Style {
    fn same_link(&self, other: &Style) -> bool {
        match (&self.link, &other.link) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
enum Piece {
    Text(String, Style),
    /// Markdown that is already final: a code span, an image, a checkbox.
    Raw(String, Style),
    Break,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Paragraph,
    List,
    /// A word processor's list paragraph: one item of a list.
    Item,
    Other,
}

#[derive(Clone, Debug)]
struct Block {
    text: String,
    kind: Kind,
}

fn join(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            let tight = block.kind == Kind::Item && blocks[index - 1].kind == Kind::Item;
            out.push_str(if tight { "\n" } else { "\n\n" });
        }
        out.push_str(&block.text);
    }
    out
}

/// How a run of inline pieces is written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Paragraph,
    /// A heading: one line, and bold already.
    Heading,
}

struct Converter<'a> {
    image: &'a mut dyn FnMut(&str) -> Option<String>,
    rich: bool,
    /// Inside a table cell, where a pipe would end the cell.
    table: bool,
    /// Inside a table's header row, which is bold as it is.
    header: bool,
}

fn tag(node: &Handle) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(&name.local),
        _ => None,
    }
}

fn attr(node: &Handle, name: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.to_string())
}

/// The value of a property in a `style` attribute, lower-cased.
fn css(node: &Handle, property: &str) -> Option<String> {
    let style = attr(node, "style")?;
    style
        .split(';')
        .filter_map(|rule| rule.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case(property))
        .map(|(_, value)| value.replace("!important", "").trim().to_ascii_lowercase())
        .next_back()
}

fn has_class(node: &Handle, wanted: &[&str]) -> bool {
    attr(node, "class").is_some_and(|classes| classes.split_whitespace().any(|class| wanted.contains(&class)))
}

fn hidden(node: &Handle) -> bool {
    attr(node, "hidden").is_some()
        || css(node, "display").is_some_and(|display| display == "none")
        || css(node, "visibility").is_some_and(|visibility| visibility == "hidden")
        || has_class(
            node,
            &["mw-editsection", "sr-only", "visually-hidden", "screen-reader-text"],
        )
}

/// Elements whose content is never part of the text.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "title", "meta", "link", "noscript", "template", "svg", "button", "select", "textarea",
    "iframe", "object", "audio", "video", "canvas", "colgroup", "col",
];

const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "body",
    "caption",
    "center",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hgroup",
    "html",
    "legend",
    "li",
    "main",
    "menu",
    "nav",
    "p",
    "section",
    "summary",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
];

/// All the text under a node, as written.
fn text_of(node: &Handle, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        NodeData::Element { name, .. } if &*name.local == "br" => out.push('\n'),
        _ => {
            for child in node.children.borrow().iter() {
                text_of(child, out);
            }
        }
    }
}

fn find(node: &Handle, wanted: &[&str]) -> bool {
    node.children
        .borrow()
        .iter()
        .any(|child| tag(child).is_some_and(|name| wanted.contains(&name)) || find(child, wanted))
}

/// A fence or code span delimiter longer than any run of backticks inside.
fn backticks(text: &str, at_least: usize) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat(at_least.max(longest + 1))
}

/// The language a code block names in its classes, the way highlighters do.
fn language(node: &Handle) -> Option<String> {
    let classes = attr(node, "class")?;
    classes.split_whitespace().find_map(|class| {
        ["language-", "lang-", "highlight-source-", "highlight-text-", "brush:"]
            .iter()
            .find_map(|prefix| class.strip_prefix(prefix))
            .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "+-#_.".contains(c)))
            .map(str::to_string)
    })
}

fn destination(url: &str) -> String {
    let url = url.trim();
    let url = if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    };
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '\n' | '\r' | '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// Backslash-escapes what Markdown would otherwise read as markup, and
/// nothing else: pasted prose should stay prose.
fn escape(text: &str, line_start: bool, table: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let brackets = ["](", "][", "[[", "[^", "]:"].iter().any(|pair| text.contains(pair));
    let mut out = String::with_capacity(text.len() + 8);
    let mut at = 0;
    if line_start {
        let rest = |from: usize| chars.get(from).is_none_or(|c| *c == ' ');
        let hashes = chars.iter().take_while(|c| **c == '#').count();
        let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
        let only = |mark: char| chars.iter().all(|c| *c == mark || *c == ' ');
        if (1..=6).contains(&hashes) && rest(hashes)
            || chars[0] == '>'
            || matches!(chars[0], '-' | '+') && rest(1)
            || chars.len() >= 3 && (only('-') || only('_'))
            || only('=')
        {
            out.push('\\');
        } else if (1..=9).contains(&digits) && matches!(chars.get(digits), Some('.' | ')')) && rest(digits + 1) {
            out.extend(&chars[..digits]);
            out.push('\\');
            at = digits;
        }
    }
    while at < chars.len() {
        let c = chars[at];
        let prev = at.checked_sub(1).map(|before| chars[before]);
        let next = chars.get(at + 1).copied();
        let escaped = match c {
            '\\' | '`' | '*' => true,
            '_' => !(prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric)),
            '~' => prev == Some('~') || next == Some('~'),
            '[' | ']' => brackets,
            '<' => next.is_some_and(|next| next.is_ascii_alphabetic() || "/!?".contains(next)),
            '&' => {
                let name: String = chars[at + 1..].iter().take_while(|c| **c != ';').take(12).collect();
                chars.get(at + 1 + name.chars().count()) == Some(&';')
                    && !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')
            }
            '|' => table,
            _ => false,
        };
        if escaped {
            out.push('\\');
        }
        out.push(c);
        at += 1;
    }
    out
}

/// One finished unit of a line: a word run, a space, a break.
struct Atom {
    text: String,
    /// The text before escaping, to recognise a link that shows its own address.
    plain: String,
    style: Style,
    space: bool,
}

const BOLD: u8 = 0;
const ITALIC: u8 = 1;
const STRIKE: u8 = 2;
const MARKERS: [&str; 3] = ["**", "*", "~~"];

impl Converter<'_> {
    /// The blocks under `node`.
    fn container(&mut self, node: &Handle, style: &Style) -> Vec<Block> {
        let (mut inline, mut out) = (Vec::new(), Vec::new());
        self.flow(node, style, &mut inline, &mut out);
        self.flush(&mut inline, &mut out);
        out
    }

    /// Ends the paragraph being collected, if there is one.
    fn flush(&mut self, inline: &mut Vec<Piece>, out: &mut Vec<Block>) {
        let pieces = std::mem::take(inline);
        // A blank line made of breaks separates paragraphs.
        let mut run: Vec<Piece> = Vec::new();
        let mut breaks = 0;
        let mut paragraphs: Vec<Vec<Piece>> = Vec::new();
        for piece in pieces {
            let blank = matches!(&piece, Piece::Text(text, _) if text.trim().is_empty());
            match piece {
                Piece::Break => breaks += 1,
                piece if blank && breaks > 0 => drop(piece),
                piece => {
                    if breaks > 1 {
                        paragraphs.push(std::mem::take(&mut run));
                    } else if breaks == 1 {
                        run.push(Piece::Break);
                    }
                    breaks = 0;
                    run.push(piece);
                }
            }
        }
        paragraphs.push(run);
        for pieces in paragraphs {
            let text = self.line(&pieces, Line::Paragraph);
            if !text.is_empty() {
                out.push(Block {
                    text,
                    kind: Kind::Paragraph,
                });
            }
        }
    }

    fn styled(&self, node: &Handle, name: &str, style: &Style) -> Style {
        let mut style = style.clone();
        match name {
            "b" | "strong" => style.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" => style.italic = true,
            "s" | "strike" | "del" => style.strike = true,
            _ => {}
        }
        if let Some(weight) = css(node, "font-weight") {
            style.bold = match weight.as_str() {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                weight => weight.parse::<f32>().map_or(style.bold, |weight| weight >= 600.0),
            };
        }
        match css(node, "font-style").as_deref() {
            Some("italic" | "oblique") => style.italic = true,
            Some("normal") => style.italic = false,
            _ => {}
        }
        let decoration = css(node, "text-decoration").or_else(|| css(node, "text-decoration-line"));
        if decoration.is_some_and(|decoration| decoration.contains("line-through")) {
            style.strike = true;
        }
        style
    }

    fn flow(&mut self, node: &Handle, style: &Style, inline: &mut Vec<Piece>, out: &mut Vec<Block>) {
        for child in node.children.borrow().iter() {
            let name = match &child.data {
                NodeData::Text { contents } => {
                    inline.push(Piece::Text(contents.borrow().to_string(), style.clone()));
                    continue;
                }
                NodeData::Element { name, .. } => &*name.local,
                _ => continue,
            };
            if SKIPPED.contains(&name) || hidden(child) {
                continue;
            }
            // A word processor's own bullet, written out as text.
            if css(child, "mso-list").is_some_and(|value| value == "ignore") {
                continue;
            }
            let own = self.styled(child, name, style);
            match name {
                "br" => inline.push(Piece::Break),
                "wbr" => {}
                "img" => {
                    if let Some(image) = self.image(child) {
                        inline.push(Piece::Raw(image, own));
                    }
                }
                "input" => {
                    if attr(child, "type").as_deref() == Some("checkbox") {
                        let mark = if attr(child, "checked").is_some() { "[x]" } else { "[ ]" };
                        inline.push(Piece::Raw(mark.to_string(), Style::default()));
                    }
                }
                "code" | "kbd" | "samp" | "tt" => {
                    let mut text = String::new();
                    text_of(child, &mut text);
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        let fence = backticks(&text, 1);
                        let pad = if text.starts_with('`') || text.ends_with('`') {
                            " "
                        } else {
                            ""
                        };
                        let text = if self.table { text.replace('|', "\\|") } else { text };
                        self.rich = true;
                        inline.push(Piece::Raw(format!("{fence}{pad}{text}{pad}{fence}"), own));
                    }
                }
                "a" => {
                    let href = attr(child, "href").unwrap_or_default();
                    let href = href.trim();
                    let mut linked = own;
                    if !href.is_empty()
                        && !href.starts_with('#')
                        && !href.to_ascii_lowercase().starts_with("javascript:")
                    {
                        linked.link = Some(Rc::new(Link {
                            href: destination(href),
                        }));
                    }
                    self.flow(child, &linked, inline, out);
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    self.flush(inline, out);
                    let mut pieces = Vec::new();
                    self.inline_only(child, style, &mut pieces);
                    let text = self.line(&pieces, Line::Heading);
                    if !text.is_empty() {
                        self.rich = true;
                        let level = (name.as_bytes()[1] - b'0') as usize;
                        out.push(Block {
                            text: format!("{} {text}", "#".repeat(level)),
                            kind: Kind::Other,
                        });
                    }
                }
                "ul" | "ol" | "dir" => {
                    self.flush(inline, out);
                    if let Some(list) = self.list(child, name == "ol", style) {
                        out.push(list);
                    }
                }
                "blockquote" => {
                    self.flush(inline, out);
                    let inner = join(&self.container(child, style));
                    if !inner.is_empty() {
                        self.rich = true;
                        let quoted: Vec<String> = inner
                            .lines()
                            .map(|line| {
                                if line.is_empty() {
                                    ">".to_string()
                                } else {
                                    format!("> {line}")
                                }
                            })
                            .collect();
                        out.push(Block {
                            text: quoted.join("\n"),
                            kind: Kind::Other,
                        });
                    }
                }
                "pre" | "xmp" | "listing" => {
                    self.flush(inline, out);
                    out.extend(self.code_block(child, node));
                }
                "table" => {
                    self.flush(inline, out);
                    out.extend(self.table(child, style));
                }
                "hr" => {
                    self.flush(inline, out);
                    self.rich = true;
                    out.push(Block {
                        text: "---".to_string(),
                        kind: Kind::Other,
                    });
                }
                "p" if css(child, "mso-list").is_some() => {
                    self.flush(inline, out);
                    out.extend(self.office_item(child, style));
                }
                "dt" => {
                    self.flush(inline, out);
                    let mut term = own;
                    term.bold = true;
                    let mut pieces = Vec::new();
                    self.flow(child, &term, &mut pieces, out);
                    self.flush(&mut pieces, out);
                }
                name => {
                    let display = css(child, "display");
                    let block = match display.as_deref() {
                        Some("inline" | "inline-block" | "contents") => false,
                        Some("block" | "flex" | "grid" | "list-item" | "table") => true,
                        _ => BLOCKS.contains(&name),
                    };
                    if block {
                        self.flush(inline, out);
                        let mut pieces = Vec::new();
                        self.flow(child, &own, &mut pieces, out);
                        self.flush(&mut pieces, out);
                    } else {
                        self.flow(child, &own, inline, out);
                    }
                }
            }
        }
    }

    /// The content of `node` as one run of inline pieces, blocks flattened.
    fn inline_only(&mut self, node: &Handle, style: &Style, pieces: &mut Vec<Piece>) {
        let mut blocks = Vec::new();
        self.flow(node, style, pieces, &mut blocks);
        for block in blocks {
            pieces.push(Piece::Raw(block.text.replace('\n', " "), Style::default()));
        }
    }

    fn image(&mut self, node: &Handle) -> Option<String> {
        let size = |name: &str| attr(node, name).and_then(|value| value.trim().parse::<f32>().ok());
        if size("width").is_some_and(|width| width <= 2.0) || size("height").is_some_and(|height| height <= 2.0) {
            return None;
        }
        let src = attr(node, "src").unwrap_or_default();
        // A lazily loaded picture keeps its address elsewhere.
        let src = match attr(node, "data-src") {
            Some(real) if src.is_empty() || src.starts_with("data:") => real,
            _ => src,
        };
        let src = src.trim();
        if src.is_empty() {
            return None;
        }
        let src = match (self.image)(src) {
            Some(replaced) => replaced,
            None if src.starts_with("data:") => return None,
            None => destination(src),
        };
        let alt = attr(node, "alt").unwrap_or_default();
        let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
        let alt = alt.replace('[', "\\[").replace(']', "\\]");
        let alt = if self.table { alt.replace('|', "\\|") } else { alt };
        self.rich = true;
        Some(format!("![{alt}]({src})"))
    }

    fn code_block(&mut self, node: &Handle, parent: &Handle) -> Option<Block> {
        let mut text = String::new();
        text_of(node, &mut text);
        let text = text.replace("\r\n", "\n").replace('\u{a0}', " ");
        let text = text.trim_matches('\n');
        if text.trim().is_empty() {
            return None;
        }
        let inner = node
            .children
            .borrow()
            .iter()
            .find(|child| tag(child) == Some("code"))
            .cloned();
        let lang = language(node)
            .or_else(|| inner.as_ref().and_then(language))
            .or_else(|| language(parent))
            .unwrap_or_default();
        let fence = backticks(text, 3);
        self.rich = true;
        Some(Block {
            text: format!("{fence}{lang}\n{text}\n{fence}"),
            kind: Kind::Other,
        })
    }

    fn list(&mut self, node: &Handle, ordered: bool, style: &Style) -> Option<Block> {
        let mut items: Vec<Vec<Block>> = Vec::new();
        for child in node.children.borrow().iter() {
            match tag(child) {
                Some("li") if !hidden(child) => items.push(self.container(child, style)),
                // Some editors hang a nested list beside its item, not inside it.
                Some(name @ ("ul" | "ol")) => {
                    if let Some(nested) = self.list(child, name == "ol", style) {
                        match items.last_mut() {
                            Some(item) => item.push(nested),
                            None => items.push(vec![nested]),
                        }
                    }
                }
                Some(_) => {
                    let blocks = self.container(child, style);
                    if !blocks.is_empty() {
                        items.push(blocks);
                    }
                }
                None => {}
            }
        }
        items.retain(|item| !item.is_empty());
        if items.is_empty() {
            return None;
        }
        self.rich = true;
        let first = attr(node, "start")
            .and_then(|start| start.trim().parse::<usize>().ok())
            .unwrap_or(1);
        let loose = items
            .iter()
            .any(|item| item.iter().skip(1).any(|block| block.kind != Kind::List));
        let mut lines = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let marker = if ordered {
                format!("{}. ", first + index)
            } else {
                "- ".to_string()
            };
            let indent = " ".repeat(marker.len());
            let body = item
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join(if loose { "\n\n" } else { "\n" });
            for (at, line) in body.lines().enumerate() {
                lines.push(match (at, line.is_empty()) {
                    (0, _) => format!("{marker}{line}"),
                    (_, true) => String::new(),
                    (_, false) => format!("{indent}{line}"),
                });
            }
            if loose && index + 1 < items.len() {
                lines.push(String::new());
            }
        }
        Some(Block {
            text: lines.join("\n"),
            kind: Kind::List,
        })
    }

    /// A list paragraph of a word processor, which has no list around it.
    fn office_item(&mut self, node: &Handle, style: &Style) -> Option<Block> {
        let level = css(node, "mso-list")
            .and_then(|value| {
                value
                    .split_whitespace()
                    .find_map(|word| word.strip_prefix("level")?.parse::<usize>().ok())
            })
            .unwrap_or(1);
        // The marker it drew for itself says whether the list is numbered.
        fn marker(node: &Handle) -> Option<String> {
            for child in node.children.borrow().iter() {
                if css(child, "mso-list").is_some_and(|value| value == "ignore") {
                    let mut text = String::new();
                    text_of(child, &mut text);
                    return Some(text.replace('\u{a0}', " ").trim().to_string());
                }
                if let Some(found) = marker(child) {
                    return Some(found);
                }
            }
            None
        }
        let drawn = marker(node).unwrap_or_default();
        let number = drawn.trim_end_matches(['.', ')']);
        let mark = if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
            format!("{number}. ")
        } else {
            "- ".to_string()
        };
        let mut pieces = Vec::new();
        self.inline_only(node, style, &mut pieces);
        let text = self.line(&pieces, Line::Paragraph);
        if text.is_empty() {
            return None;
        }
        self.rich = true;
        Some(Block {
            text: format!("{}{mark}{text}", "    ".repeat(level.saturating_sub(1))),
            kind: Kind::Item,
        })
    }

    fn table(&mut self, node: &Handle, style: &Style) -> Vec<Block> {
        struct Source {
            node: Handle,
            head: bool,
        }
        fn rows(node: &Handle, head: bool, out: &mut Vec<Source>, captions: &mut Vec<Handle>) {
            for child in node.children.borrow().iter() {
                match tag(child) {
                    Some("tr") if !hidden(child) => out.push(Source {
                        node: child.clone(),
                        head,
                    }),
                    Some("thead") => rows(child, true, out, captions),
                    Some("tbody" | "tfoot") => rows(child, false, out, captions),
                    Some("caption") => captions.push(child.clone()),
                    _ => {}
                }
            }
        }
        let (mut sources, mut captions) = (Vec::new(), Vec::new());
        rows(node, false, &mut sources, &mut captions);
        let cells_of = |row: &Handle| -> Vec<Handle> {
            row.children
                .borrow()
                .iter()
                .filter(|cell| matches!(tag(cell), Some("td" | "th")) && !hidden(cell))
                .cloned()
                .collect()
        };
        // A table used for page layout, or one holding a listing, is not data.
        let layout = sources.iter().any(|row| {
            cells_of(&row.node)
                .iter()
                .any(|cell| find(cell, &["table", "pre", "h1", "h2", "h3", "h4", "h5", "h6"]))
        });
        let mut out: Vec<Block> = Vec::new();
        for caption in &captions {
            out.extend(self.container(caption, style));
        }
        let as_blocks = |converter: &mut Self, out: &mut Vec<Block>| {
            for row in &sources {
                for cell in cells_of(&row.node) {
                    out.extend(converter.container(&cell, style));
                }
            }
        };
        if layout {
            as_blocks(self, &mut out);
            return out;
        }

        let (was_rich, was_table) = (self.rich, self.table);
        self.table = true;
        let mut grid: Vec<Vec<String>> = Vec::new();
        let mut aligns: Vec<table::Align> = Vec::new();
        // Cells still covered by a `rowspan` from an earlier row, by column.
        let mut spans: Vec<usize> = Vec::new();
        let mut head_rows = 0;
        for (index, row) in sources.iter().enumerate() {
            let cells = cells_of(&row.node);
            let all_headers = !cells.is_empty() && cells.iter().all(|cell| tag(cell) == Some("th"));
            if index == head_rows && head_rows == 0 && (row.head || all_headers) {
                head_rows = 1;
            }
            let mut line: Vec<String> = Vec::new();
            let mut cells = cells.into_iter();
            loop {
                let column = line.len();
                if spans.get(column).is_some_and(|left| *left > 0) {
                    spans[column] -= 1;
                    line.push(String::new());
                    continue;
                }
                let Some(cell) = cells.next() else { break };
                let number = |name: &str| {
                    attr(&cell, name)
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 64)
                };
                let (wide, tall) = (number("colspan"), number("rowspan"));
                self.header = index == 0;
                let cell_style = self.styled(&cell, "td", style);
                let blocks = self.container(&cell, &cell_style);
                self.header = false;
                let mut text = join(&blocks).replace("\\\n", "<br>").replace('\n', "<br>");
                while text.contains("<br><br>") {
                    text = text.replace("<br><br>", "<br>");
                }
                let align = attr(&cell, "align")
                    .map(|align| align.to_ascii_lowercase())
                    .or_else(|| css(&cell, "text-align"));
                for extra in 0..wide {
                    let column = line.len();
                    if spans.len() <= column {
                        spans.resize(column + 1, 0);
                        aligns.resize(column + 1, table::Align::None);
                    }
                    spans[column] = tall - 1;
                    if index == 0 || aligns[column] == table::Align::None && index == head_rows {
                        aligns[column] = match align.as_deref() {
                            Some("center") => table::Align::Center,
                            Some("right" | "end") => table::Align::Right,
                            _ => table::Align::None,
                        };
                    }
                    line.push(if extra == 0 {
                        std::mem::take(&mut text)
                    } else {
                        String::new()
                    });
                }
            }
            grid.push(line);
        }
        self.table = was_table;
        grid.retain(|row| row.iter().any(|cell| !cell.is_empty()));
        let columns = grid.iter().map(Vec::len).max().unwrap_or(0);
        // Columns nothing was written in (a gutter of line numbers) go.
        let used: Vec<usize> = (0..columns)
            .filter(|&column| {
                grid.iter()
                    .any(|row| row.get(column).is_some_and(|cell| !cell.is_empty()))
            })
            .collect();
        if used.len() < 2 || grid.len() < 2 && used.len() < 2 {
            // One column is a list of paragraphs, not a table.
            self.rich = was_rich;
            as_blocks(self, &mut out);
            return out;
        }
        let cell = |row: &Vec<String>, column: usize| row.get(column).cloned().unwrap_or_default();
        let mut lines: Vec<String> = Vec::with_capacity(grid.len() + 1);
        for (index, row) in grid.iter().enumerate() {
            let cells: Vec<String> = used.iter().map(|&column| cell(row, column)).collect();
            lines.push(format!("| {} |", cells.join(" | ")));
            if index == 0 {
                let rule: Vec<&str> = used
                    .iter()
                    .map(|&column| match aligns.get(column) {
                        Some(table::Align::Center) => ":-:",
                        Some(table::Align::Right) => "--:",
                        _ => "---",
                    })
                    .collect();
                lines.push(format!("| {} |", rule.join(" | ")));
            }
        }
        self.rich = true;
        out.push(Block {
            text: table::format(&lines).unwrap_or(lines).join("\n"),
            kind: Kind::Other,
        });
        out
    }

    /// Writes a run of inline pieces as Markdown.
    fn line(&mut self, pieces: &[Piece], mode: Line) -> String {
        let mut atoms: Vec<Atom> = Vec::new();
        let mut pending_space = false;
        let mut line_start = true;
        let space = |atoms: &mut Vec<Atom>| {
            atoms.push(Atom {
                text: " ".to_string(),
                plain: " ".to_string(),
                style: Style::default(),
                space: true,
            })
        };
        for piece in pieces {
            match piece {
                Piece::Text(text, style) => {
                    let text = text.replace('\u{a0}', " ");
                    let core = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if core.is_empty() {
                        pending_space |= !text.is_empty();
                        continue;
                    }
                    if (pending_space || text.starts_with(char::is_whitespace)) && !line_start {
                        space(&mut atoms);
                    }
                    atoms.push(Atom {
                        text: escape(&core, line_start, self.table),
                        plain: core,
                        style: style.clone(),
                        space: false,
                    });
                    pending_space = text.ends_with(char::is_whitespace);
                    line_start = false;
                }
                Piece::Raw(text, style) => {
                    if pending_space && !line_start {
                        space(&mut atoms);
                    }
                    atoms.push(Atom {
                        text: text.clone(),
                        plain: text.clone(),
                        style: style.clone(),
                        space: false,
                    });
                    // A checkbox is followed by a space whatever the source did.
                    pending_space = text == "[ ]" || text == "[x]";
                    line_start = false;
                }
                Piece::Break => {
                    pending_space = false;
                    if atoms.is_empty() {
                        continue;
                    }
                    if mode == Line::Heading {
                        pending_space = true;
                        continue;
                    }
                    atoms.push(Atom {
                        text: if self.table { "<br>" } else { "\\\n" }.to_string(),
                        plain: "\n".to_string(),
                        style: Style::default(),
                        space: true,
                    });
                    line_start = true;
                }
            }
        }
        while atoms.last().is_some_and(|atom| atom.space) {
            atoms.pop();
        }
        // Spaces and breaks take the style their neighbours share, so a
        // marker is never closed and reopened around one.
        for index in 0..atoms.len() {
            if !atoms[index].space {
                continue;
            }
            let before = atoms[..index]
                .iter()
                .rev()
                .find(|atom| !atom.space)
                .map(|atom| atom.style.clone());
            let after = atoms[index + 1..]
                .iter()
                .find(|atom| !atom.space)
                .map(|atom| atom.style.clone());
            if let (Some(before), Some(after)) = (before, after) {
                atoms[index].style = Style {
                    bold: before.bold && after.bold,
                    italic: before.italic && after.italic,
                    strike: before.strike && after.strike,
                    link: if before.same_link(&after) { before.link } else { None },
                };
            }
        }

        let mut out = String::new();
        let mut open: Vec<u8> = Vec::new();
        let mut link: Option<(Rc<Link>, usize, String)> = None;
        let close_link = |out: &mut String, link: &mut Option<(Rc<Link>, usize, String)>| {
            let Some((target, start, shown)) = link.take() else {
                return;
            };
            if shown.trim() == target.href && !shown.contains(' ') {
                out.truncate(start);
                out.push_str(&format!("<{}>", target.href));
            } else {
                out.push_str(&format!("]({})", target.href));
            }
        };
        for atom in &atoms {
            let style = &atom.style;
            let bold = style.bold && mode != Line::Heading && !self.header;
            let wanted = [bold, style.italic, style.strike];
            let same_link = match (&link, &style.link) {
                (Some((current, ..)), Some(next)) => Rc::ptr_eq(current, next),
                (None, None) => true,
                _ => false,
            };
            // Close what this atom does not carry: innermost first.
            let keep = if same_link {
                open.iter()
                    .position(|flag| !wanted[*flag as usize])
                    .unwrap_or(open.len())
            } else {
                0
            };
            while open.len() > keep {
                out.push_str(MARKERS[open.pop().unwrap_or(BOLD) as usize]);
            }
            if !same_link {
                close_link(&mut out, &mut link);
                if let Some(target) = &style.link {
                    link = Some((target.clone(), out.len(), String::new()));
                    out.push('[');
                    self.rich = true;
                }
            }
            for flag in [BOLD, ITALIC, STRIKE] {
                if wanted[flag as usize] && !open.contains(&flag) && !atom.space {
                    out.push_str(MARKERS[flag as usize]);
                    open.push(flag);
                    self.rich = true;
                }
            }
            out.push_str(&atom.text);
            if let Some((_, _, shown)) = &mut link {
                shown.push_str(&atom.plain);
            }
        }
        while let Some(flag) = open.pop() {
            out.push_str(MARKERS[flag as usize]);
        }
        close_link(&mut out, &mut link);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(html: &str) -> String {
        to_markdown(html, &mut |_| None).text
    }

    fn rich(html: &str) -> bool {
        to_markdown(html, &mut |_| None).rich
    }

    #[test]
    fn inline_formatting_hugs_the_words() {
        assert_eq!(
            md("<p>Some <b>bold </b>and<i> italic</i> text.</p>"),
            "Some **bold** and *italic* text."
        );
        assert_eq!(md("<b>a <i>b</i> c</b>"), "**a *b* c**");
        assert_eq!(md("<b>one</b> <b>two</b>"), "**one two**");
        assert_eq!(md("<del>gone</del> <s>too</s>"), "~~gone too~~");
        assert_eq!(
            md("Press <kbd>Ctrl</kbd> or run <code>a `b` c</code>"),
            "Press `Ctrl` or run ``a `b` c``"
        );
        assert_eq!(md("<p>a<br>b</p>"), "a\\\nb");
        assert_eq!(md("<p>a<br><br>b</p>"), "a\n\nb");
        assert_eq!(
            md("<span style='font-weight:700'>x</span> <span style='font-style:italic'>y</span>"),
            "**x** *y*"
        );
        assert_eq!(md("  <p>   spaced \n  out&nbsp;text  </p>  "), "spaced out text");
    }

    #[test]
    fn links_and_images() {
        assert_eq!(md(r#"<a href="https://a.b/c">here</a>"#), "[here](https://a.b/c)");
        assert_eq!(md(r#"<a href="https://a.b/c">https://a.b/c</a>"#), "<https://a.b/c>");
        assert_eq!(
            md(r#"see <a href="https://a.b/x (1)" title="noise"><b>this</b> one</a>."#),
            "see [**this** one](https://a.b/x%20%281%29)."
        );
        assert_eq!(
            md(r##"<a href="#top">top</a> <a href="javascript:void(0)">x</a>"##),
            "top x"
        );
        assert_eq!(
            md(r#"<img src="//cdn.x/a.png" alt="A  [cat]">"#),
            r"![A \[cat\]](https://cdn.x/a.png)"
        );
        assert_eq!(
            md(r#"<a href="https://x.y"><img src="a.png" alt=""></a>"#),
            "[![](a.png)](https://x.y)"
        );
        assert_eq!(md(r#"<img src="pixel.gif" width="1" height="1">text"#), "text");
        // A data image is kept only if someone stores it.
        assert_eq!(md(r#"<img src="data:image/png;base64,AAAA" alt="x">"#), "");
        let stored = to_markdown(r#"<img src="data:image/png;base64,AAAA" alt="x">"#, &mut |src| {
            src.starts_with("data:").then(|| "assets/x.png".to_string())
        });
        assert_eq!(stored.text, "![x](assets/x.png)");
    }

    #[test]
    fn headings_lists_quotes_and_code() {
        assert_eq!(md("<h2>A <b>title</b></h2><p>Body</p>"), "## A title\n\nBody");
        assert_eq!(
            md("<ul><li>one</li><li>two<ul><li>deep</li></ul></li></ul>"),
            "- one\n- two\n  - deep"
        );
        assert_eq!(md("<ol start=3><li>a</li><li><p>b</p></li></ol>"), "3. a\n4. b");
        assert_eq!(
            md("<ul><li><p>a</p><p>more</p></li><li>b</li></ul>"),
            "- a\n\n  more\n\n- b"
        );
        // Google Docs puts a nested list beside its item.
        assert_eq!(md("<ul><li>a</li><ul><li>b</li></ul></ul>"), "- a\n  - b");
        assert_eq!(
            md(r#"<ul><li><input type="checkbox" checked>done</li><li><input type="checkbox"> todo</li></ul>"#),
            "- [x] done\n- [ ] todo"
        );
        assert_eq!(md("<blockquote><p>a</p><p>b</p></blockquote>"), "> a\n>\n> b");
        assert_eq!(
            md("<pre><code class=\"language-rust\">fn main() {\n    let a = \"&lt;b&gt;\";\n}\n</code></pre>"),
            "```rust\nfn main() {\n    let a = \"<b>\";\n}\n```"
        );
        assert_eq!(
            md("<div class=\"highlight highlight-source-shell\"><pre>ls ``` x</pre></div>"),
            "````shell\nls ``` x\n````"
        );
        assert_eq!(md("<p>a</p><hr><p>b</p>"), "a\n\n---\n\nb");
        assert_eq!(md("<dl><dt>Term</dt><dd>Meaning</dd></dl>"), "**Term**\n\nMeaning");
    }

    #[test]
    fn text_that_looks_like_markup_is_escaped_and_prose_is_not() {
        assert_eq!(
            md("<p>2 * 3 * 4, snake_case, _private, a_ b</p>"),
            r"2 \* 3 \* 4, snake_case, \_private, a\_ b"
        );
        assert_eq!(
            md("<p># not a heading</p><p>1. not a list</p><p>- nor this</p><p>> or this</p>"),
            "\\# not a heading\n\n1\\. not a list\n\n\\- nor this\n\n\\> or this"
        );
        assert_eq!(
            md("<p>#hashtag, 3.5 kg, a - b, [1] and x < y &amp; z</p>"),
            "#hashtag, 3.5 kg, a - b, [1] and x < y & z"
        );
        assert_eq!(
            md("<p>&lt;div&gt; and &amp;amp; and [a](b) and [[wiki]]</p>"),
            r"\<div> and \&amp; and \[a\](b) and \[\[wiki\]\]"
        );
        assert_eq!(
            md("<p>use `ticks` and back\\slash</p>"),
            r"use \`ticks\` and back\\slash"
        );
        assert_eq!(md("<p>a<br>---</p>"), "a\\\n\\---");
    }

    #[test]
    fn a_table_becomes_an_aligned_pipe_table() {
        let html = r#"<table><thead><tr><th>Name</th><th align="right">Qty</th><th style="text-align:center">Note</th></tr></thead>
            <tbody><tr><td>Apple <b>pie</b></td><td>3</td><td>a | b</td></tr>
            <tr><td><p>Fig</p><p>dried</p></td><td>12</td></tr></tbody></table>"#;
        assert_eq!(
            md(html),
            [
                "| Name          | Qty |  Note  |",
                "| ------------- | --: | :----: |",
                "| Apple **pie** |   3 | a \\| b |",
                "| Fig<br>dried  |  12 |        |",
            ]
            .join("\n")
        );
        assert!(rich(html));
    }

    #[test]
    fn spans_keep_the_columns_in_place() {
        let html = "<table><tr><th colspan=2>Both</th><th>C</th></tr>\
            <tr><td rowspan=2>tall</td><td>b1</td><td>c1</td></tr><tr><td>b2</td><td>c2</td></tr></table>";
        assert_eq!(
            md(html),
            [
                "| Both |     | C   |",
                "| ---- | --- | --- |",
                "| tall | b1  | c1  |",
                "|      | b2  | c2  |"
            ]
            .join("\n")
        );
    }

    #[test]
    fn tables_that_are_not_data_are_not_tables() {
        // Layout: one column.
        assert_eq!(
            md("<table><tr><td>one</td></tr><tr><td>two</td></tr></table>"),
            "one\n\ntwo"
        );
        assert!(!rich("<table><tr><td>one</td></tr><tr><td>two</td></tr></table>"));
        // A listing with a gutter of line numbers.
        let listing = "<table><tr><td class=\"gutter\"><pre>1\n2</pre></td><td><pre>let a = 1;\nlet b = 2;</pre></td></tr></table>";
        assert_eq!(md(listing), "```\n1\n2\n```\n\n```\nlet a = 1;\nlet b = 2;\n```");
        // Line numbers drawn by the page leave an empty column behind.
        let lines =
            "<table><tr><td data-line=\"1\"></td><td>let a = 1;</td></tr><tr><td></td><td>let b = 2;</td></tr></table>";
        assert_eq!(md(lines), "let a = 1;\n\nlet b = 2;");
    }

    #[test]
    fn what_real_programs_put_on_the_clipboard() {
        // Chrome: a meta tag, then the selection with computed styles.
        let chrome = "<meta charset='utf-8'><span style=\"color: rgb(32, 33, 34); font-family: sans-serif; font-size: 14px; \
            font-style: normal; font-weight: 400; white-space: normal; display: inline !important; float: none;\">The \
            </span><a href=\"https://en.wikipedia.org/wiki/Proofreading\" style=\"color: rgb(51, 102, 204); font-weight: 400;\">proofreader</a>\
            <span style=\"font-weight: 400;\"> writes <i>stet</i>.</span>";
        assert_eq!(
            md(chrome),
            "The [proofreader](https://en.wikipedia.org/wiki/Proofreading) writes *stet*."
        );

        // Google Docs: a bold wrapper that is not bold, and styled spans.
        let docs = "<meta charset='utf-8'><b style=\"font-weight:normal;\" id=\"docs-internal-guid-1\">\
            <h1 dir=\"ltr\"><span style=\"font-size:20pt;font-weight:400;\">Plan</span></h1>\
            <p dir=\"ltr\"><span style=\"font-weight:400;\">Ship </span><span style=\"font-weight:700;\">soon</span>\
            <span style=\"font-weight:400;font-style:italic;\"> ish</span></p>\
            <ul><li dir=\"ltr\"><p dir=\"ltr\" role=\"presentation\"><span>one</span></p></li>\
            <li dir=\"ltr\"><p dir=\"ltr\" role=\"presentation\"><span>two</span></p></li></ul></b>";
        assert_eq!(md(docs), "# Plan\n\nShip **soon** *ish*\n\n- one\n- two");

        // Google Sheets and Excel: a bare table, no header cells.
        let sheets = "<google-sheets-html-origin><style type=\"text/css\"><!--td {border: 1px solid #cccccc;}--></style>\
            <table xmlns=\"http://www.w3.org/1999/xhtml\" cellspacing=\"0\" cellpadding=\"0\" dir=\"ltr\" border=\"1\">\
            <colgroup><col width=\"100\"/><col width=\"100\"/></colgroup><tbody>\
            <tr style=\"height:21px;\"><td style=\"font-weight:bold;\">Region</td><td style=\"font-weight:bold;text-align:right;\">Sales</td></tr>\
            <tr style=\"height:21px;\"><td>North</td><td style=\"text-align:right;\">1,200</td></tr>\
            <tr style=\"height:21px;\"><td>South</td><td style=\"text-align:right;\">980</td></tr></tbody></table>";
        assert_eq!(
            md(sheets),
            [
                "| Region | Sales |",
                "| ------ | ----: |",
                "| North  | 1,200 |",
                "| South  |   980 |"
            ]
            .join("\n")
        );

        // Word: list paragraphs that draw their own bullets.
        let word = "<p class=MsoNormal>Intro<o:p></o:p></p>\
            <p class=MsoListParagraphCxSpFirst style='text-indent:-.25in;mso-list:l0 level1 lfo1'>\
            <span style='mso-list:Ignore'>·<span style='font:7.0pt \"Times New Roman\"'>&nbsp;&nbsp;&nbsp;</span></span>First<o:p></o:p></p>\
            <p class=MsoListParagraphCxSpLast style='text-indent:-.25in;mso-list:l0 level2 lfo1'>\
            <span style='mso-list:Ignore'>o<span>&nbsp;&nbsp;</span></span>Second<o:p></o:p></p>";
        assert_eq!(md(word), "Intro\n\n- First\n    - Second");

        // A code editor: colours only, so the plain text is what to paste.
        let editor = "<meta charset='utf-8'><div style=\"color: #d4d4d4;background-color: #1e1e1e;font-family: Menlo;white-space: pre;\">\
            <div><span style=\"color: #569cd6;\">let</span><span> a = </span><span style=\"color: #b5cea8;\">1</span>;</div>\
            <div><span>**not bold**</span></div></div>";
        assert!(!rich(editor));

        // Windows: the fragment behind its header.
        let windows = "Version:0.9\r\nStartHTML:0000000105\r\nEndHTML:0000000200\r\n<html><body><!--StartFragment--><b>hi</b><!--EndFragment--></body></html>";
        assert_eq!(md(windows), "**hi**");
    }

    #[test]
    fn hidden_and_unwanted_content_is_left_out() {
        let html = "<style>p{}</style><script>x()</script><p>keep<span style=\"display:none\">sort key</span>\
            <span class=\"mw-editsection\">[edit]</span><button>Copy</button></p><p hidden>no</p>";
        assert_eq!(md(html), "keep");
    }
}
