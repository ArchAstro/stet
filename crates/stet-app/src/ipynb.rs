//! Jupyter's `.ipynb` in and out, and a notebook as one HTML page.
//!
//! `stet import nb.ipynb` writes `nb.md`: markdown cells become prose, code
//! cells become fenced blocks, and the outputs go to the sidecar
//! (`.stet/nb.md.out.json`, pictures beside it) so they show at once.
//! `stet export doc.md --ipynb` goes back; fences in other languages than
//! the kernel's stay as prose, so Jupyter never runs them as its own.
//! `stet export doc.md --html` is a page to read: prose, code and outputs,
//! with charts drawn as SVG.

use serde_json::{Map, Value, json};
use std::path::Path;
use stet_core::Editor;
use stet_core::notebook::{self, Cell, Item, Notebook};

/// Fences that are never code for a kernel.
const NOT_CODE: &[&str] = &["", "chart", "raw", "text", "markdown", "md", "output", "mermaid"];

/// A document cut into prose and its fenced blocks, in order.
enum Part {
    Prose(String),
    Code(Cell),
}

fn parts(text: &str) -> Vec<Part> {
    let doc = stet_core::markdown::parse(text);
    let lines: Vec<&str> = text.split('\n').collect();
    let cells = notebook::cells(&lines, &doc.fences);
    let mut parts = Vec::new();
    let prose = |from: usize, to: usize, parts: &mut Vec<Part>| {
        let text = lines[from.min(lines.len())..to.min(lines.len())].join("\n");
        let text = text.trim_matches('\n');
        if !text.trim().is_empty() {
            parts.push(Part::Prose(text.to_string()));
        }
    };
    let mut at = 0;
    for cell in cells {
        prose(at, cell.fence.start, &mut parts);
        at = cell.fence.end;
        parts.push(Part::Code(cell));
    }
    prose(at, lines.len(), &mut parts);
    parts
}

/// The language most of the document's code is in: its kernel's.
fn main_language(parts: &[Part]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for part in parts {
        if let Part::Code(cell) = part
            && !NOT_CODE.contains(&cell.info.lang.as_str())
        {
            match counts.iter_mut().find(|(lang, _)| *lang == cell.info.lang) {
                Some((_, count)) => *count += 1,
                None => counts.push((cell.info.lang.clone(), 1)),
            }
        }
    }
    // The first seen wins a tie.
    let most = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
    counts
        .into_iter()
        .find(|(_, count)| *count == most)
        .map_or("python".into(), |(lang, _)| lang)
}

/// nbformat's text: a string, or a list of lines.
fn joined(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// Text as nbformat lists it: lines, each but the last with its newline.
fn lines_of(text: &str) -> Value {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    if lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    json!(lines)
}

// --- Import ------------------------------------------------------------------------

/// A notebook as markdown, and its outputs as the sidecar holds them (with
/// pictures still inline).
pub fn import(ipynb: &Value) -> Result<(String, Value), String> {
    let cells = ipynb["cells"]
        .as_array()
        .ok_or("this is not a Jupyter notebook: it has no cells")?;
    let meta = &ipynb["metadata"];
    let lang = meta["kernelspec"]["language"]
        .as_str()
        .or(meta["language_info"]["name"].as_str())
        .unwrap_or("python")
        .to_lowercase();
    let mut blocks: Vec<String> = Vec::new();
    // Each code cell's code and what it produced, in order.
    let mut ran: Vec<(String, Option<u64>, Vec<Value>)> = Vec::new();
    for cell in cells {
        let source = joined(&cell["source"]);
        match cell["cell_type"].as_str() {
            Some("markdown") if !source.trim().is_empty() => {
                blocks.push(examples_kept(source.trim_matches('\n')));
            }
            Some("code") => {
                let code = source.trim_end_matches('\n');
                blocks.push(fenced(&lang, code));
                let outputs = cell["outputs"].as_array().cloned().unwrap_or_default();
                ran.push((code.to_string(), cell["execution_count"].as_u64(), outputs));
            }
            // Raw cells pass through untouched, in a fence of their own.
            Some("raw") if !source.trim().is_empty() => blocks.push(fenced("raw", source.trim_end_matches('\n'))),
            _ => {}
        }
    }
    let mut text = blocks.join("\n\n");
    text.push('\n');

    // Outputs go to the cells as stet counts them: markdown cells can hold
    // fences too. Code is matched in order.
    let stet_cells: Vec<Cell> = parts(&text)
        .into_iter()
        .filter_map(|part| match part {
            Part::Code(cell) => Some(cell),
            Part::Prose(_) => None,
        })
        .collect();
    let mut slots = Vec::new();
    let mut from = 0;
    for (code, count, outputs) in ran {
        let hash = notebook::code_hash(&code);
        let Some(index) = (from..stet_cells.len()).find(|&at| stet_cells[at].hash == hash) else {
            continue;
        };
        from = index + 1;
        let items: Vec<Item> = outputs.iter().filter_map(Item::from_json).collect();
        if items.is_empty() && count.is_none() {
            continue;
        }
        let failed = items.iter().any(|item| matches!(item, Item::Error { .. }));
        let count = count.map(|count| count as usize);
        slots.push(json!({
            "id": stet_cells[index].info.id,
            "hash": hash,
            "index": index,
            "language": lang,
            "status": if failed { "error" } else { "ok" },
            "execution_count": count,
            "ran_by": null,
            "started_ms": null,
            "took_ms": null,
            "outputs": items.iter().map(|item| item.to_json(count)).collect::<Vec<_>>(),
        }));
    }
    Ok((text, json!({ "stet_notebook": 1, "cells": slots })))
}

/// A markdown cell with its code examples marked `.norun`: in Jupyter they
/// never ran, and in stet they would be cells.
fn examples_kept(text: &str) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    for part in parts(text) {
        if let Part::Code(cell) = part
            && !NOT_CODE.contains(&cell.info.lang.as_str())
            && !cell.info.has_class("norun")
        {
            let line = &mut lines[cell.fence.start];
            *line = match line.find('{') {
                Some(at) => format!("{}{{.norun {}", &line[..at], &line[at + 1..]),
                None => format!("{line} {{.norun}}"),
            };
        }
    }
    lines.join("\n")
}

