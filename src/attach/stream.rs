//! Per-connection streaming state (plan T-A6): the diff session, the lag/resync/kick policy and
//! owner/size announcements. Pure with respect to I/O: it only appends to an [`OutQueue`].

use std::sync::Arc;
use std::time::Duration;

use super::hub::{LeaveGuard, Published};
use super::limits::{KICK_DEADLINE, LAG_DEADLINE};
use super::msg::{ByeReason, HostMsg};
use super::outq::OutQueue;
use super::session::ClientSession;

/// How a streaming step ended.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamEnd {
    Continue,
    /// Close the connection after sending `Bye{reason}`.
    Kick(ByeReason),
    /// The outbound queue's hard bound was exceeded (client floods without reading).
    Overflow,
}

pub struct ConnStream {
    pub session: ClientSession,
    _guard: LeaveGuard,
    announced: Option<(Option<u64>, (u16, u16))>,
    have_frames: bool,
    /// When the writer last made progress, or when the queue last became non-empty.
    stalled_since: Option<Duration>,
    resynced: bool,
}

impl ConnStream {
    pub fn new(session: ClientSession, guard: LeaveGuard) -> Self {
        Self { session, _guard: guard, announced: None, have_frames: false, stalled_since: None, resynced: false }
    }

    /// Notes a new publish (or the first one after joining).
    pub fn mark_fresh(&mut self) {
        self.have_frames = true;
    }

    /// The writer moved bytes (or the queue drained): the client is alive.
    pub fn on_progress(&mut self, now: Duration, out: &OutQueue) {
        self.stalled_since = if out.is_empty() { None } else { Some(now) };
        self.resynced = false;
    }

    /// Queues what this client is owed from the latest publish. Screen data is only produced
    /// when the previous output has been fully written (so a slow reader coalesces, never queues).
    pub fn pump(&mut self, published: &Arc<Published>, out: &mut OutQueue, now: Duration) -> StreamEnd {
        if !self.have_frames {
            return StreamEnd::Continue;
        }
        if out.is_empty() {
            self.stalled_since = None;
            if let Some(size) = published.size {
                let key = (published.owner, size);
                if self.announced != Some(key) {
                    self.announced = Some(key);
                    let m = HostMsg::Resized { cols: size.0, rows: size.1, owner_client: published.owner };
                    if !out.push(&m) {
                        return StreamEnd::Overflow;
                    }
                }
            }
            let pump = self.session.pump(&published.frames, now);
            for m in &pump.msgs {
                if !out.push(m) {
                    return StreamEnd::Overflow;
                }
            }
            if pump.kick {
                return StreamEnd::Kick(ByeReason::LaggedResyncFailed);
            }
            if !out.is_empty() {
                self.stalled_since = Some(now);
            }
        }
        StreamEnd::Continue
    }

    /// Deadline policy for a writer that is not draining: past `LAG_DEADLINE` unstarted frames are
    /// dropped and the client is resynced with one snapshot later; past `KICK_DEADLINE` it is closed.
    pub fn check_stall(&mut self, out: &mut OutQueue, now: Duration) -> StreamEnd {
        let Some(since) = self.stalled_since else {
            return StreamEnd::Continue;
        };
        let stalled = now.saturating_sub(since);
        if stalled >= KICK_DEADLINE {
            return StreamEnd::Kick(ByeReason::LaggedResyncFailed);
        }
        if stalled >= LAG_DEADLINE && !self.resynced {
            self.resynced = true;
            if out.discard_unstarted() > 0 {
                self.session.discard_unsent();
            }
        }
        StreamEnd::Continue
    }
}
