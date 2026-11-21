//! `pane.mail_state` / `pane.mail_ack`: pane-mail envelope state (lowered to `Action::PaneMail`).

use super::json::JsonValue;
use super::rpc::SnapshotPolicy;
use super::rpc::AutomationControlPlane;
use crate::action::Action;
use crate::wm::pane_mail::PaneMailOp;

impl AutomationControlPlane {
    /// `pane.mail_state {selector|pane_id}`: one pane, or every pane with mail when neither is given.
    pub(super) fn rpc_pane_mail_state(&mut self, params: &JsonValue) -> Result<JsonValue, String> {
        let pane = self.optional_mail_target(params)?;
        self.apply_mail(PaneMailOp::Status { pane })
    }

    /// `pane.mail_ack {selector|pane_id|all:true}`: mark read (the active pane by default).
    pub(super) fn rpc_pane_mail_ack(&mut self, params: &JsonValue) -> Result<JsonValue, String> {
        let all = params.get("all").and_then(JsonValue::as_bool).unwrap_or(false);
        let pane = if all { None } else { Some(self.target_pane(params)?) };
        self.apply_mail(PaneMailOp::Ack { pane, all })
    }

    fn optional_mail_target(&mut self, params: &JsonValue) -> Result<Option<usize>, String> {
        if params.get("selector").is_none() && params.get("pane_id").is_none() {
            return Ok(None);
        }
        self.target_pane(params).map(Some)
    }

    fn apply_mail(&mut self, op: PaneMailOp) -> Result<JsonValue, String> {
        self.apply_with_policy(Action::PaneMail(op), SnapshotPolicy::Trusted)?;
        let data = self.wm.take_command_data().ok_or_else(|| "no pane mail data".to_string())?;
        JsonValue::parse(&data).ok_or_else(|| "invalid pane mail data".to_string())
    }
}
