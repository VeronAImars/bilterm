//! Off-thread execution of long `dos` operations (setup/download, reset, snapshot, restore,
//! remove) so the UI loop never blocks. One job at a time; the outcome is reported by the next
//! `resilience_tick` as a status message and kept for `dos.status` / `:dos status`.

use std::sync::mpsc;

use super::WindowManager;

/// Outcome of a finished DOS job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DosJobDone {
    pub label: String,
    pub ok: bool,
    /// Human text (the operation's output).
    pub message: String,
}

impl WindowManager {
    /// Label of the in-flight DOS job, if any.
    pub fn dos_job_pending(&self) -> Option<&str> {
        self.resilience.dos_job.as_ref().map(|(l, _)| l.as_str())
    }

    /// The last finished DOS job.
    pub fn dos_job_last(&self) -> Option<&DosJobDone> {
        self.resilience.dos_last.as_ref()
    }

    /// Runs `work` (returns `(ok, message)`) on a worker thread. Refused while another DOS job runs.
    pub fn spawn_dos_job(&mut self, label: &str, work: impl FnOnce() -> (bool, String) + Send + 'static) -> Result<(), String> {
        self.poll_dos_job();
        if let Some(cur) = self.dos_job_pending() {
            return Err(format!("a DOS job is already running: {cur}"));
        }
        let (tx, rx) = mpsc::channel();
        let l = label.to_string();
        let spawned = std::thread::Builder::new().name("dos-job".to_string()).spawn(move || {
            let (ok, message) = work();
            let _ = tx.send(DosJobDone { label: l, ok, message });
        });
        spawned.map_err(|e| format!("cannot start worker: {e}"))?;
        self.resilience.dos_job = Some((label.to_string(), rx));
        Ok(())
    }

    /// Collects a finished job; returns `true` when one completed (status message updated).
    pub(super) fn poll_dos_job(&mut self) -> bool {
        let Some((_, rx)) = self.resilience.dos_job.as_ref() else { return false };
        let done = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => DosJobDone { label: "dos".into(), ok: false, message: "worker ended unexpectedly".into() },
        };
        self.resilience.dos_job = None;
        let first = done.message.lines().find(|l| !l.trim().is_empty()).unwrap_or("done");
        let last = done.message.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(first);
        let msg = format!("dos {}: {} {}", done.label, if done.ok { "ok" } else { "FAILED" }, last);
        self.resilience.dos_last = Some(done);
        self.active_pane_mut().set_status_message(Some(msg));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn job_runs_off_thread_reports_via_tick_and_refuses_overlap() {
        let mut wm = WindowManager::new_headless(80, 25);
        let (tx, rx) = mpsc::channel::<()>();
        wm.spawn_dos_job("setup", move || {
            let _ = rx.recv_timeout(Duration::from_secs(5));
            (true, "line one\nall done\n".into())
        })
        .unwrap();
        assert_eq!(wm.dos_job_pending(), Some("setup"));
        assert!(wm.spawn_dos_job("reset", || (true, String::new())).unwrap_err().contains("already running"));
        assert!(!wm.resilience_tick(Instant::now()), "worker still blocked: UI tick must not wait");
        tx.send(()).unwrap();
        let t0 = Instant::now();
        while wm.dos_job_pending().is_some() && t0.elapsed() < Duration::from_secs(5) {
            wm.resilience_tick(Instant::now());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(wm.dos_job_last().unwrap().message, "line one\nall done\n");
        assert_eq!(wm.active_pane().status_message(), Some("dos setup: ok all done"));
    }
}
