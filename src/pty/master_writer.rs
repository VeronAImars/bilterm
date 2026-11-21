//! Unix PTY input sink with the same "send newline + EOF on drop" behaviour as portable-pty's
//! own writer (so a plain close still ends a `cat`), except when the PTY is being handed to
//! another host: then the EOF must NOT be injected (`suppress` set by `PtySession::release`),
//! or the session's foreground program would see a stray `\n^D`.

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct MasterWriter {
    file: File,
    suppress_eof: Arc<AtomicBool>,
}

impl MasterWriter {
    pub fn new(fd: OwnedFd, suppress_eof: Arc<AtomicBool>) -> Self {
        Self { file: File::from(fd), suppress_eof }
    }
}

impl Write for MasterWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for MasterWriter {
    fn drop(&mut self) {
        if self.suppress_eof.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: zeroed termios is a valid out buffer for tcgetattr.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid fd and out pointer.
        if unsafe { libc::tcgetattr(self.file.as_raw_fd(), &raw mut t) } == 0 {
            // EOF is only interpreted after a newline, so send a newline followed by EOF.
            let eot = t.c_cc[libc::VEOF];
            if eot != 0 {
                let _ = self.file.write_all(&[b'\n', eot]);
            }
        }
    }
}
