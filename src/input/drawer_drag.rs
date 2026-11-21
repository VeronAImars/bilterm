//! Pointer routing for drawer row drag and drop (TUI and GUI share it: the GUI feeds the same mouse
//! events). Gesture only; the drop lowers to Actions in `wm/drawer/drag.rs`.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::AppAction;
use crate::action::Action;
use crate::wm::WindowManager;
use crate::wm::drawer::DrawerNavOp;

/// Handles press/drag/release for a row drag. `None` lets the event continue to the normal drawer handling.
pub(super) fn handle_drawer_row_drag(wm: &mut WindowManager, mouse: &MouseEvent, area: Rect) -> Option<AppAction> {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            wm.drawer_mut().row_drag = None;
            if wm.is_drawer_effectively_visible()
                && !wm.drawer().resizing_drag
                && let Some(idx) = super::drawer_hover::drawer_row_at(wm, area, mouse.column, mouse.row)
                && let Some(row) = wm.drawer_rows().get(idx)
            {
                wm.drawer_drag_begin(row.kind.clone());
            }
            None
        }
        MouseEventKind::Drag(MouseButton::Left) if wm.drawer().row_drag.is_some() => {
            if wm.drawer().resizing_drag {
                wm.drawer_mut().row_drag = None;
                return None;
            }
            let target = super::drawer_hover::drawer_row_at(wm, area, mouse.column, mouse.row);
            wm.drawer_drag_update(target);
            Some(AppAction::Handled)
        }
        MouseEventKind::Up(MouseButton::Left) if wm.drawer().row_drag.is_some() => {
            let moved = wm.drawer_drag_drop();
            moved.then_some(AppAction::Handled)
        }
        _ => None,
    }
}

/// `Esc` during a drag cancels it (lowers to `DrawerNavOp::DragCancel`). `true` when it consumed the key.
pub(super) fn cancel_drag_on_esc(wm: &mut WindowManager) -> bool {
    (wm.drawer_drag_active() || wm.window_drag_active()) && {
        let _ = wm.apply(Action::DrawerNav(DrawerNavOp::DragCancel));
        true
    }
}
