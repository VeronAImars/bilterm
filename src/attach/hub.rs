//! Host-side rendezvous between the single-owner control plane and the per-client connection
//! tasks (plan T-A6). The plane publishes one [`Published`] per tick on a `watch` channel (so a
//! slow client only ever sees the latest: that is the coalescing), owns size-owner election and
//! hands resizes back to the plane, and forwards pane-level refusals to the originating client.
//! Locks are short `std::sync::Mutex` sections that never await.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::limits::MAX_ATTACH_CLIENTS;
use super::msg::{ErrorCode, HostMsg};
use super::session::HostFrames;
use super::size_owner::{ClientId, Role as SizeRole, SizeError, SizeOwner};

/// Replies queued per client before extra refusals are dropped (they are advisory).
const REPLY_QUEUE: usize = 8;

/// One tick of host state shared by every client.
#[derive(Debug, Default)]
pub struct Published {
    pub frames: HostFrames,
    pub owner: Option<ClientId>,
    /// Size the host currently runs at (the size owner's), if any client has set one.
    pub size: Option<(u16, u16)>,
}

struct State {
    owner: SizeOwner,
    pending_apply: Option<(u16, u16)>,
    replies: BTreeMap<ClientId, mpsc::Sender<HostMsg>>,
}

pub struct AttachHub {
    epoch: tokio::time::Instant,
    tx: watch::Sender<Arc<Published>>,
    state: Mutex<State>,
}

impl std::fmt::Debug for AttachHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachHub").finish_non_exhaustive()
    }
}

/// Dropping it detaches the client from the size-owner election and the reply routing.
pub struct LeaveGuard {
    hub: Arc<AttachHub>,
    id: ClientId,
}

impl Drop for LeaveGuard {
    fn drop(&mut self) {
        self.hub.leave(self.id);
    }
}

/// A client's membership: the guard plus its two inbound streams.
pub struct Joined {
    pub guard: LeaveGuard,
    pub replies: mpsc::Receiver<HostMsg>,
    pub frames: watch::Receiver<Arc<Published>>,
}

impl Default for AttachHub {
    fn default() -> Self {
        Self::new()
    }
}

fn to_size_role(r: super::msg::Role) -> SizeRole {
    match r {
        super::msg::Role::Rw => SizeRole::Rw,
        super::msg::Role::Ro => SizeRole::Ro,
    }
}

impl AttachHub {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(Arc::new(Published::default()));
        Self {
            epoch: tokio::time::Instant::now(),
            tx,
            state: Mutex::new(State { owner: SizeOwner::default(), pending_apply: None, replies: BTreeMap::new() }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Monotonic time since the hub was created (drives sessions and the resize debounce).
    pub fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    fn note(st: &mut State, apply: Option<(u16, u16)>) {
        if apply.is_some() {
            st.pending_apply = apply;
        }
    }

    /// Registers an attached client (validates its size). The receiver is marked unseen-free so
    /// the client waits for the first publish *after* joining.
    pub fn join(self: &Arc<Self>, id: ClientId, role: super::msg::Role, cols: u16, rows: u16) -> Result<Joined, SizeError> {
        let now = self.now();
        let (rtx, rrx) = mpsc::channel(REPLY_QUEUE);
        let mut st = self.lock();
        let out = st.owner.register(id, to_size_role(role), cols, rows, now)?;
        Self::note(&mut st, out.apply);
        st.replies.insert(id, rtx);
        drop(st);
        Ok(Joined { guard: LeaveGuard { hub: self.clone(), id }, replies: rrx, frames: self.tx.subscribe() })
    }

    fn leave(&self, id: ClientId) {
        let now = self.now();
        let mut st = self.lock();
        st.replies.remove(&id);
        let out = st.owner.detach(id, now);
        Self::note(&mut st, out.apply);
    }

    /// `Resize` from `id`: `ro` and bad dimensions are rejected, the rest is debounced.
    pub fn resize(&self, id: ClientId, cols: u16, rows: u16) -> Result<(), SizeError> {
        let now = self.now();
        let mut st = self.lock();
        let out = st.owner.resize(id, cols, rows, now)?;
        Self::note(&mut st, out.apply);
        Ok(())
    }

    /// An `rw` client typed: it becomes the size owner.
    pub fn activity(&self, id: ClientId) {
        let now = self.now();
        let mut st = self.lock();
        if let Ok(out) = st.owner.activity(id, now) {
            Self::note(&mut st, out.apply);
        }
    }

    /// Advisory reply to one client (pane-level refusals). Dropped when its queue is full.
    pub fn reply(&self, id: ClientId, code: ErrorCode, message: impl Into<String>) {
        let tx = self.lock().replies.get(&id).cloned();
        if let Some(tx) = tx {
            let _ = tx.try_send(HostMsg::Error { code, message: message.into() });
        }
    }

    pub fn has_clients(&self) -> bool {
        !self.lock().replies.is_empty()
    }

    pub fn client_count(&self) -> usize {
        self.lock().replies.len().min(MAX_ATTACH_CLIENTS)
    }

    /// Plane side: runs the debounce trailing edge and returns a resize to apply, if any.
    pub fn take_resize(&self) -> Option<(u16, u16)> {
        let now = self.now();
        let mut st = self.lock();
        let out = st.owner.poll(now);
        Self::note(&mut st, out.apply);
        st.pending_apply.take()
    }

    /// Plane side: publishes this tick's frames together with the current owner and size.
    pub fn publish(&self, frames: HostFrames) {
        let (owner, size) = {
            let st = self.lock();
            (st.owner.owner(), st.owner.applied_size())
        };
        self.tx.send_replace(Arc::new(Published { frames, owner, size }));
    }
}
