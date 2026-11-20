//! Executor for `:daemon` colon commands (`DAEM-05..08`, Phase 55).

use crate::action::DaemonSupervisorOp;
use crate::api::json::JsonValue;
use crate::daemon::list_view::{collect_rows, rows_json, summary};
use crate::daemon::registry::DaemonRegistry;
use crate::daemon::supervise::last_exit_to_json;
use crate::daemon::supervisor::{
    daemon_lock_path_in, ensure_started_opts, resolve_runtime_dir, stop_daemon, DaemonLockRecord, DaemonStartResult,
};
use crate::wm::resilience::DaemonJob;
use crate::wm::WindowManager;

/// Executes a daemon supervisor operation against the local runtime directory.
pub fn execute_daemon_command(wm: &mut WindowManager, op: &DaemonSupervisorOp) -> bool {
    let runtime_dir = match resolve_runtime_dir() {
        Ok(dir) => dir,
        Err(e) => {
            wm.active_pane_mut()
                .set_status_message(Some(format!("Daemon error: {e}")));
            return false;
        }
    };

    // With a UI attached, start/stop wait on a worker; the result arrives as a status message.
    if !wm.is_headless() {
        let job = match op {
            DaemonSupervisorOp::Start { name, restart, limits, host_ptys } => Some(DaemonJob::Start {
                name: name.clone().unwrap_or_else(|| "default".to_string()),
                restart: restart.unwrap_or_default(),
                limits: limits.unwrap_or_default(),
                host_ptys: *host_ptys,
            }),
            DaemonSupervisorOp::New { name, restart, limits, host_ptys } => {
                // The name is chosen (and validated) here so the caller learns it immediately.
                match crate::daemon::create::resolve_new_name(&runtime_dir, name.as_deref()) {
                    Ok(chosen) => {
                        wm.set_command_data(new_data(&chosen, "starting", None));
                        Some(DaemonJob::New { name: chosen, restart: restart.unwrap_or_default(), limits: limits.unwrap_or_default(), host_ptys: *host_ptys })
                    }
                    Err(e) => return new_refused(wm, &runtime_dir, name.as_deref(), &e),
                }
            }
            DaemonSupervisorOp::Stop { name, force } => Some(DaemonJob::Stop {
                name: name.clone().unwrap_or_else(|| "default".to_string()),
                force: *force,
            }),
            _ => None,
        };
        if let Some(job) = job {
            let verb = if matches!(job, DaemonJob::Start { .. } | DaemonJob::New { .. }) { "starting" } else { "stopping" };
            let (DaemonJob::Start { name, .. } | DaemonJob::New { name, .. } | DaemonJob::Stop { name, .. }) = &job;
            wm.active_pane_mut().set_status_message(Some(format!("Daemon '{name}': {verb}…")));
            wm.spawn_daemon_job(&runtime_dir, job);
            wm.open_daemon_window_from(&runtime_dir);
            return false;
        }
    }

    match op {
        DaemonSupervisorOp::List => {
            // Same rows as `bilterm daemon list`, `daemon.list` and the daemon window.
            let rows = collect_rows(&runtime_dir, false);
            wm.set_command_data(rows_json(&rows).to_json_string());
            let summary = summary(&rows);
            wm.active_pane_mut().set_status_message(Some(summary));
            wm.open_daemon_window_from(&runtime_dir);
        }
        DaemonSupervisorOp::Status { name } => {
            let target = name.as_deref().unwrap_or("default");
            let active = DaemonRegistry::list_active(&runtime_dir);
            let found = active.into_iter().find(|d| d.name == target);

            let lock_path = daemon_lock_path_in(&runtime_dir, target).ok();
            let lock_rec = lock_path
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| DaemonLockRecord::from_json(&s).ok());

            if let Some(d) = found {
                let backend = crate::daemon::list_view::row_for(&runtime_dir, &d.name).limit_backend.map_or(JsonValue::Null, |b| JsonValue::String(b.to_string()));
                let res = JsonValue::obj([
                    ("name", JsonValue::String(d.name.clone())),
                    ("pid", JsonValue::Number(d.pid as i64)),
                    ("version", JsonValue::String(d.version.clone())),
                    (
                        "socket_path",
                        JsonValue::String(d.socket_path.to_string_lossy().into_owned()),
                    ),
                    (
                        "sessions_count",
                        JsonValue::Number(d.sessions_count as i64),
                    ),
                    ("panes_count", JsonValue::Number(d.panes_count as i64)),
                    ("agents_count", JsonValue::Number(d.agents_count as i64)),
                    ("status", JsonValue::String("running".to_string())),
                    ("rss_bytes", d.rss_bytes.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))),
                    ("restart_count", JsonValue::Number(i64::from(d.restart_count))),
                    ("last_exit", d.last_exit.as_ref().map_or(JsonValue::Null, last_exit_to_json)),
                    ("limit_max_bytes", d.limit_max_bytes.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))),
                    ("limit_high_bytes", d.limit_high_bytes.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))),
            ("limit_backend", backend),
                ]);
                wm.set_command_data(res.to_json_string());
                wm.active_pane_mut().set_status_message(Some(format!(
                    "Daemon '{}': running (PID {}, {} sessions, {} panes)",
                    d.name, d.pid, d.sessions_count, d.panes_count
                )));
            } else if let Some(rec) = lock_rec {
                let res = JsonValue::obj([
                    ("name", JsonValue::String(rec.name.clone())),
                    ("pid", JsonValue::Number(rec.pid as i64)),
                    ("status", JsonValue::String("crashed".to_string())),
                ]);
                wm.set_command_data(res.to_json_string());
                wm.active_pane_mut().set_status_message(Some(format!(
                    "Daemon '{}': crashed (PID {} exited without cleanup) — ':daemon start {}' restarts it",
                    rec.name, rec.pid, rec.name
                )));
            } else {
                let res = JsonValue::obj([
                    ("name", JsonValue::String(target.to_string())),
                    ("status", JsonValue::String("stopped".to_string())),
                ]);
                wm.set_command_data(res.to_json_string());
                wm.active_pane_mut()
                    .set_status_message(Some(format!("Daemon '{target}': not running")));
            }
            wm.open_daemon_window_from(&runtime_dir);
        }
        DaemonSupervisorOp::New { name, restart, limits, host_ptys } => {
            match crate::daemon::create::create_daemon(&runtime_dir, name.as_deref(), restart.unwrap_or_default(), limits.unwrap_or_default(), *host_ptys) {
                Ok((chosen, DaemonStartResult::Started { pid, socket_path })) => {
                    wm.set_command_data(new_data(&chosen, "started", Some((pid, &socket_path))));
                    wm.active_pane_mut().set_status_message(Some(format!("Daemon '{chosen}' created (PID {pid})")));
                }
                Ok((chosen, DaemonStartResult::AlreadyRunning { pid, .. })) => {
                    // create_daemon maps this to an error; kept exhaustive and non-destructive.
                    return new_refused(wm, &runtime_dir, Some(&chosen), &format!("daemon '{chosen}' already exists (running, PID {pid})"));
                }
                Err(e) => return new_refused(wm, &runtime_dir, name.as_deref(), &e),
            }
        }
        DaemonSupervisorOp::Start { name, restart, limits, host_ptys } => {
            let target = name.as_deref().unwrap_or("default");
            match ensure_started_opts(&runtime_dir, target, restart.unwrap_or_default(), limits.unwrap_or_default(), *host_ptys) {
                Ok(DaemonStartResult::Started { pid, socket_path }) => {
                    let res = JsonValue::obj([
                        ("name", JsonValue::String(target.to_string())),
                        ("status", JsonValue::String("started".to_string())),
                        ("pid", JsonValue::Number(pid as i64)),
                        (
                            "socket_path",
                            JsonValue::String(socket_path.to_string_lossy().into_owned()),
                        ),
                    ]);
                    wm.set_command_data(res.to_json_string());
                    wm.active_pane_mut().set_status_message(Some(format!(
                        "Daemon '{target}' started (PID {pid})"
                    )));
                }
                Ok(DaemonStartResult::AlreadyRunning { pid, socket_path }) => {
                    let res = JsonValue::obj([
                        ("name", JsonValue::String(target.to_string())),
                        ("status", JsonValue::String("already_running".to_string())),
                        ("pid", JsonValue::Number(pid as i64)),
                        (
                            "socket_path",
                            JsonValue::String(socket_path.to_string_lossy().into_owned()),
                        ),
                    ]);
                    wm.set_command_data(res.to_json_string());
                    wm.active_pane_mut().set_status_message(Some(format!(
                        "Daemon '{target}' is already running (PID {pid})"
                    )));
                }
                Err(e) => {
                    let res = JsonValue::obj([
                        ("name", JsonValue::String(target.to_string())),
                        ("status", JsonValue::String("error".to_string())),
                        ("error", JsonValue::String(e.to_string())),
                    ]);
                    wm.set_command_data(res.to_json_string());
                    wm.active_pane_mut().set_status_message(Some(format!(
                        "Failed to start daemon '{target}': {e}"
                    )));
                }
            }
        }
        DaemonSupervisorOp::Stop { name, force } => {
            let target = name.as_deref().unwrap_or("default");
            match stop_daemon(&runtime_dir, target, *force) {
                Ok(stopped) => {
                    let status = if stopped { "stopped" } else { "not_running" };
                    let res = JsonValue::obj([
                        ("name", JsonValue::String(target.to_string())),
                        ("status", JsonValue::String(status.to_string())),
                    ]);
                    wm.set_command_data(res.to_json_string());
                    let msg = if stopped {
                        format!("Daemon '{target}' stopped")
                    } else {
                        format!("Daemon '{target}' was not running")
                    };
                    wm.active_pane_mut().set_status_message(Some(msg));
                }
                Err(e) => {
                    let res = JsonValue::obj([
                        ("name", JsonValue::String(target.to_string())),
                        ("status", JsonValue::String("error".to_string())),
                        ("error", JsonValue::String(e.to_string())),
                    ]);
                    wm.set_command_data(res.to_json_string());
                    wm.active_pane_mut().set_status_message(Some(format!(
                        "Failed to stop daemon '{target}': {e}"
                    )));
                }
            }
        }
        DaemonSupervisorOp::Switch { name } => {
            let res = JsonValue::obj([
                ("name", JsonValue::String(name.clone())),
                ("status", JsonValue::String("switched".to_string())),
            ]);
            wm.set_command_data(res.to_json_string());
            wm.active_pane_mut().set_status_message(Some(format!(
                "Active daemon context switched to '{name}'"
            )));
        }
        DaemonSupervisorOp::Prune => {
            let pruned = DaemonRegistry::prune_dead(&runtime_dir);
            let res = JsonValue::obj([
                ("status", JsonValue::String("pruned".to_string())),
                ("pruned_count", JsonValue::Number(pruned as i64)),
            ]);
            wm.set_command_data(res.to_json_string());
            wm.active_pane_mut().set_status_message(Some(format!(
                "Pruned {pruned} dead daemon record(s)"
            )));
        }
        DaemonSupervisorOp::HostState { name } => {
            super::execute_daemon_host::execute_host_state(wm, &runtime_dir, name.as_deref());
        }
        DaemonSupervisorOp::AttachClients => crate::attach::view::execute_attach_clients(wm),
        DaemonSupervisorOp::AttachKick { client_id, all_ro } => crate::attach::view::execute_attach_kick(wm, *client_id, *all_ro),
    }
    match op {
        // Start/Stop/Switch surface the live supervisor state in the floating window.
        DaemonSupervisorOp::Start { .. }
        | DaemonSupervisorOp::New { .. }
        | DaemonSupervisorOp::Stop { .. }
        | DaemonSupervisorOp::Switch { .. } => wm.open_daemon_window_from(&runtime_dir),
        DaemonSupervisorOp::Prune if wm.daemon_window().active => {
            wm.refresh_daemon_window_from(&runtime_dir);
        }
        _ => {}
    }
    false
}

/// JSON for `New` results: always carries the chosen `name`.
fn new_data(name: &str, status: &str, started: Option<(u32, &std::path::Path)>) -> String {
    let mut fields = vec![
        ("name", JsonValue::String(name.to_string())),
        ("status", JsonValue::String(status.to_string())),
    ];
    if let Some((pid, sock)) = started {
        fields.push(("pid", JsonValue::Number(i64::from(pid))));
        fields.push(("socket_path", JsonValue::String(sock.to_string_lossy().into_owned())));
    }
    JsonValue::obj(fields).to_json_string()
}

/// Reports a refused/failed create (`status:"error"`) and surfaces the window.
fn new_refused(wm: &mut WindowManager, runtime_dir: &std::path::Path, name: Option<&str>, err: &str) -> bool {
    let res = JsonValue::obj([
        ("name", name.map_or(JsonValue::Null, |n| JsonValue::String(n.to_string()))),
        ("status", JsonValue::String("error".to_string())),
        ("error", JsonValue::String(err.to_string())),
    ]);
    wm.set_command_data(res.to_json_string());
    wm.active_pane_mut().set_status_message(Some(format!("Daemon not created: {err}")));
    wm.open_daemon_window_from(runtime_dir);
    false
}
