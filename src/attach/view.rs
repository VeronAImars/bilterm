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
