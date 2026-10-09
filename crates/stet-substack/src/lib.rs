//! Markdown to a Substack draft, through the private API the web editor uses.
//!
//! Substack has no official publishing API. This talks to the same
//! `/api/v1/...` endpoints the editor does, signed in with the user's
//! `substack.sid` session cookie. None of it is documented or promised to
//! stay put.
//!
//! The cookie is a password. It is kept in the macOS login keychain
//! ([`keychain`]) and nowhere else: never in a file, an environment variable
//! or a command line, and never printed. It is sent over HTTPS to Substack's
//! own hosts, and to a custom domain only once the account's profile has
//! listed that domain; redirects are not followed.
//!
//! - [`convert`]: Markdown to Substack's document JSON. Pure.
//! - [`Client`]: one method per endpoint, over a [`Transport`].
//! - [`create_draft_from_file`]: the whole trip. It makes a draft and never
//!   publishes; [`Client::publish`] is a separate, explicit call.
//!
//! # Not confirmed against Substack
//!
//! Written from other people's clients (python-substack, and two Node MCP
//! servers), never run against a real account. Where they disagree this crate
//! picks one and says so:
//!
//! - The code block node name (`highlighted_code_block`, `code_block` or
//!   `codeBlock`): see [`CodeBlockNode`].
//! - Mark names `strong`/`em` (python-substack, and a client that read the live
//!   editor) against `bold`/`italic` (one client). This crate uses `strong`/`em`.
//! - The draft body fields sent on create: only those every client sends.
//! - The publish body: `{"send": bool}` only.
//! - Footnote placement (after the block that cites it) and image size attrs.
//!
//! `examples/probe.rs` checks these against a real account with your cookie.

mod client;
mod convert;
mod error;
mod flow;
mod http;
mod image;
pub mod keychain;

pub use client::{Client, Cookie, DraftRef, Fields, Profile, Publication, PublicationInfo, USER_AGENT, UploadedImage};
pub use convert::{CodeBlockNode, Document, Hosted, Options, convert, is_remote, youtube_id};
pub use error::{Error, Result};
pub use flow::{Created, DraftOptions, create_draft_from_file, create_draft_from_markdown, resolve};
pub use http::{Method, Request, Response, Transport, Ureq};
