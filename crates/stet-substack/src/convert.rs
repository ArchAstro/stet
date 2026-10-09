//! Markdown to Substack's document: a ProseMirror-style JSON tree. Pure; no
//! network and no files. Images keep the address they were written with, and
//! `Document::local_images` / `set_image` let the caller swap in hosted ones.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, MetadataBlockKind, Options as Md, Parser, Tag, TagEnd};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Which node name carries a code block. Implementations disagree and none is
/// confirmed against Substack, so it is a switch rather than a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CodeBlockNode {
    /// `highlighted_code_block`, `attrs.language`: what one client read off the live editor.
    #[default]
    Highlighted,
    /// `code_block`, `attrs.language`: older posts, and another client's choice.
    Legacy,
    /// `codeBlock`, `attrs.language`: python-substack; reported as not rendering.
    CamelCase,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub code_block: CodeBlockNode,
    /// Show an image's alt text as its caption when it has no title.
    pub alt_as_caption: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    /// `{"type":"doc","content":[...]}`.
    pub body: Value,
    /// Things that were approximated or dropped.
    pub warnings: Vec<String>,
}

/// A hosted image to swap in for a local one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hosted {
    pub url: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

impl Document {
    /// Image addresses that are not http(s) or data URIs, in order, without repeats.
    pub fn local_images(&self) -> Vec<String> {
        let mut found = Vec::new();
        walk(&self.body, &mut |node| {
            if let Some(src) = image_src(node)
                && !is_remote(src)
                && !found.iter().any(|seen| seen == src)
            {
                found.push(src.to_string());
            }
        });
        found
    }

    /// Points every image written as `src` at its hosted copy.
    pub fn set_image(&mut self, src: &str, hosted: &Hosted) {
        walk_mut(&mut self.body, &mut |node| {
            if image_src(node) != Some(src) {
                return;
            }
            let Some(attrs) = node.get_mut("attrs").and_then(Value::as_object_mut) else {
                return;
            };
            attrs.insert("src".into(), json!(hosted.url));
            if let (Some(width), Some(height)) = (hosted.width, hosted.height) {
                attrs.insert("width".into(), json!(width));
                attrs.insert("height".into(), json!(height));
                attrs.insert("resizeWidth".into(), json!(width));
            }
        });
    }
}

fn image_src(node: &Value) -> Option<&str> {
    if node.get("type")?.as_str()? != "image2" {
        return None;
    }
    node.get("attrs")?.get("src")?.as_str()
}

fn walk(node: &Value, visit: &mut dyn FnMut(&Value)) {
    visit(node);
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            walk(child, visit);
        }
    }
}

fn walk_mut(node: &mut Value, visit: &mut dyn FnMut(&mut Value)) {
    visit(node);
    if let Some(children) = node.get_mut("content").and_then(Value::as_array_mut) {
        for child in children {
            walk_mut(child, visit);
        }
    }
}

/// Whether an address is already somewhere Substack can reach.
pub fn is_remote(src: &str) -> bool {
    let lower = src.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("data:") || src.starts_with("//")
}

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "webm", "ogv", "avi", "mkv"];

