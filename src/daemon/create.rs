//! Create a NEW named daemon (`:daemon new`, `bilterm daemon new`, `daemon.new`, MCP `new`, the
//! daemon window's `[n] New`). One implementation shared by every surface so the rules agree:
//! the name is validated, an auto name is the first free `daemon-N`, and a name that already
//! has a daemon in any state (running, restarting, failed, crashed) is refused, never overwritten.

use std::io;
use std::path::Path;

use crate::daemon::limits::MemoryLimits;
use crate::daemon::list_view::collect_rows;
use crate::daemon::restart_policy::RestartMode;
use crate::daemon::supervisor::{ensure_started_opts, DaemonStartResult};
use crate::secure_fs::validate_safe_name;

/// Prefix of auto-generated names.
pub const AUTO_PREFIX: &str = "daemon-";
/// Upper bound on the auto-name search (bounds the loop; far above any realistic daemon count).
const AUTO_MAX: u32 = 9999;

/// Status of daemon `name` when one is already known to the runtime dir (any state), else `None`.
pub fn existing_status(runtime_dir: &Path, name: &str) -> Option<&'static str> {
    collect_rows(runtime_dir, true).into_iter().find(|r| r.name == name).map(|r| r.status)
}

/// First free `daemon-N` (N from 1): no running, restarting, failed or crashed daemon of that name.
pub fn next_free_name(runtime_dir: &Path) -> String {
    let taken: Vec<String> = collect_rows(runtime_dir, true).into_iter().map(|r| r.name).collect();
    (1..=AUTO_MAX)
        .map(|n| format!("{AUTO_PREFIX}{n}"))
        .find(|c| !taken.contains(c))
        .unwrap_or_else(|| format!("{AUTO_PREFIX}{}", std::process::id()))
}

/// Validates an explicit name for creation (shape + not already present). `Ok(name)` is safe to start.
pub fn check_new_name(runtime_dir: &Path, name: &str) -> Result<String, String> {
    validate_safe_name(name).map_err(|e| format!("invalid daemon name '{name}': {e}"))?;
    if let Some(status) = existing_status(runtime_dir, name) {
        let hint = if status == "running" { "pick another name" } else { "use `daemon start` to restart it" };
        return Err(format!("daemon '{name}' already exists ({status}); {hint}"));
    }
    Ok(name.to_string())
}

/// The chosen name for a create request: the validated explicit name, else the next free one.
pub fn resolve_new_name(runtime_dir: &Path, name: Option<&str>) -> Result<String, String> {
    match name {
        Some(n) => check_new_name(runtime_dir, n),
        None => Ok(next_free_name(runtime_dir)),
    }
}

/// Creates and starts daemon `name` (auto-named when `None`). Returns the chosen name and result.
/// Refuses an existing name; a race that loses to another starter is reported as an error too.
pub fn create_daemon(
    runtime_dir: &Path,
    name: Option<&str>,
    restart: RestartMode,
    limits: MemoryLimits,
    host_ptys: bool,
) -> Result<(String, DaemonStartResult), String> {
    let name = resolve_new_name(runtime_dir, name)?;
    start_checked(runtime_dir, &name, restart, limits, host_ptys).map(|r| (name, r))
}

/// Starts an already-validated new `name`; `AlreadyRunning` (lost race) becomes an error.
pub fn start_checked(
    runtime_dir: &Path,
    name: &str,
    restart: RestartMode,
    limits: MemoryLimits,
    host_ptys: bool,
) -> Result<DaemonStartResult, String> {
    match ensure_started_opts(runtime_dir, name, restart, limits, host_ptys) {
        Ok(DaemonStartResult::AlreadyRunning { pid, .. }) => {
            Err(format!("daemon '{name}' already exists (running, PID {pid}); pick another name"))
        }
        Ok(r) => Ok(r),
        Err(e) => Err(format!("failed to start daemon '{name}': {}", describe(&e))),
    }
}

fn describe(e: &io::Error) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_name_is_first_free() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(next_free_name(dir.path()), "daemon-1");
    }

    #[test]
    fn rejects_invalid_names() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_new_name(dir.path(), "../x").is_err());
        assert!(check_new_name(dir.path(), "a b").is_err());
        assert_eq!(check_new_name(dir.path(), "ok-1").unwrap(), "ok-1");
    }

    #[test]
    fn existing_crashed_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        // A lock record for a dead pid reads as `crashed`.
        let lock = crate::daemon::supervisor::daemon_lock_path_in(dir.path(), "old").unwrap();
        let rec = crate::daemon::supervisor::DaemonLockRecord {
            name: "old".into(),
            pid: 2_000_000_000,
            version: "0".into(),
            started_at_ms: 1,
            socket_path: dir.path().join("x.sock"),
        };
        crate::secure_fs::write_private_file(&lock, rec.to_json().as_bytes()).unwrap();
        let err = check_new_name(dir.path(), "old").unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(next_free_name(dir.path()), "daemon-1");
    }
}
