//! YouTube, where a published document's videos go: the sheet that signs in
//! and says to which channel, and the work behind its buttons. As with
//! Substack, the network is never touched on the UI thread, and what is
//! typed goes from the sheet into the keychain without being kept here.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use stet_core::Editor;
use stet_core::editor::{Button, Row, Sheet};
use stet_youtube::{Channel, Client, Error, Privacy, Ureq, keychain, oauth};

pub const SHEET: &str = "youtube";
/// How long the browser has to come back from Google.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

enum Event {
    /// The channel the stored sign-in uploads to; the flag says Google refused it.
    Channel(Result<Channel, (bool, String)>),
}

pub struct Connector {
    wake: Arc<dyn Fn() + Send + Sync>,
    sender: Sender<(u64, Event)>,
    receiver: Receiver<(u64, Event)>,
    /// Which opening of the sheet answers belong to; late ones are dropped.
    run: u64,
    /// No keychain and no network: a screenshot wants only the sheet.
    pub offline: bool,
}

/// Who sees uploads, as last chosen.
pub fn privacy() -> Privacy {
    crate::platform::State::load()
        .youtube_privacy
        .as_deref()
        .and_then(Privacy::parse)
        .unwrap_or_default()
}

fn sign_in_rows(client_id: &str) -> [Row; 3] {
    [
        Row::text("client-id", "Client ID", client_id, "….apps.googleusercontent.com"),
        Row::secret("client-secret", "Client secret", "GOCSPX-…"),
        Row::note(
            "client-help",
            "",
            "In Google Cloud: enable the YouTube Data API v3, then Credentials → Create OAuth client ID → Desktop app",
        ),
    ]
}

fn signed_in_rows() -> [Row; 3] {
    let labels = Privacy::ALL.map(Privacy::label);
    let chosen = Privacy::ALL.iter().position(|p| *p == privacy()).unwrap_or(0);
    [
        Row::note("channel", "Channel", "Checking your sign-in…"),
        Row::choice("privacy", "Privacy", &labels, chosen),
        Row::note(
            "about",
            "",
            "Publishing uploads each video once; publishing again reuses it.",
        ),
    ]
}

fn signed_in_buttons() -> Vec<Button> {
    vec![Button::new("signout", "Sign out"), Button::new("done", "Done")]
}

/// The sheet as it is when signed out, with `why` if there is something to say.
fn show_signed_out(sheet: &mut Sheet, why: Option<String>) {
    // The client id is not secret; it stays for another try.
    let client_id = sheet.text("client-id").unwrap_or("").to_string();
    for id in ["channel", "privacy", "about"] {
        sheet.remove(id);
    }
    for (at, row) in sign_in_rows(&client_id).into_iter().enumerate() {
        sheet.put(at, row);
    }
    sheet.buttons = vec![Button::new("signin", "Sign in with Google")];
    sheet.busy = false;
    sheet.focus_first();
    sheet.status = why.map(|why| (why, true));
}

impl Connector {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Connector {
        let (sender, receiver) = channel();
        Connector {
            wake,
            sender,
            receiver,
            run: 0,
            offline: false,
        }
    }

    fn spawn(&self, work: impl FnOnce() -> Event + Send + 'static) {
        let (sender, wake, run) = (self.sender.clone(), self.wake.clone(), self.run);
        std::thread::spawn(move || {
            let _ = sender.send((run, work()));
            wake();
        });
    }

    /// Asks Google which channel the stored sign-in belongs to.
    fn check(&self) {
        self.spawn(|| {
            let channel = Client::from_keychain().and_then(|client| client.channel());
            Event::Channel(channel.map_err(|err| (matches!(err, Error::SignedOut(_)), err.to_string())))
        });
    }

