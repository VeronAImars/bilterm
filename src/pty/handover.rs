//! Live-handover primitives for a PTY session (`H1`, unix only): quiesce the reader, release
//! the master and child WITHOUT killing, and adopt a released master in another session.
//! See `docs/plans/session-host-architecture.md` section 7.2 and
//! `docs/plans/session-handover-spike.md`.
//!
//! Rules enforced here:
//! - the reader is a pollable thread that is provably stopped before the fd leaves (verdict 1d);
//! - the releasing side keeps every fd copy open until [`ReleasedPty::ack`] (macOS drops queued
//!   output when the last copy closes early, verdict 1e);
//! - `O_NONBLOCK` is never set on the master (shared with the adopter).
//!
//! Exit status of an adopted child is obtainable only by its parent (`ChildWatch`); a non-parent
//! gets liveness (master EOF/EIO, `kill(0)` + zombie check) and `exit_code` is `None`.

use std::fs::File;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, MasterPty};
use tokio::sync::mpsc;

use super::{fdpass, reader, writer, PtyEvent, PtySession, ReaderCtl};
use crate::error::{BiltermError, Result};

/// Sets the window size on a master fd (`TIOCSWINSZ`); delivers `SIGWINCH` to the child.
pub(super) fn set_winsize(fd: &OwnedFd, rows: u16, cols: u16) -> std::io::Result<()> {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: valid fd and winsize pointer.
    if unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ as _, &raw const ws) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Everything the adopting side needs besides the fd itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdoptMeta {
    pub pid: u32,
    pub rows: u16,
    pub cols: u16,
}

/// A detached PTY: the child keeps running, the old session is inert. Holds every fd copy and
/// the child handle open until [`ack`](Self::ack) (or drop).
pub struct ReleasedPty {
    fd: OwnedFd,
    meta: AdoptMeta,
    pending: Vec<PtyEvent>,
    _master: Option<Box<dyn MasterPty + Send>>,
    _child: Option<Box<dyn Child + Send + Sync>>,
}

impl ReleasedPty {
    pub fn meta(&self) -> AdoptMeta {
        self.meta
    }

