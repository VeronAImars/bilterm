//! Recording byte-cap enforcement: auto-stops runaway recordings and surfaces a notice.

use super::WindowManager;
use crate::vt::recording::WorkspaceRecorder;

impl WindowManager {
    /// Starts whole-workspace `.cast` compositor frame recording (`REC-02`), capped at
    /// `[recording] max_bytes`.
    pub fn start_workspace_recording(&mut self, path: std::path::PathBuf, private: bool) {
        let (rows, cols) = self.default_size;
        if let Ok(mut guard) = self.workspace_recorder.lock() {
            let mut rec = WorkspaceRecorder::new(path, private, cols, rows);
            rec.set_max_bytes(self.config.recording.max_bytes());
            *guard = Some(rec);
        }
    }

    /// App-tick hook: applies `[recording] max_bytes` to every pane, finalises a workspace
    /// recorder that hit its cap, and shows a status notice for any recording that auto-stopped.
    pub(super) fn poll_recording_caps(&mut self) {
        let cap = self.config.recording.max_bytes();
        for pane in self.panes.values_mut() {
            let vt = pane.vt_mut();
            vt.set_recording_max_bytes(cap);
            if let Some(notice) = vt.take_recording_notice() {
                pane.set_status_message(Some(notice));
            }
        }
        let capped = self
            .workspace_recorder
            .lock()
            .map(|g| g.as_ref().is_some_and(|r| r.is_capped()))
            .unwrap_or(false);
        if capped {
            for pane in self.panes.values_mut() {
                pane.vt_mut().cancel_recording();
            }
            let msg = match self.stop_workspace_recording() {
                Ok(Some(path)) => format!(
                    "Workspace recording stopped: reached {} cap; saved to {}",
                    crate::vt::scrollback::format_mib(cap as u64),
                    path.display()
                ),
                Ok(None) => "Workspace recording stopped".to_string(),
                Err(e) => format!("Workspace recording stopped at cap; save failed: {e}"),
            };
            self.active_pane_mut().set_status_message(Some(msg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn workspace_recording_auto_stops_at_cap_with_notice() {
        let mut wm = WindowManager::new_headless(20, 5);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.cast");
        wm.config.recording.max_bytes = crate::vt::recording::ByteSize(2048);
        wm.start_workspace_recording(path.clone(), false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        for i in 0..200 {
            let ch = char::from(b'a' + (i % 26) as u8);
            buf.set_string(0, 0, ch.to_string().repeat(20), ratatui::style::Style::default());
            wm.record_workspace_frame(&buf);
        }
        wm.poll_recording_caps();
        assert!(!wm.is_workspace_recording(), "auto-stopped at cap");
        assert!(path.exists(), "recording saved");
        let msg = wm.active_pane_mut().status_message().unwrap_or_default().to_string();
        assert!(msg.contains("reached"), "notice surfaced: {msg}");
    }
}
