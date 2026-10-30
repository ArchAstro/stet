//! Folded code cells: a folded cell shows its opening fence with a pill
//! saying how much is hidden, and its outputs; the code takes no room.

use super::{Layer, Metrics, Quad, Target, TextItem, View};
use crate::gpu::linear;
use crate::text::{Face, color};
use stet_core::Editor;

/// `line` is inside a folded cell: it is not drawn, though the outputs
/// under it are.
pub(super) fn folded(ed: &Editor, line: usize) -> bool {
    ed.notebook().is_some_and(|book| book.hidden(line))
}

impl View {
    /// The cell whose opening fence is under this point, from the last frame.
    pub(super) fn fence_at(&self, x: f32, y: f32) -> Option<usize> {
        let inside =
            |[left, top, width, height]: [f32; 4]| x >= left && x < left + width && y >= top && y < top + height;
        self.fences
            .iter()
            .rev()
            .find(|(rect, _)| inside(*rect))
            .map(|(_, at)| *at)
    }

    /// The fold handle after a cell's opening fence, laid out as `key` at
    /// `left`, `y`: "▸ N lines" while folded, and "▾" to fold it while the
    /// pointer or the cursor is on the cell.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_fold(
        &mut self,
        ed: &Editor,
        line: usize,
        key: u64,
        left: f32,
        y: f32,
        m: &Metrics,
        layer: &mut Layer,
    ) {
        let Some(book) = ed.notebook().filter(|book| book.on) else {
            return;
        };
        let Some(at) = book.cells.iter().position(|cell| cell.fence.start == line) else {
            return;
        };
        let cell = &book.cells[at];
        let shut = book.is_folded(at);
        let clip = |rect: [f32; 4]| {
            let top = rect[1].max(m.top);
            let bottom = (rect[1] + rect[3]).min(m.bottom);
            [rect[0], top, rect[2], (bottom - top).max(0.0)]
        };
        let height = self.layouts[&key].height;
        self.fences.push((clip([m.origin, y, m.right - m.origin, height]), at));
        let cursor_in = cell.fence.contains(&ed.buf.line_of(ed.cursor));
        if !shut && !cursor_in && self.fence_hover != Some(at) {
            return;
        }
        let text = match cell.code.lines().count() {
            _ if !shut => "▾".to_string(),
            1 => "▸ 1 line".to_string(),
            n => format!("▸ {n} lines"),
        };
        let c = ed.theme.colors;
        let pad = (m.font * 0.18).round();
        let small = (m.font * 0.74).round();
        let label = self.label(ed, &text, c.muted, small, Face::Ui, false);
        let layout = &self.layouts[&key];
        let Some(last) = layout.rows.last() else { return };
        let x = left + last.width + m.advance * 1.5;
        let (width, label_height) = (self.layouts[&label].rows[0].width, self.layouts[&label].height);
        let middle = y + last.top + last.height / 2.0;
        let rect = [
            x - pad * 2.0,
            middle - label_height / 2.0,
            width + pad * 4.0,
            label_height,
        ];
        layer.back.push(Quad {
            rect: clip(rect),
            color: linear(c.code_background, 1.0),
            radius: label_height / 2.0,
        });
        layer.texts.push(TextItem {
            key: label,
            left: x.round(),
            top: (middle - label_height / 2.0).round(),
            clip: [m.origin, m.top, m.right, m.bottom],
            color: color(c.muted),
        });
        let seen = clip(rect);
        if seen[3] > 0.0 {
            self.targets.push((seen, Target::Fold(at)));
        }
    }
}
