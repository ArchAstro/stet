//! One open document: the editor plus its view, and the effect loop that
//! connects them. The window and the screenshot path both drive a `Session`.

use crate::view::View;
use md_core::{Editor, Effect, KeyEvent};

pub struct Session {
    pub editor: Editor,
    pub view: View,
}

impl Session {
    /// Feeds a key and resolves layout effects. Returns what only the shell
    /// can do (quit, dialogs, fullscreen).
    pub fn key(&mut self, event: KeyEvent) -> Vec<Effect> {
        self.editor.handle_key(event);
        self.settle()
    }

    pub fn text(&mut self, text: &str) -> Vec<Effect> {
        self.editor.insert_text(text);
        self.settle()
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
