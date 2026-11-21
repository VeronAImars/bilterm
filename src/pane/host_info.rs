//! Read-only PTY facts for the session host's serializable state.

use super::CompositePane;

impl CompositePane {
    /// OS pid of the local PTY child, if this pane owns one.
    pub fn child_pid(&self) -> Option<u32> {
        match &self.transport {
            super::transport::PaneTransport::Local(p) => p.child_pid(),
            _ => None,
        }
    }
}

impl CompositePane {
    /// Raw PTY master descriptor of a local pane (Unix); `None` for remote/none transports.
    /// Read-only use: `tcgetpgrp` / `ptsname` for `:find --pty` / `--pid` (session find).
    #[cfg(unix)]
    pub fn pty_master_fd(&self) -> Option<std::os::fd::RawFd> {
        match &self.transport {
            super::transport::PaneTransport::Local(p) => p.master_raw_fd(),
            _ => None,
        }
    }
}
