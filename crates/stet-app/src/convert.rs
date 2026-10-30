//! `stet import` and `stet export`: every format in and out. One export
//! serves `:export`, the command line and `stet ctl export`.

use crate::ipynb::Code;
use crate::{docx, gdoc, ipynb};
use std::path::{Path, PathBuf};
use stet_core::Editor;
use stet_core::editor::{EXPORT_FORMATS, NEW_DOC, docs_html};
use stet_core::notebook;

pub const USAGE: &str = "stet import <notebook.ipynb> [out.md]
    the notebook as markdown: code cells become fenced blocks, outputs are kept beside it

stet import <doc.docx> [out.md]
stet import <Google Docs link> [out.md] [--account <email|N>] [--browser] [--md]
    the document as markdown: suggestions as CriticMarkup, comments in the margin pinned to
    their words, pictures beside it. A Google Doc shared by link is fetched directly; any
    other is downloaded by your browser as you (--account picks which login, --browser goes
    there at once). --md takes Google's own Markdown, which has no suggestions or comments

stet export <doc.md> --docx|--html|--pdf|--ipynb|--gdoc [out] [--account <email|N>] [--no-open]
             [--code|--no-code]
    docx   Word, suggestions as tracked changes and margin notes as comments
    html   one page to read: prose, code, outputs, charts
    pdf    the same page as a PDF, printed by Chrome, Chromium or Edge
    ipynb  a Jupyter notebook with the last outputs
    gdoc   a new Google Doc: pasted, or with suggestions or notes, a .docx to drop into Drive
    html and pdf leave out the code of folded cells; --no-code leaves out all code, --code none";

/// Where Google Drive takes an upload.
const DRIVE: &str = "https://drive.google.com/drive/my-drive";

/// What an export left to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Exported {
    Wrote(PathBuf),
    /// Paste into a new Google Doc; the caller puts it on the clipboard.
    Paste,
    /// A Word file that carries the review, for Google Drive.
    Upload(PathBuf),
}

fn target(ed: &Editor, out: Option<&Path>, extension: &str) -> Result<PathBuf, String> {
    match (out, &ed.path) {
        // Named in the editor: beside the document.
        (Some(out), Some(path)) if out.is_relative() => Ok(path.parent().unwrap_or(Path::new(".")).join(out)),
        (Some(out), _) => Ok(out.to_path_buf()),
        (None, Some(path)) => Ok(path.with_extension(extension)),
        (None, None) => Err("save the document first, or name the file to write".to_string()),
    }
}

/// Writes the document as `format`, to `out` or beside it.
pub fn export(ed: &mut Editor, format: &str, out: Option<&Path>) -> Result<Exported, String> {
    export_with(ed, format, out, Code::default())
}

