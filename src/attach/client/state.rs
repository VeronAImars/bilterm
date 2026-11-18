//! Pure client-side model of what the host has told us. No I/O, no terminal.

use std::collections::BTreeMap;

use crate::attach::layout::WireLayout;
use crate::attach::limits::MAX_PANE_FILTER;
use crate::attach::msg::{ByeReason, ErrorCode, HostMsg, PaneId};
use crate::attach::wire_grid::{WireDelta, WireGrid};
use crate::vt::snapshot::GridSnapshot;

/// A protocol violation by the host; the client exits with `EXIT_PROTOCOL`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateError(pub String);

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "protocol error: {}", self.0)
    }
}

/// Side effect the caller must perform after [`ClientState::apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Nothing,
    /// Screen data changed (redraw).
    Redraw,
    /// Screen data changed and `Ack{pane, seq}` must be sent.
    Ack { pane: PaneId, seq: u64 },
    Bye(ByeReason),
    Error(ErrorCode, String),
}

pub struct PaneView {
    grid: WireGrid,
    generation: u64,
    snap: Option<GridSnapshot>,
    /// `Some(status)` once the pane's child exited.
    pub exited: Option<Option<i32>>,
}

#[derive(Default)]
pub struct ClientState {
    pub my_id: u64,
    pub layout: WireLayout,
    panes: BTreeMap<PaneId, PaneView>,
    /// User-chosen pane; `None` follows the host's active pane.
    selected: Option<PaneId>,
    /// Effective host size and its owner, from `Resized`.
    pub host_size: Option<(u16, u16)>,
    pub owner: Option<u64>,
    pub has_layout: bool,
}

impl ClientState {
    pub fn new(my_id: u64) -> Self {
        Self { my_id, ..Self::default() }
    }

    /// Pane being shown/typed into: the user's choice if it still exists, else the host's active pane.
    pub fn current_pane(&self) -> Option<PaneId> {
        let known = |id: &PaneId| self.layout.panes.iter().any(|p| p.id == *id);
        self.selected.filter(known).or_else(|| Some(self.layout.active).filter(known)).or_else(|| self.layout.panes.first().map(|p| p.id))
    }

    pub fn pane_title(&self, id: PaneId) -> Option<&str> {
        self.layout.panes.iter().find(|p| p.id == id).map(|p| p.title.as_str())
    }

    /// 1-based position of `id` among panes, and the total.
    pub fn pane_index(&self, id: PaneId) -> Option<(usize, usize)> {
        self.layout.panes.iter().position(|p| p.id == id).map(|i| (i + 1, self.layout.panes.len()))
    }

    /// Moves the selection by `delta` panes (wrapping); returns the new pane.
    pub fn cycle(&mut self, delta: isize) -> Option<PaneId> {
        let cur = self.current_pane()?;
        let n = self.layout.panes.len() as isize;
        let i = self.layout.panes.iter().position(|p| p.id == cur)? as isize;
        let next = self.layout.panes[(i + delta).rem_euclid(n) as usize].id;
        self.selected = Some(next);
        Some(next)
    }

    pub fn pane_exited(&self, id: PaneId) -> Option<Option<i32>> {
        self.panes.get(&id).and_then(|p| p.exited)
    }

    /// Decoded grid of `id` for rendering (cached until the next update).
    pub fn snapshot(&mut self, id: PaneId) -> Option<&GridSnapshot> {
        let p = self.panes.get_mut(&id)?;
        if p.snap.is_none() {
            p.snap = p.grid.to_snapshot(p.generation).ok();
        }
        p.snap.as_ref()
    }

    /// Visible text of `id`, one string per row (trailing blanks trimmed). For tests and `--dump`-style use.
    pub fn pane_text(&mut self, id: PaneId) -> Option<Vec<String>> {
        let s = self.snapshot(id)?;
        Some(
            s.grid_rows
                .iter()
                .map(|r| {
                    let t: String = r.cells.iter().filter(|c| c.width != 0).map(|c| if c.grapheme.is_empty() { " " } else { c.grapheme.as_str() }).collect();
                    t.trim_end().to_string()
                })
                .collect(),
        )
    }

