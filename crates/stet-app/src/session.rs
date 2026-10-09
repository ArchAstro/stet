//! One open document: the editor plus its view, and the effect loop that
//! connects them. The window and the screenshot path both drive a `Session`.

use crate::images;
use crate::retouch::{self, Outcome, Retouch};
use crate::view::View;
use stet_core::editor::Message;
use stet_core::{Editor, Effect, Key, KeyEvent};

pub struct Session {
    pub editor: Editor,
    pub view: View,
    #[cfg(feature = "substack")]
    pub publish: crate::publish::Publisher,
}

impl Session {
    /// Feeds a key and resolves layout effects. Returns what only the shell
    /// can do (quit, dialogs, fullscreen).
    pub fn key(&mut self, event: KeyEvent) -> Vec<Effect> {
        // A picture being retouched has the keyboard, except to quit.
        if let Some(retouch) = &mut self.view.retouch {
            let quit = event.mods.sup && event.key == Key::Char('q');
            if quit || retouch.key(event) == Outcome::Done {
                self.finish_retouch();
            }
            if !quit {
                return Vec::new();
            }
        }
        self.editor.handle_key(event);
        self.settle()
    }

    pub fn text(&mut self, text: &str) -> Vec<Effect> {
        if let Some(retouch) = &mut self.view.retouch {
            retouch.text(text);
            return Vec::new();
        }
        self.editor.insert_text(text);
        self.settle()
    }

    fn say(&mut self, said: Result<String, String>) {
        let (text, error) = match said {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        self.editor.message = Some(Message { text, error });
    }

    /// Takes the picture `url` on `line` out of the text to be retouched,
    /// growing from `from`, where it was drawn.
    pub fn retouch(&mut self, line: usize, url: &str, from: Option<[f32; 4]>) {
        let editor = &self.editor;
        let opened = images::resolve(url, editor.path.as_deref(), editor.config.remote_images)
            .filter(|source| source.video().is_none())
            .ok_or(format!("{url} is not a picture that can be retouched"))
            .and_then(|source| self.view.images.bytes(&source).ok_or(format!("{url} cannot be read")))
            .and_then(|bytes| retouch::decode(&bytes));
        match opened {
            Ok(picture) => {
                let mut retouch = Retouch::new(picture, line, url.to_string(), from);
                retouch.still |= self.view.images.blocking;
                self.view.retouch = Some(retouch);
            }
            Err(err) => self.say(Err(err)),
        }
    }

    /// `:image`: the picture on the cursor's line.
    fn retouch_here(&mut self) {
        self.editor.refresh();
        let line = self.editor.buf.line_of(self.editor.cursor);
        let url = self.editor.doc().images_on(line).next().map(|image| image.url.clone());
        match url {
            Some(url) => self.retouch(line, &url, self.view.picture_rect(line, &url)),
            None => self.say(Err("there is no picture on this line".to_string())),
        }
    }

    pub fn retouch_press(&mut self, x: f32, y: f32) {
        let view = &mut self.view;
        if let Some(retouch) = &mut view.retouch
            && retouch.press(x, y, &mut view.fonts) == Outcome::Done
        {
            self.finish_retouch();
        }
    }

    /// Puts the picture back. If anything was done to it, the result is
    /// kept beside the original and the text refers to that instead.
    pub fn finish_retouch(&mut self) {
        let view = &mut self.view;
        let Some(retouch) = view.retouch.as_mut().filter(|retouch| !retouch.closing()) else {
            return;
        };
        retouch.close();
        let (line, url) = (retouch.line, retouch.url.clone());
        let Some(png) = retouch.finished(&mut view.fonts) else {
            return;
        };
        let kept = png.and_then(|png| self.editor.replace_image(line, &url, &png));
        self.say(kept.map(|dest| format!("kept as {dest}  ·  undo brings the original back")));
    }

    /// Applies pending effects and keeps the cursor on screen.
    pub fn settle(&mut self) -> Vec<Effect> {
        let mut rest = Vec::new();
        let mut moved_visually = false;
        for effect in self.editor.take_effects() {
            match effect {
                Effect::VisualMove(delta) => {
                    self.view.visual_move(&mut self.editor, delta);
                    moved_visually = true;
                }
                Effect::Scroll(to) => {
                    self.view.follow_cursor(&mut self.editor);
                    self.view.scroll_cursor_to(&mut self.editor, to);
                    return rest;
                }
                Effect::ThemeChanged => {
                    self.view.invalidate();
                    rest.push(Effect::ThemeChanged);
                }
                Effect::FontChanged => {
                    self.view.fonts.resolve(&self.editor.config);
                    self.view.invalidate();
                    rest.push(Effect::FontChanged);
                }
                Effect::EditImage => self.retouch_here(),
                #[cfg(feature = "substack")]
                Effect::Publish => self.publish.open(&mut self.editor),
                #[cfg(feature = "substack")]
                Effect::Sheet { sheet, button } if sheet == crate::publish::SHEET => {
                    self.publish.act(&mut self.editor, button)
                }
                #[cfg(not(feature = "substack"))]
                Effect::Publish => self.say(Err("this build of stet cannot publish".to_string())),
                Effect::Zoom(step) => {
                    self.view.set_zoom(step);
                    rest.push(Effect::Zoom(step));
                }
                other => rest.push(other),
            }
        }
        if !moved_visually {
            self.view.goal_x = None;
        }
        self.view.follow_cursor(&mut self.editor);
        rest
    }
}