/// [`export`], with a say in which code an html or pdf page shows.
pub fn export_with(ed: &mut Editor, format: &str, out: Option<&Path>, code: Code) -> Result<Exported, String> {
    let write = |target: &Path, text: String| {
        std::fs::write(target, text).map_err(|err| format!("{}: {err}", target.display()))
    };
    match format {
        "html" | "ipynb" => {
            let target = target(ed, out, format)?;
            let notebook = ed.path.as_deref().map(notebook::load).unwrap_or_default();
            let files = ed.path.as_deref().map(notebook::files_dir).filter(|dir| dir.exists());
            let text = if format == "ipynb" {
                let value = ipynb::export_ipynb(&ed.buf.text(), &notebook, files.as_deref());
                serde_json::to_string_pretty(&value).map_err(|err| err.to_string())? + "\n"
            } else {
                ipynb::export_html(ed, &notebook, files.as_deref(), code)
            };
            write(&target, text)?;
            Ok(Exported::Wrote(target))
        }
        "pdf" => {
            let target = target(ed, out, "pdf")?;
            let notebook = ed.path.as_deref().map(notebook::load).unwrap_or_default();
            let files = ed.path.as_deref().map(notebook::files_dir).filter(|dir| dir.exists());
            // Beside the document, so pictures linked from the prose resolve.
            let folder = ed.path.as_deref().and_then(Path::parent).unwrap_or(Path::new("."));
            let page = folder.join(format!(".stet-print-{}.html", std::process::id()));
            write(&page, ipynb::export_html(ed, &notebook, files.as_deref(), code))?;
            let printed = print_pdf(&page, &target);
            let _ = std::fs::remove_file(&page);
            printed?;
            Ok(Exported::Wrote(target))
        }
        "docx" => {
            let target = target(ed, out, "docx")?;
            docx::export(ed, &target)?;
            Ok(Exported::Wrote(target))
        }
        // Pasting loses suggestions and notes; a Word file keeps them.
        "gdoc" if docx::has_review(ed) => {
            let target = target(ed, out, "docx")?;
            docx::export(ed, &target)?;
            Ok(Exported::Upload(target))
        }
        "gdoc" => Ok(Exported::Paste),
        _ => Err(format!(
            "cannot export as {format}; there is {}",
            EXPORT_FORMATS.join(", ")
        )),
    }
}

/// A browser that prints pages to PDF: `STET_BROWSER`, else Chrome,
/// Chromium, Edge or Brave wherever they usually are.
fn browser() -> Option<PathBuf> {
    if let Some(chosen) = std::env::var_os("STET_BROWSER") {
        return Some(PathBuf::from(chosen));
    }
    let apps = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ];
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ];
    let on_path = |name: &str| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    };
    apps.iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .or_else(|| names.iter().find_map(|name| on_path(name)))
}

/// Prints the html file `page` to `pdf` with a headless browser.
fn print_pdf(page: &Path, pdf: &Path) -> Result<(), String> {
    let browser = browser().ok_or("printing a PDF needs Chrome, Chromium, Edge or Brave (or STET_BROWSER)")?;
    let page = std::path::absolute(page).map_err(|err| err.to_string())?;
    let pdf = std::path::absolute(pdf).map_err(|err| err.to_string())?;
    // A profile of its own: the user's open browser holds theirs.
    let profile = std::env::temp_dir().join(format!("stet-print-{}", std::process::id()));
    let _ = std::fs::remove_file(&pdf);
    let mut child = std::process::Command::new(&browser)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-pdf-header-footer",
            "--no-first-run",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", pdf.display()))
        .arg(format!("file://{}", page.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| format!("{}: {err}", browser.display()))?;
    // Chrome can linger after writing the file, so a finished file is
    // enough: one whose size has stopped changing.
    let size = || std::fs::metadata(&pdf).map_or(0, |meta| meta.len());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(PRINT_SECONDS);
    let mut last = 0;
    let printed = loop {
        let now = size();
        if now > 0 && now == last {
            break true;
        }
        last = now;
        match child.try_wait() {
            Ok(Some(_)) => break size() > 0,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(250))
            }
            _ => break false,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&profile);
    if printed {
        Ok(())
    } else {
        Err(format!(
            "{} did not print it within {PRINT_SECONDS} seconds",
            browser.display()
        ))
    }
}

/// How long a browser gets to print a page.
const PRINT_SECONDS: u64 = 60;

/// Shows `path` in the file manager.
fn reveal(path: &Path) {
    use std::process::Command;
    let shown = if cfg!(target_os = "macos") {
        Command::new("open").arg("-R").arg(path).spawn().is_ok()
    } else if cfg!(windows) {
        Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .is_ok()
    } else {
        false
    };
    if !shown && let Some(folder) = path.parent() {
        let _ = open::that_detached(folder);
    }
}

