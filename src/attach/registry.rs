//! Attach client registry: who is connected, in what role and at what size. Pure bookkeeping
//! shared (behind a short `std::sync::Mutex`) between connection tasks and the control plane so
//! `attach.clients` can read it. Capacity is enforced at slot reservation (`MAX_ATTACH_CLIENTS`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;
use std::time::Instant;

use serde_json::{json, Value};

use super::limits::MAX_ATTACH_CLIENTS;
use super::msg::{ClientKind, Role};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientState {
    /// Connected, `Hello` not yet accepted.
    Handshaking,
    /// `HelloOk` sent.
    Ready,
}

#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub id: u64,
    pub state: ClientState,
    pub kind: Option<ClientKind>,
    pub name: String,
    pub caps: Vec<String>,
    pub role: Option<Role>,
    pub session: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub connected: Instant,
    /// Wakes the connection task so it sends `Bye{kicked}` and closes (`attach.kick`).
    pub kick: Arc<Notify>,
}

#[derive(Debug)]
pub struct ClientRegistry {
    next_id: u64,
    max: usize,
    clients: BTreeMap<u64, ClientInfo>,
}

/// The registry is full.
#[derive(Debug, PartialEq, Eq)]
pub struct RegistryFull;

/// `attach.kick` named a client id that is not connected.
#[derive(Debug, PartialEq, Eq)]
pub struct UnknownClient(pub u64);

