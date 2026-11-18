//! Per-client attach state machine (plan section 3.5, T-A4). Pure: no sockets, no clocks (the
//! caller passes a monotonic `now`), no `WindowManager`.
//!
//! Per pane a client is in one of: *needs snapshot* (`view == None`), *synced* (window open) or
//! *blocked* (`MAX_UNACKED_FRAMES` in flight; nothing is queued, state keeps coalescing because
//! each frame is diffed against the last-sent view at send time). Memory per client is
//! `O(panes * rows)` regardless of PTY throughput.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::diff::{ClientPaneView, DiffOutcome, PaneFrame};
use super::layout::WireLayout;
use super::limits::*;
use super::msg::{HostMsg, PaneId, Role};

/// Everything the host exports for one tick; shared by all clients (built once).
#[derive(Debug, Default)]
pub struct HostFrames {
    pub layout: WireLayout,
    pub panes: BTreeMap<PaneId, PaneFrame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckError {
    UnknownPane,
    /// Ack for a seq that was never sent.
    FromFuture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Detached,
}

/// Result of one pump.
#[derive(Debug, Default)]
pub struct Pump {
    pub msgs: Vec<HostMsg>,
    /// Some pane has been blocked past `KICK_DEADLINE`: caller sends `Bye{LaggedResyncFailed}`.
    pub kick: bool,
    /// Panes moved to resync (needs-snapshot) this pump because they were blocked past `LAG_DEADLINE`.
    pub resynced: Vec<PaneId>,
}

#[derive(Debug, Default)]
struct PaneSync {
    sent_seq: u64,
    acked_seq: u64,
    view: Option<ClientPaneView>,
    blocked_since: Option<Duration>,
    resync_forced: bool,
}

impl PaneSync {
    fn inflight(&self) -> u64 {
        self.sent_seq - self.acked_seq
    }
}

#[derive(Debug)]
pub struct ClientSession {
    role: Role,
    filter: Option<BTreeSet<PaneId>>,
    state: SessionState,
    last_layout: Option<WireLayout>,
    panes: BTreeMap<PaneId, PaneSync>,
}

impl ClientSession {
    /// `pane_filter` is capped at `MAX_PANE_FILTER` entries (extra ids are ignored).
    pub fn new(role: Role, pane_filter: Option<&[PaneId]>) -> Self {
        let filter = pane_filter.map(|f| f.iter().copied().take(MAX_PANE_FILTER).collect());
        Self { role, filter, state: SessionState::Active, last_layout: None, panes: BTreeMap::new() }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn detach(&mut self) {
        self.state = SessionState::Detached;
        self.panes.clear();
        self.last_layout = None;
    }

    /// Forces the next frame for `pane` to be a full `Snapshot` (client request or lag).
    pub fn request_resync(&mut self, pane: PaneId) {
        if let Some(p) = self.panes.get_mut(&pane) {
            p.view = None;
        }
    }

    /// Forces a fresh `Layout` and a `Snapshot` for every pane.
    pub fn request_full_resync(&mut self) {
        self.last_layout = None;
        for p in self.panes.values_mut() {
            p.view = None;
        }
    }

    /// Frames queued for the client were dropped before reaching it (lag): treat everything sent
    /// as acknowledged and send fresh snapshots, so flow control does not wait for lost acks.
    pub fn discard_unsent(&mut self) {
        self.last_layout = None;
        for p in self.panes.values_mut() {
            p.acked_seq = p.sent_seq;
            p.view = None;
            p.blocked_since = None;
            p.resync_forced = false;
        }
    }

    pub fn on_ack(&mut self, pane: PaneId, seq: u64) -> Result<(), AckError> {
        let p = self.panes.get_mut(&pane).ok_or(AckError::UnknownPane)?;
        if seq > p.sent_seq {
            return Err(AckError::FromFuture);
        }
        p.acked_seq = p.acked_seq.max(seq);
        if p.inflight() < MAX_UNACKED_FRAMES as u64 {
            p.blocked_since = None;
            p.resync_forced = false;
        }
        Ok(())
    }

    /// Frames in flight for `pane` (sent, not yet acked).
    pub fn inflight(&self, pane: PaneId) -> u64 {
        self.panes.get(&pane).map_or(0, PaneSync::inflight)
    }

    /// Approximate bytes of per-client diff state (bound assertions).
    pub fn state_bytes(&self) -> usize {
        self.panes.values().map(|p| std::mem::size_of::<PaneSync>() + p.view.as_ref().map_or(0, |v| v.approx_bytes())).sum()
    }

    fn visible(&self, id: PaneId) -> bool {
        self.filter.as_ref().is_none_or(|f| f.contains(&id))
    }

    /// Produces the messages this client is owed for `frames`, in order: `Layout` (when changed)
    /// then per-pane `Snapshot`/`Delta`. Never blocks and never queues.
    pub fn pump(&mut self, frames: &HostFrames, now: Duration) -> Pump {
        let mut out = Pump::default();
        if self.state == SessionState::Detached {
            return out;
        }
        let layout = frames.layout.filtered(self.filter.as_ref());
        if self.last_layout.as_ref() != Some(&layout) {
            out.msgs.push(HostMsg::Layout { body: layout.to_body() });
            self.last_layout = Some(layout);
        }
        // Panes that disappeared: drop their state (bounds memory by the live pane set).
        let live: BTreeSet<PaneId> = frames.panes.keys().copied().filter(|id| self.visible(*id)).collect();
        self.panes.retain(|id, _| live.contains(id));

        for id in live {
            let frame = &frames.panes[&id];
            let s = frame.snapshot();
            if s.rows > MAX_ATTACH_ROWS || s.cols > MAX_ATTACH_COLS {
                continue; // never ship (or hash-track) a grid beyond the wire clamp
            }
            let ps = self.panes.entry(id).or_default();
            if ps.inflight() >= MAX_UNACKED_FRAMES as u64 {
                let since = *ps.blocked_since.get_or_insert(now);
                let blocked = now.saturating_sub(since);
                if blocked >= KICK_DEADLINE {
                    out.kick = true;
                } else if blocked >= LAG_DEADLINE && !ps.resync_forced {
                    ps.resync_forced = true;
                    ps.view = None; // drop diff tracking; one Snapshot when the window frees
                    out.resynced.push(id);
                }
                continue;
            }
            match ps.view.as_mut() {
                None => {
                    ps.sent_seq += 1;
                    out.msgs.push(HostMsg::Snapshot {
                        pane: id,
                        seq: ps.sent_seq,
                        generation: s.generation,
                        body: frame.to_wire_grid().to_body(),
                    });
                    ps.view = Some(ClientPaneView::of(frame));
                }
                Some(view) => {
                    let base_gen = view.generation;
                    match view.advance(frame) {
                        DiffOutcome::Unchanged => {}
                        DiffOutcome::Delta(d) => {
                            ps.sent_seq += 1;
                            out.msgs.push(HostMsg::Delta {
                                pane: id,
                                base_gen,
                                generation: s.generation,
                                seq: ps.sent_seq,
                                body: d.to_body(),
                            });
                        }
                        DiffOutcome::NeedSnapshot => {
                            ps.sent_seq += 1;
                            out.msgs.push(HostMsg::Snapshot {
                                pane: id,
                                seq: ps.sent_seq,
                                generation: s.generation,
                                body: frame.to_wire_grid().to_body(),
                            });
                            ps.view = Some(ClientPaneView::of(frame));
                        }
                    }
                }
            }
        }
        out
    }
}