    pub fn apply(&mut self, msg: HostMsg) -> Result<Applied, StateError> {
        Ok(match msg {
            HostMsg::Layout { body } => {
                let layout = WireLayout::from_body(&body).map_err(StateError)?;
                if layout.panes.len() > MAX_PANE_FILTER {
                    return Err(StateError("layout lists too many panes".into()));
                }
                self.panes.retain(|id, _| layout.panes.iter().any(|p| p.id == *id));
                self.layout = layout;
                self.has_layout = true;
                Applied::Redraw
            }
            HostMsg::Snapshot { pane, seq, generation, body } => {
                if self.panes.len() >= MAX_PANE_FILTER && !self.panes.contains_key(&pane) {
                    return Err(StateError("too many panes".into()));
                }
                let grid = WireGrid::from_body(&body).map_err(|e| StateError(e.to_string()))?;
                let snap = grid.to_snapshot(generation).map_err(|e| StateError(e.to_string()))?;
                let exited = self.panes.get(&pane).and_then(|p| p.exited);
                self.panes.insert(pane, PaneView { grid, generation, snap: Some(snap), exited });
                Applied::Ack { pane, seq }
            }
            HostMsg::Delta { pane, base_gen, generation, seq, body } => {
                let p = self.panes.get_mut(&pane).ok_or_else(|| StateError(format!("delta for unknown pane {pane}")))?;
                if p.generation != base_gen {
                    return Err(StateError(format!("delta base {base_gen} does not match generation {}", p.generation)));
                }
                let d = WireDelta::from_body(&body).map_err(|e| StateError(e.to_string()))?;
                p.grid.apply_delta(&d).map_err(|e| StateError(e.to_string()))?;
                p.generation = generation;
                p.snap = None;
                Applied::Ack { pane, seq }
            }
            HostMsg::Resized { cols, rows, owner_client } => {
                self.host_size = Some((cols, rows));
                self.owner = owner_client;
                Applied::Redraw
            }
            HostMsg::Exit { pane, status } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.exited = Some(status);
                }
                Applied::Redraw
            }
            HostMsg::Title { pane, title } => {
                if let Some(p) = self.layout.panes.iter_mut().find(|p| p.id == pane) {
                    p.title = crate::attach::layout::clamp_title(&title);
                }
                Applied::Redraw
            }
            HostMsg::Bye { reason } => Applied::Bye(reason),
            HostMsg::Error { code, message } => Applied::Error(code, message),
            HostMsg::Pong { .. }
            | HostMsg::Bell { .. }
            | HostMsg::Scrollback { .. }
            | HostMsg::ClipboardSet { .. }
            | HostMsg::HelloOk { .. }
            | HostMsg::Refuse { .. } => Applied::Nothing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::layout::WirePaneInfo;
    use crate::attach::wire_grid::WireRow;

    fn layout(ids: &[PaneId], active: PaneId) -> HostMsg {
        let l = WireLayout {
            panes: ids.iter().map(|&id| WirePaneInfo { id, title: format!("p{id}"), rows: 3, cols: 10, read_only: false }).collect(),
            z_order: ids.to_vec(),
            active,
        };
        HostMsg::Layout { body: l.to_body() }
    }

    fn snap(pane: PaneId, seq: u64, gn: u64, text: &str) -> HostMsg {
        let mut g = GridSnapshot::empty(3, 10);
        for (i, ch) in text.chars().enumerate() {
            g.grid_rows[0].cells[i].grapheme = ch.to_string();
        }
        HostMsg::Snapshot { pane, seq, generation: gn, body: WireGrid::from(&g).to_body() }
    }

    fn delta(pane: PaneId, seq: u64, base: u64, gn: u64, row0: &str) -> HostMsg {
        let mut g = GridSnapshot::empty(3, 10);
        for (i, ch) in row0.chars().enumerate() {
            g.grid_rows[0].cells[i].grapheme = ch.to_string();
        }
        let d = WireDelta { rows: vec![(0, WireRow::from_row(&g.grid_rows[0]))], ..Default::default() };
        HostMsg::Delta { pane, base_gen: base, generation: gn, seq, body: d.to_body() }
    }

    #[test]
    fn snapshot_then_delta_then_ack_and_text() {
        let mut s = ClientState::new(5);
        assert_eq!(s.apply(layout(&[1, 2], 2)).unwrap(), Applied::Redraw);
        assert_eq!(s.current_pane(), Some(2));
        assert_eq!(s.apply(snap(1, 1, 10, "hi")).unwrap(), Applied::Ack { pane: 1, seq: 1 });
        assert_eq!(s.pane_text(1).unwrap()[0], "hi");
        assert_eq!(s.apply(delta(1, 2, 10, 11, "yo")).unwrap(), Applied::Ack { pane: 1, seq: 2 });
        assert_eq!(s.pane_text(1).unwrap()[0], "yo");
    }

    #[test]
    fn stale_or_unknown_delta_is_a_protocol_error() {
        let mut s = ClientState::new(1);
        s.apply(layout(&[1], 1)).unwrap();
        assert!(s.apply(delta(1, 1, 0, 1, "x")).is_err());
        s.apply(snap(1, 1, 10, "a")).unwrap();
        assert!(s.apply(delta(1, 2, 9, 11, "x")).is_err());
        assert_eq!(s.pane_text(1).unwrap()[0], "a", "failed delta must not change the grid");
    }

    #[test]
    fn selection_cycles_and_survives_layout_changes() {
        let mut s = ClientState::new(1);
        s.apply(layout(&[1, 2, 3], 1)).unwrap();
        assert_eq!(s.cycle(1), Some(2));
        assert_eq!(s.cycle(1), Some(3));
        assert_eq!(s.cycle(1), Some(1));
        assert_eq!(s.cycle(-1), Some(3));
        s.apply(layout(&[1, 2], 2)).unwrap();
        assert_eq!(s.current_pane(), Some(2), "removed selection falls back to host active pane");
        assert_eq!(s.pane_index(2), Some((2, 2)));
    }

    #[test]
    fn resized_exit_bye_error_and_bounds() {
        let mut s = ClientState::new(1);
        s.apply(layout(&[1], 1)).unwrap();
        s.apply(HostMsg::Resized { cols: 80, rows: 24, owner_client: Some(2) }).unwrap();
        assert_eq!((s.host_size, s.owner), (Some((80, 24)), Some(2)));
        s.apply(snap(1, 1, 1, "")).unwrap();
        s.apply(HostMsg::Exit { pane: 1, status: Some(3) }).unwrap();
        assert_eq!(s.pane_exited(1), Some(Some(3)));
        assert_eq!(s.apply(HostMsg::Bye { reason: ByeReason::Kicked }).unwrap(), Applied::Bye(ByeReason::Kicked));
        assert!(matches!(s.apply(HostMsg::Error { code: ErrorCode::RateLimited, message: "m".into() }).unwrap(), Applied::Error(ErrorCode::RateLimited, _)));
        // Hostile snapshot dims are rejected before allocation.
        let bad = HostMsg::Snapshot { pane: 1, seq: 9, generation: 1, body: {
            let mut b = WireGrid::from(&GridSnapshot::empty(3, 10)).to_body();
            b.insert("cols".into(), 60000.into());
            b
        } };
        assert!(s.apply(bad).is_err());
    }
}
