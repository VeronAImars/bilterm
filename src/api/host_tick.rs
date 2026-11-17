//! Session-host housekeeping for the headless daemon (P1): a daemon has no UI loop, so with
//! `--host-ptys` it must itself pump PTY output into the VT grids and keep the serializable host
//! state file current. Never runs unless the workspace is a host.

use std::path::PathBuf;
use std::time::Duration;

use super::rpc::AutomationControlPlane;
use crate::daemon::host::{write_host_state, HostState};

/// Pump interval for host PTYs.
pub const HOST_TICK: Duration = Duration::from_millis(25);

/// Where and what the host last persisted.
pub(super) struct HostPersist {
    name: String,
    path: PathBuf,
    last: Option<HostState>,
}

impl AutomationControlPlane {
    /// Persist the host's state record to `path` whenever it changes.
    pub fn with_host_state_file(mut self, name: &str, path: PathBuf) -> Self {
        self.host_persist = Some(HostPersist { name: name.to_string(), path, last: None });
        self
    }

    /// True when the daemon must run [`Self::host_tick`] periodically.
    pub fn is_host(&self) -> bool {
        self.wm.is_host()
    }

    /// Pumps PTY output, then rewrites the host state file if it changed. Returns whether the
    /// workspace changed.
    pub fn host_tick(&mut self) -> bool {
        let changed = self.wm.poll_all_ptys();
        if let Some(p) = self.host_persist.as_mut() {
            let state = self.wm.host_state(&p.name);
            if p.last.as_ref() != Some(&state) {
                if let Err(e) = write_host_state(&p.path, &state) {
                    tracing::warn!(error = %e, "host state write failed");
                } else {
                    p.last = Some(state);
                }
            }
        }
        changed
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
