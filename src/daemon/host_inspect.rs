//! Read-only inspection of a session host's persisted state (P1 gap fixes; no restore, that is P6).
//!
//! Staleness is decided from the daemon lockfile `flock` ([`daemon_liveness`]), never from the bare
//! PID stored in the record: the kernel drops the lock when its holder dies, so a recycled PID cannot
//! make a dead host look alive.

use std::path::{Path, PathBuf};

use super::host::{host_state_path_in, read_host_state, HostState};
use super::liveness::{daemon_liveness, DaemonLiveness};

/// What is on disk for host `name`.
#[derive(Debug, PartialEq, Eq)]
pub enum HostInspection {
    /// No state file.
    Absent { path: PathBuf },
    /// File present but unreadable, corrupt, oversized, or of an unknown version.
    Invalid { path: PathBuf, error: String },
    /// File decoded. `stale` is `Some(reason)` when no live host owns it.
    Loaded { path: PathBuf, state: HostState, stale: Option<String> },
}

/// Why `state` cannot belong to a live host named `name`, or `None` when a live daemon holds it.
pub fn stale_reason(runtime_dir: &Path, name: &str, state: &HostState) -> Option<String> {
    if state.host_name != name {
        return Some(format!("record names host '{}', expected '{name}'", state.host_name));
    }
    match daemon_liveness(runtime_dir, name) {
        DaemonLiveness::Running(rec) if rec.pid == state.host_pid => None,
        DaemonLiveness::Running(rec) => Some(format!(
            "daemon '{name}' now runs as pid {}, record was written by pid {}",
            rec.pid, state.host_pid
        )),
        DaemonLiveness::Crashed(_) | DaemonLiveness::Stopped => {
            Some(format!("no live daemon '{name}' holds the lock (writer pid {})", state.host_pid))
        }
    }
}

/// Loads and classifies the state file of host `name` in `runtime_dir`. Never panics, never signals,
/// never deletes.
pub fn inspect_host_state(runtime_dir: &Path, name: &str) -> Result<HostInspection, String> {
    let path = host_state_path_in(runtime_dir, name)?;
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(HostInspection::Absent { path });
    }
    Ok(match read_host_state(&path) {
        Ok(state) => {
            let stale = stale_reason(runtime_dir, name, &state);
            HostInspection::Loaded { path, state, stale }
        }
        Err(error) => HostInspection::Invalid { path, error },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::host::{write_host_state, PtyOwnership, HOST_STATE_VERSION};

    fn state(name: &str, pid: u32) -> HostState {
        HostState {
            version: HOST_STATE_VERSION,
            host_name: name.into(),
            host_pid: pid,
            ownership: PtyOwnership::Host,
            active_pane: 1,
            panes: vec![],
        }
    }

    fn tmp() -> PathBuf {
        let d = crate::secure_fs::short_test_root().join(format!("bt-hinsp-{}", crate::secure_fs::unique_suffix()));
        crate::secure_fs::create_private_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn absent_invalid_and_stale_are_distinguished() {
        let dir = tmp();
        assert!(matches!(inspect_host_state(&dir, "h").unwrap(), HostInspection::Absent { .. }));
        assert!(inspect_host_state(&dir, "../x").is_err());

        let p = host_state_path_in(&dir, "h").unwrap();
        std::fs::write(&p, b"garbage").unwrap();
        assert!(matches!(inspect_host_state(&dir, "h").unwrap(), HostInspection::Invalid { .. }));

        // A record claiming our own (alive) pid is still stale: no daemon lock backs it.
        write_host_state(&p, &state("h", std::process::id())).unwrap();
        match inspect_host_state(&dir, "h").unwrap() {
            HostInspection::Loaded { stale: Some(r), .. } => assert!(r.contains("no live daemon"), "{r}"),
            other => panic!("{other:?}"),
        }
        // Name mismatch is reported too.
        write_host_state(&p, &state("other", 1)).unwrap();
        match inspect_host_state(&dir, "h").unwrap() {
            HostInspection::Loaded { stale: Some(r), .. } => assert!(r.contains("expected 'h'"), "{r}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn live_lock_holder_with_matching_pid_is_not_stale() {
        let dir = tmp();
        let sock = dir.join("d.sock");
        let _lock = match crate::daemon::supervisor::acquire_lock(&dir, "h", sock).unwrap() {
            crate::daemon::supervisor::DaemonLockResult::Acquired(l) => l,
            _ => panic!("lock should be free"),
        };
        let p = host_state_path_in(&dir, "h").unwrap();
        write_host_state(&p, &state("h", std::process::id())).unwrap();
        match inspect_host_state(&dir, "h").unwrap() {
            HostInspection::Loaded { stale: None, .. } => {}
            other => panic!("{other:?}"),
        }
        // Same live daemon, but the record was written by a different (recycled/foreign) pid.
        write_host_state(&p, &state("h", std::process::id() + 1)).unwrap();
        match inspect_host_state(&dir, "h").unwrap() {
            HostInspection::Loaded { stale: Some(r), .. } => assert!(r.contains("now runs as pid"), "{r}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
