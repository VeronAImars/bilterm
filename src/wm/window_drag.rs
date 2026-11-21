//! Window drag-and-drop with explicit drop zones (owner request: "a clear demarcation on where to drag a
//! window"). Dragging a tiled window by its title-bar grip records only the pointer; nothing is rearranged
//! until release. While dragging, [`WindowManager::window_drag_view`] lists every valid drop zone (swap onto a
//! pane, split edge left/right/top/bottom, drawer session row, float) and the one under the pointer, which the
//! renderer outlines (`ui/drag_zones.rs`, TUI and GUI share it). A release lowers to
//! `WindowManageOp::Place` (swap / split / float) or the drawer's `Move` (`drawer_drop_action`), so the
//! keyboard, colon, RPC and MCP surfaces reach the same behaviour. `Esc` cancels.

use ratatui::layout::Rect;

use super::window_drag_place::PlaceHow;
use super::{WindowManager, WorkspaceLayoutMode, rect_contains};
use crate::action::Action;
use crate::pane::PaneId;
use crate::wm::drawer::DrawerRowKind;
use crate::wm::window_manage::WindowManageOp;

/// What a drop at a zone does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZoneKind {
    /// Swap or split relative to `target` (`how` is never `Float` here).
    Place { target: PaneId, how: PlaceHow },
    /// Move the window to the session of drawer row `row`.
    Drawer(usize),
    /// Detach into the Stacked layout.
    Float,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropZone {
    pub rect: Rect,
    pub kind: DropZoneKind,
}

/// Drag in progress (private to the window manager; read through [`WindowDragView`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowDragState {
    source: PaneId,
    pointer: (u16, u16),
    hover: Option<DropZoneKind>,
}

/// Everything the renderer needs while a window drag is in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowDragView {
    pub source: PaneId,
    pub source_rect: Option<Rect>,
    pub pointer: (u16, u16),
    pub zones: Vec<DropZone>,
    /// Zone under the pointer; `None` = "no drop here".
    pub hover: Option<DropZoneKind>,
    pub drawer_rect: Option<Rect>,
}

/// The five zones of a pane rect: four edge bands and the centre (swap). Small panes get swap only.
pub fn pane_zone_rects(r: Rect) -> Vec<(PlaceHow, Rect)> {
    if r.width < 10 || r.height < 6 {
        return vec![(PlaceHow::Swap, r)];
    }
    let bw = (r.width / 4).clamp(2, 8);
    let bh = (r.height / 4).clamp(1, 4);
    let (mw, mh) = (r.width - 2 * bw, r.height - 2 * bh);
    vec![
        (PlaceHow::Left, Rect::new(r.x, r.y, bw, r.height)),
        (PlaceHow::Right, Rect::new(r.x + r.width - bw, r.y, bw, r.height)),
        (PlaceHow::Up, Rect::new(r.x + bw, r.y, mw, bh)),
        (PlaceHow::Down, Rect::new(r.x + bw, r.y + r.height - bh, mw, bh)),
        (PlaceHow::Swap, Rect::new(r.x + bw, r.y + bh, mw, mh)),
    ]
}

/// Float tray: a 3-row box in the bottom-right corner of the workspace (needs `height >= 8`).
pub fn float_zone_rect(area: Rect) -> Option<Rect> {
    (area.height >= 8 && area.width >= 16).then(|| {
        let w = area.width.min(26);
        Rect::new(area.x + area.width - w, area.y + area.height - 3, w, 3)
    })
}

/// Columns `(first, last)` of the title-bar grip hit area: the single cell at the left end of the title
/// bar (the space before the title text), clear of the right-hand `[_][□][X]` badges and the coordinate badge.
/// `None` when the window is too narrow.
pub fn grip_hit_span(outer: Rect) -> Option<(u16, u16)> {
    (outer.width >= 12 && outer.height >= 3).then(|| (outer.x + 1, outer.x + 1))
}

/// Column of the grip glyph itself.
pub fn grip_glyph_x(outer: Rect) -> Option<u16> {
    grip_hit_span(outer).map(|(a, _)| a)
}

impl WindowManager {
    /// Pane whose grip is hovered (for the highlight), `None` while dragging or when off any grip.
    pub fn grip_hover(&self) -> Option<PaneId> {
        self.grip_hover
    }

    /// Pane being dragged, if a zone drag is in progress.
    pub fn window_drag_source(&self) -> Option<PaneId> {
        self.window_drag.map(|d| d.source)
    }

    /// Tracks the pointer for the grip hover highlight. Returns `true` when the hovered pane changed.
    pub fn update_grip_hover(&mut self, col: u16, row: u16, workspace_area: Rect) -> bool {
        let hit = self.compute_workspace_rects(workspace_area).into_iter().find_map(|(id, outer)| {
            let (a, b) = grip_hit_span(outer)?;
            (row == outer.y && (a..=b).contains(&col)).then_some(id)
        });
        let changed = self.grip_hover != hit;
        self.grip_hover = hit;
        changed
    }

