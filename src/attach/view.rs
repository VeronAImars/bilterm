//! `attach.clients` read model, lowered through `Action::Daemon(DaemonSupervisorOp::AttachClients)`.

use crate::wm::WindowManager;

/// Executes `:daemon attach` / the `attach.clients` RPC: stores the JSON as command data and a
/// one-line status.
pub fn execute_attach_clients(wm: &mut WindowManager) {
    let (json, summary) = match wm.attach_registry() {
        Some(reg) => {
            let j = reg.to_json();
            let s = format!("Attach clients: {}/{}", j["count"], j["max"]);
            (j, s)
        }
        None => (super::registry::disabled_json(), "Attach listener not enabled (--attach)".to_string()),
    };
    wm.set_command_data(json.to_string());
    wm.active_pane_mut().set_status_message(Some(summary));
}

/// Executes `:daemon attach kick ...` / the `attach.kick` RPC. Same trust as the other daemon
/// mutations (local control socket, peer-uid checked), and refused outright when the action
/// originates from an attach client's `Act`, so a client can never kick another.
pub fn execute_attach_kick(wm: &mut WindowManager, client_id: Option<u64>, all_ro: bool) {
    let fail = |wm: &mut WindowManager, msg: String| {
        wm.active_pane_mut().set_status_message(Some(msg.clone()));
        wm.record_automation_error_msg(msg);
    };
    if wm.is_attach_origin() {
        tracing::warn!("attach.kick refused: requested by an attach client");
        return fail(wm, "attach clients cannot kick other clients".to_string());
    }
    if client_id.is_some() == all_ro {
        return fail(wm, "attach.kick needs exactly one of client_id or ro".to_string());
    }
    let Some(reg) = wm.attach_registry().cloned() else {
        return fail(wm, "Attach listener not enabled (--attach)".to_string());
    };
    let kicked: Vec<u64> = match client_id {
        Some(id) => match reg.lock().kick(id) {
            Ok(()) => vec![id],
            Err(_) => return fail(wm, format!("unknown attach client {id}")),
        },
        None => reg.lock().kick_all_ro(),
    };
    tracing::warn!(?kicked, origin = if wm.is_api_origin() { "api" } else { "local" }, "attach.kick");
    let ids = kicked.iter().map(|i| crate::api::json::JsonValue::Number(*i as i64)).collect();
    let res = crate::api::json::JsonValue::obj([
        ("kicked", crate::api::json::JsonValue::Array(ids)),
        ("count", crate::api::json::JsonValue::Number(kicked.len() as i64)),
    ]);
    wm.set_command_data(res.to_json_string());
    wm.active_pane_mut().set_status_message(Some(format!("Kicked {} attach client(s)", kicked.len())));
}
