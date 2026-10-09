//! Tries the client against the real Substack with your own cookie.
//!
//!   cargo run -p stet-substack --example probe -- <publication> <command> [arg]
//!
//! Commands:
//!   store            ask for the substack.sid cookie (hidden) and keep it in the keychain
//!   forget           remove it from the keychain
//!   dump FILE        convert FILE and print the JSON; no network, no cookie needed
//!   whoami           GET the profile: user id and publications (read-only)
//!   draft FILE       upload FILE's pictures and create a DRAFT; prints its edit URL
//!   show ID          GET a draft as stored (read-only); prints the body it holds
//!   prepublish ID    GET Substack's pre-publish checks (read-only)
//!
//! It never publishes. The cookie is read from the keychain and never printed.

use stet_substack::{Client, DraftOptions, Options, convert, create_draft_from_file, keychain};

fn main() {
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [publication, command, rest @ ..] = args.as_slice() else {
        return Err("usage: probe <publication> <store|forget|dump|whoami|draft|show|prepublish> [FILE|ID]".into());
    };
    let arg = rest.first().map(String::as_str);
    if command == "dump" {
        let path = arg.ok_or("dump needs a FILE")?;
        let markdown = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let doc = convert(&markdown, &Options::default());
        println!("title: {:?}\nsubtitle: {:?}", doc.title, doc.subtitle);
        for warning in &doc.warnings {
            println!("warning: {warning}");
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&doc.body).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if command == "store" {
        println!("Paste the value of the substack.sid cookie (it is not shown), twice:");
        keychain::store().map_err(|e| e.to_string())?;
        println!("kept in the login keychain as {:?}", keychain::SERVICE);
        return Ok(());
    }
    if command == "forget" {
        let gone = keychain::forget().map_err(|e| e.to_string())?;
        println!("{}", if gone { "removed" } else { "there was none" });
        return Ok(());
    }
    let client = Client::from_keychain(publication).map_err(|e| e.to_string())?;
    // First to Substack itself: is the cookie good, and is the publication this account's?
    let profile = match client.profile() {
        Ok(profile) => profile,
        // Say as much as can be said about why, without saying the cookie.
        Err(stet_substack::Error::CookieRejected { status }) => {
            println!("sent under both names: HTTP {status}");
            for name in stet_substack::COOKIE_NAMES {
                let alone = Client::from_keychain(publication)
                    .map_err(|e| e.to_string())?
                    .cookie_name(name);
                match alone.profile() {
                    Ok(_) => println!("sent as {name} alone: accepted"),
                    Err(err) => println!("sent as {name} alone: {err}"),
                }
            }
            if !client.cookie_looks_signed() {
                println!("the stored value does not begin `s%3A`, as Substack's session cookie does");
            }
            return Err("Substack did not accept the stored cookie".into());
        }
        Err(err) => return Err(err.to_string()),
    };
    let id = || -> Result<u64, String> {
        arg.and_then(|a| a.parse().ok())
            .ok_or_else(|| "this command needs a numeric ID".to_string())
    };
    match command.as_str() {
        "whoami" => {
            println!("user id: {}", profile.user_id);
            for p in profile.publications {
                println!("publication: {} ({}) -> {}", p.name, p.subdomain, p.origin);
            }
        }
        "draft" => {
            let path = arg.ok_or("draft needs a FILE")?;
            let created = create_draft_from_file(&client, std::path::Path::new(path), &DraftOptions::default())
                .map_err(|e| e.to_string())?;
            for warning in &created.warnings {
                println!("warning: {warning}");
            }
            println!("uploaded images: {}", created.uploaded_images);
            println!("draft {}: {}", created.draft.id, created.draft.edit_url);
        }
        "show" => {
            let draft = client.get_draft(id()?).map_err(|e| e.to_string())?;
            let body = draft["draft_body"].as_str().unwrap_or("null");
            println!("title: {}", draft["draft_title"]);
            // What Substack made of the body, if it says: the tags it rendered.
            if let Some(html) = draft["body_html"].as_str().or(draft["draft_body_html"].as_str()) {
                let mut tags: Vec<&str> = html
                    .split('<')
                    .skip(1)
                    .filter(|tag| !tag.starts_with('/'))
                    .filter_map(|tag| tag.split([' ', '>', '/']).next())
                    .collect();
                tags.dedup();
                eprintln!("rendered html: {} bytes, tags in order: {}", html.len(), tags.join(" "));
            } else {
                let mut keys: Vec<&String> = draft.as_object().map(|o| o.keys().collect()).unwrap_or_default();
                keys.retain(|key| key.contains("html") || key.contains("body") || key.contains("preview"));
                eprintln!("no rendered html in the draft; body-like fields: {keys:?}");
            }
            println!("{body}");
        }
        "prepublish" => {
            let value = client.prepublish(id()?).map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?);
        }
        other => return Err(format!("unknown command {other:?}")),
    }
    Ok(())
}
