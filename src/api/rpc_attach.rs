//! `attach.clients` RPC: read-only list of attach clients, lowered through
//! `Action::Daemon(DaemonSupervisorOp::AttachClients)` like `daemon.host_state`.

use super::json::JsonValue;
use super::rpc::AutomationControlPlane;
use crate::action::{Action, DaemonSupervisorOp};

impl AutomationControlPlane {
    pub(super) fn rpc_attach_clients(&mut self) -> Result<JsonValue, String> {
        self.apply_with_policy(Action::Daemon(DaemonSupervisorOp::AttachClients), super::rpc::SnapshotPolicy::Trusted)?;
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

    /// Installs the streaming hub shared with attach connections (`attach::host::start_attach`).
    pub fn set_attach_hub(&mut self, hub: std::sync::Arc<crate::attach::hub::AttachHub>) {
        self.attach_hub = Some(hub);
    }

    fn attach_reply(&self, client: u64, code: crate::attach::msg::ErrorCode, message: String) {
        if let Some(h) = &self.attach_hub {
            h.reply(client, code, message);
        }
    }

    /// Applies queued attach-client input/`Act` through `WindowManager::apply`. Returns true if any ran.
    /// Pane-level refusals are answered to the originating client as `Error` messages.
    pub fn drain_attach_requests(&mut self) -> bool {
        use crate::attach::input::HostRequest;
        use crate::attach::msg::ErrorCode;
        let mut any = false;
        for _ in 0..MAX_REQUESTS_PER_TICK {
            let Some(req) = self.attach_requests.as_mut().and_then(|rx| rx.try_recv().ok()) else {
                break;
            };
            any = true;
            let (client, action) = match req {
                HostRequest::PaneInput { client, pane, bytes } => {
                    if let Err(why) = self.wm.pane_input_allowed(pane) {
                        let code = if why.contains("read-only") { ErrorCode::ReadOnly } else { ErrorCode::BadRequest };
                        self.attach_reply(client, code, why);
                        continue;
                    }
                    (client, Action::PaneInputBytes { pane, bytes })
                }
                HostRequest::Act { client, action } => {
                    if let Some(why) = self.wm.config_write_denied(&action) {
                        tracing::debug!(client, why, "attach act refused");
                        self.attach_reply(client, ErrorCode::ReadOnly, format!("refused: {why}"));
                        continue;
                    }
                    (client, action)
                }
            };
            // A host never quits because a client asked; errors are surfaced in the status line.
            if let Err(e) = self.apply_with_policy(action, super::rpc::SnapshotPolicy::Confined) {
                tracing::debug!(error = %e, "attach request failed");
                self.attach_reply(client, ErrorCode::BadRequest, e);
            }
        }
        any
    }

    /// Applies the size owner's (debounced) resize via `Action::AttachResize`. Returns true if one ran.
    pub(super) fn attach_tick_pre(&mut self) -> bool {
        let Some(hub) = self.attach_hub.clone() else {
            return false;
        };
        let Some((cols, rows)) = hub.take_resize() else {
            return false;
        };
        if let Err(e) = self.apply_with_policy(Action::AttachResize { cols, rows }, super::rpc::SnapshotPolicy::Confined) {
            tracing::debug!(error = %e, "attach resize failed");
        }
        true
    }

    /// Publishes the frames every attached client diffs against; free when nobody is attached.
    pub(super) fn attach_publish(&mut self) {
        if let Some(hub) = self.attach_hub.as_ref().filter(|h| h.has_clients()) {
            hub.publish(self.wm.host_view().frames());
        }
    }
}
