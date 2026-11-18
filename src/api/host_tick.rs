//! Session-host housekeeping for the headless daemon (P1): a daemon has no UI loop, so with
//! `--host-ptys` it must itself pump PTY output into the VT grids and keep the serializable host
//! state file current. Never runs unless the workspace is a host.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::json::JsonValue;
use super::rpc::AutomationControlPlane;
use crate::action::{Action, DaemonSupervisorOp};
use crate::daemon::host::{remove_host_state, write_host_state, HostState};

/// Pump interval for host PTYs.
pub const HOST_TICK: Duration = Duration::from_millis(25);
/// Minimum spacing between state-file rewrites. A burst of changes (e.g. a program retitling its
/// pane on every prompt) coalesces into at most one write per interval; the newest state always
/// lands on a later tick because the comparison runs every tick.
pub const HOST_PERSIST_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Where and what the host last persisted.
pub(super) struct HostPersist {
    name: String,
    path: PathBuf,
    last: Option<HostState>,
    last_attempt: Option<Instant>,
    min_interval: Duration,
}

impl HostPersist {
    /// Writes `state` if it differs from the last written one and the debounce window has
    /// elapsed. Returns true when a write was attempted.
    fn maybe_write(&mut self, state: HostState, now: Instant) -> bool {
        if self.last.as_ref() == Some(&state) {
            return false;
        }
        if self.last_attempt.is_some_and(|t| now.saturating_duration_since(t) < self.min_interval) {
            return false;
        }
        self.last_attempt = Some(now);
        match write_host_state(&self.path, &state) {
            Ok(()) => self.last = Some(state),
            Err(e) => tracing::warn!(error = %e, "host state write failed"),
        }
        true
    }
}

impl AutomationControlPlane {
    /// Persist the host's state record to `path` whenever it changes (debounced).
    pub fn with_host_state_file(mut self, name: &str, path: PathBuf) -> Self {
        self.host_persist = Some(HostPersist {
            name: name.to_string(),
            path,
            last: None,
            last_attempt: None,
            min_interval: HOST_PERSIST_MIN_INTERVAL,
        });
        self
    }

    /// True when the daemon must run [`Self::host_tick`] periodically.
    pub fn is_host(&self) -> bool {
        self.wm.is_host()
    }

    /// Pumps PTY output, then rewrites the host state file if it changed (debounced). Returns
    /// whether the workspace changed.
    pub fn host_tick(&mut self) -> bool {
        let drained = self.drain_attach_requests();
        let resized = self.attach_tick_pre();
        let changed = self.wm.poll_all_ptys() | drained | resized;
        self.attach_publish();
        if let Some(p) = self.host_persist.as_mut() {
            let state = self.wm.host_state(&p.name);
            p.maybe_write(state, Instant::now());
        }
        changed
    }

    /// Clean shutdown: the host no longer owns anything, so its state file is removed rather than
    /// left behind as a stale record (a pending debounced write is moot once the file is gone).
    pub fn host_shutdown(&mut self) {
        if let Some(p) = self.host_persist.take() {
            remove_host_state(&p.path);
        }
    }

    /// `daemon.host_state`: read-only report of a host's persisted state, lowered through
    /// `Action::Daemon(HostState)`. `name` defaults to this host, else `default`.
    pub(super) fn rpc_daemon_host_state(&mut self, params: &JsonValue) -> Result<JsonValue, String> {
        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .or_else(|| self.host_persist.as_ref().map(|p| p.name.clone()));
        self.apply_with_policy(
            Action::Daemon(DaemonSupervisorOp::HostState { name }),
            super::rpc::SnapshotPolicy::Trusted,
        )?;
        let data = self.wm.take_command_data().ok_or_else(|| "no host state data".to_string())?;
        JsonValue::parse(&data).ok_or_else(|| "invalid host state data".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_tick_persists_state_and_noop_when_not_host() {
        let plane = AutomationControlPlane::new_headless(24, 80);
        assert!(!plane.is_host());

        let dir = std::env::temp_dir().join(format!("bt-hosttick-{}", crate::secure_fs::unique_suffix()));
        crate::secure_fs::create_private_dir_all(&dir).unwrap();
        let path = dir.join("host-t.state");
        let wm = crate::wm::WindowManager::new_host(24, 80).expect("spawn host pane");
        let mut plane = AutomationControlPlane::new(wm, ratatui::layout::Rect::new(0, 0, 80, 24))
            .with_host_state_file("t", path.clone());
        assert!(plane.is_host());
        plane.host_tick();
        let st = crate::daemon::host::read_host_state(&path).unwrap();
        assert_eq!(st.host_name, "t");
        assert_eq!(st.panes.len(), 1);
        assert!(st.panes[0].child_pid.is_some(), "host pane owns a real child");

        // Clean shutdown removes the file.
        plane.host_shutdown();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn st(title: &str) -> HostState {
        use crate::daemon::host::{HostPaneRecord, PtyOwnership, HOST_STATE_VERSION};
        HostState {
            version: HOST_STATE_VERSION,
            host_name: "d".into(),
            host_pid: 1,
            ownership: PtyOwnership::Host,
            active_pane: 1,
            panes: vec![HostPaneRecord { id: 1, child_pid: None, rows: 24, cols: 80, title: title.into() }],
        }
    }

    #[test]
    fn persist_is_debounced_and_skips_unchanged() {
        let dir = std::env::temp_dir().join(format!("bt-hostdb-{}", crate::secure_fs::unique_suffix()));
        crate::secure_fs::create_private_dir_all(&dir).unwrap();
        let path = dir.join("host-d.state");
        let mut p = HostPersist {
            name: "d".into(),
            path: path.clone(),
            last: None,
            last_attempt: None,
            min_interval: Duration::from_millis(500),
        };
        let t0 = Instant::now();
        assert!(p.maybe_write(st("a"), t0), "first write goes through");
        assert!(!p.maybe_write(st("a"), t0 + Duration::from_secs(5)), "unchanged content is never rewritten");
        // A burst of title changes inside the window is coalesced away...
        for i in 0..20 {
            assert!(!p.maybe_write(st(&format!("t{i}")), t0 + Duration::from_millis(10 + i)));
        }
        assert_eq!(crate::daemon::host::read_host_state(&path).unwrap().panes[0].title, "a");
        // ...and the newest state lands once the window has elapsed.
        assert!(p.maybe_write(st("t19"), t0 + Duration::from_millis(600)));
        assert_eq!(crate::daemon::host::read_host_state(&path).unwrap().panes[0].title, "t19");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
