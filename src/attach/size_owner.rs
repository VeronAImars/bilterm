//! Size-owner election and resize debounce (plan section 3.6, Q2; task T-A3).
//!
//! Pure logic with an injected clock (`now` is a monotonic [`Duration`]). The owner is the most
//! recent `rw` client to send `Input` or `Resize`; the host runs at the owner's size. `ro` clients
//! never own or resize. This deliberately does NOT use `negotiate_session_viewport` (min-clamp).
//! Applying a size is debounced: the first apply is immediate, later ones wait until
//! [`RESIZE_DEBOUNCE`] has elapsed since the previous apply (trailing edge via [`SizeOwner::poll`]).

use super::limits::{LimitError, MAX_ATTACH_CLIENTS, validate_dims};
use std::time::Duration;

/// Minimum spacing between two applied host resizes (prevents two-client ping-pong).
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(150);

pub type ClientId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Rw,
    Ro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeError {
    BadDims(LimitError),
    ReadOnly,
    UnknownClient,
    DuplicateClient,
    TooManyClients,
}

impl From<LimitError> for SizeError {
    fn from(e: LimitError) -> Self {
        Self::BadDims(e)
    }
}

/// Result of an event: what the host should do now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Resize the host to `(cols, rows)` now (feeds `Action::AttachResize`).
    pub apply: Option<(u16, u16)>,
    /// Owner after this event if it changed (announce via `Resized{owner_client}`).
    pub owner_changed: Option<Option<ClientId>>,
    /// A resize is pending; call [`SizeOwner::poll`] at or after this time.
    pub retry_at: Option<Duration>,
}

#[derive(Clone, Debug)]
struct Entry {
    id: ClientId,
    role: Role,
    cols: u16,
    rows: u16,
    /// Monotonic activity stamp (higher = more recent).
    seq: u64,
}

#[derive(Debug)]
pub struct SizeOwner {
    clients: Vec<Entry>,
    owner: Option<ClientId>,
    applied: Option<(u16, u16)>,
    last_apply: Option<Duration>,
    seq: u64,
    debounce: Duration,
}

impl Default for SizeOwner {
    fn default() -> Self {
        Self::new(RESIZE_DEBOUNCE)
    }
}

impl SizeOwner {
    pub fn new(debounce: Duration) -> Self {
        Self { clients: Vec::new(), owner: None, applied: None, last_apply: None, seq: 0, debounce }
    }

    pub fn owner(&self) -> Option<ClientId> {
        self.owner
    }

    pub fn applied_size(&self) -> Option<(u16, u16)> {
        self.applied
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Registers a client. An `rw` client becomes owner (it just attached: most recent activity).
    pub fn register(&mut self, id: ClientId, role: Role, cols: u16, rows: u16, now: Duration) -> Result<Outcome, SizeError> {
        validate_dims(cols, rows)?;
        if self.clients.iter().any(|c| c.id == id) {
            return Err(SizeError::DuplicateClient);
        }
        if self.clients.len() >= MAX_ATTACH_CLIENTS {
            return Err(SizeError::TooManyClients);
        }
        let seq = self.bump();
        self.clients.push(Entry { id, role, cols, rows, seq });
        Ok(self.reconcile(now))
    }

    /// `Resize` from a client. `ro` is rejected; dims are validated before anything changes.
    pub fn resize(&mut self, id: ClientId, cols: u16, rows: u16, now: Duration) -> Result<Outcome, SizeError> {
        validate_dims(cols, rows)?;
        let seq = self.bump();
        let e = self.clients.iter_mut().find(|c| c.id == id).ok_or(SizeError::UnknownClient)?;
        if e.role == Role::Ro {
            return Err(SizeError::ReadOnly);
        }
        e.cols = cols;
        e.rows = rows;
        e.seq = seq;
        Ok(self.reconcile(now))
    }

    /// `Input` from a client: an `rw` sender becomes owner. `ro` input is a no-op here
    /// (the input path rejects it separately).
    pub fn activity(&mut self, id: ClientId, now: Duration) -> Result<Outcome, SizeError> {
        let seq = self.bump();
        let e = self.clients.iter_mut().find(|c| c.id == id).ok_or(SizeError::UnknownClient)?;
        if e.role == Role::Rw {
            e.seq = seq;
        }
        Ok(self.reconcile(now))
    }

    /// Client detached/crashed. If it owned, ownership falls to the next-most-recent `rw`
    /// client; with none left the applied size is kept (no SIGWINCH storm on detach).
    pub fn detach(&mut self, id: ClientId, now: Duration) -> Outcome {
        self.clients.retain(|c| c.id != id);
        self.reconcile(now)
    }

    /// Trailing-edge tick: applies a debounced pending resize once due.
    pub fn poll(&mut self, now: Duration) -> Outcome {
        self.reconcile(now)
    }

    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn reconcile(&mut self, now: Duration) -> Outcome {
        let mut out = Outcome::default();
        let new_owner = self.clients.iter().filter(|c| c.role == Role::Rw).max_by_key(|c| c.seq).map(|c| c.id);
        if new_owner.is_some() && new_owner != self.owner {
            self.owner = new_owner;
            out.owner_changed = Some(self.owner);
        } else if new_owner.is_none() && self.owner.is_some() {
            // No rw client left: keep size, announce that nobody owns it any more.
            self.owner = None;
            out.owner_changed = Some(None);
        }
        let Some(want) = self.owner.and_then(|o| self.clients.iter().find(|c| c.id == o)).map(|c| (c.cols, c.rows)) else {
            return out;
        };
        if self.applied == Some(want) {
            return out;
        }
        match self.last_apply {
            Some(t) if now.saturating_sub(t) < self.debounce => out.retry_at = Some(t + self.debounce),
            _ => {
                self.applied = Some(want);
                self.last_apply = Some(now);
                out.apply = Some(want);
            }
        }
        out
    }
}