fn is_video(src: &str) -> bool {
    let path = src.split(['?', '#']).next().unwrap_or(src);
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The id in a YouTube watch, short or embed address.
pub fn youtube_id(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .unwrap_or(host);
    let id = match host {
        "youtu.be" => path.split(['?', '#', '/']).next()?.to_string(),
        "youtube.com" => {
            if let Some(tail) = path.strip_prefix("embed/").or_else(|| path.strip_prefix("shorts/")) {
                tail.split(['?', '#', '/']).next()?.to_string()
            } else {
                let query = path.strip_prefix("watch")?.trim_start_matches('?');
                let query = query.split('#').next()?;
                query.split('&').find_map(|pair| pair.strip_prefix("v="))?.to_string()
            }
        }
        _ => return None,
    };
    (id.len() >= 6 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then_some(id)
}

pub fn convert(markdown: &str, options: &Options) -> Document {
    let flags =
        Md::ENABLE_TABLES | Md::ENABLE_FOOTNOTES | Md::ENABLE_STRIKETHROUGH | Md::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let events: Vec<Event> = Parser::new_ext(markdown, flags).collect();
    let mut conv = Conv {
        events,
        pos: 0,
        options,
        warnings: Vec::new(),
        numbers: HashMap::new(),
        refs: Vec::new(),
    };
    conv.document()
}

/// Inline output: a run of text nodes, or a block lifted out of the paragraph.
enum Seg {
    Inline(Vec<Value>),
    Block(Value),
}

struct Conv<'a> {
    events: Vec<Event<'a>>,
    pos: usize,
    options: &'a Options,
    warnings: Vec<String>,
    /// Footnote label to its number, by first reference.
    numbers: HashMap<String, usize>,
    /// Labels referenced inside the top-level block being read.
    refs: Vec<String>,
}

impl<'a> Conv<'a> {
    fn document(&mut self) -> Document {
        let mut title = None;
        let mut subtitle = None;
        let mut top: Vec<(Vec<Value>, Vec<String>)> = Vec::new();
        let mut defs: HashMap<String, Vec<Value>> = HashMap::new();
        while let Some(event) = self.events.get(self.pos).cloned() {
            match event {
                Event::Start(Tag::MetadataBlock(MetadataBlockKind::YamlStyle)) => {
                    self.pos += 1;
                    let text = self.plain_until_end();
                    let (t, s) = front_matter(&text);
                    title = t.or(title);
                    subtitle = s.or(subtitle);
                }
                Event::Start(Tag::FootnoteDefinition(label)) => {
                    self.pos += 1;
                    let blocks = self.blocks_until_end();
                    defs.insert(label.to_string(), blocks);
                }
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H1,
                    ..
                }) if title.is_none() => {
                    let blocks = self.block();
                    title = Some(blocks.iter().map(plain_text).collect::<String>().trim().to_string());
                    self.refs.clear();
                }
                _ => {
                    let blocks = self.block();
                    top.push((blocks, std::mem::take(&mut self.refs)));
                }
            }
        }
        let mut content = Vec::new();
        for (blocks, labels) in top {
            content.extend(blocks);
            for label in labels {
                let number = self.numbers[&label];
                let body = match defs.get(&label) {
                    Some(blocks) if !blocks.is_empty() => blocks.clone(),
                    _ => {
                        self.warnings.push(format!("footnote [^{label}] has no definition"));
                        vec![paragraph(Vec::new())]
                    }
                };
                content.push(json!({"type": "footnote", "attrs": {"number": number}, "content": body}));
            }
        }
        if content.is_empty() {
            content.push(paragraph(Vec::new()));
        }
        Document {
            title: title.filter(|t| !t.is_empty()),
            subtitle: subtitle.filter(|s| !s.is_empty()),
            body: json!({"type": "doc", "content": content}),
            warnings: std::mem::take(&mut self.warnings),
        }
    }

    /// Reads blocks up to and including the enclosing End, or to the end of input.
    fn blocks_until_end(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(event) = self.events.get(self.pos) {
            match event {
                Event::End(_) => {
                    self.pos += 1;
                    break;
                }
                // A tight list item holds its text with no paragraph around it.
                e if is_inline(e) => {
                    let segs = self.inline_run();
                    out.extend(self.paragraph_blocks(segs));
                }
                _ => out.extend(self.block()),
            }
        }
        out
    }

    /// Reads one block. Consumes at least one event.
    fn block(&mut self) -> Vec<Value> {
        let Some(event) = self.events.get(self.pos).cloned() else {
            return Vec::new();
        };
        match event {
            Event::Start(tag) => {
                self.pos += 1;
                self.block_in(tag)
            }
            Event::Rule => {
                self.pos += 1;
                vec![json!({"type": "horizontal_rule"})]
            }
            e if is_inline(&e) => {
                let segs = self.inline_run();
                self.paragraph_blocks(segs)
            }
            _ => {
                self.pos += 1;
                Vec::new()
            }
        }
    }

    fn block_in(&mut self, tag: Tag<'a>) -> Vec<Value> {
        match tag {
            Tag::Paragraph => {
                let segs = self.inline_run();
                self.pos += 1;
                self.paragraph_blocks(segs)
            }
            Tag::Heading { level, .. } => {
                let segs = self.inline_run();
                self.pos += 1;
                let mut inline = Vec::new();
                for seg in segs {
                    match seg {
                        Seg::Inline(nodes) => inline.extend(nodes),
                        Seg::Block(_) => self.warnings.push("an image inside a heading was dropped".into()),
                    }
                }
                let level = level as usize;
                vec![json!({"type": "heading", "attrs": {"level": level}, "content": inline})]
            }
            Tag::BlockQuote(_) => {
                let mut content = self.blocks_until_end();
                if content.is_empty() {
                    content.push(paragraph(Vec::new()));
                }
                vec![json!({"type": "blockquote", "content": content})]
            }
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_string),
                    CodeBlockKind::Indented => None,
                };
                let code = self.plain_until_end_raw();
                vec![self.code_block(code.strip_suffix('\n').unwrap_or(&code), language.as_deref())]
            }
            Tag::List(start) => {
                let mut items = Vec::new();
                while let Some(event) = self.events.get(self.pos) {
                    match event {
                        Event::Start(Tag::Item) => {
                            self.pos += 1;
                            let mut content = self.blocks_until_end();
                            if content.first().and_then(|n| n["type"].as_str()) != Some("paragraph") {
                                content.insert(0, paragraph(Vec::new()));
                            }
                            items.push(json!({"type": "list_item", "content": content}));
                        }
                        Event::End(_) => {
                            self.pos += 1;
                            break;
                        }
                        _ => self.pos += 1,
                    }
                }
                match start {
                    None => vec![json!({"type": "bullet_list", "content": items})],
                    // `order` is the attribute that renders; the editor writes `start` beside it.
                    Some(n) if n != 1 => {
                        vec![json!({"type": "ordered_list", "attrs": {"start": n, "order": n}, "content": items})]
                    }
                    Some(_) => vec![json!({"type": "ordered_list", "content": items})],
                }
            }
            Tag::Table(_) => vec![self.table()],
            Tag::HtmlBlock => {
                let html = self.plain_until_end_raw();
                let trimmed = html.trim();
                if trimmed.is_empty() || trimmed.starts_with("<!--") {
                    return Vec::new();
                }
                self.warnings
                    .push("HTML has no Substack equivalent and was kept as plain text".into());
                vec![paragraph(vec![text(trimmed, &[])])]
            }
            // Front matter anywhere but the top, and anything unknown.
            _ => {
                self.skip_to_end();
                Vec::new()
            }
        }
    }

    /// Substack has no table node as far as is known, so a table becomes an
    /// aligned, pipe-delimited block of monospace text.
    fn table(&mut self) -> Value {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut header_rows = 0;
        while let Some(event) = self.events.get(self.pos).cloned() {
            self.pos += 1;
            match event {
                Event::Start(Tag::TableHead) => {
                    rows.push(Vec::new());
                    header_rows = 1;
                }
                Event::Start(Tag::TableRow) => rows.push(Vec::new()),
                Event::Start(Tag::TableCell) => {
                    let cell = self.plain_until_end().replace('\n', " ");
                    if let Some(row) = rows.last_mut() {
                        row.push(cell.trim().to_string());
                    }
                }
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        self.warnings
            .push("a table became preformatted text; Substack has no table node".into());
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..columns)
            .map(|c| {
                rows.iter()
                    .filter_map(|r| r.get(c))
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect();
        let line = |row: &[String]| -> String {
            let cells: Vec<String> = (0..columns)
                .map(|c| {
                    let cell = row.get(c).map(String::as_str).unwrap_or("");
                    format!("{cell}{}", " ".repeat(widths[c] - cell.chars().count()))
                })
                .collect();
            format!("| {} |", cells.join(" | "))
        };
        let mut lines = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            lines.push(line(row));
            if i + 1 == header_rows {
                let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
                lines.push(format!("| {} |", rule.join(" | ")));
            }
        }
        self.code_block(&lines.join("\n"), None)
    }

    fn code_block(&self, code: &str, language: Option<&str>) -> Value {
        let name = match self.options.code_block {
            CodeBlockNode::Highlighted => "highlighted_code_block",
            CodeBlockNode::Legacy => "code_block",
            CodeBlockNode::CamelCase => "codeBlock",
        };
        let content = if code.is_empty() {
            json!([])
        } else {
            json!([text(code, &[])])
        };
        let mut node = json!({"type": name, "content": content});
        if let Some(language) = language {
            node["attrs"] = json!({"language": language.to_ascii_lowercase()});
        }
        node
    }

    /// Reads inline events until a block boundary, without consuming it.
    fn inline_run(&mut self) -> Vec<Seg> {
        let mut segs = Vec::new();
        let mut nodes: Vec<Value> = Vec::new();
        let mut marks: Vec<Value> = Vec::new();
        let mut depth = 0usize;
        while let Some(event) = self.events.get(self.pos).cloned() {
            match event {
                Event::Text(t) => nodes.push(text(&t, &marks)),
                Event::Code(c) => {
                    let mut with_code = marks.clone();
                    with_code.push(json!({"type": "code"}));
                    nodes.push(text(&c, &with_code));
                }
                Event::InlineHtml(h) | Event::Html(h) => nodes.push(text(&h, &marks)),
                Event::SoftBreak => nodes.push(text(" ", &marks)),
                Event::HardBreak => nodes.push(json!({"type": "hard_break"})),
                Event::FootnoteReference(label) => nodes.push(self.footnote_anchor(&label, &marks)),
                Event::Start(Tag::Emphasis) => {
                    marks.push(json!({"type": "em"}));
                    depth += 1;
                }
                Event::Start(Tag::Strong) => {
                    marks.push(json!({"type": "strong"}));
                    depth += 1;
                }
                Event::Start(Tag::Strikethrough) => {
                    marks.push(json!({"type": "strikethrough"}));
                    depth += 1;
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    marks.push(json!({"type": "link", "attrs": {"href": dest_url.to_string()}}));
                    depth += 1;
                }
                Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link) if depth > 0 => {
                    marks.pop();
                    depth -= 1;
                }
                Event::Start(Tag::Image { dest_url, title, .. }) => {
                    self.pos += 1;
                    let alt = self.plain_until_end();
                    self.pos -= 1; // the shared advance below steps past the image's End
                    let href = marks
                        .iter()
                        .rev()
                        .find(|m| m["type"] == "link")
                        .and_then(|m| m["attrs"]["href"].as_str().map(str::to_string));
                    let before = coalesce(std::mem::take(&mut nodes));
                    if !before.is_empty() {
                        segs.push(Seg::Inline(before));
                    }
                    segs.extend(self.image(&dest_url, &alt, &title, href.as_deref()));
                }
                _ => break,
            }
            self.pos += 1;
        }
        let rest = coalesce(nodes);
        if !rest.is_empty() {
            segs.push(Seg::Inline(rest));
        }
        segs
    }

    fn image(&mut self, src: &str, alt: &str, title: &str, href: Option<&str>) -> Vec<Seg> {
        if let Some(id) = youtube_id(src) {
            return vec![Seg::Block(json!({"type": "youtube2", "attrs": {"videoId": id}}))];
        }
        if is_video(src) {
            // image2 only takes pictures and a video cannot be uploaded this way: link it.
            self.warnings
                .push(format!("video {src} cannot be uploaded; it became a link"));
            let label = if alt.is_empty() {
                src.rsplit('/').next().unwrap_or(src)
            } else {
                alt
            };
            let mark = json!({"type": "link", "attrs": {"href": src}});
            return vec![Seg::Inline(vec![text(label, &[mark])])];
        }
        let caption = match (title.is_empty(), self.options.alt_as_caption && !alt.is_empty()) {
            (false, _) => Some(title),
            (true, true) => Some(alt),
            _ => None,
        };
        let mut content = vec![json!({"type": "image2", "attrs": {
            "src": src,
            "srcNoWatermark": null,
            "fullscreen": false,
            "imageSize": "normal",
            "height": null,
            "width": null,
            "resizeWidth": null,
            "bytes": null,
            "alt": (!alt.is_empty()).then_some(alt),
            "title": null,
            "type": null,
            "href": href,
            "belowTheFold": false,
            "topImage": false,
            "internalRedirect": null,
        }})];
        if let Some(caption) = caption {
            content.push(json!({"type": "caption", "content": [text(caption, &[])]}));
        }
        vec![Seg::Block(json!({"type": "captionedImage", "content": content}))]
    }

    fn footnote_anchor(&mut self, label: &str, marks: &[Value]) -> Value {
        if let Some(number) = self.numbers.get(label) {
            // The editor has one anchor per footnote.
            self.warnings.push(format!(
                "footnote [^{label}] is referenced twice; the repeat became plain text"
            ));
            return text(&format!("[{number}]"), marks);
        }
        let number = self.numbers.len() + 1;
        self.numbers.insert(label.to_string(), number);
        self.refs.push(label.to_string());
        json!({"type": "footnoteAnchor", "attrs": {"number": number}})
    }

    fn paragraph_blocks(&mut self, segs: Vec<Seg>) -> Vec<Value> {
        // A paragraph that is only a YouTube address becomes the embed.
        if let [Seg::Inline(nodes)] = segs.as_slice()
            && let [node] = nodes.as_slice()
        {
            let marks = node["marks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            if let ([mark], Some(body)) = (marks, node["text"].as_str()) {
                let href = mark["attrs"]["href"].as_str().unwrap_or("");
                if mark["type"] == "link"
                    && body == href
                    && let Some(id) = youtube_id(href)
                {
                    return vec![json!({"type": "youtube2", "attrs": {"videoId": id}})];
                }
            }
        }
        segs.into_iter()
            .filter_map(|seg| match seg {
                Seg::Inline(nodes)
                    if nodes
                        .iter()
                        .all(|n| n["text"].as_str().is_some_and(|t| t.trim().is_empty())) =>
                {
                    None
                }
                Seg::Inline(nodes) => Some(paragraph(nodes)),
                Seg::Block(block) => Some(block),
            })
            .collect()
    }

    /// Text up to and including the matching End, flattened: markup is dropped,
    /// breaks become spaces.
    fn plain_until_end(&mut self) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) if depth == 0 => break,
                Event::End(_) => depth -= 1,
                Event::Text(t) | Event::Code(t) | Event::InlineHtml(t) | Event::Html(t) => out.push_str(t),
                Event::SoftBreak | Event::HardBreak => out.push(' '),
                _ => {}
            }
        }
        out
    }

    /// Like `plain_until_end`, but keeps every newline: code and HTML.
    fn plain_until_end_raw(&mut self) -> String {
        let mut out = String::new();
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::End(_) => break,
                Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) | Event::Code(t) => out.push_str(t),
                Event::SoftBreak | Event::HardBreak => out.push('\n'),
                _ => {}
            }
        }
        out
    }

    fn skip_to_end(&mut self) {
        let mut depth = 0usize;
        while let Some(event) = self.events.get(self.pos) {
            self.pos += 1;
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) if depth == 0 => break,
                Event::End(_) => depth -= 1,
                _ => {}
            }
        }
    }
}