/// The opposite of [`examples_kept`], for one fence's opening line.
fn example_restored(line: &str) -> String {
    match line.strip_suffix(" {.norun}") {
        Some(line) => line.to_string(),
        None => line.replacen("{.norun ", "{", 1),
    }
}

/// `code` in a fence longer than any run of backticks inside it.
fn fenced(lang: &str, code: &str) -> String {
    let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat((longest + 1).max(3));
    format!("{ticks}{lang}\n{code}\n{ticks}")
}

/// Import `source` to `out`, pictures to files beside it. Says what it did.
pub fn import_file(source: &Path, out: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(source).map_err(|err| format!("{}: {err}", source.display()))?;
    let ipynb: Value = serde_json::from_str(&text).map_err(|err| format!("{}: {err}", source.display()))?;
    let (markdown, mut sidecar) = import(&ipynb)?;
    if out.exists() {
        return Err(format!("{} exists; name another file to write", out.display()));
    }
    let files = notebook::files_dir(out);
    for slot in sidecar["cells"].as_array_mut().into_iter().flatten() {
        for output in slot["outputs"].as_array_mut().into_iter().flatten() {
            if let Some(data) = output.get_mut("data").and_then(Value::as_object_mut) {
                notebook::store_files(data, &files);
            }
        }
    }
    std::fs::write(out, &markdown).map_err(|err| format!("{}: {err}", out.display()))?;
    let notebook = Notebook::from_json(&sidecar);
    let doc = stet_core::markdown::parse(&markdown);
    let lines: Vec<&str> = markdown.split('\n').collect();
    let cells = notebook::cells(&lines, &doc.fences);
    notebook::save(out, &notebook, &cells).map_err(|err| format!("{}: {err}", out.display()))?;
    Ok(format!(
        "{} cells, {} with outputs → {}",
        cells.len(),
        notebook.slots.len(),
        out.display()
    ))
}

// --- Export ------------------------------------------------------------------------

/// A cell's outputs, its execution count, and whether its code changed since.
type Ran<'a> = (&'a [Item], Option<usize>, bool);

/// The outputs of each of `cells`, if it ran.
fn outputs_of<'a>(notebook: &'a Notebook, cells: &[Cell]) -> Vec<Option<Ran<'a>>> {
    notebook
        .matching(cells)
        .into_iter()
        .map(|found| {
            found.map(|found| {
                let slot = &notebook.slots[found.slot];
                (slot.outputs.as_slice(), slot.count, found.stale)
            })
        })
        .collect()
}

