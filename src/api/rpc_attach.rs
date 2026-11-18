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
