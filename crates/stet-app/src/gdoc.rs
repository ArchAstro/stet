//! Google Docs in and out, with no account to connect.
//!
//! `stet import <link>` reads a doc through the address its own File →
//! Download uses, as a Word file, so suggestions arrive as CriticMarkup and
//! comments in the margin ([`crate::docx`]); `--md` takes Google's Markdown,
//! which has neither. A doc shared as "Anyone with the link" comes straight
//! over the network. Any other doc (one shared inside a work account, or
//! only with you) is fetched by the browser instead, where you are already
//! signed in: stet opens the same address there, with `authuser` naming the
//! account, and reads the download when it lands.
//!
//! Export ([`crate::convert`]) needs no account either: a document with no
//! review is pasted into a new doc; one with suggestions or notes is written
//! as a Word file to drop into Drive, where Docs turns them back.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use stet_core::Editor;

/// Google answered with its sign-in page, not the doc.
const NOT_SHARED: &str = "not shared by link";

/// How long the browser has to sign in, ask which account, and download.
const BROWSER_WAIT: Duration = Duration::from_secs(180);

/// The signed-in account a link names (`/document/u/1/d/…`), as an index.
pub fn account_in(link: &str) -> Option<&str> {
    let (_, rest) = link.split_once("docs.google.com/document/u/")?;
    let account = rest.split('/').next()?;
    (!account.is_empty() && account.bytes().all(|b| b.is_ascii_digit())).then_some(account)
}

/// The export address for `format`. `account` (an email or an index)
/// picks which of the browser's Google accounts asks for it.
pub fn export_url(id: &str, format: &str, account: Option<&str>) -> String {
    let mut url = format!("https://docs.google.com/document/d/{id}/export?format={format}");
    if let Some(account) = account {
        url.push_str("&authuser=");
        url.push_str(&account_query(account));
    }
    url
}

/// An account as a query value.
pub fn account_query(account: &str) -> String {
    account.replace('%', "%25").replace('+', "%2B").replace('&', "%26")
}

/// The doc ID in a Google Docs link, or a bare ID.
pub fn doc_id(link: &str) -> Option<&str> {
    let id = match link.split_once("docs.google.com/document/") {
        Some((_, rest)) => rest
            .strip_prefix("u/")
            .map_or(rest, |rest| rest.split_once('/').map_or(rest, |(_, rest)| rest)),
        None if !link.contains('/') && !link.contains('.') => return valid(link),
        None => return None,
    };
    valid(id.strip_prefix("d/")?.split(['/', '?', '#']).next()?)
}

fn valid(id: &str) -> Option<&str> {
    let ok = id.len() >= 25 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(id)
}

/// Every `data:image/…;base64,…` in `markdown` replaced by what `keep`
/// returns for it (a file it saved); left as it is where `keep` declines.
pub fn keep_pictures(markdown: &str, keep: &mut dyn FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(at) = rest.find("data:image/") {
        out.push_str(&rest[..at]);
        let uri = &rest[at..];
        let end = uri.find(";base64,").map_or(0, |comma| {
            let data = &uri[comma + 8..];
            let len = data
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')))
                .unwrap_or(data.len());
            comma + 8 + len
        });
        let end = end.max("data:image/".len());
        match keep(&uri[..end]) {
            Some(kept) => out.push_str(&kept),
            None => out.push_str(&uri[..end]),
        }
        rest = &uri[end..];
    }
    out.push_str(rest);
    out
}

/// A file name from the doc's first line, its title.
pub fn file_name(markdown: &str) -> String {
    let title = markdown
        .lines()
        .map(|line| line.trim_start_matches('#').replace('\\', ""))
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let words: Vec<String> = title
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                .collect()
        })
        .filter(|word: &String| !word.is_empty())
        .collect();
    let mut name = words.join("-");
    if let Some((cut, _)) = name.char_indices().nth(60) {
        name.truncate(cut);
    }
    if name.is_empty() {
        name = "google-doc".to_string();
    }
    name + ".md"
}

/// What a download is: its bytes, and the name Google gave it (the doc's
/// title).
struct Download {
    bytes: Vec<u8>,
    name: Option<String>,
}

/// The doc as File → Download gives it, in `format` (`docx` or `md`).
#[cfg(feature = "gdocs")]
fn fetch(id: &str, format: &str) -> Result<Download, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into();
    let url = export_url(id, format, None);
    let mut response = agent.get(&url).call().map_err(|err| err.to_string())?;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let (kind, disposition) = (header("content-type"), header("content-disposition"));
    match status {
        // A private doc ends on Google's sign-in page, which is HTML.
        200 if !kind.starts_with("text/html") => Ok(Download {
            bytes: response
                .body_mut()
                .with_config()
                .limit(256 << 20)
                .read_to_vec()
                .map_err(|err| err.to_string())?,
            name: disposition
                .split_once("filename=\"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .map(|(name, _)| name.to_string()),
        }),
        200 | 401 | 403 | 404 => Err(NOT_SHARED.to_string()),
        _ => Err(format!("Google answered {status}")),
    }
}

