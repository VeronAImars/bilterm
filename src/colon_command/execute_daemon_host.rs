//! `:daemon host [name]` (`daemon.host_state` RPC): read-only report of a session host's persisted
//! state file (P1). Inspection only; nothing is restored or deleted here.

use std::path::Path;

use crate::api::json::JsonValue;
use crate::daemon::host::HostState;
use crate::daemon::host_inspect::{inspect_host_state, HostInspection};
use crate::wm::WindowManager;

fn num(n: impl Into<i64>) -> JsonValue {
    JsonValue::Number(n.into())
}

fn state_json(s: &HostState) -> JsonValue {
    let panes = s
        .panes
        .iter()
        .map(|p| {
            JsonValue::obj([
                ("id", JsonValue::Number(p.id as i64)),
                ("child_pid", p.child_pid.map_or(JsonValue::Null, num)),
                ("rows", num(p.rows)),
                ("cols", num(p.cols)),
                ("title", JsonValue::String(p.title.clone())),
            ])
        })
        .collect();
    JsonValue::obj([
        ("version", num(s.version)),
        ("host_name", JsonValue::String(s.host_name.clone())),
        ("host_pid", num(s.host_pid)),
        ("active_pane", JsonValue::Number(s.active_pane as i64)),
        ("panes", JsonValue::Array(panes)),
    ])
}

/// Builds the report object and a one-line summary for host `name`.
pub fn host_state_report(runtime_dir: &Path, name: &str) -> (JsonValue, String) {
    let s = |v: &str| JsonValue::String(v.to_string());
    match inspect_host_state(runtime_dir, name) {
        Err(e) => (
            JsonValue::obj([("name", s(name)), ("status", s("error")), ("error", JsonValue::String(e.clone()))]),
            format!("Host state '{name}': {e}"),
        ),
        Ok(HostInspection::Absent { path }) => (
            JsonValue::obj([
                ("name", s(name)),
                ("status", s("absent")),
                ("path", JsonValue::String(path.to_string_lossy().into_owned())),
            ]),
            format!("Host state '{name}': no state file"),
        ),
        Ok(HostInspection::Invalid { path, error }) => (
            JsonValue::obj([
                ("name", s(name)),
                ("status", s("invalid")),
                ("path", JsonValue::String(path.to_string_lossy().into_owned())),
                ("error", JsonValue::String(error.clone())),
            ]),
            format!("Host state '{name}' invalid: {error}"),
        ),
        Ok(HostInspection::Loaded { path, state, stale }) => {
            let status = if stale.is_some() { "stale" } else { "live" };
            let summary = match &stale {
                Some(r) => format!("Host state '{name}': stale ({r}), {} pane(s)", state.panes.len()),
                None => format!("Host state '{name}': live, pid {}, {} pane(s)", state.host_pid, state.panes.len()),
            };
            (
                JsonValue::obj([
                    ("name", s(name)),
                    ("status", s(status)),
                    ("path", JsonValue::String(path.to_string_lossy().into_owned())),
                    ("stale_reason", stale.map_or(JsonValue::Null, JsonValue::String)),
                    ("state", state_json(&state)),
                ]),
                summary,
            )
        }
    }
}

/// Executes `Action::Daemon(HostState)`: stores the JSON as command data and a status line.
pub(super) fn execute_host_state(wm: &mut WindowManager, runtime_dir: &Path, name: Option<&str>) {
    let (json, summary) = host_state_report(runtime_dir, name.unwrap_or("default"));
    wm.set_command_data(json.to_json_string());
    wm.active_pane_mut().set_status_message(Some(summary));
}
