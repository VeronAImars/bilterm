//! Executor for `:daemon transfer` and `:daemon restart --live` (lowered from
//! `Action::Daemon(SessionTransfer | RestartLive)`; unix only, other platforms report so).

use std::path::Path;

use crate::action::DaemonSupervisorOp;
use crate::api::json::JsonValue;
use crate::wm::WindowManager;

fn fail(wm: &mut WindowManager, what: &str, err: &str) {
    let res = JsonValue::obj([("status", JsonValue::String("error".into())), ("error", JsonValue::String(err.to_string()))]);
    wm.set_command_data(res.to_json_string());
    wm.active_pane_mut().set_status_message(Some(format!("{what} failed: {err}")));
}

fn strings(v: &[String]) -> JsonValue {
    JsonValue::Array(v.iter().cloned().map(JsonValue::String).collect())
}

#[cfg(unix)]
pub(super) fn execute_transfer_op(wm: &mut WindowManager, runtime_dir: &Path, op: &DaemonSupervisorOp) {
    use crate::daemon::transfer::{self, SocketKind, TransferOpts};
    match op {
        DaemonSupervisorOp::SessionTransfer { sessions, all, to, takeover, retire } => {
            let res = (|| -> Result<transfer::TransferReport, String> {
                if !wm.is_host() {
                    return Err("this workspace is not a session host (--headless --host-ptys)".into());
                }
                crate::secure_fs::validate_safe_name(to)?;
                if !*takeover && wm.host_name() == Some(to.as_str()) {
                    return Err("cannot transfer sessions to the host they already live on".into());
                }
                let names = if *all {
                    wm.live_session_names()
                } else if sessions.is_empty() {
                    vec![wm.live_session_names()[0].clone()]
                } else {
                    sessions.clone()
                };
                let kind = if *takeover { SocketKind::Takeover } else { SocketKind::Accept };
                let target = transfer::socket_path_in(runtime_dir, to, kind)?;
                transfer::transfer_sessions(wm, &names, &target, *retire, TransferOpts::default())
            })();
            match res {
                Ok(r) => {
                    if *retire {
                        wm.request_retire();
                    }
                    let data = JsonValue::obj([
                        ("status", JsonValue::String("transferred".into())),
                        ("to", JsonValue::String(to.clone())),
                        ("sessions", strings(&r.sessions)),
                        ("panes", JsonValue::Number(r.panes as i64)),
                        ("pids", JsonValue::Array(r.pids.iter().map(|p| JsonValue::Number(i64::from(*p))).collect())),
                    ]);
                    wm.set_command_data(data.to_json_string());
                    wm.active_pane_mut().set_status_message(Some(format!("Transferred {} session(s), {} window(s) to '{to}'", r.sessions.len(), r.panes)));
                }
                Err(e) => fail(wm, "Session transfer", &e),
            }
        }
        DaemonSupervisorOp::RestartLive { name } => {
            let target = name.clone().or_else(|| wm.host_name().map(str::to_string)).unwrap_or_else(|| "default".to_string());
            match crate::daemon::restart_live::restart_live(Some(&mut *wm), runtime_dir, &target) {
                Ok(r) => {
                    let data = JsonValue::obj([
                        ("status", JsonValue::String(if r.new_is_serving { "restarted" } else { "handed_over" }.into())),
                        ("name", JsonValue::String(r.name.clone())),
                        ("old_pid", JsonValue::Number(i64::from(r.old_pid))),
                        ("new_pid", JsonValue::Number(i64::from(r.new_pid))),
                        ("sessions", strings(&r.sessions)),
                        ("panes", JsonValue::Number(r.panes as i64)),
                        ("pids", JsonValue::Array(r.pids.iter().map(|p| JsonValue::Number(i64::from(*p))).collect())),
                    ]);
                    wm.set_command_data(data.to_json_string());
                    wm.active_pane_mut().set_status_message(Some(format!("Daemon '{}' restarted live: PID {} -> {}, {} window(s) kept", r.name, r.old_pid, r.new_pid, r.panes)));
                }
                Err(e) => fail(wm, "Live restart", &e),
            }
        }
        _ => {}
    }
}

#[cfg(not(unix))]
pub(super) fn execute_transfer_op(wm: &mut WindowManager, _runtime_dir: &Path, _op: &DaemonSupervisorOp) {
    fail(wm, "Session transfer", "live session transfer is unix only (ConPTY handles cannot be passed between processes)");
}
