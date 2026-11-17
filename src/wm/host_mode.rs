//! Session-host mode (P1, `docs/plans/session-host-architecture.md`): a headless daemon whose
//! panes own real PTYs. Off by default; every other mode is unchanged.

use super::{PaneId, WindowManager};
use crate::daemon::host::{HostPaneRecord, HostState, PtyOwnership, HOST_STATE_VERSION};
use crate::error::Result;
use crate::pane::CompositePane;
use crate::pty::PtyConfig;

impl WindowManager {
    /// Headless workspace (no UI, daemon key profile) whose panes spawn real shells.
    pub fn new_host(rows: u16, cols: u16) -> Result<Self> {
        let pane = CompositePane::spawn(1, PtyConfig { rows, cols, ..Default::default() })?;
        let mut wm = Self::from_initial_pane(pane, true);
        wm.host_ptys = true;
        Ok(wm)
    }

    /// True when this workspace is a PTY-owning session host.
    pub fn is_host(&self) -> bool {
        self.host_ptys
    }

    /// Pane-creation sites use this instead of `headless_mode`: model-only unless hosting PTYs.
    pub(super) fn model_only_panes(&self) -> bool {
        self.headless_mode && !self.host_ptys
    }

    /// Serializable description of everything this host owns (no fds; see `daemon::host`).
    pub fn host_state(&self, host_name: &str) -> HostState {
        let panes = self
            .panes
            .iter()
            .map(|(&id, p): (&PaneId, &CompositePane)| {
                let (rows, cols) = p.vt().size();
                HostPaneRecord { id, child_pid: p.child_pid(), rows, cols, title: p.title().to_string() }
            })
            .collect();
        HostState {
            version: HOST_STATE_VERSION,
            host_name: host_name.to_string(),
            host_pid: std::process::id(),
            ownership: PtyOwnership::Host,
            active_pane: self.active_pane_id(),
            panes,
        }
    }
}