/// Opens what comes next (a new doc, or Drive and the file), unless `open`
/// is false, and says what was done.
pub fn finish(exported: &Exported, account: Option<&str>, open: bool) -> String {
    let as_account = |url: &str| match account {
        Some(account) => format!("{url}?authuser={}", gdoc::account_query(account)),
        None => url.to_string(),
    };
    match exported {
        Exported::Wrote(path) => format!("wrote {}", path.display()),
        Exported::Paste => {
            if open {
                // docs.new cannot choose an account; this address can.
                let url = account.map_or(NEW_DOC.to_string(), |_| {
                    as_account("https://docs.google.com/document/create")
                });
                let _ = open::that_detached(url);
            }
            let paste = if cfg!(target_os = "macos") { "⌘V" } else { "Ctrl+V" };
            format!("copied; {paste} in a new Google Doc")
        }
        Exported::Upload(path) => {
            if open {
                reveal(path);
                let _ = open::that_detached(as_account(DRIVE));
            }
            format!(
                "wrote {} with its suggestions and notes; drop it into Google Drive and open it with Google Docs",
                path.display()
            )
        }
    }
}

/// `:export` and `stet ctl export`, against the live document.
pub fn in_window(
    ed: &mut Editor,
    format: &str,
    out: Option<&Path>,
    account: Option<&str>,
    open: bool,
) -> Result<(Exported, String), String> {
    let exported = export(ed, format, out)?;
    if exported == Exported::Paste {
        ed.copy_for_docs();
    }
    let said = finish(&exported, account, open);
    Ok((exported, said))
}

// --- Command line ---------------------------------------------------------------

pub fn editor_for(path: &Path) -> Result<Editor, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut ed = Editor::new(
        stet_core::Config::default(),
        Box::new(stet_core::MemoryClipboard::default()),
    );
    ed.set_text(&text);
    ed.path = Some(std::path::absolute(path).unwrap_or(path.to_path_buf()));
    ed.refresh();
    Ok(ed)
}

/// `stet import`. Returns the file written and what it holds.
pub fn import(args: &[String]) -> Result<(PathBuf, String), String> {
    let (mut sources, mut how) = (Vec::new(), gdoc::Import::default());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--browser" => how.browser = true,
            "--md" => how.markdown = true,
            "--account" => how.account = Some(args.next().ok_or("--account needs an email or a number")?),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => sources.push(arg.as_str()),
        }
    }
    let (source, out) = match sources[..] {
        [source] => (source, None),
        [source, out] => (source, Some(Path::new(out))),
        _ => return Err(String::new()),
    };
    let path = Path::new(source);
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    if !path.exists() && gdoc::doc_id(source).is_some() {
        how.out = out;
        return gdoc::import(source, &how);
    }
    let out = out.map_or_else(|| path.with_extension("md"), Path::to_path_buf);
    match extension.as_deref() {
        Some("ipynb") => ipynb::import_file(path, &out).map(|said| (out, said)),
        Some("docx") => {
            let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
            let imported = docx::import(&bytes, &out)?;
            Ok((out.clone(), format!("wrote {} ({imported})", out.display())))
        }
        _ if !path.exists() => Err(format!("{source}: no such file, and not a Google Docs link")),
        _ => Err(format!("{source}: stet imports .ipynb, .docx and Google Docs links")),
    }
}

/// `stet export`. Says what it did.
fn export_cli(args: &[String]) -> Result<String, String> {
    let (mut files, mut format, mut account, mut open) = (Vec::new(), None, None, true);
    let mut code = Code::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--account" => account = Some(args.next().ok_or("--account needs an email or a number")?.as_str()),
            "--no-open" => open = false,
            "--code" => code = Code::All,
            "--no-code" => code = Code::None,
            flag if flag
                .strip_prefix("--")
                .is_some_and(|name| EXPORT_FORMATS.contains(&name)) =>
            {
                format = Some(&flag[2..])
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => files.push(Path::new(arg)),
        }
    }
    let (Some(format), [source, rest @ ..]) = (format, &files[..]) else {
        return Err(String::new());
    };
    // Named on the command line: from where it was typed.
    let out = match rest {
        [] => None,
        [out] => Some(std::path::absolute(out).unwrap_or(out.to_path_buf())),
        _ => return Err(String::new()),
    };
    let mut ed = editor_for(source)?;
    let exported = export_with(&mut ed, format, out.as_deref(), code)?;
    let said = finish(&exported, account, open);
    if exported == Exported::Paste {
        let text = ed.buf.text();
        let html = docs_html(&text, ed.path.as_deref().and_then(Path::parent));
        gdoc::paste(text, html)?;
    }
    Ok(said)
}

