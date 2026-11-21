//! Executor for `:agent` colon commands (`AGT-01..03`, Phase 52).

use crate::wm::WindowManager;
use crate::wm::agent_guide::AgentGuideOp;
use crate::wm::agent_mail::AgentMailOp;

/// Executes one `:agent` operation; always returns `false` (never quits).
pub(super) fn execute_agent_mail_command(wm: &mut WindowManager, op: AgentMailOp) -> bool {
    match wm.agent_mail_apply(&op) {
        Ok(msg) => {
            wm.active_pane_mut().set_status_message(Some(msg));
        }
        Err(err) => {
            wm.record_automation_error(err.clone());
            wm.active_pane_mut()
                .set_status_message(Some(format!("Agent mail error: {err}")));
        }
    }
    false
}

/// Executes `:agent guide [PANE|all|--print]`; always returns `false` (never quits).
pub(super) fn execute_agent_guide(wm: &mut WindowManager, op: AgentGuideOp) -> bool {
    match wm.agent_guide_apply(&op) {
        Ok(msg) => wm.active_pane_mut().set_status_message(Some(msg)),
        Err(err) => {
            wm.record_automation_error(err.clone());
            wm.active_pane_mut().set_status_message(Some(format!("Agent guide error: {err}")));
        }
    }
    false
}

/// Executes `:mail status|ack`; always returns `false` (never quits).
pub(super) fn execute_pane_mail_command(wm: &mut WindowManager, op: crate::wm::pane_mail::PaneMailOp) -> bool {
    match wm.pane_mail_apply(&op) {
        Ok(msg) => wm.active_pane_mut().set_status_message(Some(msg)),
        Err(err) => {
            wm.record_automation_error(err.clone());
            wm.active_pane_mut().set_status_message(Some(format!("Pane mail error: {err}")));
        }
    }
    false
}
