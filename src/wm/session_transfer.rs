//! Workspace side of live session transfer between session hosts (unix; `docs/plans/session-handover-spike.md`).
//!
//! Sender: [`WindowManager::stage_transfer`] releases every PTY of the named sessions (shells keep
//! running, nothing is closed), after which [`commit_transfer`](WindowManager::commit_transfer)
//! drops the sessions or [`rollback_transfer`](WindowManager::rollback_transfer) reclaims every PTY.
//! Receiver: [`WindowManager::install_transfer`] adopts the fds into panes under the same session
//! names and pane ids.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::time::Duration;

use super::window_manage::BackgroundSessionState;
use super::{PaneId, WindowManager};
use crate::daemon::transfer::wire::{Offer, SessionBundle, MAX_PANES, MAX_SESSIONS, XFER_VERSION};
use crate::pane::CompositePane;
use crate::pty::handover::{AdoptMeta, ReleasedPty};
use crate::pty::PtySession;

/// How long the sender waits for a reader to stop or input to drain before giving up.
const RELEASE_PATIENCE: Duration = Duration::from_secs(3);

/// PTYs detached from the sender, waiting for the receiver's verdict.
pub struct StagedTransfer {
    pub sessions: Vec<SessionBundle>,
    entries: Vec<(String, PaneId, ReleasedPty)>,
}

impl StagedTransfer {
    /// Master fds in offer order (sessions, then panes).
    pub fn raw_fds(&self) -> Vec<std::os::fd::RawFd> {
        self.entries.iter().map(|(_, _, r)| r.raw_fd()).collect()
    }
}

impl WindowManager {
    fn panes_of_mut(&mut self, name: &str) -> Option<&mut BTreeMap<PaneId, CompositePane>> {
        if self.config.history_session == name {
            return Some(&mut self.panes);
        }
        self.window_manage.background_sessions.get_mut(name).map(|b| &mut b.panes)
    }

    /// Names of every live session, active first.
    pub fn live_session_names(&self) -> Vec<String> {
        let mut v = vec![self.config.history_session.clone()];
        v.extend(self.window_manage.background_sessions.keys().filter(|k| **k != self.config.history_session).cloned());
        v
    }

    /// `(window id, child pid, terminal replay bytes)` of every window of live session `name`
    /// (diagnostics and tests: the replay is the full history + screen state).
    pub fn session_window_dump(&mut self, name: &str) -> Vec<(PaneId, Option<u32>, Vec<u8>)> {
        self.panes_of_mut(name)
            .map(|m| m.values_mut().map(|p| (p.id(), p.child_pid(), p.vt_mut().export_replay())).collect())
            .unwrap_or_default()
    }

    /// Name this host runs under (set by the daemon), used to refuse a transfer to itself.
    pub fn host_name(&self) -> Option<&str> {
        self.host_name.as_deref()
    }

    pub fn set_host_name(&mut self, name: &str) {
        self.host_name = Some(name.to_string());
    }

    /// Asks the daemon loop to exit shortly (after the reply to the current request went out).
    pub fn request_retire(&mut self) {
        self.retire_at = Some(std::time::Instant::now() + Duration::from_millis(400));
    }

    pub fn retire_due(&self) -> bool {
        self.retire_at.is_some_and(|t| std::time::Instant::now() >= t)
    }

    #[allow(clippy::type_complexity)]
    fn session_parts(&self, name: &str) -> Option<(crate::wm::split_tree::SplitTree, crate::wm::z_stack::ZStack, Option<PaneId>, super::WorkspaceLayoutMode, Vec<PaneId>)> {
        if self.config.history_session == name {
            return Some((self.split_tree.clone(), self.z_stack.clone(), self.zoomed_pane, self.layout_mode, self.ordered_pane_ids()));
        }
        let bg = self.window_manage.background_sessions.get(name)?;
        Some((bg.split_tree.clone(), bg.z_stack.clone(), bg.zoomed_pane, bg.layout_mode, bg.window_order.clone()))
    }