/// The document as a Jupyter notebook. Pictures come back inline from
/// `files`, the document's output folder.
pub fn export_ipynb(text: &str, notebook: &Notebook, files: Option<&Path>) -> Value {
    let parts = parts(text);
    let lang = main_language(&parts);
    let code_cells: Vec<Cell> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Code(cell) => Some(cell.clone()),
            Part::Prose(_) => None,
        })
        .collect();
    let outputs = outputs_of(notebook, &code_cells);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut cells: Vec<Value> = Vec::new();
    let mut prose: Vec<String> = Vec::new();
    let mut code_at = 0;
    let flush = |prose: &mut Vec<String>, cells: &mut Vec<Value>| {
        if !prose.is_empty() {
            let source = prose.join("\n\n");
            cells.push(json!({
                "cell_type": "markdown",
                "id": format!("cell-{}", cells.len() + 1),
                "metadata": {},
                "source": lines_of(&source),
            }));
            prose.clear();
        }
    };
    for part in &parts {
        match part {
            Part::Prose(text) => prose.push(text.clone()),
            Part::Code(cell) if cell.info.lang != lang || cell.info.has_class("norun") => {
                // Another language's block, or an example: kept as written.
                let end = cell.fence.end.min(lines.len());
                let mut block: Vec<String> = lines[cell.fence.start..end]
                    .iter()
                    .map(|line| line.to_string())
                    .collect();
                block[0] = example_restored(&block[0]);
                prose.push(block.join("\n"));
                code_at += 1;
            }
            Part::Code(cell) => {
                flush(&mut prose, &mut cells);
                let (items, count) = match outputs[code_at] {
                    Some((items, count, _)) => (items, count),
                    None => (&[][..], None),
                };
                let outputs: Vec<Value> = items
                    .iter()
                    .map(|item| {
                        let mut value = item.to_json(count);
                        if let (Some(files), Some(data)) = (files, value.get_mut("data").and_then(Value::as_object_mut))
                        {
                            notebook::load_files(data, files);
                        }
                        value
                    })
                    .collect();
                let mut metadata = Map::new();
                if let Some(id) = &cell.info.id {
                    metadata.insert("tags".into(), json!([id]));
                }
                cells.push(json!({
                    "cell_type": "code",
                    "id": format!("cell-{}", cells.len() + 1),
                    "metadata": metadata,
                    "execution_count": count,
                    "source": lines_of(&cell.code),
                    "outputs": outputs,
                }));
                code_at += 1;
            }
        }
    }
    flush(&mut prose, &mut cells);
    let kernelspec = match lang.as_str() {
        "python" => json!({ "name": "python3", "display_name": "Python 3 (ipykernel)", "language": "python" }),
        other => json!({ "name": other, "display_name": other, "language": other }),
    };
    json!({
        "cells": cells,
        "metadata": { "kernelspec": kernelspec, "language_info": { "name": lang } },
        "nbformat": 4,
        "nbformat_minor": 5,
    })
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn markdown_html(text: &str) -> String {
    let parser = pulldown_cmark::Parser::new_ext(
        text,
        pulldown_cmark::Options::ENABLE_TABLES
            | pulldown_cmark::Options::ENABLE_FOOTNOTES
            | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
            | pulldown_cmark::Options::ENABLE_TASKLISTS,
    );
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    out
}

/// What draws charts and finds data for the HTML page.
struct Page<'a> {
    ed: &'a Editor,
    files: Option<&'a Path>,
}