fn is_inline(event: &Event) -> bool {
    matches!(
        event,
        Event::Text(_)
            | Event::Code(_)
            | Event::InlineHtml(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::FootnoteReference(_)
            | Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. })
    )
}

fn text(value: &str, marks: &[Value]) -> Value {
    let mut node = Map::new();
    node.insert("type".into(), json!("text"));
    node.insert("text".into(), json!(value));
    if !marks.is_empty() {
        node.insert("marks".into(), Value::Array(marks.to_vec()));
    }
    Value::Object(node)
}

fn paragraph(content: Vec<Value>) -> Value {
    if content.is_empty() {
        json!({"type": "paragraph"})
    } else {
        json!({"type": "paragraph", "content": content})
    }
}

/// Merges neighbouring text nodes that carry the same marks.
fn coalesce(nodes: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for node in nodes {
        if let Some(last) = out.last_mut()
            && last["type"] == "text"
            && node["type"] == "text"
            && last.get("marks") == node.get("marks")
        {
            let merged = format!(
                "{}{}",
                last["text"].as_str().unwrap_or(""),
                node["text"].as_str().unwrap_or("")
            );
            last["text"] = json!(merged);
            continue;
        }
        out.push(node);
    }
    out
}

fn plain_text(node: &Value) -> String {
    if let Some(text) = node["text"].as_str() {
        return text.to_string();
    }
    node["content"]
        .as_array()
        .map(|children| children.iter().map(plain_text).collect())
        .unwrap_or_default()
}

