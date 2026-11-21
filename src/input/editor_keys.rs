//! Keyboard and paste routing for editor windows (`EDIT-01`).
//!
//! - With the editor focused every key goes to the buffer, except Alt/`Ctrl+Alt` chords and
//!   function keys, which stay window-manager shortcuts (leader, user and reserved chords are
//!   resolved before this).
//! - `Ctrl+]` hands the keyboard to the pane's colon prompt and back (the same escape hatch as
//!   the browser canvas).

use crossterm::event::{KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::input::AppAction;
use crate::input::browser_keys::is_focus_toggle_key;
use crate::wm::WindowManager;

pub(super) fn handle_editor_key(wm: &mut WindowManager, key: KeyEvent) -> Option<AppAction> {
    wm.active_pane().editor()?;
    if is_focus_toggle_key(key) {
        if key.kind == KeyEventKind::Press {
            wm.editor_toggle_focus();
        }
        return Some(AppAction::Handled);
    }
    wm.editor_key(key).map(|_| AppAction::Handled)
}

/// Bracketed paste into a focused editor window.
pub(super) fn handle_editor_paste(wm: &mut WindowManager, text: &str) -> Option<AppAction> {
    wm.editor_paste(text).then_some(AppAction::Handled)
}

/// Mouse on an editor window: press places the cursor (and takes the keyboard), drag selects,
/// the wheel scrolls. `None` when the pane is not an editor or the event is outside its canvas.
pub(super) fn editor_mouse(pane: &mut crate::pane::CompositePane, m: MouseEvent, pane_inner: Rect) -> Option<AppAction> {
    pane.editor()?;
    let canvas = pane.canvas_rect(pane_inner);
    let inside = m.column >= canvas.x && m.column < canvas.x + canvas.width && m.row >= canvas.y && m.row < canvas.y + canvas.height;
    if !inside {
        return None;
    }
    let (row, col) = (m.row - canvas.y, m.column - canvas.x);
    let ed = pane.editor_mut()?;
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            ed.focused = true;
            ed.click(row, col);
        }
        MouseEventKind::Drag(MouseButton::Left) => ed.drag(row, col),
        MouseEventKind::ScrollUp => ed.scroll(-3),
        MouseEventKind::ScrollDown => ed.scroll(3),
        _ => return None,
    }
    pane.paint_editor();
    Some(AppAction::Handled)
}