    /// Releases every PTY of `names`. All-or-nothing: on any failure everything already released is
    /// reclaimed and the sessions are exactly as before.
    pub fn stage_transfer(&mut self, names: &[String]) -> Result<StagedTransfer, String> {
        if !self.host_ptys {
            return Err("session transfer needs a session host (--headless --host-ptys)".into());
        }
        if names.is_empty() {
            return Err("no session to transfer".into());
        }
        let active = self.config.history_session.clone();
        for n in names {
            let Some(panes) = self.panes_of_mut(n) else {
                return Err(format!("session '{n}' is not live in this host"));
            };
            if panes.is_empty() {
                return Err(format!("session '{n}' has no windows"));
            }
            if let Some(p) = panes.values().find(|p| !p.can_transfer()) {
                return Err(format!("session '{n}': window {} has no live local PTY (exited, remote or model-only)", p.id()));
            }
        }
        let mut entries: Vec<(String, PaneId, ReleasedPty)> = Vec::new();
        let mut metas: Vec<(String, PaneId, AdoptMeta)> = Vec::new();
        let mut failure = None;
        'outer: for n in names {
            let ids: Vec<PaneId> = self.panes_of_mut(n).map(|p| p.keys().copied().collect()).unwrap_or_default();
            for id in ids {
                let Some(pane) = self.panes_of_mut(n).and_then(|p| p.get_mut(&id)) else { continue };
                match pane.release_for_transfer(RELEASE_PATIENCE) {
                    Ok(r) => {
                        metas.push((n.clone(), id, r.meta()));
                        entries.push((n.clone(), id, r));
                    }
                    Err(e) => {
                        failure = Some(format!("session '{n}' window {id}: {e}"));
                        break 'outer;
                    }
                }
            }
        }
        let mut staged = StagedTransfer { sessions: Vec::new(), entries };
        if let Some(why) = failure {
            self.rollback_transfer(staged);
            return Err(why);
        }
        for n in names {
            let (split_tree, z_stack, zoomed_pane, layout_mode, window_order) =
                self.session_parts(n).ok_or_else(|| format!("session '{n}' vanished"))?;
            let mut panes = Vec::new();
            for (sn, id, meta) in metas.iter().filter(|(sn, ..)| sn == n) {
                let _ = sn;
                if let Some(p) = self.panes_of_mut(n).and_then(|m| m.get_mut(id)) {
                    panes.push(p.export_bundle(*meta));
                }
            }
            staged.sessions.push(SessionBundle {
                name: n.clone(),
                active: *n == active,
                panes,
                split_tree,
                z_stack,
                zoomed_pane,
                layout_mode,
                window_order,
            });
        }
        Ok(staged)
    }

    /// The receiver owns the PTYs now: close our copies and drop the transferred sessions.
    pub fn commit_transfer(&mut self, staged: StagedTransfer) {
        let active = self.config.history_session.clone();
        let names: Vec<String> = staged.sessions.iter().map(|s| s.name.clone()).collect();
        for n in names.iter().filter(|n| **n != active) {
            self.window_manage.background_sessions.remove(n);
            self.daemon_state.session_panes.remove(n);
        }
        if names.contains(&active) {
            if self.end_active_session_and_move_on_pub().is_none() {
                // Nothing else to show: keep the workspace non-empty with a fresh session.
                let _ = self.create_new_session(None, false);
                self.window_manage.background_sessions.remove(&active);
            }
            self.daemon_state.session_panes.remove(&active);
        }
        self.drawer.invalidate_saved_sessions();
        for (_, _, r) in staged.entries {
            r.ack();
        }
    }

    /// The receiver did not take over: reclaim every PTY; sessions keep working untouched.
    pub fn rollback_transfer(&mut self, staged: StagedTransfer) {
        for (n, id, r) in staged.entries {
            let res = match self.panes_of_mut(&n).and_then(|p| p.get_mut(&id)) {
                Some(pane) => pane.reclaim_after_transfer(r),
                None => Err(crate::error::BiltermError::Pty("pane vanished during transfer".into())),
            };
            if let Err(e) = res {
                tracing::error!(session = %n, pane = id, error = %e, "transfer rollback failed to reclaim a PTY");
            }
        }
    }

    /// Cheap checks the receiver runs before it replies `Ready` (nothing is adopted yet).
    pub fn validate_offer(&self, offer: &Offer, nfds: usize, takeover: bool) -> Result<(), String> {
        if !self.host_ptys {
            return Err("receiver is not a session host (--host-ptys)".into());
        }
        if offer.version != XFER_VERSION {
            return Err(format!("offer version {} unsupported", offer.version));
        }
        if offer.sessions.is_empty() || offer.sessions.len() > MAX_SESSIONS || offer.pane_count() == 0 || offer.pane_count() > MAX_PANES {
            return Err("offer session/pane count out of range".into());
        }
        if offer.pane_count() != nfds {
            return Err("offer pane count does not match received fds".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for s in &offer.sessions {
            super::window_manage::validate_session_path(&s.name)?;
            if !seen.insert(s.name.clone()) {
                return Err(format!("duplicate session '{}' in offer", s.name));
            }
            if !takeover && (self.config.history_session == s.name || self.window_manage.background_sessions.contains_key(&s.name)) {
                return Err(format!("session '{}' already exists on the receiver", s.name));
            }
            let mut ids = std::collections::BTreeSet::new();
            for p in &s.panes {
                let ok = p.pid > 1
                    && (1..=1000).contains(&p.pty_rows) && (1..=1000).contains(&p.pty_cols)
                    && (1..=1000).contains(&p.vt_rows) && (1..=1000).contains(&p.vt_cols)
                    && ids.insert(p.id);
                if !ok {
                    return Err(format!("session '{}': invalid window record #{}", s.name, p.id));
                }
            }
            if s.panes.is_empty() {
                return Err(format!("session '{}' has no windows", s.name));
            }
        }
        Ok(())
    }

    /// Adopts every PTY and installs the sessions (`takeover`: this host's own pristine session is
    /// replaced and the sender's active session becomes active). Returns the installed names.
    pub fn install_transfer(&mut self, offer: &Offer, fds: Vec<OwnedFd>, takeover: bool) -> Result<Vec<String>, String> {
        self.validate_offer(offer, fds.len(), takeover)?;
        let mut fds = fds.into_iter();
        let mut built: Vec<(&SessionBundle, BTreeMap<PaneId, CompositePane>)> = Vec::new();
        for s in &offer.sessions {
            let mut panes = BTreeMap::new();
            for pb in &s.panes {
                let fd = fds.next().ok_or("fewer fds than panes")?;
                let meta = AdoptMeta { pid: pb.pid, rows: pb.pty_rows, cols: pb.pty_cols };
                let pty = PtySession::adopt(fd, meta).map_err(|e| format!("adopt window {}: {e}", pb.id))?;
                panes.insert(pb.id, CompositePane::from_bundle(pb, pty));
            }
            built.push((s, panes));
        }
        if takeover {
            let cur = self.config.history_session.clone();
            self.park_active_session();
            self.window_manage.background_sessions.remove(&cur);
        }
        let mut names = Vec::new();
        for (s, panes) in built {
            let state = BackgroundSessionState {
                panes,
                split_tree: s.split_tree.clone(),
                z_stack: s.z_stack.clone(),
                zoomed_pane: s.zoomed_pane,
                selected_windows: Default::default(),
                layout_mode: s.layout_mode,
                pane_chrome: Default::default(),
                parked_seq: super::window_manage::next_park_seq(),
                window_order: s.window_order.clone(),
            };
            self.window_manage.background_sessions.insert(s.name.clone(), state);
            names.push(s.name.clone());
        }
        if takeover {
            let pick = offer.sessions.iter().find(|s| s.active).unwrap_or(&offer.sessions[0]).name.clone();
            if self.activate_parked_session(&pick) {
                self.reflow_default_viewport();
            }
        }
        for bg in self.window_manage.background_sessions.values_mut().filter(|b| !b.panes.is_empty()) {
            for p in bg.panes.values_mut() {
                p.nudge_repaint_after_transfer();
            }
        }
        for p in self.panes.values_mut() {
            p.nudge_repaint_after_transfer();
        }
        self.drawer.invalidate_saved_sessions();
        Ok(names)
    }

    /// Best-effort undo of [`install_transfer`] (the sender went away before it learned of it).
    pub fn uninstall_transfer(&mut self, names: &[String]) {
        for n in names {
            self.window_manage.background_sessions.remove(n);
        }
    }
}