/// `title` and `subtitle` from a YAML block: flat `key: value` lines only.
fn front_matter(block: &str) -> (Option<String>, Option<String>) {
    let (mut title, mut subtitle) = (None, None);
    for line in block.lines() {
        if line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let value = match (value.chars().next(), value.chars().last()) {
            (Some(open @ ('"' | '\'')), Some(close)) if open == close && value.len() >= 2 => &value[1..value.len() - 1],
            _ => value,
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => title = Some(value.to_string()),
            "subtitle" => subtitle = Some(value.to_string()),
            _ => {}
        }
    }
    (title, subtitle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(markdown: &str) -> Document {
        convert(markdown, &Options::default())
    }

    fn content(markdown: &str) -> Value {
        doc(markdown).body["content"].clone()
    }

    #[test]
    fn paragraph_with_marks() {
        let got = content("a **b *c*** ~~d~~ `e`\n");
        assert_eq!(
            got,
            json!([{"type": "paragraph", "content": [
                {"type": "text", "text": "a "},
                {"type": "text", "text": "b ", "marks": [{"type": "strong"}]},
                {"type": "text", "text": "c", "marks": [{"type": "strong"}, {"type": "em"}]},
                {"type": "text", "text": " "},
                {"type": "text", "text": "d", "marks": [{"type": "strikethrough"}]},
                {"type": "text", "text": " "},
                {"type": "text", "text": "e", "marks": [{"type": "code"}]},
            ]}])
        );
    }

    #[test]
    fn link_mark_carries_href_and_nested_marks() {
        let got = content("[**hi**](https://a.example/x?y=1)");
        assert_eq!(
            got[0]["content"][0],
            json!({"type": "text", "text": "hi", "marks": [
                {"type": "link", "attrs": {"href": "https://a.example/x?y=1"}}, {"type": "strong"}]})
        );
    }

    #[test]
    fn soft_break_is_a_space_and_hard_break_a_node() {
        let got = content("one\ntwo  \nthree");
        assert_eq!(
            got[0]["content"],
            json!([{"type": "text", "text": "one two"}, {"type": "hard_break"}, {"type": "text", "text": "three"}])
        );
    }

    #[test]
    fn headings_keep_their_level() {
        let got = content("## Two\n\n### Three");
        assert_eq!(
            got[0],
            json!({"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Two"}]})
        );
        assert_eq!(got[1]["attrs"]["level"], 3);
    }

    #[test]
    fn first_h1_is_the_title_and_leaves_the_body() {
        let d = doc("# My *Title*\n\nBody\n\n# Later");
        assert_eq!(d.title.as_deref(), Some("My Title"));
        let blocks = d.body["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "paragraph");
        assert_eq!(blocks[1]["type"], "heading");
    }

    #[test]
    fn front_matter_wins_and_is_stripped() {
        let d = doc("---\ntitle: \"Hello: World\"\nsubtitle: A sub\ntags: [a]\n---\n\n# Heading stays\n\nBody");
        assert_eq!(d.title.as_deref(), Some("Hello: World"));
        assert_eq!(d.subtitle.as_deref(), Some("A sub"));
        let blocks = d.body["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "heading");
        assert_eq!(blocks.len(), 2);
        assert!(!d.body.to_string().contains("tags"));
    }

    #[test]
    fn front_matter_without_title_falls_back_to_h1() {
        let d = doc("---\nsubtitle: S\n---\n# From H1\n\nx");
        assert_eq!(d.title.as_deref(), Some("From H1"));
        assert_eq!(d.subtitle.as_deref(), Some("S"));
    }

    #[test]
    fn no_title_anywhere() {
        let d = doc("just text");
        assert_eq!(d.title, None);
        assert_eq!(d.subtitle, None);
    }

    #[test]
    fn empty_input_still_has_a_paragraph() {
        assert_eq!(content(""), json!([{"type": "paragraph"}]));
    }

    #[test]
    fn blockquote_holds_paragraphs() {
        let got = content("> a\n>\n> b");
        assert_eq!(got[0]["type"], "blockquote");
        assert_eq!(got[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(got[0]["content"][1]["content"][0]["text"], "b");
    }

    #[test]
    fn rule() {
        assert_eq!(content("a\n\n---\n\nb")[1], json!({"type": "horizontal_rule"}));
    }

    #[test]
    fn tight_bullets_wrap_text_in_paragraphs() {
        let got = content("- a\n- b");
        assert_eq!(
            got[0],
            json!({"type": "bullet_list", "content": [
                {"type": "list_item", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "a"}]}]},
                {"type": "list_item", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "b"}]}]},
            ]})
        );
    }

    #[test]
    fn loose_and_nested_lists() {
        let got = content("1. one\n   - inner\n   - inner2\n\n2. two\n");
        let list = &got[0];
        assert_eq!(list["type"], "ordered_list");
        assert!(list.get("attrs").is_none());
        let first = &list["content"][0]["content"];
        assert_eq!(first[0]["type"], "paragraph");
        assert_eq!(first[1]["type"], "bullet_list");
        assert_eq!(first[1]["content"].as_array().unwrap().len(), 2);
        assert_eq!(list["content"][1]["content"][0]["content"][0]["text"], "two");
    }

    #[test]
    fn ordered_list_start() {
        let got = content("3. c\n4. d");
        assert_eq!(got[0]["attrs"], json!({"start": 3, "order": 3}));
    }

    #[test]
    fn fenced_code_with_language() {
        let got = content("```Rust ignore\nfn main() {}\n\n```\n");
        assert_eq!(
            got[0],
            json!({"type": "highlighted_code_block", "attrs": {"language": "rust"},
                   "content": [{"type": "text", "text": "fn main() {}\n"}]})
        );
    }

    #[test]
    fn code_without_language_has_no_attrs() {
        let got = content("```\nx\n```");
        assert!(got[0].get("attrs").is_none());
        assert_eq!(got[0]["content"][0]["text"], "x");
    }

    #[test]
    fn code_block_node_name_is_switchable() {
        for (node, name) in [
            (CodeBlockNode::Legacy, "code_block"),
            (CodeBlockNode::CamelCase, "codeBlock"),
        ] {
            let d = convert(
                "```js\nx\n```",
                &Options {
                    code_block: node,
                    ..Options::default()
                },
            );
            assert_eq!(d.body["content"][0]["type"], name);
        }
    }

    #[test]
    fn standalone_image_is_a_block_with_alt() {
        let got = content("![a cat](pics/cat.png)");
        assert_eq!(got[0]["type"], "captionedImage");
        let image = &got[0]["content"][0];
        assert_eq!(image["type"], "image2");
        assert_eq!(image["attrs"]["src"], "pics/cat.png");
        assert_eq!(image["attrs"]["alt"], "a cat");
        assert_eq!(got[0]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn title_becomes_caption_and_alt_can_too() {
        let got = content("![alt](a.png \"Figure 1\")");
        assert_eq!(
            got[0]["content"][1],
            json!({"type": "caption", "content": [{"type": "text", "text": "Figure 1"}]})
        );
        let d = convert(
            "![alt](a.png)",
            &Options {
                alt_as_caption: true,
                ..Options::default()
            },
        );
        assert_eq!(d.body["content"][0]["content"][1]["content"][0]["text"], "alt");
    }

    #[test]
    fn image_inside_text_splits_the_paragraph() {
        let got = content("before ![x](a.png) after");
        let types: Vec<&str> = got
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["paragraph", "captionedImage", "paragraph"]);
        assert_eq!(got[0]["content"][0]["text"], "before ");
        assert_eq!(got[2]["content"][0]["text"], " after");
    }

    #[test]
    fn linked_image_sets_href_and_leaves_no_empty_text() {
        let got = content("[![x](a.png)](https://e.example)");
        assert_eq!(got.as_array().unwrap().len(), 1);
        assert_eq!(got[0]["content"][0]["attrs"]["href"], "https://e.example");
    }

    #[test]
    fn local_and_remote_images() {
        let mut d = doc("![](a.png)\n\n![](https://x.example/b.png)\n\n![](a.png)\n\n![](data:image/png;base64,AAAA)");
        assert_eq!(d.local_images(), ["a.png"]);
        d.set_image(
            "a.png",
            &Hosted {
                url: "https://cdn.example/a".into(),
                width: Some(800),
                height: Some(600),
            },
        );
        assert!(d.local_images().is_empty());
        let attrs = &d.body["content"][2]["content"][0]["attrs"];
        assert_eq!(attrs["src"], "https://cdn.example/a");
        assert_eq!(attrs["width"], 800);
        assert_eq!(attrs["height"], 600);
        assert_eq!(
            d.body["content"][1]["content"][0]["attrs"]["src"],
            "https://x.example/b.png"
        );
    }

    #[test]
    fn local_video_falls_back_to_a_link() {
        let d = doc("![demo](clips/demo.MP4)\n\n![](x.webm)");
        assert_eq!(
            d.body["content"][0],
            json!({"type": "paragraph", "content": [{"type": "text", "text": "demo",
                "marks": [{"type": "link", "attrs": {"href": "clips/demo.MP4"}}]}]})
        );
        assert_eq!(d.body["content"][1]["content"][0]["text"], "x.webm");
        assert_eq!(d.warnings.len(), 2);
        assert!(d.local_images().is_empty());
    }

    #[test]
    fn youtube_image_and_bare_link_become_embeds() {
        let d = doc(
            "![](https://youtu.be/dQw4w9WgXcQ?t=3)\n\n<https://www.youtube.com/watch?v=dQw4w9WgXcQ&x=1>\n\n[a video](https://youtu.be/dQw4w9WgXcQ)",
        );
        let c = d.body["content"].as_array().unwrap();
        assert_eq!(c[0], json!({"type": "youtube2", "attrs": {"videoId": "dQw4w9WgXcQ"}}));
        assert_eq!(c[1], json!({"type": "youtube2", "attrs": {"videoId": "dQw4w9WgXcQ"}}));
        assert_eq!(c[2]["type"], "paragraph");
    }

    #[test]
    fn youtube_ids() {
        assert_eq!(
            youtube_id("https://www.youtube.com/embed/abcDEF123_-").as_deref(),
            Some("abcDEF123_-")
        );
        assert_eq!(youtube_id("https://example.com/watch?v=abcdefghijk"), None);
        assert_eq!(youtube_id("https://youtu.be/"), None);
    }

    #[test]
    fn footnotes_number_by_reference_and_follow_their_block() {
        let md = "First[^b] and second[^a].\n\nNext paragraph.\n\n[^a]: Note A\n\n[^b]: Note **B**\n";
        let d = doc(md);
        let c = d.body["content"].as_array().unwrap();
        let types: Vec<&str> = c.iter().map(|n| n["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["paragraph", "footnote", "footnote", "paragraph"]);
        assert_eq!(
            c[0]["content"][1],
            json!({"type": "footnoteAnchor", "attrs": {"number": 1}})
        );
        assert_eq!(
            c[0]["content"][3],
            json!({"type": "footnoteAnchor", "attrs": {"number": 2}})
        );
        assert_eq!(c[1]["attrs"]["number"], 1);
        assert_eq!(c[1]["content"][0]["content"][1]["marks"], json!([{"type": "strong"}]));
        assert_eq!(c[2]["content"][0]["content"][0]["text"], "Note A");
        assert!(d.warnings.is_empty());
    }

    #[test]
    fn repeated_footnote_becomes_text_and_undefined_stays_literal() {
        let d = doc("a[^x] b[^x] c[^y]\n\n[^x]: X\n");
        assert_eq!(d.warnings.len(), 1);
        let c = d.body["content"].as_array().unwrap();
        assert_eq!(
            c[0]["content"][1],
            json!({"type": "footnoteAnchor", "attrs": {"number": 1}})
        );
        assert_eq!(c[0]["content"][2]["text"], " b[1] c[^y]");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn table_becomes_an_aligned_code_block() {
        let d = doc("| a | bb |\n|---|:-:|\n| 1 | 2222 |\n| héé | 3 |\n");
        let block = &d.body["content"][0];
        assert_eq!(block["type"], "highlighted_code_block");
        assert_eq!(
            block["content"][0]["text"],
            "| a   | bb   |\n| --- | ---- |\n| 1   | 2222 |\n| héé | 3    |"
        );
        assert_eq!(d.warnings.len(), 1);
    }

    #[test]
    fn table_cells_flatten_markup() {
        let d = doc("| h |\n|---|\n| **x** [l](u) `c` |\n");
        assert!(
            d.body["content"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("| x l c |")
        );
    }

    #[test]
    fn html_comment_dropped_other_html_kept_as_text() {
        let d = doc("<!-- note -->\n\n<div>hi</div>\n\nok");
        let c = d.body["content"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["content"][0]["text"], "<div>hi</div>");
        assert_eq!(d.warnings.len(), 1);
    }

    #[test]
    fn json_is_valid_prosemirror_shaped() {
        // Every text node has text; every non-text, non-leaf node has content.
        let d = doc("# T\n\nx **y** [z](http://a)\n\n- a\n  - b\n\n> q\n\n```c\nx\n```\n\n![i](a.png)\n");
        walk(&d.body, &mut |n| {
            if n["type"] == "text" {
                assert!(n["text"].is_string() && !n["text"].as_str().unwrap().is_empty());
            }
            if matches!(n["type"].as_str(), Some("bullet_list" | "list_item" | "blockquote")) {
                assert!(n["content"].is_array());
            }
        });
    }
}