    /// Events the old reader had already consumed but nobody drained; they travel in the
    /// handover payload (the adopter never sees these bytes on the fd).
    pub fn take_pending(&mut self) -> Vec<PtyEvent> {
        std::mem::take(&mut self.pending)
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Sends `payload` and the master fd on `sock`; all local copies stay open.
    pub fn export(&self, sock: RawFd, payload: &[u8]) -> std::io::Result<usize> {
        fdpass::send_fds(sock, payload, &[self.fd.as_raw_fd()])
    }

    /// The new host confirmed adoption: close our copies. The shell is not touched. When we are
    /// still the child's parent a detached thread reaps it at exit, so it never lingers as a
    /// zombie for as long as this process lives (its status is not reported to the adopter).
    pub fn ack(mut self) {
        if let Some(mut child) = self._child.take() {
            let _ = std::thread::Builder::new().name("bilterm-pty-reaper".into()).spawn(move || {
                let _ = child.wait();
            });
        }
    }

    /// Rollback: the new host never confirmed. Puts master and child back into `session` (the
    /// one this was released from), restarts its writer and reader, and re-queues any events not
    /// yet taken with [`take_pending`](Self::take_pending) ahead of new output. No byte of the
    /// stream is lost: the PTY bytes were never read by anyone else (the adopter's reader starts
    /// only after commit).
    pub fn reclaim(mut self, session: &mut PtySession) -> Result<()> {
        if !session.released {
            return Err(BiltermError::Pty("session was not released".into()));
        }
        let wfd = reader::dup_cloexec(self.fd.as_raw_fd()).map_err(BiltermError::Io)?;
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = writer::PtyWriter::spawn(Box::new(super::master_writer::MasterWriter::new(wfd, flag.clone())), session.traffic.clone())
            .map_err(BiltermError::Io)?;
        session.writer = Arc::new(writer);
        session.suppress_eof = flag;
        if let Some(m) = self._master.take() {
            *session.master.lock().unwrap_or_else(|p| p.into_inner()) = Some(m);
        }
        if let Some(c) = self._child.take() {
            *session.child.lock().unwrap_or_else(|p| p.into_inner()) = Some(c);
        }
        for ev in self.pending.drain(..).rev() {
            session.replay.push_front(ev);
        }
        session.released = false;
        session.resume_reader()
    }
}

impl PtySession {
    /// Stops the reader and returns every event it had already queued (`H1.2`). On `Ok` the
    /// reader thread is gone and no byte is consumed from the PTY until [`resume_reader`].
    /// The returned events are in order; a trailing `Exited` means the child's side closed.
    pub fn quiesce_reader(&mut self, patience: Duration) -> Result<Vec<PtyEvent>> {
        let Some(ctl) = self.reader.as_ref() else {
            return Ok(Vec::new());
        };
        let deadline = Instant::now() + patience;
        let mut events = Vec::new();
        loop {
            // Keep draining so a reader blocked on a full channel can finish its send.
            while let Ok(ev) = self.output_rx.try_recv() {
                events.push(ev);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match ctl.poll.stop(left.min(Duration::from_millis(5))) {
                Ok(()) => break,
                Err(_) if Instant::now() >= deadline => {
                    return Err(BiltermError::Pty("PTY reader did not quiesce in time".into()));
                }
                Err(_) => {}
            }
        }
        while let Ok(ev) = self.output_rx.try_recv() {
            events.push(ev);
        }
        Ok(events)
    }

    /// Restarts a quiesced reader on a fresh dup of the master.
    pub fn resume_reader(&mut self) -> Result<()> {
        let fd = self.master_fd.ok_or_else(|| BiltermError::Pty("no master fd".into()))?;
        let Some(ctl) = self.reader.as_mut() else {
            return Err(BiltermError::Pty("session has no reader".into()));
        };
        if !ctl.poll.finished() {
            return Err(BiltermError::Pty("reader still running".into()));
        }
        let own = reader::dup_cloexec(fd).map_err(BiltermError::Io)?;
        ctl.poll = reader::PollReader::start(own, ctl.tx.clone(), self.traffic.clone())
            .map_err(BiltermError::Io)?;
        Ok(())
    }

    /// Detaches master fd and child WITHOUT killing the child (the normal `Drop` hangs up and
    /// kills). Quiesces the reader and flushes queued input first. After `Ok` this session is
    /// inert (`write_all` fails, nothing is read); on `Err` it is still usable.
    pub fn release(&mut self, patience: Duration) -> Result<ReleasedPty> {
        if self.released {
            return Err(BiltermError::Pty("session already released".into()));
        }
        let fd = self.master_fd.ok_or_else(|| BiltermError::Pty("no master fd".into()))?;
        let pid = self
            .child_pid
            .ok_or_else(|| BiltermError::Pty("no child pid".into()))?;
        let pending = self.quiesce_reader(patience)?;
        if pending.iter().any(|e| matches!(e, PtyEvent::Exited(_))) {
            // The stream already ended; hand the events back through the queue untouched.
            return Err(BiltermError::Pty("child already exited".into()));
        }
        // Input accepted but not yet written would otherwise be lost with the writer.
        if !self.writer.wait_drained(patience) {
            self.requeue_or_resume(pending);
            return Err(BiltermError::Pty("queued input did not drain in time".into()));
        }
        let dup = match reader::dup_cloexec(fd) {
            Ok(d) => d,
            Err(e) => {
                self.requeue_or_resume(pending);
                return Err(BiltermError::Io(e));
            }
        };
        // portable-style EOF-on-drop must not reach the child: it keeps running elsewhere.
        self.suppress_eof.store(true, std::sync::atomic::Ordering::SeqCst);
        self.writer.close();
        let (rows, cols) = self.size();
        self.released = true;
        Ok(ReleasedPty {
            fd: dup,
            meta: AdoptMeta { pid, rows, cols },
            pending,
            _master: self.master.lock().ok().and_then(|mut g| g.take()),
            _child: self.child.lock().ok().and_then(|mut g| g.take()),
        })
    }

    /// Failure path of `release`: the events already taken go back in front of new output.
    fn requeue_or_resume(&mut self, pending: Vec<PtyEvent>) {
        for ev in pending.into_iter().rev() {
            self.replay.push_front(ev);
        }
        let _ = self.resume_reader();
    }

    /// Adopts a master fd released by another session or process. The child is not ours: no
    /// kill on drop beyond the kernel hang-up when the last master copy closes, and
    /// `exit_code` is always `None` (exit is reported through EOF/EIO).
    pub fn adopt(fd: OwnedFd, meta: AdoptMeta) -> Result<Self> {
        let raw = fd.as_raw_fd();
        let write_fd = reader::dup_cloexec(raw).map_err(BiltermError::Io)?;
        let read_fd = reader::dup_cloexec(raw).map_err(BiltermError::Io)?;
        let traffic = crate::traffic::ChannelCounters::new();
        let (tx, rx) = mpsc::channel(32);
        let writer = writer::PtyWriter::spawn(Box::new(File::from(write_fd)), traffic.clone())
            .map_err(BiltermError::Io)?;
        // Last fallible step: a failed adoption must not have consumed any PTY byte.
        let poll = reader::PollReader::start(read_fd, tx.clone(), traffic.clone())
            .map_err(BiltermError::Io)?;
        let rows = meta.rows.max(1);
        let cols = meta.cols.max(1);
        Ok(Self {
            writer: Arc::new(writer),
            exit_reported: false,
            master: Mutex::new(None),
            child: Mutex::new(None),
            output_rx: rx,
            current_size: Mutex::new((rows, cols)),
            sigwinch_count: Arc::new(AtomicUsize::new(0)),
            child_pid: Some(meta.pid),
            traffic,
            master_fd: Some(raw),
            reader: Some(ReaderCtl { poll, tx }),
            adopted_fd: Some(fd),
            released: false,
            suppress_eof: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            replay: std::collections::VecDeque::new(),
            watch: Some(super::child_watch::ChildWatch::new(meta.pid)),
        })
    }

    /// Receives one handover message (payload + master fd) from `sock` and adopts it.
    pub fn adopt_from_socket(sock: RawFd, meta: AdoptMeta, buf: &mut [u8]) -> Result<(usize, Self)> {
        let (n, mut fds) = fdpass::recv_fds(sock, buf).map_err(BiltermError::Io)?;
        if fds.len() != 1 {
            return Err(BiltermError::Pty(format!("expected 1 fd, got {}", fds.len())));
        }
        Ok((n, Self::adopt(fds.remove(0), meta)?))
    }
}
