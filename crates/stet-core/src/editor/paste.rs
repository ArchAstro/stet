//! The clipboard beyond plain text. Copying offers other programs an HTML
//! rendering beside the Markdown; pasting turns what they offer (a web
//! page's HTML, a picture, files) into Markdown, and keeps pictures as
//! files beside the document.

use super::{Editor, Mode};
use crate::html;
use std::path::{Path, PathBuf};

/// Everything the system clipboard offers except a picture, which is only
/// fetched when it is wanted.
#[derive(Clone, Debug, Default)]
pub struct Clip {
    pub text: Option<String>,
    pub html: Option<String>,
    /// Files copied in a file manager.
    pub files: Vec<PathBuf>,
}

/// What a paste is about to insert.
pub(super) struct Pasted {
    pub text: String,
    /// Block content (a table, a list, several paragraphs) that belongs on
    /// lines of its own.
    pub block: bool,
    /// Converted from something richer than text.
    pub rich: bool,
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "tif", "tiff"];

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "webm", "mkv", "avi"];

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.contains(&ext.to_ascii_lowercase().as_str()))
}

pub fn is_picture(path: &Path) -> bool {
    has_extension(path, IMAGE_EXTENSIONS)
}

/// A video is embedded the way a picture is, and shown as a frame to click.
pub fn is_video(path: &Path) -> bool {
    has_extension(path, VIDEO_EXTENSIONS)
}

/// A path as a link destination.
pub(super) fn encode(path: &str) -> String {
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_string()
    };
    path.replace('%', "%25")
        .replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

/// Starts with something that only means what it should at the start of a line.
fn starts_block(text: &str) -> bool {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    text.starts_with('|')
        || text.starts_with("```")
        || text.starts_with("> ")
        || text.starts_with("- ")
        || text.starts_with("---")
        || text.trim_start_matches('#').starts_with(' ') && text.starts_with('#')
        || digits > 0 && text[digits..].starts_with(". ")
}

/// A list item or a quote can follow another line directly; anything else
/// needs a blank line before it to stay its own block.
fn continues(text: &str) -> bool {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    text.starts_with("- ") || text.starts_with("> ") || digits > 0 && text[digits..].starts_with(". ")
}

/// A file name for a new version of the picture at `url`, without the
/// mark an earlier version left on it.
fn picture_name(url: &str) -> String {
    let file = url.split(['?', '#']).next().unwrap_or_default();
    let file = file.rsplit(['/', '\\']).next().unwrap_or_default();
    let stem = match file.rsplit_once('.') {
        _ if url.starts_with("data:") => "",
        Some((stem, _)) => stem,
        None => file,
    };
    let stem: String = stem
        .replace("%20", "-")
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-');
    let stem = match stem.rsplit_once("-edit") {
        Some((before, after))
            if after.is_empty()
                || after
                    .strip_prefix('-')
                    .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            before
        }
        _ => stem,
    };
    if stem.is_empty() { "picture" } else { stem }.to_string()
}

fn single_url(text: &str) -> Option<&str> {
    let text = text.trim();
    let scheme = ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| text.starts_with(scheme));
    (scheme && text.len() > 8 && !text.contains(char::is_whitespace)).then_some(text)
}

/// `![alt](dest)`, alone or inside a link: what "copy image" in a browser becomes.
fn lone_image(text: &str) -> bool {
    let inner = text
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("]("))
        .map_or(text, |(image, _)| image);
    inner.starts_with("![") && inner.ends_with(')') && !inner.contains('\n') && inner.matches("![").count() == 1
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        bits = bits << 6 | value as u32;
        count += 1;
        if count == 4 {
            out.extend([(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]);
            (bits, count) = (0, 0);
        }
    }
    match count {
        2 => out.push((bits >> 4) as u8),
        3 => out.extend([(bits >> 10) as u8, (bits >> 2) as u8]),
        _ => {}
    }
    Some(out)
}

impl Editor {
    /// Puts text on the clipboard. Markdown with any structure goes with an
    /// HTML rendering, so it arrives formatted in a mail or a document.
    pub(crate) fn copy_out(&mut self, text: &str) {
        self.copied = Some(text.to_string());
        match self.rendered(text) {
            Some(html) => self.clipboard.set_rich(text, &html),
            None => self.clipboard.set(text),
        }
    }

