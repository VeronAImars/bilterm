//! JSON-RPC 2.0 Handlers for `daemon.*` Methods (`DAEM-05..08`, Phase 55).

use crate::api::json::JsonValue;
use crate::daemon::list_view::{collect_rows, rows_json};
use crate::daemon::registry::DaemonRegistry;
use crate::daemon::limits::MemoryLimits;
use crate::daemon::restart_policy::RestartMode;
use crate::daemon::supervise::last_exit_to_json;
use crate::daemon::supervisor::{
    daemon_lock_path_in, ensure_started_limited, resolve_runtime_dir, stop_daemon, DaemonLockRecord, DaemonStartResult,
};

/// Handles `daemon.list`. Rows come from the same view model as the CLI, `:daemon list` and the
/// daemon window (RSS, restarts, last exit, limits, `restarting`/`failed` hosts).
pub fn handle_daemon_list() -> Result<JsonValue, String> {
    let runtime_dir = resolve_runtime_dir().map_err(|e| format!("runtime directory error: {e}"))?;
    Ok(rows_json(&collect_rows(&runtime_dir, false)))
}

/// Handles `daemon.status`.
pub fn handle_daemon_status(params: &JsonValue) -> Result<JsonValue, String> {
    let runtime_dir = resolve_runtime_dir().map_err(|e| format!("runtime directory error: {e}"))?;
    let active = DaemonRegistry::list_active(&runtime_dir);
    // No `name`: report the daemon answering this request (else `default`), so a client that
    // targeted a named daemon sees that daemon.
    let own = active.iter().find(|d| d.is_current()).map(|d| d.name.clone());
    let name = params
        .get("name")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .or(own)
        .unwrap_or_else(|| "default".to_string());
    let name = name.as_str();

    if let Some(d) = active.into_iter().find(|item| item.name == name) {
        let is_cur = d.is_current();
        let backend = crate::daemon::list_view::row_for(&runtime_dir, &d.name).limit_backend.map_or(JsonValue::Null, |b| JsonValue::String(b.to_string()));
        return Ok(JsonValue::obj([
            ("name", JsonValue::String(d.name)),
            ("pid", JsonValue::Number(d.pid as i64)),
            ("version", JsonValue::String(d.version)),
            ("started_at_ms", JsonValue::Number(d.started_at_ms as i64)),
            (
                "socket_path",
                JsonValue::String(d.socket_path.to_string_lossy().into_owned()),
            ),
            ("sessions_count", JsonValue::Number(d.sessions_count as i64)),
            ("panes_count", JsonValue::Number(d.panes_count as i64)),
            ("agents_count", JsonValue::Number(d.agents_count as i64)),
            ("status", JsonValue::String("running".to_string())),
            ("is_current", JsonValue::Bool(is_cur)),
            ("rss_bytes", d.rss_bytes.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))),
            ("restart_count", JsonValue::Number(i64::from(d.restart_count))),
            ("last_exit", d.last_exit.as_ref().map_or(JsonValue::Null, last_exit_to_json)),
            ("limit_max_bytes", d.limit_max_bytes.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))),
            ("limit_high_bytes", d.limit_high_bytes.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))),
            ("limit_backend", backend),
            ("host_ptys", d.host_ptys.map_or(JsonValue::Null, JsonValue::Bool)),
        ]));
    }

    let lock_path = daemon_lock_path_in(&runtime_dir, name).ok();
    let lock_rec = lock_path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| DaemonLockRecord::from_json(&s).ok());

    if let Some(rec) = lock_rec {
        Ok(JsonValue::obj([
            ("name", JsonValue::String(rec.name)),
            ("pid", JsonValue::Number(rec.pid as i64)),
            ("version", JsonValue::String(rec.version)),
            (
                "socket_path",
                JsonValue::String(rec.socket_path.to_string_lossy().into_owned()),
            ),
            ("status", JsonValue::String("crashed".to_string())),
        ]))
    } else {
        Ok(JsonValue::obj([
            ("name", JsonValue::String(name.to_string())),
            ("status", JsonValue::String("stopped".to_string())),
        ]))
    }
}

/// Handles `daemon.start`.
pub fn handle_daemon_start(params: &JsonValue) -> Result<JsonValue, String> {
    let runtime_dir = resolve_runtime_dir().map_err(|e| format!("runtime directory error: {e}"))?;
    let name = params
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or("default");

    let restart = match params.get("restart").and_then(JsonValue::as_str) {
        Some(v) => RestartMode::parse(v)?,
        None => RestartMode::Never,
    };

    // memory_max / memory_high: "2G" strings or plain byte counts (high requires max).
    let size = |k: &str| -> Option<String> {
        match params.get(k) {
            Some(JsonValue::String(s)) => Some(s.clone()),
            Some(JsonValue::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    };
    let limits = MemoryLimits::from_args(size("memory_max").as_deref(), size("memory_high").as_deref())?;

    match ensure_started_limited(&runtime_dir, name, restart, limits) {
        Ok(DaemonStartResult::Started { pid, socket_path }) => Ok(JsonValue::obj([
            ("status", JsonValue::String("started".to_string())),
            ("name", JsonValue::String(name.to_string())),
            ("pid", JsonValue::Number(pid as i64)),
            (
                "socket_path",
                JsonValue::String(socket_path.to_string_lossy().into_owned()),
            ),
        ])),
        Ok(DaemonStartResult::AlreadyRunning { pid, socket_path }) => Ok(JsonValue::obj([
            ("status", JsonValue::String("already_running".to_string())),
            ("name", JsonValue::String(name.to_string())),
            ("pid", JsonValue::Number(pid as i64)),
            (
                "socket_path",
                JsonValue::String(socket_path.to_string_lossy().into_owned()),
            ),
        ])),
        Err(e) => Err(format!("failed to start daemon '{name}': {e}")),
    }
}

/// Handles `daemon.stop`.
pub fn handle_daemon_stop(params: &JsonValue) -> Result<JsonValue, String> {
    let runtime_dir = resolve_runtime_dir().map_err(|e| format!("runtime directory error: {e}"))?;
    let name = params
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or("default");
    let force = params
        .get("force")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    match stop_daemon(&runtime_dir, name, force) {
        Ok(stopped) => {
            let status = if stopped { "stopped" } else { "not_running" };
            Ok(JsonValue::obj([
                ("status", JsonValue::String(status.to_string())),
                ("name", JsonValue::String(name.to_string())),
            ]))
        }
        Err(e) => Err(format!("failed to stop daemon '{name}': {e}")),
    }
}

/// Handles `daemon.switch`.
pub fn handle_daemon_switch(params: &JsonValue) -> Result<JsonValue, String> {
    let name = params
        .get("name")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing 'name' parameter for daemon.switch".to_string())?;

    Ok(JsonValue::obj([
        ("status", JsonValue::String("switched".to_string())),
        ("name", JsonValue::String(name.to_string())),
    ]))
}
