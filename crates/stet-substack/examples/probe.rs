//! Tries the client against the real Substack with your own cookie.
//!
//!   SUBSTACK_SID='s%3A...' cargo run -p stet-substack --example probe -- <publication> <command> [arg]
//!
//! Commands:
//!   dump FILE        convert FILE and print the JSON; no network, no cookie needed
//!   whoami           GET the profile: user id and publications (read-only)
//!   draft FILE       upload FILE's pictures and create a DRAFT; prints its edit URL
//!   show ID          GET a draft as stored (read-only); prints the body it holds
//!   prepublish ID    GET Substack's pre-publish checks (read-only)
//!
//! It never publishes. The cookie is read from the environment and never printed.

use stet_substack::{Client, DraftOptions, Options, convert, create_draft_from_file};

fn main() {
    if let Err(message) = run() {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [publication, command, rest @ ..] = args.as_slice() else {
        return Err("usage: probe <publication> <dump|whoami|draft|show|prepublish> [FILE|ID]".into());
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
    let cookie = std::env::var("SUBSTACK_SID").map_err(|_| "set SUBSTACK_SID to the substack.sid cookie value")?;
    let client = Client::new(&cookie, publication).map_err(|e| e.to_string())?;
    let id = || -> Result<u64, String> {
        arg.and_then(|a| a.parse().ok())
            .ok_or_else(|| "this command needs a numeric ID".to_string())
    };
    match command.as_str() {
        "whoami" => {
            let profile = client.profile().map_err(|e| e.to_string())?;
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