impl Page<'_> {
    fn chart(&self, spec: &Value) -> Result<String, String> {
        let theme = crate::notebook::chart_theme(self.ed, 11.0);
        let doc = self.ed.doc();
        let mut resolve = |name: &str| {
            let data = doc.data_ref(name)?;
            let path = crate::data::resolve(&data.path, self.ed.path.as_deref());
            let peek = stet_data::peek(&path, data.table.as_deref(), crate::data::CHART_ROWS).ok()?;
            Some(stet_chart::Table {
                columns: peek.columns.iter().map(|column| column.name.clone()).collect(),
                rows: peek.rows,
            })
        };
        let chart = stet_chart::draw(spec, &theme, 680.0, &mut resolve)?;
        Ok(format!(
            "<figure class=\"chart\"><img alt=\"chart\" src=\"data:image/svg+xml;base64,{}\"></figure>",
            notebook::base64_encode(chart.svg.as_bytes())
        ))
    }

    fn table(&self, table: &Value) -> String {
        let cell = |value: &Value| match value {
            Value::String(text) => escape(text),
            Value::Null => String::new(),
            other => escape(&other.to_string()),
        };
        let mut out = String::from("<table><thead><tr>");
        for column in table["columns"].as_array().into_iter().flatten() {
            out.push_str(&format!("<th>{}</th>", cell(column)));
        }
        out.push_str("</tr></thead><tbody>");
        let rows = table["rows"].as_array().cloned().unwrap_or_default();
        for row in &rows {
            out.push_str("<tr>");
            for value in row.as_array().into_iter().flatten() {
                let class = if value.is_number() { " class=\"n\"" } else { "" };
                out.push_str(&format!("<td{class}>{}</td>", cell(value)));
            }
            out.push_str("</tr>");
        }
        out.push_str("</tbody></table>");
        let total = table["total"].as_u64().unwrap_or(rows.len() as u64);
        if total > rows.len() as u64 {
            out.push_str(&format!("<p class=\"note\">{} of {total} rows</p>", rows.len()));
        }
        out
    }

    /// The richest form of an output that a page can show.
    fn data(&self, data: &Map<String, Value>) -> String {
        let picture = |mime: &str| -> Option<String> {
            let value = data.get(mime)?;
            let encoded = match (notebook::file_ref(value), value.as_str()) {
                (Some(name), _) => notebook::base64_encode(&std::fs::read(self.files?.join(name)).ok()?),
                (None, Some(text)) if mime == "image/svg+xml" => notebook::base64_encode(text.as_bytes()),
                (None, Some(text)) => text.replace('\n', ""),
                _ => return None,
            };
            Some(format!("<img alt=\"output\" src=\"data:{mime};base64,{encoded}\">"))
        };
        // A plotting library's own picture is the chart as its author styled
        // it; the native chart is drawn when there is no picture.
        for mime in ["image/png", "image/jpeg", "image/gif", "image/svg+xml"] {
            if let Some(picture) = picture(mime) {
                return picture;
            }
        }
        let chart = data
            .iter()
            .find(|(mime, _)| *mime == stet_chart::MIME || mime.starts_with("application/vnd.vegalite.v"))
            .and_then(|(_, spec)| self.chart(spec).ok());
        if let Some(chart) = chart {
            return chart;
        }
        if let Some(table) = data.get(stet_kernel::TABLE) {
            return self.table(table);
        }
        if let Some(html) = data.get("text/html") {
            return format!("<div class=\"html\">{}</div>", joined(html));
        }
        if let Some(markdown) = data.get("text/markdown") {
            return markdown_html(&joined(markdown));
        }
        match data.get("text/plain") {
            Some(text) => format!("<pre class=\"out\">{}</pre>", escape(&plain(&joined(text)))),
            None => String::new(),
        }
    }

    fn item(&self, item: &Item) -> String {
        match item {
            Item::Stream { stderr, text } => {
                let class = if *stderr { "out err" } else { "out" };
                format!("<pre class=\"{class}\">{}</pre>", escape(&plain(text)))
            }
            Item::Display { data } | Item::Result { data } => self.data(data),
            Item::Error { name, value, traceback } => {
                let trace = traceback.iter().map(|part| plain(part)).collect::<Vec<_>>().join("\n");
                let trace = if trace.trim().is_empty() {
                    format!("{name}: {value}")
                } else {
                    trace
                };
                format!("<pre class=\"out err\">{}</pre>", escape(&trace))
            }
        }
    }
}

fn plain(text: &str) -> String {
    crate::view::outputs::plain_lines(text).join("\n")
}

/// Which cells' code a page shows. Outputs always show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Code {
    /// Every cell's code.
    All,
    /// The code of cells that are not folded: the page is what the window shows.
    #[default]
    AsFolded,
    /// No code, only prose and outputs.
    None,
}

