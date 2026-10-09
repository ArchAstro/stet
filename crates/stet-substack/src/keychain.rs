//! Where the cookie is kept: the macOS login keychain, through the system's
//! own `security` tool. The value is typed into that tool's hidden prompt
//! and read back by it, so it is never a command-line argument, an
//! environment variable or a file of ours.

use crate::client::Cookie;
use crate::error::{Error, Result};
use std::process::{Command, Stdio};

const TOOL: &str = "/usr/bin/security";
pub const SERVICE: &str = "stet-substack";
pub const ACCOUNT: &str = "substack.sid";

fn tool(action: &str) -> Result<Command> {
    if !cfg!(target_os = "macos") {
        return Err(Error::Config(
            "the cookie is kept in the macOS keychain; there is none here".into(),
        ));
    }
    let mut command = Command::new(TOOL);
    command.args([action, "-s", SERVICE, "-a", ACCOUNT]);
    Ok(command)
}

/// The stored cookie.
pub fn cookie() -> Result<Cookie> {
    let found = tool("find-generic-password")?
        .arg("-w")
        .stderr(Stdio::null())
        .output()
        .map_err(|err| Error::Config(format!("{TOOL}: {err}")))?;
    if !found.status.success() {
        return Err(Error::Config(format!(
            "no cookie in the keychain (service {SERVICE}); store one first"
        )));
    }
    Cookie::new(String::from_utf8_lossy(&found.stdout).trim())
}

/// Asks for the cookie at the terminal, without showing it, and keeps it;
/// an earlier one is replaced. Needs a terminal to ask at.
pub fn store() -> Result<()> {
    // `-w` last and bare: `security` prompts for the value itself.
    let kept = tool("add-generic-password")?
        .args(["-U", "-l", "Stet: Substack session", "-w"])
        .status()
        .map_err(|err| Error::Config(format!("{TOOL}: {err}")))?;
    match kept.success() {
        // Whatever was typed must at least look like a cookie.
        true => cookie().map(drop),
        false => Err(Error::Config("the cookie was not stored".into())),
    }
}

/// Removes the stored cookie. True if there was one.
pub fn forget() -> Result<bool> {
    let gone = tool("delete-generic-password")?
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| Error::Config(format!("{TOOL}: {err}")))?;
    Ok(gone.success())
}
