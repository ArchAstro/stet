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

/// Keeps a cookie the user gave another way (a field in a window). It
/// reaches `security` on its standard input, not its command line, where
/// any process could read it.
pub fn store_value(cookie: &Cookie) -> Result<()> {
    keep(SERVICE, cookie)
}

fn keep(service: &str, cookie: &Cookie) -> Result<()> {
    use std::io::Write;
    if !cfg!(target_os = "macos") {
        return Err(Error::Config(
            "the cookie is kept in the macOS keychain; there is none here".into(),
        ));
    }
    let failed = |err: std::io::Error| Error::Config(format!("{TOOL}: {err}"));
    let mut shell = Command::new(TOOL)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failed)?;
    // A cookie has no quotes, spaces or backslashes in it (`Cookie::new`).
    let line = format!(
        "add-generic-password -U -s {service} -a {ACCOUNT} -l \"Stet: Substack session\" -w \"{}\"\n",
        cookie.value()
    );
    shell
        .stdin
        .take()
        .map(|mut stdin| stdin.write_all(line.as_bytes()))
        .transpose()
        .map_err(failed)?;
    match shell.wait().map_err(failed)?.success() {
        true => Ok(()),
        false => Err(Error::Config("the cookie could not be stored in the keychain".into())),
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // Run by hand (`cargo test -- --ignored`): it writes to the login keychain.
    #[test]
    #[ignore = "writes to the login keychain"]
    fn a_cookie_is_kept_and_read_back() {
        // Under a name of its own, so a real cookie is never touched.
        let service = "stet-substack-selftest";
        let find = |service: &str| {
            let found = Command::new(TOOL)
                .args(["find-generic-password", "-s", service, "-a", ACCOUNT, "-w"])
                .stderr(Stdio::null())
                .output()
                .unwrap();
            found
                .status
                .success()
                .then(|| String::from_utf8_lossy(&found.stdout).trim().to_string())
        };
        keep(service, &Cookie::new("s%3Anot-a-real-cookie.x%2By").unwrap()).unwrap();
        assert_eq!(find(service).as_deref(), Some("s%3Anot-a-real-cookie.x%2By"));
        let gone = Command::new(TOOL)
            .args(["delete-generic-password", "-s", service, "-a", ACCOUNT])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(gone.success());
        assert_eq!(find(service), None);
    }
}
