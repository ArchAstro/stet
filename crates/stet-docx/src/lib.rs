//! Markdown to and from Word's `.docx`, review included.
//!
//! The format is how suggestions travel to and from Google Docs: Docs
//! downloads a document's suggestions as tracked changes and its comments as
//! Word comments, and opening a `.docx` in Docs turns them back.
//!
//! - Suggestions are CriticMarkup ([`stet_core::critic`]): `{++new++}`,
//!   `{--old--}` and `{~~old~>new~~}`, each followed by its identity
//!   `{>>id:s_… by:Author<<}`. They map to `w:ins` and `w:del`.
//! - Comments are [`Comment`]s: stet keeps them in the margin, pinned to the
//!   words they are about, not in the text. A standalone `{>>note<<}` in the
//!   text is written as a comment at that point too.
//! - Pictures go through callbacks, so this crate never touches the disk.

mod read;
mod write;

pub use read::read;
pub use write::write;

/// A comment on some words of the document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    /// The comment, replies after it as `Author: text` paragraphs.
    pub text: String,
    /// The words it is about, as plain text (markup stripped). Empty for a
    /// comment at a point.
    pub anchor: String,
}

/// A `.docx` as Markdown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    /// The text, suggestions as CriticMarkup.
    pub markdown: String,
    pub comments: Vec<Comment>,
}

/// How a document is written.
#[derive(Clone, Debug)]
pub struct Options<'a> {
    /// Who standalone `{>>notes<<}` are by.
    pub author: &'a str,
    /// `w:date` for changes and comments (`2026-10-29T00:00:00Z`), if any.
    pub date: Option<&'a str>,
}

/// Saves a picture's bytes (with its extension, `png`) and returns the
/// link destination to use for it in the Markdown, or `None` to drop it.
pub type KeepPicture<'a> = &'a mut dyn FnMut(&[u8], &str) -> Option<String>;

/// The bytes and extension of the picture a link destination names, or
/// `None` to leave it out.
pub type FindPicture<'a> = &'a mut dyn FnMut(&str) -> Option<(Vec<u8>, String)>;
