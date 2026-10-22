//! A document's local videos, uploaded to YouTube so a Substack draft can
//! embed them.
//!
//! YouTube is reached through the YouTube Data API v3, signed in with Google
//! OAuth as an installed app: the user registers their own OAuth client
//! (a "Desktop app" client in Google Cloud, with the YouTube Data API
//! enabled), and [`oauth`] runs the loopback sign-in with PKCE.
//!
//! The client id, its secret and the refresh token are kept together in the
//! macOS login keychain ([`keychain`]) and nowhere else: never in a file, an
//! environment variable or a command line, and never printed.
//!
//! A video is uploaded once. [`Ledger`] remembers each file by the SHA-256 of
//! its bytes and the video it became; publishing the same post again finds
//! the video there, checks it is still on the channel, and reuses it.
//!
//! - [`Client`]: one method per endpoint, over a [`Transport`].
//! - [`upload_once`]: the ledger, the check and the upload together.
//!
//! # Google's rules that apply
//!
//! - Videos uploaded through an API project that has not passed YouTube's
//!   audit are locked to private, whatever privacy was asked for.
//! - An OAuth client whose consent screen is still "Testing" gets refresh
//!   tokens that lapse after seven days; signing in again renews it.

mod client;
mod error;
mod http;
pub mod keychain;
mod ledger;
pub mod oauth;

pub use client::{Channel, Client, Privacy, UploadedVideo, VideoState};
pub use error::{Error, Result};
pub use http::{Body, Method, Request, Response, Transport, Ureq};
pub use keychain::Credentials;
pub use ledger::{Entry, Ledger, Outcome, file_digest, upload_once};

/// A watch address for a video id: what the Substack converter embeds.
pub fn watch_url(id: &str) -> String {
    format!("https://youtu.be/{id}")
}