#[cfg(not(feature = "gdocs"))]
fn fetch(_id: &str, _format: &str) -> Result<Download, String> {
    Err("this build of stet cannot reach Google Docs".to_string())
}

/// The newest `.extension` file in `dir` written since `since`. Browsers
/// write to a `.crdownload`/`.part`/`.download` name first, so the file is
/// whole once its size holds still.
fn newest_download(dir: &Path, extension: &str, since: SystemTime) -> Option<(PathBuf, u64)> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
        })
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let modified = meta.modified().ok()?;
            (meta.is_file() && modified >= since).then(|| (modified, entry.path(), meta.len()))
        })
        .max_by_key(|(modified, ..)| *modified)
        .map(|(_, path, len)| (path, len))
}

/// The doc as the browser downloads it, signed in as `account` (or its
/// default account), and where the download landed.
fn from_browser(id: &str, format: &str, account: Option<&str>) -> Result<(Download, PathBuf), String> {
    let downloads = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .ok_or("no Downloads folder to watch for the browser's download")?;
    // A little slack for filesystems that keep coarse times.
    let since = SystemTime::now() - Duration::from_secs(2);
    let url = export_url(id, format, account);
    open::that_detached(&url).map_err(|err| format!("{url}: {err}"))?;
    eprintln!(
        "stet: not shared by link, so your browser is downloading it as you{}; waiting for it in {}",
        account.map(|account| format!(" ({account})")).unwrap_or_default(),
        downloads.display()
    );
    let deadline = Instant::now() + BROWSER_WAIT;
    let mut last = None;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        let newest = newest_download(&downloads, format, since);
        if let Some((path, len)) = &newest
            && *len > 0
            && last.as_ref() == newest.as_ref()
        {
            let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned());
            return Ok((Download { bytes, name }, path.clone()));
        }
        last = newest;
    }
    Err(format!(
        "nothing reached {} in {} minutes. If the browser showed \"You need access\" or the wrong \
         account, run again with --account you@work.com (or the account's number in a /u/N/ link)",
        downloads.display(),
        BROWSER_WAIT.as_secs() / 60
    ))
}

/// How to import a doc.
#[derive(Default)]
pub struct Import<'a> {
    pub out: Option<&'a Path>,
    /// Which of the browser's Google accounts to ask as; going through the
    /// browser at once.
    pub account: Option<&'a str>,
    /// Through the browser even if the doc is shared by link.
    pub browser: bool,
    /// Google's own Markdown, which has no suggestions or comments.
    pub markdown: bool,
}

/// Imports the doc at `link`. Returns the file written and what it holds.
pub fn import(link: &str, how: &Import) -> Result<(PathBuf, String), String> {
    let id = doc_id(link).ok_or_else(|| format!("{link} is not a Google Docs link"))?;
    let account = how.account.or(account_in(link));
    let format = if how.markdown { "md" } else { "docx" };
    let (download, downloaded) = match (how.browser || account.is_some(), fetch(id, format)) {
        (false, Ok(download)) => (download, None),
        (false, Err(err)) if err != NOT_SHARED => return Err(err),
        _ => {
            let (download, path) = from_browser(id, format, account)?;
            (download, Some(path))
        }
    };
    let titled = |name: &Option<String>| {
        let stem = name.as_deref().and_then(|name| Path::new(name).file_stem()?.to_str());
        stem.map(|stem| PathBuf::from(file_name(stem)))
    };
    let said = if how.markdown {
        let markdown = String::from_utf8_lossy(&download.bytes).into_owned();
        let out = how
            .out
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(file_name(&markdown)));
        (out.clone(), import_markdown(&markdown, &out)?)
    } else {
        let out = how
            .out
            .map(Path::to_path_buf)
            .or_else(|| titled(&download.name))
            .unwrap_or_else(|| PathBuf::from("google-doc.md"));
        let imported = crate::docx::import(&download.bytes, &out)?;
        (out, imported.to_string())
    };
    let left = downloaded
        .map(|path| format!("; the download is still at {}", path.display()))
        .unwrap_or_default();
    Ok((said.0.clone(), format!("wrote {} ({}){left}", said.0.display(), said.1)))
}