    /// `:youtube`: signed in, the channel and who sees uploads; if not, how to sign in.
    pub fn open(&mut self, editor: &mut Editor) {
        self.run += 1;
        let signed_in = !self.offline && keychain::signed_in();
        let sheet = match signed_in {
            true => Sheet::new(SHEET, "YouTube", signed_in_rows().to_vec(), signed_in_buttons()),
            false => Sheet::new(
                SHEET,
                "Connect YouTube",
                sign_in_rows("").to_vec(),
                vec![Button::new("signin", "Sign in with Google")],
            ),
        };
        if signed_in {
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
            "signin" => {
                let id = sheet.text("client-id").unwrap_or("").trim().to_string();
                let secret = sheet.text("client-secret").unwrap_or("").trim().to_string();
                if id.is_empty() || secret.is_empty() {
                    sheet.status = Some(("Paste the client ID and its secret first.".into(), true));
                    return;
                }
                let begun = match oauth::begin(&id) {
                    Ok(begun) => begun,
                    Err(err) => {
                        sheet.status = Some((err.to_string(), true));
                        return;
                    }
                };
                if let Err(err) = open::that_detached(&begun.url) {
                    sheet.status = Some((format!("cannot open the browser: {err}"), true));
                    return;
                }
                // What was typed does not stay in the window while the browser is out.
                sheet.remove("client-secret");
                sheet.busy = true;
                sheet.status = Some(("Finish signing in in your browser…".into(), false));
                self.spawn(move || {
                    let channel = begun
                        .finish(&Ureq::default(), &id, &secret, SIGN_IN_WAIT)
                        .and_then(|credentials| {
                            keychain::store(&credentials)?;
                            Client::signed_in(Ureq::default(), &credentials)?.channel()
                        });
                    Event::Channel(channel.map_err(|err| (false, err.to_string())))
                });
            }
            "signout" => {
                let said = match keychain::forget() {
                    Ok(_) => "Signed out. Videos already uploaded stay on YouTube.".to_string(),
                    Err(err) => err.to_string(),
                };
                sheet.title = "Connect YouTube".into();
                show_signed_out(sheet, None);
                sheet.status = Some((said, false));
            }
            "done" => {
                if let Some(chosen) = sheet.chosen("privacy") {
                    let mut state = crate::platform::State::load();
                    state.youtube_privacy = Some(Privacy::ALL[chosen.min(2)].as_str().to_string());
                    state.save();
                }
                editor.close_sheet();
            }
            _ => editor.close_sheet(),
        }
    }

    /// Takes in what Google answered. True if anything changed.
    pub fn poll(&mut self, editor: &mut Editor) -> bool {
        let mut changed = false;
        while let Ok((run, event)) = self.receiver.try_recv() {
            if run != self.run {
                continue;
            }
            changed = true;
            let Some(sheet) = editor.sheet.as_mut().filter(|sheet| sheet.id == SHEET) else {
                continue;
            };
            match event {
                Event::Channel(Ok(channel)) => {
                    let had_sign_in = sheet.row("client-id").is_some();
                    sheet.title = "YouTube".into();
                    for id in ["client-id", "client-secret", "client-help"] {
                        sheet.remove(id);
                    }
                    for (at, row) in signed_in_rows().into_iter().enumerate() {
                        sheet.put(at, row);
                    }
                    sheet.put(0, Row::note("channel", "Channel", &channel.title));
                    sheet.buttons = signed_in_buttons();
                    sheet.busy = false;
                    sheet.focus_first();
                    sheet.status = had_sign_in.then(|| ("Signed in. Videos will go to this channel.".into(), false));
                }
                Event::Channel(Err((true, _))) => show_signed_out(
                    sheet,
                    Some("Google no longer accepts the saved sign-in. Sign in again.".into()),
                ),
                Event::Channel(Err((false, why))) => match sheet.row("client-id") {
                    // A sign-in that failed: try again from the start.
                    Some(_) => show_signed_out(sheet, Some(why)),
                    None => {
                        sheet.busy = false;
                        sheet.status = Some((why, true));
                    }
                },
            }
        }
        changed
    }
}