/// The document as one page: prose, code, and what each cell last showed.
pub fn export_html(ed: &Editor, notebook: &Notebook, files: Option<&Path>, code: Code) -> String {
    let text = ed.buf.text();
    let parts = parts(without_front_matter(&text));
    let code_cells: Vec<Cell> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Code(cell) => Some(cell.clone()),
            Part::Prose(_) => None,
        })
        .collect();
    let outputs = outputs_of(notebook, &code_cells);
    let page = Page { ed, files };
    // Folds as the window has them, else as last saved.
    let folded = ed
        .notebook()
        .filter(|book| book.on)
        .map_or(&notebook.folded, |book| &book.notebook.folded);
    let mut body = String::new();
    let mut code_at = 0;
    for part in &parts {
        match part {
            Part::Prose(text) => body.push_str(&markdown_html(text)),
            Part::Code(cell) => {
                let lang = &cell.info.lang;
                let spec = (lang == "chart")
                    .then(|| serde_json::from_str::<Value>(&cell.code).ok())
                    .flatten();
                match spec.map(|spec| page.chart(&spec)) {
                    Some(Ok(chart)) => body.push_str(&chart),
                    _ if code == Code::None || (code == Code::AsFolded && folded.contains(&cell.fold_key())) => {}
                    other => {
                        body.push_str(&format!(
                            "<pre class=\"code\"><code class=\"language-{}\">{}</code></pre>",
                            escape(lang),
                            escape(&cell.code)
                        ));
                        if let Some(Err(err)) = other {
                            body.push_str(&format!("<p class=\"note\">chart: {}</p>", escape(&err)));
                        }
                    }
                }
                if let Some((items, _, stale)) = outputs[code_at] {
                    let class = if stale { "outputs stale" } else { "outputs" };
                    body.push_str(&format!("<div class=\"{class}\">"));
                    for item in items {
                        body.push_str(&page.item(item));
                    }
                    body.push_str("</div>");
                }
                code_at += 1;
            }
        }
    }
    let title = text
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .or_else(|| {
            ed.path
                .as_deref()
                .and_then(Path::file_stem)
                .and_then(|stem| stem.to_str())
        })
        .unwrap_or("Notebook");
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{}</title><style>{STYLE}</style></head><body><main>{body}</main></body></html>\n",
        escape(title)
    )
}

/// `text` after its front matter, which is settings, not something to read.
fn without_front_matter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    let end = rest
        .match_indices("\n---")
        .map(|(at, _)| at + 4)
        .find(|&end| rest[end..].is_empty() || rest[end..].starts_with('\n'));
    end.map_or(text, |end| &rest[end..])
}

