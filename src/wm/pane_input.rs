//! Raw input from an attach client (`Action::PaneInputBytes`, plan T-A5).

use super::{PaneId, WindowManager};

impl WindowManager {
    /// Checks that raw bytes may be written to `pane` (exists, not locked read-only).
    pub fn pane_input_allowed(&self, pane: PaneId) -> Result<(), String> {
        match self.pane(pane) {
            None => Err(format!("pane #{pane} not found")),
            Some(p) if p.is_read_only() => Err(format!("pane #{pane} is locked read-only (PROT)")),
            Some(_) => Ok(()),
        }
    }

    /// Writes attach-client bytes to `pane`'s PTY. Refusals and write errors surface in the
    /// status line (the action path has no reply channel); the PROT lock is enforced here too.
    pub(crate) fn pane_input_bytes(&mut self, pane: PaneId, bytes: &[u8]) {
        let msg = match self.pane_input_allowed(pane) {
            Err(e) => Some(format!("Attach input rejected: {e}")),
            Ok(()) => self
                .pane_mut(pane)
                .and_then(|p| p.write_input(bytes).err())
                .map(|e| format!("Attach input error: {e}")),
        };
        if let Some(m) = msg {
            self.active_pane_mut().set_status_message(Some(m));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::action::Action;
    use crate::wm::WindowManager;

    #[test]
    fn unknown_pane_is_refused_without_panic() {
        let mut wm = WindowManager::new_headless(24, 80);
        assert!(wm.pane_input_allowed(99).is_err());
        wm.apply(Action::PaneInputBytes { pane: 99, bytes: b"x".to_vec() }).unwrap();
        assert!(wm.pane_input_allowed(1).is_ok());
        // Headless panes have no PTY: the write is a no-op/err, never a panic.
        wm.apply(Action::PaneInputBytes { pane: 1, bytes: b"ls\r".to_vec() }).unwrap();
    }

    #[test]
    fn read_only_pane_refuses() {
        let mut wm = WindowManager::new_headless(24, 80);
        wm.pane_mut(1).unwrap().set_read_only(true);
        assert!(wm.pane_input_allowed(1).unwrap_err().contains("read-only"));
        wm.apply(Action::PaneInputBytes { pane: 1, bytes: b"x".to_vec() }).unwrap();
    }
}