impl ClientRegistry {
    pub fn new(max: usize) -> Self {
        Self { next_id: 1, max, clients: BTreeMap::new() }
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// Reserves a slot (counted against the cap from accept time, before any bytes are read).
    /// Ids are host-assigned and never reused within a process.
    pub fn reserve(&mut self, now: Instant) -> Result<u64, RegistryFull> {
        if self.clients.len() >= self.max {
            return Err(RegistryFull);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.clients.insert(
            id,
            ClientInfo {
                id,
                state: ClientState::Handshaking,
                kind: None,
                name: String::new(),
                caps: Vec::new(),
                role: None,
                session: None,
                cols: 0,
                rows: 0,
                connected: now,
                kick: Arc::new(Notify::new()),
            },
        );
        Ok(id)
    }

    pub fn set_hello(&mut self, id: u64, kind: ClientKind, name: String, caps: Vec<String>) {
        if let Some(c) = self.clients.get_mut(&id) {
            c.state = ClientState::Ready;
            c.kind = Some(kind);
            c.name = name;
            c.caps = caps;
        }
    }

    pub fn set_attached(&mut self, id: u64, session: String, role: Role, cols: u16, rows: u16) {
        if let Some(c) = self.clients.get_mut(&id) {
            c.session = Some(session);
            c.role = Some(role);
            c.cols = cols;
            c.rows = rows;
        }
    }

    pub fn is_attached(&self, id: u64) -> bool {
        self.clients.get(&id).is_some_and(|c| c.session.is_some())
    }

    /// The role the client attached with; `None` before `Attach`.
    pub fn role_of(&self, id: u64) -> Option<Role> {
        self.clients.get(&id).and_then(|c| c.role)
    }

    /// Signals client `id` to close with `Bye{kicked}`. The entry itself is removed when its
    /// connection task ends (the slot guard), so `attach.clients` converges within a tick.
    pub fn kick(&mut self, id: u64) -> Result<(), UnknownClient> {
        self.clients.get(&id).map(|c| c.kick.notify_one()).ok_or(UnknownClient(id))
    }

    /// Kicks every attached read-only client; returns their ids (possibly empty).
    pub fn kick_all_ro(&mut self) -> Vec<u64> {
        let ids: Vec<u64> = self.clients.values().filter(|c| c.role == Some(Role::Ro)).map(|c| c.id).collect();
        for id in &ids {
            let _ = self.kick(*id);
        }
        ids
    }

    pub fn remove(&mut self, id: u64) {
        self.clients.remove(&id);
    }

    pub fn list(&self) -> Vec<ClientInfo> {
        self.clients.values().cloned().collect()
    }

    /// JSON for `attach.clients` / `:daemon attach`. `owner` is reserved for the size-owner
    /// election (T-A3) and is always null here.
    pub fn to_json(&self, now: Instant) -> Value {
        let clients: Vec<Value> = self
            .clients
            .values()
            .map(|c| {
                json!({
                    "id": c.id,
                    "state": match c.state { ClientState::Handshaking => "handshaking", ClientState::Ready => "ready" },
                    "kind": c.kind,
                    "name": c.name,
                    "role": c.role,
                    "session": c.session,
                    "cols": c.cols,
                    "rows": c.rows,
                    "caps": c.caps,
                    "age_ms": u64::try_from(now.saturating_duration_since(c.connected).as_millis()).unwrap_or(u64::MAX),
                })
            })
            .collect();
        json!({ "enabled": true, "count": clients.len(), "max": self.max, "owner": null, "clients": clients })
    }
}

/// JSON reported when this workspace has no attach listener.
pub fn disabled_json() -> Value {
    json!({ "enabled": false, "count": 0, "max": 0, "owner": null, "clients": [] })
}

/// Cloneable handle to the registry.
#[derive(Clone, Debug)]
pub struct SharedRegistry(Arc<Mutex<ClientRegistry>>);

impl Default for SharedRegistry {
    fn default() -> Self {
        Self::new(MAX_ATTACH_CLIENTS)
    }
}

impl SharedRegistry {
    pub fn new(max: usize) -> Self {
        Self(Arc::new(Mutex::new(ClientRegistry::new(max))))
    }

    /// Locks, recovering from poisoning (a panicking task must not wedge the host).
    pub fn lock(&self) -> MutexGuard<'_, ClientRegistry> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reserves a slot; dropping the returned guard releases it.
    pub fn reserve(&self) -> Result<ClientSlot, RegistryFull> {
        let (id, kick) = {
            let mut g = self.lock();
            let id = g.reserve(Instant::now())?;
            let kick = g.clients.get(&id).map(|c| c.kick.clone()).unwrap_or_default();
            (id, kick)
        };
        Ok(ClientSlot { registry: self.clone(), id, kick })
    }

    pub fn to_json(&self) -> Value {
        self.lock().to_json(Instant::now())
    }
}

/// RAII slot: removed from the registry when the connection ends, however it ends.
#[derive(Debug)]
pub struct ClientSlot {
    registry: SharedRegistry,
    id: u64,
    kick: Arc<Notify>,
}

impl ClientSlot {
    /// Completes when the control plane kicks this client (permit is stored: no lost wakeup).
    pub async fn kicked(&self) {
        self.kick.notified().await;
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn registry(&self) -> &SharedRegistry {
        &self.registry
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.registry.lock().remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_ids_and_release() {
        let reg = SharedRegistry::new(2);
        let a = reg.reserve().unwrap();
        let b = reg.reserve().unwrap();
        assert_ne!(a.id(), b.id());
        assert!(reg.reserve().is_err());
        let a_id = a.id();
        drop(a);
        let c = reg.reserve().unwrap();
        assert!(c.id() > b.id() && c.id() != a_id, "ids are never reused");
        assert_eq!(reg.lock().len(), 2);
    }

    #[test]
    fn kick_unknown_and_ro_selection() {
        let reg = SharedRegistry::new(4);
        let a = reg.reserve().unwrap();
        let b = reg.reserve().unwrap();
        reg.lock().set_attached(a.id(), "main".into(), Role::Rw, 80, 24);
        reg.lock().set_attached(b.id(), "main".into(), Role::Ro, 80, 24);
        assert_eq!(reg.lock().kick(99), Err(UnknownClient(99)));
        assert_eq!(reg.lock().kick_all_ro(), vec![b.id()]);
        assert!(reg.lock().kick(a.id()).is_ok());
    }

    #[tokio::test]
    async fn kick_wakes_slot_even_if_signalled_first() {
        let reg = SharedRegistry::new(2);
        let a = reg.reserve().unwrap();
        reg.lock().kick(a.id()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), a.kicked()).await.expect("stored permit");
    }

    #[test]
    fn json_shape_and_states() {
        let reg = SharedRegistry::new(4);
        let s = reg.reserve().unwrap();
        let j = reg.to_json();
        assert_eq!(j["clients"][0]["state"], "handshaking");
        reg.lock().set_hello(s.id(), ClientKind::Tui, "t".into(), vec!["diff".into()]);
        reg.lock().set_attached(s.id(), "main".into(), Role::Ro, 80, 24);
        let j = reg.to_json();
        assert_eq!(j["count"], 1);
        assert_eq!(j["max"], 4);
        assert_eq!(j["clients"][0]["state"], "ready");
        assert_eq!(j["clients"][0]["role"], "ro");
        assert_eq!(j["clients"][0]["kind"], "tui");
        assert!(j["owner"].is_null());
        assert_eq!(disabled_json()["enabled"], false);
    }
}