/// Google's Markdown to `out`, its inline pictures to files beside it.
fn import_markdown(markdown: &str, out: &Path) -> Result<String, String> {
    if out.exists() {
        return Err(format!("{} exists; name another file to write", out.display()));
    }
    let mut ed = Editor::new(
        stet_core::Config::default(),
        Box::new(stet_core::MemoryClipboard::default()),
    );
    ed.path = Some(std::path::absolute(out).unwrap_or(out.to_path_buf()));
    let mut pictures = 0;
    let markdown = keep_pictures(markdown, &mut |uri| {
        let kept = ed.keep_data_image(uri);
        pictures += kept.is_some() as usize;
        kept
    });
    std::fs::write(out, &markdown).map_err(|err| format!("{}: {err}", out.display()))?;
    Ok(format!("{pictures} pictures"))
}

/// Puts `text`, rendered as `html`, on the system clipboard from the
/// command line.
pub fn paste(text: String, html: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|err| format!("clipboard: {err}"))?;
    // On Linux the clipboard belongs to whoever set it, so this process
    // stays until it is pasted over or a while has passed.
    #[cfg(target_os = "linux")]
    {
        use arboard::SetExtLinux;
        eprintln!("stet: holding the clipboard for two minutes, or until something else is copied");
        let deadline = Instant::now() + Duration::from_secs(120);
        clipboard
            .set()
            .wait_until(deadline)
            .html(html, Some(text))
            .map_err(|err| format!("clipboard: {err}"))
    }
    #[cfg(not(target_os = "linux"))]
    clipboard
        .set_html(html, Some(text))
        .map_err(|err| format!("clipboard: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_links_and_bare_ids_give_the_id() {
        let id = "195j9eDD3ccgjQRttHhJPymLJUCOUjs-jmwTrekvdjFE";
        for link in [
            format!("https://docs.google.com/document/d/{id}/edit?tab=t.0"),
            format!("https://docs.google.com/document/d/{id}"),
            format!("docs.google.com/document/u/1/d/{id}/edit#heading=h.x"),
            id.to_string(),
        ] {
            assert_eq!(doc_id(&link), Some(id), "{link}");
        }
        assert_eq!(doc_id("https://docs.google.com/spreadsheets/d/abc/edit"), None);
        assert_eq!(doc_id("notes.md"), None);
        assert_eq!(doc_id("short"), None);
    }

    #[test]
    fn the_account_rides_along_to_the_browser() {
        let id = "195j9eDD3ccgjQRttHhJPymLJUCOUjs-jmwTrekvdjFE";
        assert_eq!(
            account_in(&format!("https://docs.google.com/document/u/2/d/{id}/edit")),
            Some("2")
        );
        assert_eq!(
            account_in(&format!("https://docs.google.com/document/d/{id}/edit")),
            None
        );
        assert_eq!(
            export_url(id, "md", Some("me+x@work.com")),
            format!("https://docs.google.com/document/d/{id}/export?format=md&authuser=me%2Bx@work.com")
        );
        assert_eq!(
            export_url(id, "md", None),
            format!("https://docs.google.com/document/d/{id}/export?format=md")
        );
    }

    #[test]
    fn the_newest_finished_download_is_the_doc() {
        let dir = std::env::temp_dir().join(format!("stet-gdoc-downloads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let since = SystemTime::now() - Duration::from_secs(5);
        // Downloaded before the import began.
        std::fs::write(dir.join("old.md"), "old").unwrap();
        let old = std::fs::File::options().write(true).open(dir.join("old.md")).unwrap();
        old.set_modified(since - Duration::from_secs(60)).unwrap();
        assert_eq!(newest_download(&dir, "md", since), None);
        // Still arriving.
        std::fs::write(dir.join("Plan.md.crdownload"), "partial").unwrap();
        assert_eq!(newest_download(&dir, "md", since), None);
        std::fs::write(dir.join("Plan.md"), "# Plan").unwrap();
        assert_eq!(newest_download(&dir, "md", since), Some((dir.join("Plan.md"), 6)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_pictures_become_files() {
        let markdown = "# T\n\n![][image1]\n\n[image1]: <data:image/png;base64,iVBORw0KGgo=>\n\nafter data:image/x";
        let mut seen = Vec::new();
        let out = keep_pictures(markdown, &mut |uri| {
            seen.push(uri.to_string());
            uri.contains("png").then(|| "assets/t-1.png".to_string())
        });
        assert_eq!(seen, ["data:image/png;base64,iVBORw0KGgo=", "data:image/"]);
        assert_eq!(
            out,
            "# T\n\n![][image1]\n\n[image1]: <assets/t-1.png>\n\nafter data:image/x"
        );
    }

    #[test]
    fn the_title_names_the_file() {
        assert_eq!(file_name("# Q3 plan: draft\\!\n\nbody"), "Q3-plan-draft.md");
        assert_eq!(file_name("\n\n"), "google-doc.md");
    }
}
