//! Publishing to Substack: what the sheet says, and the work behind its
//! buttons. The network is never touched on the UI thread, and the cookie
//! is never held here: it goes from the sheet straight into the keychain,
//! and the client reads it from there.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use stet_core::Editor;
use stet_core::editor::{Button, Message, Row, Sheet};
use stet_substack::{
    Client, Cookie, Created, DraftOptions, Error, Options, Profile, PublicationInfo, convert,
    create_draft_from_markdown, keychain,
};

pub const SHEET: &str = "publish";
const AUDIENCES: [(&str, &str); 2] = [("Everyone", "everyone"), ("Paid subscribers", "only_paid")];

enum Event {
    /// Who the stored cookie belongs to; the flag says Substack refused it.
    Profile(Result<Profile, (bool, String)>),
    /// The draft, and which publication it went to.
    Made(Result<Created, String>, String),
}

pub struct Publisher {
    /// Wakes the event loop when the network answers.
    wake: Arc<dyn Fn() + Send + Sync>,
    sender: Sender<(u64, Event)>,
    receiver: Receiver<(u64, Event)>,
    /// Which opening of the sheet answers belong to; late ones are dropped.
    run: u64,
    /// The account's publications, in the order the sheet lists them.
    publications: Vec<PublicationInfo>,
    /// Where the draft can be edited, once there is one.
    made: Option<String>,
    /// No keychain and no network: a screenshot wants only the sheet.
    pub offline: bool,
}

fn sign_in_rows() -> [Row; 2] {
    [
        Row::secret("cookie", "Cookie", "paste your substack.sid"),
        Row::note(
            "cookie-help",
            "",
            "In your browser, on substack.com: DevTools → Application → Cookies → substack.sid",
        ),
    ]
}

fn primary(id: &'static str, label: &str, enabled: bool) -> Button {
    Button {
        enabled,
        ..Button::new(id, label)
    }
}

impl Publisher {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Publisher {
        let (sender, receiver) = channel();
        Publisher {
            wake,
            sender,
            receiver,
            run: 0,
            publications: Vec::new(),
            made: None,
            offline: false,
        }
    }

    /// Runs `work` off the UI thread and wakes the window with its answer.
    fn spawn(&self, work: impl FnOnce() -> Event + Send + 'static) {
        let (sender, wake, run) = (self.sender.clone(), self.wake.clone(), self.run);
        std::thread::spawn(move || {
            let _ = sender.send((run, work()));
            wake();
        });
    }

    /// Asks Substack whose cookie the keychain holds.
    fn check(&self) {
        self.spawn(|| {
            // The profile lives on substack.com itself, whatever the publication.
            let profile = Client::from_keychain("substack").and_then(|client| client.profile());
            Event::Profile(profile.map_err(|err| (matches!(err, Error::CookieRejected { .. }), err.to_string())))
        });
    }

    /// `:publish`: the sheet, filled in from the document.
    pub fn open(&mut self, editor: &mut Editor) {
        self.run += 1;
        self.made = None;
        self.publications.clear();
        let document = convert(&editor.buf.text(), &Options::default());
        let name = editor.file_name();
        let title = document
            .title
            .clone()
            .unwrap_or_else(|| name.rsplit_once('.').map_or(name.clone(), |(stem, _)| stem.to_string()));
        let signed_in = !self.offline && keychain::cookie().is_ok();

        let mut rows = Vec::new();
        match signed_in {
            true => rows.push(Row::note("publication", "Publication", "Checking your sign-in…")),
            false => rows.extend(sign_in_rows()),
        }
        rows.push(Row::text("title", "Title", &title, "Untitled"));
        rows.push(Row::text(
            "subtitle",
            "Subtitle",
            document.subtitle.as_deref().unwrap_or(""),
            "optional",
        ));
        let audiences = AUDIENCES.map(|(label, _)| label);
        rows.push(Row::choice("audience", "Audience", &audiences, 0));
        let pictures = match document.local_images().len() {
            0 => String::new(),
            1 => " One picture will be uploaded.".to_string(),
            count => format!(" {count} pictures will be uploaded."),
        };
        rows.push(Row::note(
            "summary",
            "",
            &format!("It goes up as a draft; nothing is sent to readers.{pictures}"),
        ));
        if let Some(first) = document.warnings.first() {
            let more = match document.warnings.len() - 1 {
                0 => String::new(),
                more => format!(" (and {more} more)"),
            };
            rows.push(Row::note("warnings", "", &format!("Note: {first}{more}")));
        }
        let button = match signed_in {
            true => primary("draft", "Create draft", false),
            false => primary("signin", "Sign in", true),
        };
        let mut sheet = Sheet::new(SHEET, "Publish to Substack", rows, vec![button]);
        if signed_in {
            sheet.focus = sheet.rows.iter().position(|row| row.id == "title").unwrap_or(0);
            self.check();
        }
        editor.open_sheet(sheet);
    }

