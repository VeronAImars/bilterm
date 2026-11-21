//! `remote.status`: per-pane origin (lowered to `Action::RemoteStatus`).
use super::json::JsonValue;
use super::rpc::AutomationControlPlane;
use super::rpc::SnapshotPolicy;
use crate::action::Action;

impl AutomationControlPlane {
    /// `remote.status {selector|pane_id}`: one pane, or every pane when neither is given.
    pub(super) fn rpc_remote_status(&mut self, params: &JsonValue) -> Result<JsonValue, String> {
        let pane = if params.get("selector").is_none() && params.get("pane_id").is_none() { None } else { Some(self.target_pane(params)?) };
        self.apply_with_policy(Action::RemoteStatus { pane }, SnapshotPolicy::Trusted)?;
        let data = self.wm.take_command_data().ok_or_else(|| "no remote status data".to_string())?;
        JsonValue::parse(&data).ok_or_else(|| "invalid remote status data".to_string())
    }
}