const STYLE: &str = ":root{--bg:#fbf7f1;--fg:#1f1d1a;--muted:#7a746b;--code:#f3eee5;--rule:#e4ddd1;--err:#b4382c}\
@media (prefers-color-scheme:dark){:root{--bg:#1b1a18;--fg:#ebe6dd;--muted:#9a9388;--code:#262420;--rule:#36332e;--err:#e0705f}}\
body{margin:0;background:var(--bg);color:var(--fg);font:17px/1.6 ui-serif,Georgia,serif}\
main{max-width:46rem;margin:0 auto;padding:3rem 16px 5rem}\
h1,h2,h3{font-family:ui-sans-serif,system-ui,sans-serif;line-height:1.25}\
pre,code{font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}\
pre.code{background:var(--code);padding:.8rem 1rem;border-radius:6px;overflow-x:auto}\
.outputs{border-left:2px solid var(--rule);padding-left:1rem;margin:0 0 1.5rem}\
.outputs.stale{opacity:.6}\
pre.out{margin:.4rem 0;white-space:pre-wrap}pre.err{color:var(--err)}\
.outputs img,.chart img{max-width:100%;height:auto}figure.chart{margin:1rem 0}\
table{border-collapse:collapse;font:14px/1.4 ui-monospace,Menlo,monospace;margin:.5rem 0;display:block;overflow-x:auto}\
th{text-align:left;border-bottom:1px solid var(--rule)}th,td{padding:.2rem .8rem .2rem 0}td.n{text-align:right}\
tbody tr:nth-child(even){background:var(--code)}.note{color:var(--muted);font-size:14px}\
a{color:inherit}\
@media print{:root{--bg:#fff;--fg:#1f1d1a;--muted:#6b665e;--code:#f4f1ec;--rule:#ddd6ca;--err:#b4382c}\
body{font-size:11pt}main{max-width:none;padding:0}pre.code,pre.out{white-space:pre-wrap}\
table{display:table}pre,figure,img,table,.outputs{break-inside:avoid}h1,h2,h3{break-after:avoid}}";

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "cells": [
                {"cell_type": "markdown", "metadata": {}, "source": ["# Title\n", "\n", "Some *prose*."]},
                {"cell_type": "code", "execution_count": 3, "metadata": {}, "source": ["x = 1\n", "print(x)"],
                 "outputs": [{"output_type": "stream", "name": "stdout", "text": ["1\n"]}]},
                {"cell_type": "markdown", "metadata": {}, "source": "An example:\n\n```python\nnot run\n```"},
                {"cell_type": "code", "execution_count": 4, "metadata": {}, "source": "x + 1",
                 "outputs": [{"output_type": "execute_result", "execution_count": 4, "metadata": {},
                              "data": {"text/plain": "2"}}]},
                {"cell_type": "code", "execution_count": null, "metadata": {}, "source": "s = '```'", "outputs": []},
            ],
            "metadata": {"kernelspec": {"name": "python3", "language": "python"}},
            "nbformat": 4, "nbformat_minor": 5
        })
    }

    #[test]
    fn notebooks_import_as_markdown_with_outputs_beside() {
        let (text, sidecar) = import(&sample()).unwrap();
        assert!(text.starts_with("# Title\n\nSome *prose*.\n\n```python\nx = 1\nprint(x)\n```\n"));
        assert!(text.contains("````python\ns = '```'\n````"), "a longer fence: {text}");
        let slots = sidecar["cells"].as_array().unwrap();
        // The fence inside the markdown cell counts as a cell: `x + 1` is the third.
        assert_eq!(slots.len(), 2);
        assert_eq!(
            (slots[0]["index"].as_u64(), slots[1]["index"].as_u64()),
            (Some(0), Some(2))
        );
        assert_eq!(slots[1]["outputs"][0]["data"]["text/plain"], "2");
        let notebook = Notebook::from_json(&sidecar);
        let lines: Vec<&str> = text.split('\n').collect();
        let cells = notebook::cells(&lines, &stet_core::markdown::parse(&text).fences);
        let found = notebook.matching(&cells);
        assert!(found[0].is_some_and(|found| !found.stale) && found[1].is_none() && found[2].is_some());
    }

    #[test]
    fn notebooks_round_trip_with_no_code_lost() {
        let (text, sidecar) = import(&sample()).unwrap();
        let back = export_ipynb(&text, &Notebook::from_json(&sidecar), None);
        let code: Vec<String> = back["cells"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|cell| cell["cell_type"] == "code")
            .map(|cell| joined(&cell["source"]))
            .collect();
        assert_eq!(code, ["x = 1\nprint(x)", "x + 1", "s = '```'"]);
        let first = &back["cells"][1];
        assert_eq!(
            (first["execution_count"].as_u64(), joined(&first["outputs"][0]["text"])),
            (Some(3), "1\n".into())
        );
        // The example fence stays prose, inside its markdown cell.
        assert!(joined(&back["cells"][2]["source"]).contains("An example:\n\n```python\nnot run\n```"));
        assert!(text.contains("```python {.norun}\nnot run"), "{text}");
        assert_eq!(back["metadata"]["kernelspec"]["name"], "python3");
    }

    #[test]
    fn pages_show_code_as_asked_and_skip_front_matter() {
        let mut ed = Editor::new(
            stet_core::Config::default(),
            Box::new(stet_core::MemoryClipboard::default()),
        );
        ed.set_text("---\nkernel: python3\n---\n\n# Report\n\n```python {#load}\nloaded = 1\n```\n\n```python {#plot}\nplotted = 2\n```\n");
        ed.refresh();
        ed.run_command("fold load");
        let page = |code| export_html(&ed, &Notebook::default(), None, code);
        let folded = page(Code::AsFolded);
        assert!(!folded.contains("kernel:"), "front matter is settings, not prose");
        assert!(folded.contains("<h1>Report</h1>") || folded.contains("Report</h1>"));
        assert!(
            !folded.contains("loaded = 1") && folded.contains("plotted = 2"),
            "a folded cell's code stays out"
        );
        assert!(page(Code::All).contains("loaded = 1"));
        let none = page(Code::None);
        assert!(!none.contains("loaded = 1") && !none.contains("plotted = 2"));
    }

    #[test]
    fn a_plots_own_picture_wins_over_its_native_chart() {
        let ed = Editor::new(
            stet_core::Config::default(),
            Box::new(stet_core::MemoryClipboard::default()),
        );
        let page = Page { ed: &ed, files: None };
        let data = json!({stet_chart::MIME: {"mark": "bar"}, "image/svg+xml": "<svg/>"});
        assert!(
            page.data(data.as_object().unwrap())
                .contains("data:image/svg+xml;base64,")
        );
    }

    #[test]
    fn other_languages_stay_prose_in_ipynb() {
        let text = "```python\na\n```\n\n```chart\n{}\n```\n\n```python\nb\n```\n";
        let back = export_ipynb(text, &Notebook::default(), None);
        let kinds: Vec<&str> = back["cells"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cell| cell["cell_type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["code", "markdown", "code"]);
    }
}