    /// `text` as HTML, or `None` if it is a plain run of words.
    fn rendered(&self, text: &str) -> Option<String> {
        use pulldown_cmark::{Event, Parser, Tag, TagEnd};
        if !self.config.copy_html || text.len() > 1 << 20 {
            return None;
        }
        let folder = self.path.as_deref().and_then(Path::parent);
        let mut structured = false;
        let events: Vec<Event> = Parser::new_ext(text, crate::markdown::options())
            .map(|event| {
                structured |= !matches!(
                    event,
                    Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) | Event::Text(_) | Event::SoftBreak
                );
                match (event, folder) {
                    // A picture beside the document, by its full address.
                    (
                        Event::Start(Tag::Image {
                            link_type,
                            dest_url,
                            title,
                            id,
                        }),
                        Some(folder),
                    ) if !dest_url.contains(':') && !dest_url.starts_with('/') => Event::Start(Tag::Image {
                        link_type,
                        dest_url: format!("file://{}", encode(&folder.join(&*dest_url).to_string_lossy())).into(),
                        title,
                        id,
                    }),
                    (event, _) => event,
                }
            })
            .collect();
        if !structured {
            return None;
        }
        let mut out = String::with_capacity(text.len() * 2);
        pulldown_cmark::html::push_html(&mut out, events.into_iter());
        Some(out)
    }

    /// What the clipboard holds, as text to insert. `plain` takes its text
    /// as it is.
    pub(super) fn pasted(&mut self, plain: bool) -> Option<Pasted> {
        let clip = self.clipboard.contents();
        let text = clip.text.filter(|text| !text.is_empty());
        let as_text = |text: Option<String>| {
            text.map(|text| Pasted {
                text,
                block: false,
                rich: false,
            })
        };
        // Our own copy comes back exactly as it left.
        let ours = text.is_some() && text == self.copied;
        if plain || ours || !self.config.smart_paste || self.cmdline.is_some() || self.palette.is_some() {
            return as_text(text);
        }
        let block = |text: String| Pasted {
            block: text.contains('\n') || starts_block(&text),
            text,
            rich: true,
        };
        if !clip.files.is_empty() {
            let links: Vec<String> = clip
                .files
                .iter()
                .filter_map(|file| match self.file_link(file) {
                    Ok(link) => Some(link),
                    Err(err) => {
                        self.error(err);
                        None
                    }
                })
                .collect();
            return (!links.is_empty()).then(|| block(links.join("\n")));
        }
        if let Some(source) = clip.html.filter(|html| !html.trim().is_empty()) {
            let converted = html::to_markdown(&source, &mut |src| self.keep_data_image(src));
            if converted.rich && !converted.text.is_empty() {
                // "Copy image" in a browser: the picture itself is better
                // than a link back to where it was.
                if lone_image(&converted.text)
                    && let Some(link) = self.pasted_image()
                {
                    return Some(block(link));
                }
                return Some(block(converted.text));
            }
        }
        if text.as_deref().is_none_or(|text| text.trim().is_empty())
            && let Some(link) = self.pasted_image()
        {
            return Some(block(link));
        }
        as_text(text)
    }

    /// The clipboard's picture, saved beside the document, as its Markdown.
    fn pasted_image(&mut self) -> Option<String> {
        let png = self.clipboard.image()?;
        match self.keep_image(&png, "png", None) {
            Ok(dest) => Some(format!("![]({dest})")),
            Err(err) => {
                self.error(err);
                None
            }
        }
    }

    fn keep_data_image(&mut self, src: &str) -> Option<String> {
        let (kind, data) = src.strip_prefix("data:image/")?.split_once(";base64,")?;
        let extension = match kind {
            "jpeg" => "jpg",
            "svg+xml" => "svg",
            "png" | "gif" | "webp" | "bmp" | "avif" => kind,
            _ => return None,
        };
        let bytes = base64(data).filter(|bytes| bytes.len() > 64)?;
        self.keep_image(&bytes, extension, None).ok()
    }

    /// Pastes the clipboard at the cursor, over the selection if there is one.
    pub(crate) fn paste(&mut self, plain: bool) {
        let Some(pasted) = self.pasted(plain) else { return };
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        let mut text = pasted.text;
        // A link pasted over words links those words.
        if !plain
            && let Some(url) = single_url(&text)
            && !range.is_empty()
            && self.mode != Mode::VisualLine
        {
            let selected = self.buf.slice(range.clone());
            if !selected.contains('\n') && single_url(&selected).is_none() && !selected.trim().is_empty() {
                text = format!("[{selected}]({url})");
            }
        } else if pasted.block {
            text = self.apart(range, text);
        }
        self.insert_text(&text);
        if pasted.rich {
            self.info(format!(
                "pasted as Markdown  ·  {} pastes it as plain text",
                self.chord(true, "V")
            ));
        }
    }

    /// Pads block content with the blank lines it needs to stand apart from
    /// the text around where it will go.
    pub(super) fn apart(&self, range: std::ops::Range<usize>, text: String) -> String {
        let buf = &self.buf;
        let (first, last) = (buf.line_of(range.start), buf.line_of(range.end));
        let before = buf.slice(buf.line_start(first)..range.start);
        let after = buf.slice(range.end..buf.line_end(last));
        let lead = if !before.trim().is_empty() {
            "\n\n"
        } else if first > 0 && !buf.line_is_blank(first - 1) && !continues(&text) {
            "\n"
        } else {
            ""
        };
        let trail = if !after.trim().is_empty() {
            "\n\n"
        } else if last + 1 < buf.line_count() && !buf.line_is_blank(last + 1) {
            "\n"
        } else {
            ""
        };
        format!("{lead}{text}{trail}")
    }

    /// Where this document's pictures are kept: `(folder, link prefix, name stem)`.
    fn image_home(&self) -> Result<(PathBuf, String, String), String> {
        if self.margin_active {
            let owner = self.margin_owner().ok_or("the margin has no document")?;
            let folder = super::margin::files_dir(&owner);
            let name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
            return Ok((folder, format!("{}/", encode(&name)), "pasted".to_string()));
        }
        let stem = |path: &Path| {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            stem.split_whitespace().collect::<Vec<_>>().join("-")
        };
        match &self.path {
            Some(path) => {
                let parent = path.parent().unwrap_or(Path::new("."));
                let inside = self.config.image_dir.trim_matches('/');
                Ok(if inside.is_empty() {
                    (parent.to_path_buf(), String::new(), stem(path))
                } else {
                    (parent.join(inside), format!("{}/", encode(inside)), stem(path))
                })
            }
            // Nowhere beside the document yet: saving it moves them there.
            None => {
                let folder = self
                    .unsaved_images()
                    .ok_or("save the document first, so the picture has somewhere to live")?;
                let prefix = format!("{}/", encode(&folder.to_string_lossy()));
                Ok((folder, prefix, "pasted".to_string()))
            }
        }
    }

    fn unsaved_images(&self) -> Option<PathBuf> {
        self.recovery_dir.as_ref().map(|dir| dir.join("pasted"))
    }

    /// Writes a picture into this document's folder and returns the link
    /// destination. The same picture is written once.
    pub(super) fn keep_image(&mut self, bytes: &[u8], extension: &str, name: Option<&str>) -> Result<String, String> {
        let (folder, prefix, stem) = self.image_home()?;
        std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        let mut number = 1;
        let kept = loop {
            let file = match name {
                Some(name) if number == 1 => format!("{name}.{extension}"),
                Some(name) => format!("{name}-{number}.{extension}"),
                None => format!("{stem}-{number}.{extension}"),
            };
            let target = folder.join(&file);
            match std::fs::read(&target) {
                Ok(existing) if existing == bytes => break file,
                Ok(_) => number += 1,
                Err(_) => {
                    std::fs::write(&target, bytes).map_err(|err| format!("{}: {err}", target.display()))?;
                    break file;
                }
            }
        };
        Ok(format!("{prefix}{}", encode(&kept)))
    }

    /// Copies a video into this document's folder and returns the link
    /// destination. It is never read whole: a file of the same name and
    /// size is taken to be the same video.
    fn keep_video(&mut self, file: &Path, name: &str, extension: &str) -> Result<String, String> {
        let (folder, prefix, _) = self.image_home()?;
        std::fs::create_dir_all(&folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        let size = std::fs::metadata(file)
            .map_err(|err| format!("{}: {err}", file.display()))?
            .len();
        let mut number = 1;
        let kept = loop {
            let kept = match number {
                1 => format!("{name}.{extension}"),
                _ => format!("{name}-{number}.{extension}"),
            };
            let target = folder.join(&kept);
            match std::fs::metadata(&target) {
                Ok(existing) if existing.len() == size => break kept,
                Ok(_) => number += 1,
                Err(_) => {
                    std::fs::copy(file, &target).map_err(|err| format!("{}: {err}", target.display()))?;
                    break kept;
                }
            }
        };
        Ok(format!("{prefix}{}", encode(&kept)))
    }

    /// Keeps a retouched picture beside the document and points the
    /// reference to `url` on `line` at it. The original stays where it
    /// was, so undo brings it back. Returns the new destination.
    pub fn replace_image(&mut self, line: usize, url: &str, png: &[u8]) -> Result<String, String> {
        let line = line.min(self.buf.line_count().saturating_sub(1));
        // The reference ends on `line`; a long one may have begun above it.
        let near = (line.saturating_sub(3)..=line).rev().find_map(|line| {
            let text = self.buf.line_text(line);
            let found = text.match_indices(url).map(|(byte, _)| byte);
            let byte = found
                .clone()
                .find(|byte| text[..*byte].ends_with(['(', '<']))
                .or(found.clone().next())?;
            Some(self.buf.line_start(line) + text[..byte].chars().count())
        });
        // Or it is named by a definition somewhere else.
        let anywhere = || {
            let text = self.buf.text();
            text.find(url).map(|byte| text[..byte].chars().count())
        };
        let start = near
            .or_else(anywhere)
            .ok_or(format!("cannot find where the text refers to {url}"))?;
        let dest = self.keep_image(png, "png", Some(&format!("{}-edit", picture_name(url))))?;
        let end = start + url.chars().count();
        self.close_group();
        self.raw_edit(start..end, &dest);
        self.close_group();
        if self.cursor >= end {
            self.cursor = self.cursor + dest.chars().count() - (end - start);
        }
        self.clamp_cursor();
        Ok(dest)
    }

    /// The Markdown that refers to a file dropped or pasted here. A picture
    /// or a video from outside the document's folder is copied in, so the
    /// document keeps working when it moves.
    pub fn file_link(&mut self, file: &Path) -> Result<String, String> {
        let name = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let video = is_video(file);
        let image = is_picture(file) || video;
        if self.margin_active {
            return self.keep_in_margin(file);
        }
        let folder = self.path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let inside = folder.as_deref().and_then(|folder| file.strip_prefix(folder).ok());
        let dest = match (inside, &folder) {
            (Some(relative), _) => encode(&relative.to_string_lossy()),
            (None, Some(_)) if image => {
                let extension = file
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let name = name.split_whitespace().collect::<Vec<_>>().join("-");
                if video {
                    self.keep_video(file, &name, &extension)?
                } else {
                    let bytes = std::fs::read(file).map_err(|err| format!("{}: {err}", file.display()))?;
                    self.keep_image(&bytes, &extension, Some(&name))?
                }
            }
            _ => encode(&file.to_string_lossy()),
        };
        Ok(if image {
            format!("![]({dest})")
        } else {
            format!("[{}]({dest})", name.replace('[', "\\[").replace(']', "\\]"))
        })
    }

    /// After an untitled document is first saved to `path`: pictures pasted
    /// while it had no folder move in beside it.
    pub(super) fn adopt_images(&mut self, path: &Path) {
        let Some(holding) = self.unsaved_images() else { return };
        let prefix = format!("{}/", encode(&holding.to_string_lossy()));
        let text = self.buf.text();
        if !text.contains(&prefix) {
            return;
        }
        let was = self.path.replace(path.to_path_buf());
        let mut moved: Vec<(String, String)> = Vec::new();
        for (at, _) in text.match_indices(&prefix) {
            let rest = &text[at + prefix.len()..];
            let file: String = rest
                .chars()
                .take_while(|c| !matches!(c, ')' | ' ' | '\n' | '>'))
                .collect();
            let from = holding.join(&file);
            let old = format!("{prefix}{file}");
            if moved.iter().any(|(seen, _)| *seen == old) {
                continue;
            }
            let extension = from.extension().unwrap_or_default().to_string_lossy().into_owned();
            let Ok(bytes) = std::fs::read(&from) else { continue };
            if let Ok(dest) = self.keep_image(&bytes, &extension, None) {
                let _ = std::fs::remove_file(&from);
                moved.push((old, dest));
            }
        }
        self.path = was;
        for (old, new) in moved {
            let mut from = 0;
            while let Some(found) = self.buf.text()[from..].find(&old) {
                let text = self.buf.text();
                let start = text[..from + found].chars().count();
                let end = start + old.chars().count();
                self.raw_edit(start..end, &new);
                from += found + new.len();
            }
        }
        self.close_group();
    }
}
