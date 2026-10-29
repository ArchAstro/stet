//! Word files, and the review they carry: suggestions stay in the text as
//! CriticMarkup, comments live in the margin pinned to the words they are
//! about. This is also how review travels to and from Google Docs.

use std::path::{Path, PathBuf};
use stet_core::editor::{margin_path, percent_decode};
use stet_core::{Editor, critic};
use stet_docx::{Comment, Options};

/// A margin section titled this way is someone else's comment.
const FROM: &str = "Comment from ";

/// Margin anchors longer than this are kept as their beginning and end, as
/// the editor writes them.
const ANCHOR_MAX: usize = 96;
const ELLIPSIS: &str = " … ";

fn anchor_for(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= ANCHOR_MAX {
        return text;
    }
    let head: String = chars[..56].iter().collect();
    let tail: String = chars[chars.len() - 32..].iter().collect();
    format!("{}{ELLIPSIS}{}", head.trim_end(), tail.trim_start())
}

/// Comments as margin sections, each pinned to its words.
pub fn margin_of(comments: &[Comment]) -> String {
    let sections: Vec<String> = comments
        .iter()
        .map(|comment| {
            let author = Some(comment.author.trim()).filter(|author| !author.is_empty());
            let mut section = format!("## {FROM}{}\n", author.unwrap_or("someone"));
            // An anchor starting with `#` would pin to a heading of that name.
            let anchor = anchor_for(&comment.anchor);
            let anchor = anchor.trim_start_matches('#').trim();
            if !anchor.is_empty() {
                section.push_str(&format!("@ {anchor}\n"));
            }
            section.push('\n');
            section.push_str(comment.text.trim());
            section.push('\n');
            section
        })
        .collect();
    sections.join("\n")
}

/// The margin's pinned sections as comments. `Comment from Ann` is Ann's,
/// with its body as the comment; any other is `author`'s, title included.
pub fn comments_of(margin: &str, author: &str) -> Vec<Comment> {
    struct Section<'a> {
        title: &'a str,
        anchor: Option<&'a str>,
        body: Vec<&'a str>,
    }
    let mut sections: Vec<Section> = Vec::new();
    let mut fence = false;
    for line in margin.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
        }
        let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
        if !fence && (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            sections.push(Section {
                title: trimmed[hashes..].trim(),
                anchor: None,
                body: Vec::new(),
            });
            continue;
        }
        let Some(section) = sections.last_mut() else { continue };
        match line.strip_prefix("@ ") {
            Some(anchor) if section.anchor.is_none() && section.body.iter().all(|line| line.trim().is_empty()) => {
                section.anchor = Some(anchor.trim());
            }
            _ => section.body.push(line),
        }
    }
    sections
        .into_iter()
        .filter_map(|section| {
            let anchor = section.anchor.filter(|anchor| !anchor.is_empty())?;
            // Pinned to a heading, or by its beginning and end: the heading's
            // words, or the beginning, are where the comment goes.
            let anchor = anchor.trim_start_matches('#').trim();
            let anchor = anchor.split_once(ELLIPSIS).map_or(anchor, |(head, _)| head);
            let body = section.body.join("\n").trim().to_string();
            let (author, text) = match section.title.strip_prefix(FROM) {
                Some(by) => (by.trim().to_string(), body),
                None if body.is_empty() => (author.to_string(), section.title.to_string()),
                None => (author.to_string(), format!("{}\n\n{body}", section.title)),
            };
            Some(Comment {
                author,
                text,
                anchor: anchor.to_string(),
            })
        })
        .collect()
}

/// The margin of the document at `path`, as saved.
fn margin_text(path: Option<&Path>) -> String {
    path.and_then(|path| std::fs::read_to_string(margin_path(path)).ok())
        .unwrap_or_default()
}

/// Whether the document has review to carry: suggestions or notes in its
/// text, or notes pinned in its margin.
pub fn has_review(ed: &Editor) -> bool {
    let parsed = critic::parse(&ed.buf.text());
    !parsed.suggestions.is_empty()
        || !parsed.comments.is_empty()
        || !comments_of(&margin_text(ed.path.as_deref()), &ed.config.author).is_empty()
}

