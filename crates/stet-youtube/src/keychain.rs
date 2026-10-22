//! Where the sign-in is kept: the macOS login keychain, through the system's
//! own `security` tool, as one item holding the OAuth client and its refresh
//! token. The value reaches `security` on its standard input, hex encoded,
//! never on a command line, in an environment variable or in a file of ours.

use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::fmt;
use std::process::{Command, Stdio};

const TOOL: &str = "/usr/bin/security";
pub const SERVICE: &str = "stet-youtube";
pub const ACCOUNT: &str = "google-oauth";
const LABEL: &str = "Stet: YouTube sign-in";

/// The registered OAuth client and what signing in gave it.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    fn to_json(&self) -> String {
        json!({
            "client_id": self.client_id,
            "client_secret": self.client_secret,
            "refresh_token": self.refresh_token,
        })
        .to_string()
    }

    fn from_json(text: &str) -> Result<Credentials> {
        let value: Value = serde_json::from_str(text.trim())
            .map_err(|_| Error::Config("the YouTube sign-in in the keychain is unreadable; sign in again".into()))?;
        let field = |name: &str| {
            value[name]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| Error::Config(format!("the YouTube sign-in in the keychain has no {name}")))
        };
        Ok(Credentials {
            client_id: field("client_id")?,
            client_secret: field("client_secret")?,
            refresh_token: field("refresh_token")?,
        })
    }
}

fn macos() -> Result<()> {
    match cfg!(target_os = "macos") {
        true => Ok(()),
        false => Err(Error::Config(
            "the YouTube sign-in is kept in the macOS keychain; there is none here".into(),
        )),
    }
}

/// The stored sign-in.
pub fn credentials() -> Result<Credentials> {
    read(SERVICE)
}

/// Whether there is a stored sign-in, without asking Google if it still works.
pub fn signed_in() -> bool {
    credentials().is_ok()
}

/// Keeps the sign-in, replacing an earlier one.
pub fn store(credentials: &Credentials) -> Result<()> {
    keep(SERVICE, credentials)
}

/// Removes the stored sign-in. True if there was one.
pub fn forget() -> Result<bool> {
    macos()?;
    let gone = Command::new(TOOL)
        .args(["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| Error::Config(format!("{TOOL}: {err}")))?;
    Ok(gone.success())
}

fn read(service: &str) -> Result<Credentials> {
    macos()?;
    let found = Command::new(TOOL)
        .args(["find-generic-password", "-s", service, "-a", ACCOUNT, "-w"])
        .stderr(Stdio::null())
        .output()
        .map_err(|err| Error::Config(format!("{TOOL}: {err}")))?;
    if !found.status.success() {
        return Err(Error::Config("YouTube is not signed in".into()));
    }
    Credentials::from_json(&String::from_utf8_lossy(&found.stdout))
}

fn keep(service: &str, credentials: &Credentials) -> Result<()> {
    use std::io::Write;
    macos()?;
    let failed = |err: std::io::Error| Error::Config(format!("{TOOL}: {err}"));
    let mut shell = Command::new(TOOL)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failed)?;
    // Hex, so the JSON's quotes need no escaping in `security`'s own syntax.
    let hex: String = credentials
        .to_json()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let line = format!("add-generic-password -U -s {service} -a {ACCOUNT} -l \"{LABEL}\" -X {hex}\n");
    shell
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(line.as_bytes()))
        .transpose()
        .map_err(failed)?;
    match shell.wait().map_err(failed)?.success() {
        true => Ok(()),
        false => Err(Error::Config(
            "the YouTube sign-in could not be stored in the keychain".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Credentials {
        Credentials {
            client_id: "1-abc.apps.googleusercontent.com".into(),
            client_secret: "GOCSPX-\"quoted\"".into(),
            refresh_token: "1//0g-refresh".into(),
        }
    }

    #[test]
    fn credentials_round_trip_through_json() {
        assert_eq!(Credentials::from_json(&sample().to_json()).unwrap(), sample());
        assert!(Credentials::from_json("{\"client_id\":\"x\"}").is_err());
        assert!(Credentials::from_json("not json").is_err());
    }

    #[test]
    fn debug_shows_no_secrets() {
        let shown = format!("{:?}", sample());
        assert!(!shown.contains("GOCSPX") && !shown.contains("refresh"), "{shown}");
    }

    // Run by hand (`cargo test -- --ignored`): it writes to the login keychain.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the login keychain"]
    fn a_sign_in_is_kept_and_read_back() {
        // Under a name of its own, so a real sign-in is never touched.
        let service = "stet-youtube-selftest";
        keep(service, &sample()).unwrap();
        assert_eq!(read(service).unwrap(), sample());
        let gone = Command::new(TOOL)
            .args(["delete-generic-password", "-s", service, "-a", ACCOUNT])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(gone.success());
        assert!(read(service).is_err());
    }
}