/// `stet import` and `stet export`. Returns the process exit code.
pub fn cli(command: &str, args: &[String]) -> i32 {
    let result = match command {
        "import" => import(args).map(|(_, said)| said),
        _ => export_cli(args),
    };
    match result {
        Ok(said) => {
            eprintln!("stet: {said}");
            0
        }
        Err(err) if err.is_empty() => {
            eprintln!("{USAGE}");
            2
        }
        Err(err) => {
            eprintln!("stet {command}: {err}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(dir: &Path, text: &str) -> Editor {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, text).unwrap();
        editor_for(&path).unwrap()
    }

    #[test]
    fn every_format_lands_beside_the_document() {
        let dir = std::env::temp_dir().join(format!("stet-convert-{}", std::process::id()));
        let mut ed = doc(&dir, "# Plan\n\nPlain words.\n");
        for (format, extension) in [("html", "html"), ("ipynb", "ipynb"), ("docx", "docx")] {
            assert_eq!(
                export(&mut ed, format, None),
                Ok(Exported::Wrote(dir.join(format!("doc.{extension}"))))
            );
            assert!(dir.join(format!("doc.{extension}")).exists());
        }
        // Nothing to review: pasted. With a suggestion: a Word file for Drive.
        assert_eq!(export(&mut ed, "gdoc", None), Ok(Exported::Paste));
        ed.set_text("# Plan\n\n{++New ++}{>>id:s_aaaaaaaa by:Ann<<}words.\n");
        assert_eq!(
            export(&mut ed, "gdoc", Some(Path::new("out.docx"))),
            Ok(Exported::Upload(dir.join("out.docx")))
        );
        assert!(
            export(&mut ed, "rtf", None)
                .unwrap_err()
                .contains("cannot export as rtf")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_word_file_comes_back_with_its_review() {
        let dir = std::env::temp_dir().join(format!("stet-convert-docx-{}", std::process::id()));
        let mut ed = doc(
            &dir,
            "# Plan\n\nWe ship {~~Friday~>Monday~~}{>>id:s_aaaaaaaa by:Ann<<} after review.\n",
        );
        let margin = stet_core::editor::margin_path(&dir.join("doc.md"));
        std::fs::create_dir_all(margin.parent().unwrap()).unwrap();
        std::fs::write(&margin, "## Comment from Bo\n@ after review\n\nWhose review?\n").unwrap();
        export(&mut ed, "docx", None).unwrap();
        let args = [dir.join("doc.docx"), dir.join("back.md")].map(|path| path.to_string_lossy().into_owned());
        let (out, said) = import(&args).unwrap();
        assert!(said.contains("1 suggestions, 1 comments"), "{said}");
        let text = std::fs::read_to_string(&out).unwrap();
        let suggestions = stet_core::critic::parse_suggestions(&text);
        assert_eq!(suggestions.len(), 1, "{text}");
        assert_eq!(
            (
                suggestions[0].old_text.as_str(),
                suggestions[0].new_text.as_str(),
                suggestions[0].author.as_str()
            ),
            ("Friday", "Monday", "Ann")
        );
        let margin = std::fs::read_to_string(stet_core::editor::margin_path(&out)).unwrap();
        assert!(
            margin.contains("## Comment from Bo\n@ after review\n\nWhose review?"),
            "{margin}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
