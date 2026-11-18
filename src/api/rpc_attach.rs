//! `attach.clients` RPC: read-only list of attach clients, lowered through
//! `Action::Daemon(DaemonSupervisorOp::AttachClients)` like `daemon.host_state`.

use super::json::JsonValue;
use super::rpc::AutomationControlPlane;
use crate::action::{Action, DaemonSupervisorOp};

impl AutomationControlPlane {
    pub(super) fn rpc_attach_clients(&mut self) -> Result<JsonValue, String> {
        self.apply_with_policy(
            Action::Daemon(DaemonSupervisorOp::AttachClients),
            super::rpc::SnapshotPolicy::Trusted,
        )?;
        let data = self.wm.take_command_data().ok_or_else(|| "no attach data".to_string())?;
        JsonValue::parse(&data).ok_or_else(|| "invalid attach data".to_string())
    }
}

/// Most requests applied per tick, so a flood cannot starve PTY pumping (the queue is bounded too).
const MAX_REQUESTS_PER_TICK: usize = crate::attach::limits::IN_QUEUE_FRAMES;

impl AutomationControlPlane {
    /// Installs the receiving end of the attach request queue (`attach::host::start_attach`).
    pub fn set_attach_requests(&mut self, rx: tokio::sync::mpsc::Receiver<crate::attach::input::HostRequest>) {
        self.attach_requests = Some(rx);
    }

    /// Applies queued attach-client input/`Act` through `WindowManager::apply`. Returns true if any ran.
    pub fn drain_attach_requests(&mut self) -> bool {
        use crate::attach::input::HostRequest;
        let mut any = false;
        for _ in 0..MAX_REQUESTS_PER_TICK {
            let Some(req) = self.attach_requests.as_mut().and_then(|rx| rx.try_recv().ok()) else { break };
            any = true;
            let action = match req {
                HostRequest::PaneInput { pane, bytes, .. } => Action::PaneInputBytes { pane, bytes },
                HostRequest::Act { client, action } => {
                    if let Some(why) = self.wm.config_write_denied(&action) {
                        tracing::debug!(client, why, "attach act refused");
                        continue;
                    }
                    action
                }
            };
            // A host never quits because a client asked; errors are surfaced in the status line.
            if let Err(e) = self.apply_with_policy(action, super::rpc::SnapshotPolicy::Confined) {
                tracing::debug!(error = %e, "attach request failed");
            }
        }
        any
    }
}
