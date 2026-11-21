//! Tracking a child that this process may not have spawned (`H1.4`, unix only).
//!
//! After a handover the adopter is usually not the child's parent. `waitpid` then fails with
//! `ECHILD` and no exit status exists for it; liveness still works through `kill(pid, 0)` plus a
//! zombie check (`kill` succeeds for an unreaped zombie). When the adopter IS the parent (same
//! process, or the old host kept the `Child`) `waitpid` yields the real status. pid reuse is an
//! accepted residual risk for the non-parent path.

/// What a poll of the child found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildState {
    Running,
    /// Gone. `Some(code)` when the status was obtainable (we are the parent); a signal death is
    /// reported as `128 + signal`.
    Exited(Option<i32>),
}

/// Status reaped once is remembered: a second `waitpid` would only say `ECHILD`.
#[derive(Clone, Debug)]
pub struct ChildWatch {
    pid: libc::pid_t,
    reaped: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

const NONE: i64 = i64::MIN;

/// Decodes a `waitpid` status word.
pub fn decode_status(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        status
    }
}

impl ChildWatch {
    pub fn new(pid: u32) -> Self {
        Self { pid: pid as libc::pid_t, reaped: std::sync::Arc::new(std::sync::atomic::AtomicI64::new(NONE)) }
    }

    pub fn poll(&self) -> ChildState {
        let known = self.reaped.load(std::sync::atomic::Ordering::SeqCst);
        if known != NONE {
            return ChildState::Exited(Some(known as i32));
        }
        let mut status: libc::c_int = 0;
        // SAFETY: valid out pointer; WNOHANG never blocks.
        let r = unsafe { libc::waitpid(self.pid, &raw mut status, libc::WNOHANG) };
        if r == self.pid {
            let code = decode_status(status);
            self.reaped.store(i64::from(code), std::sync::atomic::Ordering::SeqCst);
            return ChildState::Exited(Some(code));
        }
        if r == 0 {
            return ChildState::Running;
        }
        // Not our child (ECHILD) or already reaped by someone else: liveness only.
        // SAFETY: signal 0 only checks existence.
        if unsafe { libc::kill(self.pid, 0) } != 0 {
            return ChildState::Exited(None);
        }
        if is_zombie(self.pid) {
            return ChildState::Exited(None);
        }
        ChildState::Running
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn is_zombie(pid: libc::pid_t) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (comm) S ...`: comm may contain spaces/parens, so split at the LAST ')'.
    stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with(['Z', 'X']))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn is_zombie(_pid: libc::pid_t) -> bool {
    false
}