    /// `true` while a window drag with drop zones is in progress.
    pub fn window_drag_active(&self) -> bool {
        self.window_drag.is_some()
    }

    /// Drop zones are shown (default on; `:drag zones off` / `[appearance] drag_zones`).
    pub fn drag_zones_enabled(&self) -> bool {
        self.config.appearance.drag_zones.unwrap_or(true)
    }

    /// Whether a title-bar drag of a tiled window uses the drop-zone gesture.
    pub fn window_drag_uses_zones(&self) -> bool {
        self.layout_mode == WorkspaceLayoutMode::Tiled && self.panes.len() > 1
    }

    /// Pointer moved while dragging `source`. `drawer_row` is the drawer row under the pointer (hit-tested by
    /// the input layer). Returns `true` when the highlighted zone changed.
    pub fn window_drag_update(&mut self, source: PaneId, col: u16, row: u16, workspace_area: Rect, drawer_row: Option<usize>) -> bool {
        let hover = self.zone_at(source, col, row, workspace_area, drawer_row);
        let was = self.window_drag.map(|d| (d.hover, d.pointer));
        self.window_drag = Some(WindowDragState { source, pointer: (col, row), hover });
        was != Some((hover, (col, row)))
    }

    fn zone_at(&self, source: PaneId, col: u16, row: u16, workspace_area: Rect, drawer_row: Option<usize>) -> Option<DropZoneKind> {
        let eff = self.effective_workspace_area(workspace_area);
        if float_zone_rect(eff).is_some_and(|r| rect_contains(r, col, row)) {
            return Some(DropZoneKind::Float);
        }
        if let Some(r) = drawer_row
            && self.drawer_drop_action(&DrawerRowKind::Pane(source), r).is_some()
        {
            return Some(DropZoneKind::Drawer(r));
        }
        self.pane_zones(source, eff)
            .into_iter()
            .find(|z| rect_contains(z.rect, col, row))
            .map(|z| z.kind)
    }

    fn pane_zones(&self, source: PaneId, eff: Rect) -> Vec<DropZone> {
        self.split_tree
            .compute_rects(eff)
            .into_iter()
            .filter(|(id, _)| *id != source)
            .flat_map(|(id, rect)| {
                pane_zone_rects(rect)
                    .into_iter()
                    .map(move |(how, rect)| DropZone { rect, kind: DropZoneKind::Place { target: id, how } })
            })
            .collect()
    }

    /// The overlay model, `None` unless a drag is in progress and zones are enabled.
    pub fn window_drag_view(&self, workspace_area: Rect) -> Option<WindowDragView> {
        let d = self.window_drag?;
        if !self.drag_zones_enabled() {
            return None;
        }
        let eff = self.effective_workspace_area(workspace_area);
        let mut zones = self.pane_zones(d.source, eff);
        if let Some(r) = float_zone_rect(eff) {
            zones.push(DropZone { rect: r, kind: DropZoneKind::Float });
        }
        Some(WindowDragView {
            source: d.source,
            source_rect: self.split_tree.compute_rects(eff).into_iter().find(|(id, _)| *id == d.source).map(|(_, r)| r),
            pointer: d.pointer,
            zones,
            hover: d.hover,
            drawer_rect: self.drawer_rect(workspace_area),
        })
    }

    /// Cancels the drag without changing anything. `true` when one was in progress.
    pub fn window_drag_cancel(&mut self) -> bool {
        self.window_drag.take().is_some()
    }

    /// Ends the drag on release: applies the hovered zone (through `apply`), or does nothing when the pointer
    /// is on "no drop here". Returns the status line to show.
    pub fn window_drag_drop(&mut self) -> Option<String> {
        let d = self.window_drag.take()?;
        let action = match d.hover {
            Some(DropZoneKind::Place { target, how }) => Action::WindowManage(WindowManageOp::Place {
                window_id: Some(d.source),
                target: Some(target),
                how,
            }),
            Some(DropZoneKind::Float) => {
                Action::WindowManage(WindowManageOp::Place { window_id: Some(d.source), target: None, how: PlaceHow::Float })
            }
            Some(DropZoneKind::Drawer(row)) => self.drawer_drop_action(&DrawerRowKind::Pane(d.source), row)?,
            None => return Some("No drop here: window left where it was".to_string()),
        };
        Some(match self.apply(action) {
            Ok(_) => format!("Window #{} dropped", d.source),
            Err(e) => format!("Drop failed: {e}"),
        })
    }
}

#[cfg(test)]
#[path = "window_drag_tests.rs"]
mod tests;
