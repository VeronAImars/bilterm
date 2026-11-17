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