/// Writes the document, its suggestions and its margin's notes to `out`.
pub fn export(ed: &Editor, out: &Path) -> Result<(), String> {
    let folder = ed.path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    let comments = comments_of(&margin_text(ed.path.as_deref()), &ed.config.author);
    let date = now();
    let options = Options {
        author: &ed.config.author,
        date: Some(&date),
    };
    let bytes = stet_docx::write(&ed.buf.text(), &comments, &options, &mut |dest| {
        if dest.contains("://") || dest.starts_with("data:") {
            return None;
        }
        let file = PathBuf::from(percent_decode(dest));
        let file = match &folder {
            Some(folder) if file.is_relative() => folder.join(file),
            _ => file,
        };
        let extension = file.extension()?.to_str()?.to_ascii_lowercase();
        Some((std::fs::read(&file).ok()?, extension))
    })?;
    std::fs::write(out, bytes).map_err(|err| format!("{}: {err}", out.display()))
}

/// Now, as Word dates its changes: `2026-10-29T18:04:05Z`.
fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    // Days to a civil date (Howard Hinnant's algorithm).
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    let (h, m, s) = (rest / 3600, rest / 60 % 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// What an import brought in, for saying so.
pub struct Imported {
    pub suggestions: usize,
    pub comments: usize,
    pub pictures: usize,
}

impl std::fmt::Display for Imported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} suggestions, {} comments in the margin, {} pictures",
            self.suggestions, self.comments, self.pictures
        )
    }
}

/// Imports `.docx` bytes to `out`: pictures beside it, comments in its
/// margin.
pub fn import(bytes: &[u8], out: &Path) -> Result<Imported, String> {
    if out.exists() {
        return Err(format!("{} exists; name another file to write", out.display()));
    }
    let mut ed = Editor::new(
        stet_core::Config::default(),
        Box::new(stet_core::MemoryClipboard::default()),
    );
    let absolute = std::path::absolute(out).unwrap_or(out.to_path_buf());
    ed.path = Some(absolute.clone());
    let mut pictures = 0;
    let imported = stet_docx::read(bytes, &mut |bytes, extension| {
        let kept = ed.keep_picture(bytes, extension).ok();
        pictures += kept.is_some() as usize;
        kept
    })?;
    std::fs::write(out, &imported.markdown).map_err(|err| format!("{}: {err}", out.display()))?;
    if !imported.comments.is_empty() {
        let margin = margin_path(&absolute);
        if let Some(dir) = margin.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        }
        std::fs::write(&margin, margin_of(&imported.comments)).map_err(|err| format!("{}: {err}", margin.display()))?;
    }
    Ok(Imported {
        suggestions: critic::parse_suggestions(&imported.markdown).len(),
        comments: imported.comments.len(),
        pictures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_what_word_writes() {
        let now = now();
        assert_eq!(now.len(), 20, "{now}");
        assert!(
            now.starts_with("20") && now.ends_with('Z') && now.as_bytes()[10] == b'T',
            "{now}"
        );
    }

    #[test]
    fn comments_go_to_the_margin_and_come_back() {
        let comments = vec![
            Comment {
                author: "Ann".into(),
                text: "Too strong?\n\nBo: agreed".into(),
                anchor: "the  best\nplan".into(),
            },
            Comment {
                author: String::new(),
                text: "General".into(),
                anchor: String::new(),
            },
        ];
        let margin = margin_of(&comments);
        assert_eq!(
            margin,
            "## Comment from Ann\n@ the best plan\n\nToo strong?\n\nBo: agreed\n\n## Comment from someone\n\nGeneral\n"
        );
        // Only pinned sections go out; the unpinned one stays a note.
        assert_eq!(
            comments_of(&margin, "Me"),
            vec![Comment {
                author: "Ann".into(),
                text: "Too strong?\n\nBo: agreed".into(),
                anchor: "the best plan".into(),
            }]
        );
    }

    #[test]
    fn the_writers_own_pinned_notes_are_their_comments() {
        let margin = "# Research\n\nloose notes\n\n## Check this\n@ a long passage … its end\n\nsource?\n\n```\n# not a heading\n```\n";
        assert_eq!(
            comments_of(margin, "Me"),
            vec![Comment {
                author: "Me".into(),
                text: "Check this\n\nsource?\n\n```\n# not a heading\n```".into(),
                anchor: "a long passage".into(),
            }]
        );
    }
}
