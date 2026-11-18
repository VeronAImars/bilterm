//! Mouse-hover row hit-testing for the drawer. A pointer move lowers to
//! `Action::DrawerNav(DrawerNavOp::Hover(row))` (`ARCH-R01`); this module only resolves which row, if any,
//! is under the pointer, for the Navigator and Classic styles.

use ratatui::layout::Rect;

use crate::action::Action;
use crate::ui::drawer::navigator::navigator_row_at;
use crate::wm::drawer::{DrawerNavOp, DrawerStyle};
use crate::wm::{WindowManager, rect_contains};

/// Index of the drawer row under `(col, row)`, `None` over chrome, gaps or outside the drawer.
pub fn drawer_row_at(wm: &WindowManager, workspace_area: Rect, col: u16, row: u16) -> Option<usize> {
    let dr = wm.drawer_rect(workspace_area)?;
    if !rect_contains(dr, col, row) {
        return None;
    }
    if wm.drawer_config().style == DrawerStyle::Navigator {
        return navigator_row_at(wm, workspace_area, col, row);
    }
    let rows_len = wm.drawer_rows().len();
    let selected = wm.drawer().selected_index.min(rows_len.saturating_sub(1));
    let idx = if wm.drawer_config().edge.is_vertical() {
        let inner_h = dr.height.saturating_sub(2);
        let (hdr_h, ftr_h, row_step) =
            crate::ui::drawer::drawer_vertical_layout_metrics(wm.config(), inner_h, rows_len);
        let list_start_y = dr.y + 1 + hdr_h;
        let list_end_y = dr.y + dr.height.saturating_sub(1 + ftr_h);
        if row < list_start_y || row >= list_end_y || col == dr.x || col + 1 >= dr.x + dr.width {
            return None;
        }
        let list_rows_px = usize::from(inner_h.saturating_sub(hdr_h + ftr_h)).max(1);
        let cap = (list_rows_px / row_step).max(1);
        let scroll = wm.drawer().effective_scroll(selected, cap, rows_len);
        let rel = usize::from(row - list_start_y) / row_step;
        if rel >= cap {
            return None;
        }
        scroll + rel
    } else {
        // Ribbon mode: the first line is the ribbon, the rows below it end at the selected one.
        if row <= dr.y + 1 {
            return None;
        }
        let remaining = usize::from(dr.height.saturating_sub(3));
        let first = selected.saturating_sub(remaining.saturating_sub(1));
        let rel = usize::from(row - (dr.y + 2));
        if rel >= remaining {
            return None;
        }
        first + rel
    };
    (idx < rows_len).then_some(idx)
}

/// Applies the hover for a pointer at `(col, row)`; `true` when the highlight changed (a redraw is due).
pub fn apply_drawer_hover(wm: &mut WindowManager, workspace_area: Rect, col: u16, row: u16) -> bool {
    let target = if wm.is_drawer_effectively_visible() { drawer_row_at(wm, workspace_area, col, row) } else { None };
    if wm.drawer().hover_row == target {
        return false;
    }
    wm.apply(Action::DrawerNav(DrawerNavOp::Hover(target))).is_ok()
}

#[cfg(test)]
#[path = "drawer_hover_tests.rs"]
mod tests;
