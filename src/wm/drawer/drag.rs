//! Drag and drop of drawer rows (M4 of `docs/plans/drawer-session-model.md`).
//!
//! The pointer gesture only records a source row and the row under the pointer (the drop indicator);
//! nothing is rearranged while dragging. A release over a row lowers to the very Actions the keyboard,
//! colon, RPC and MCP use (`WindowManageOp::Reorder`/`Move`, `SessionManageOp::MoveOrder`) through
//! `WindowManager::apply`. `Esc` (`DrawerNavOp::DragCancel`) or a release outside the rows cancels.

use crate::action::Action;
use crate::pane::PaneId;
use crate::wm::WindowManager;
use crate::wm::drawer::{DrawerRow, DrawerRowKind, RowDrag, WorkspaceDrawer};
use crate::wm::window_manage::{SessionManageOp, WindowManageOp};

impl WorkspaceDrawer {
    /// Row to draw the drop indicator on, only while a drag is in progress.
    pub fn drop_target_row(&self) -> Option<usize> {
        self.row_drag.as_ref().filter(|d| d.active).and_then(|d| d.target)
    }
}

/// Session header the row at `idx` belongs to (itself when it is a header).
fn session_of_row(rows: &[DrawerRow], idx: usize) -> Option<String> {
    rows.get(..=idx)?.iter().rev().find_map(|r| match &r.kind {
        DrawerRowKind::SessionHeader(s) => Some(s.clone()),
        _ => None,
    })
}

impl WindowManager {
    /// `true` while a row drag is in progress (past the press, pointer left the source row).
    pub fn drawer_drag_active(&self) -> bool {
        self.drawer.row_drag.as_ref().is_some_and(|d| d.active)
    }

    /// Records a press on a draggable row (session header or window); nothing is active yet.
    pub fn drawer_drag_begin(&mut self, source: DrawerRowKind) {
        self.drawer.row_drag = matches!(source, DrawerRowKind::Pane(_) | DrawerRowKind::SessionHeader(_))
            .then_some(RowDrag { source, active: false, target: None });
    }

    /// Pointer over row `target` (or none). Returns `true` when the indicator changed.
    pub fn drawer_drag_update(&mut self, target: Option<usize>) -> bool {
        let rows = self.drawer_rows();
        let Some(d) = self.drawer.row_drag.as_mut() else { return false };
        let on_source = target.and_then(|t| rows.get(t)).is_some_and(|r| r.kind == d.source);
        let was = (d.active, d.target);
        if !d.active && target.is_some() && !on_source {
            d.active = true;
        }
        d.target = target;
        was != (d.active, d.target)
    }

    /// Drops the drag state without doing anything. `true` when a drag was in progress.
    /// Also cancels a window drag-and-drop in progress (`Esc` for both gestures).
    pub fn drawer_drag_cancel(&mut self) -> bool {
        let win = self.window_drag_cancel();
        if win {
            self.set_wm_mouse_drag(None);
        }
        self.drawer.row_drag.take().is_some_and(|d| d.active) || win
    }

    /// The Action a drop of `source` onto row `target` lowers to, `None` when it would change nothing.
    pub fn drawer_drop_action(&self, source: &DrawerRowKind, target: usize) -> Option<Action> {
        let rows = self.drawer_rows();
        let tsess = session_of_row(&rows, target)?;
        match source {
            DrawerRowKind::Pane(id) => {
                let src_idx = rows.iter().position(|r| r.kind == *source)?;
                let ssess = session_of_row(&rows, src_idx)?;
                if tsess != ssess {
                    return Some(Action::WindowManage(WindowManageOp::Move {
                        window_id: Some(*id),
                        target_session: tsess,
                        switch: false,
                    }));
                }
                let DrawerRowKind::Pane(other) = rows.get(target)?.kind else { return None };
                if other == *id {
                    return None;
                }
                let panes: Vec<PaneId> = rows
                    .iter()
                    .enumerate()
                    .filter(|(i, r)| matches!(r.kind, DrawerRowKind::Pane(_)) && session_of_row(&rows, *i).as_deref() == Some(&ssess))
                    .filter_map(|(_, r)| if let DrawerRowKind::Pane(p) = r.kind { Some(p) } else { None })
                    .collect();
                let n = panes.iter().position(|&p| p == other)? + 1;
                Some(Action::WindowManage(WindowManageOp::Reorder { window_id: Some(*id), to: n.to_string() }))
            }
            DrawerRowKind::SessionHeader(name) => {
                if &tsess == name {
                    return None;
                }
                let order = self.ordered_sessions_with_active();
                let n = order.iter().position(|s| *s == tsess)? + 1;
                Some(Action::SessionManage(SessionManageOp::MoveOrder {
                    name: Some(name.clone()),
                    target_pos: n.to_string(),
                }))
            }
            _ => None,
        }
    }

    /// Ends the gesture on release: applies the drop when there is one, keeps the selection on the moved row.
    /// `true` when a drag was in progress (the event is consumed).
    pub fn drawer_drag_drop(&mut self) -> bool {
        let Some(d) = self.drawer.row_drag.take() else { return false };
        if !d.active {
            return false;
        }
        if let Some(action) = d.target.and_then(|t| self.drawer_drop_action(&d.source, t)) {
            let _ = self.apply(action);
            if let Some(pos) = self.drawer_rows().iter().position(|r| r.kind == d.source) {
                self.drawer.selected_index = pos;
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;