    /// A button of the sheet was pressed.
    pub fn act(&mut self, editor: &mut Editor, button: &str) {
        let Some(sheet) = editor.sheet.as_mut().filter(|sheet| sheet.id == SHEET) else {
            return;
        };
        match button {
            "open" => {
                if let Some(url) = &self.made {
                    let _ = open::that_detached(url);
                }
                editor.close_sheet();
            }
            "signin" => {
                let kept =
                    Cookie::new(sheet.text("cookie").unwrap_or("")).and_then(|cookie| keychain::store_value(&cookie));
                // Kept or not, what was typed does not stay in the window.
                let [cookie, help] = sign_in_rows();
                match kept {
                    Ok(()) => {
                        sheet.remove("cookie");
                        sheet.remove("cookie-help");
                        sheet.put(0, Row::note("publication", "Publication", "Checking your sign-in…"));
                        *sheet.buttons.last_mut().expect("a sheet has buttons") =
                            primary("draft", "Create draft", false);
                        sheet.status = None;
                        self.check();
                    }
                    Err(err) => {
                        sheet.put(0, cookie);
                        sheet.put(1, help);
                        sheet.status = Some((err.to_string(), true));
                    }
                }
            }
            "draft" => {
                let chosen = sheet.chosen("publication").unwrap_or(0);
                let Some(publication) = self.publications.get(chosen).map(|info| info.subdomain.clone()) else {
                    return;
                };
                let markdown = editor.buf.text();
                let local = !convert(&markdown, &Options::default()).local_images().is_empty();
                // Pictures are found beside the document; without any, anywhere will do.
                let beside = editor.path.as_deref().and_then(std::path::Path::parent);
                let Some(folder) = beside.or((!local).then_some(std::path::Path::new("."))) else {
                    sheet.status = Some(("Save the document first, so its pictures can be found.".into(), true));
                    return;
                };
                let folder = folder.to_path_buf();
                let options = DraftOptions {
                    audience: AUDIENCES[sheet.chosen("audience").unwrap_or(0).min(1)].1.to_string(),
                    title: sheet.text("title").map(str::to_string),
                    subtitle: sheet.text("subtitle").map(str::to_string),
                    ..DraftOptions::default()
                };
                sheet.busy = true;
                sheet.status = Some(("Creating the draft…".into(), false));
                self.spawn(move || {
                    let made = Client::from_keychain(&publication).and_then(|client| {
                        create_draft_from_markdown(&client, &markdown, &folder, "Untitled", &options)
                    });
                    Event::Made(made.map_err(|err| err.to_string()), publication)
                });
            }
            _ => editor.close_sheet(),
        }
    }

    /// Takes in what the network answered. True if anything changed.
    pub fn poll(&mut self, editor: &mut Editor) -> bool {
        let mut changed = false;
        while let Ok((run, event)) = self.receiver.try_recv() {
            if run != self.run {
                continue;
            }
            changed = true;
            let sheet = editor.sheet.as_mut().filter(|sheet| sheet.id == SHEET);
            match (event, sheet) {
                (Event::Profile(Ok(profile)), Some(sheet)) => {
                    self.publications = profile.publications;
                    let names: Vec<&str> = self.publications.iter().map(|info| info.name.as_str()).collect();
                    let remembered = crate::platform::State::load().substack;
                    let chosen = self
                        .publications
                        .iter()
                        .position(|info| Some(&info.subdomain) == remembered.as_ref())
                        .unwrap_or(0);
                    match names.as_slice() {
                        [] => {
                            sheet.status = Some(("This Substack account has no publication to write to.".into(), true));
                            continue;
                        }
                        [only] => sheet.put(0, Row::note("publication", "Publication", only)),
                        _ => sheet.put(0, Row::choice("publication", "Publication", &names, chosen)),
                    }
                    if let Some(button) = sheet.buttons.last_mut() {
                        button.enabled = true;
                    }
                }
                (Event::Profile(Err((refused, why))), Some(sheet)) => {
                    if refused {
                        let [cookie, help] = sign_in_rows();
                        sheet.remove("publication");
                        sheet.put(0, cookie);
                        sheet.put(1, help);
                        sheet.focus_first();
                        *sheet.buttons.last_mut().expect("a sheet has buttons") = primary("signin", "Sign in", true);
                    }
                    let said = match refused {
                        true => "Substack did not accept the saved sign-in. Paste a fresh cookie.".to_string(),
                        false => why,
                    };
                    sheet.status = Some((said, true));
                }
                (Event::Made(Ok(created), publication), sheet) => {
                    let mut state = crate::platform::State::load();
                    state.substack = Some(publication);
                    state.save();
                    let pictures = match created.uploaded_images {
                        0 => String::new(),
                        1 => " One picture uploaded.".to_string(),
                        count => format!(" {count} pictures uploaded."),
                    };
                    match sheet {
                        Some(sheet) => {
                            sheet.busy = false;
                            // What was about to happen has happened.
                            sheet.remove("summary");
                            sheet.status = Some((format!("The draft is on Substack.{pictures}"), false));
                            sheet.buttons = vec![Button::new("open", "Open in Substack")];
                            sheet.focus_button("open");
                        }
                        // Put away while it worked: say so where it will be seen.
                        None => {
                            editor.message = Some(Message {
                                text: format!("the draft is on Substack: {}", created.draft.edit_url),
                                error: false,
                            })
                        }
                    }
                    self.made = Some(created.draft.edit_url);
                }
                (Event::Made(Err(why), _), Some(sheet)) => {
                    sheet.busy = false;
                    sheet.status = Some((why, true));
                }
                (Event::Made(Err(why), _), None) => {
                    editor.message = Some(Message {
                        text: format!("the draft was not made: {why}"),
                        error: true,
                    })
                }
                (Event::Profile(_), None) => {}
            }
        }
        changed
    }
}
