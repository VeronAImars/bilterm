//! Read-only export of host state for attach clients (plan section 7, T-A4). Lives next to
//! `host_mode.rs` so `WindowManager`/`CompositePane` fields stay private (ARCH-R02). Works for
//! every pane kind: each `CompositePane` owns a `VtParser`, tmux-backed panes included.

use super::{PaneId, WindowManager};
use crate::attach::diff::PaneFrame;
use crate::attach::layout::{clamp_title, WireLayout, WirePaneInfo};
use crate::attach::limits::MAX_PANE_FILTER;
use crate::attach::session::HostFrames;
use crate::vt::snapshot::GridSnapshot;

/// Borrowed, read-only view of a host workspace.
pub struct HostView<'a> {
    wm: &'a WindowManager,
}

impl WindowManager {
    pub fn host_view(&self) -> HostView<'_> {
        HostView { wm: self }
    }
}

impl HostView<'_> {
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.wm.panes.keys().copied().collect()
    }

    /// Window/pane tree description (capped at `MAX_PANE_FILTER` panes, titles clamped).
    pub fn layout(&self) -> WireLayout {
        let panes: Vec<WirePaneInfo> = self
            .wm
            .panes
            .iter()
            .take(MAX_PANE_FILTER)
            .map(|(&id, p)| {
                let (rows, cols) = p.vt().size();
                WirePaneInfo { id, title: clamp_title(p.title()), rows, cols, read_only: p.is_read_only() }
            })
            .collect();
        let z_order = self.wm.z_stack.order().iter().copied().filter(|id| panes.iter().any(|p| p.id == *id)).collect();
        WireLayout { panes, z_order, active: self.wm.active_pane_id() }
    }

    /// Cloned point-in-time grid of one pane (`None` if the pane does not exist).
    pub fn pane_snapshot(&self, id: PaneId) -> Option<GridSnapshot> {
        self.wm.panes.get(&id).map(|p| p.vt().snapshot())
    }

    /// One tick's export, shared by every attached client.
    pub fn frames(&self) -> HostFrames {
        let layout = self.layout();
        let panes = layout
            .panes
            .iter()
            .filter_map(|p| self.pane_snapshot(p.id).map(|s| (p.id, PaneFrame::new(s))))
            .collect();
        HostFrames { layout, panes }
    }
}
