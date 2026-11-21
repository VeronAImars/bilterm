//! Pollable PTY reader (unix) for live handover (`H1.2`).
//!
//! The reader thread owns its own `dup` of the PTY master and waits in `poll(master, wake)`
//! before every `read`, so it can be stopped at any moment without ever sitting in a blocking
//! `read()` that would steal bytes after the master was handed to another process. Once
//! [`PollReader::stop`] returns `Ok`, the thread has exited and every byte it consumed has been
//! queued on the output channel; nothing is read from the PTY afterwards.
//!
//! The master's `O_NONBLOCK` flag is never touched (it lives on the open file description,
//! which an adopter shares).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::{wake, PtyEvent};
use crate::traffic::TrafficTap;

/// Duplicates `fd` with `FD_CLOEXEC` set.
pub(super) fn dup_cloexec(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: plain fcntl on a caller-provided open descriptor; the result is owned below.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if new < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `new` is a fresh descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(new) })
}

/// Self-pipe used to interrupt the reader's `poll`.
struct WakePipe {
    rd: OwnedFd,
    wr: OwnedFd,
}

impl WakePipe {
    fn new() -> io::Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors `pipe` writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both are fresh descriptors from `pipe`.
        let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        for fd in [&rd, &wr] {
            // SAFETY: flag updates on descriptors we own.
            unsafe {
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                let fl = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        Ok(Self { rd, wr })
    }

    fn wake(&self) {
        let b = 1u8;
        // SAFETY: one byte from a valid buffer; a full pipe (EAGAIN) already means "woken".
        unsafe { libc::write(self.wr.as_raw_fd(), (&raw const b).cast(), 1) };
    }
}

struct Shared {
    stop: AtomicBool,
    done: Mutex<bool>,
    done_cv: Condvar,
    wake: WakePipe,
}

/// Handle on a running pollable reader thread.
pub(super) struct PollReader {
    shared: Arc<Shared>,
}

impl PollReader {
    /// Starts a reader over `fd` (an owned dup of the master) feeding `tx`.
    pub(super) fn start(
        fd: OwnedFd,
        tx: mpsc::Sender<PtyEvent>,
        traffic: TrafficTap,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
            wake: WakePipe::new()?,
        });
        let s = shared.clone();
        thread::Builder::new()
            .name("bilterm-pty-reader".to_string())
            .spawn(move || {
                run(&s, &fd, &tx, &traffic);
                drop(fd);
                *s.done.lock().unwrap_or_else(|p| p.into_inner()) = true;
                s.done_cv.notify_all();
                wake::notify_output();
            })?;
        Ok(Self { shared })
    }

    /// Asks the thread to stop and waits up to `patience` for it to exit. `Err(TimedOut)` when
    /// it is still delivering a chunk (the output channel is full: keep draining it).
    pub(super) fn stop(&self, patience: Duration) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.wake();
        let deadline = Instant::now() + patience;
        let mut done = self.shared.done.lock().unwrap_or_else(|p| p.into_inner());
        while !*done {
            let now = Instant::now();
            if now >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            done = self
                .shared
                .done_cv
                .wait_timeout(done, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        Ok(())
    }

    /// True once the thread has exited (stopped, EOF or error).
    pub(super) fn finished(&self) -> bool {
        *self.shared.done.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for PollReader {
    /// Dropping the handle only nudges the thread; it ends on its own at EOF or when asked.
    fn drop(&mut self) {
        // A session being dropped closes its channel, which ends the thread at its next send;
        // the nudge makes an idle one leave immediately instead of waiting for output.
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.wake();
    }
}

fn run(s: &Shared, fd: &OwnedFd, tx: &mpsc::Sender<PtyEvent>, traffic: &TrafficTap) {
    let mut buf = [0u8; 8192];
    loop {
        if s.stop.load(Ordering::SeqCst) {
            return;
        }
        let mut pfds = [
            libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: s.wake.rd.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: `pfds` is a valid array of two pollfd structs.
        let rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, -1) };
        if rc < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            let _ = tx.blocking_send(PtyEvent::Exited(None));
            return;
        }
        // Stop wins over readable data: bytes stay in the kernel buffer for the adopter.
        if pfds[1].revents != 0 || s.stop.load(Ordering::SeqCst) {
            return;
        }
        if pfds[0].revents == 0 {
            continue;
        }
        // Readable (or hung up): this read cannot block, since only this thread reads.
        // SAFETY: `buf` is a valid writable buffer of the stated length.
        let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            match io::Error::last_os_error().kind() {
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
                _ => {
                    let _ = tx.blocking_send(PtyEvent::Exited(None));
                    return;
                }
            }
        }
        if n == 0 {
            let _ = tx.blocking_send(PtyEvent::Exited(None));
            return;
        }
        let n = n as usize;
        traffic.rx(n);
        if tx.blocking_send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
            return;
        }
        wake::notify_output();
    }
}
