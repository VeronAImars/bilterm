//! `Action::DosRun`: opens a DOS pane (feature `dos`, PR D2a).
//!
//! The pane is an ordinary PTY pane whose program is this executable running
//! `bilterm dos run ...` (see [`crate::dos::launch`]); that process owns the image lock, the
//! `.dirty` marker, and QEMU's lifetime, so the window manager needs no exit hook. Headless /
//! model-only managers create the pane without a PTY, exactly like `spawn_ssh_pane`.

use std::path::Path;

use super::WindowManager;
use crate::dos::launch::{MIN_COLS, MIN_ROWS, RunEnv, RunRequest, pane_argv, preflight};
use crate::error::{BiltermError, Result};
use crate::pane::{CompositePane, PaneId};
use crate::pty::PtyConfig;
use crate::wm::split_tree::SplitDirection;

impl WindowManager {
    /// Opens a DOS pane for `req`. `exe` is the bilterm executable the pane will run.
    ///
    /// Refuses (no pane is left behind) when the host cannot run DOS mode, the image or emulator
    /// is missing, or the pane cannot be given at least 80x25 cells even when zoomed.
    pub fn spawn_dos_pane(&mut self, req: &RunRequest, env: &RunEnv, exe: &Path) -> Result<PaneId> {
        preflight(req, env).map_err(|e| BiltermError::Config(e.to_string()))?;
        let id = self.split_focused(SplitDirection::Vertical)?;
        let size_of = |wm: &Self| wm.panes.get(&id).map(|p| p.vt().size()).unwrap_or(wm.default_size);
        let (mut rows, mut cols) = size_of(self);
        if rows < MIN_ROWS || cols < MIN_COLS {
            // A split is often too small for 80x25: give the pane the whole workspace instead.
            self.zoomed_pane = Some(id);
            self.reflow_default_viewport();
            (rows, cols) = size_of(self);
        }
        if rows < MIN_ROWS || cols < MIN_COLS {
            self.remove_pane(id);
            return Err(BiltermError::Config(format!(
                "pane is {cols}x{rows}; the DOS screen needs at least {MIN_COLS}x{MIN_ROWS} (enlarge the window)"
            )));
        }
        let title = format!("DOS:{} [#{id}]", req.name.as_deref().unwrap_or("default"));
        if !self.model_only_panes() {
            let mut cfg = PtyConfig {
                program: exe.to_string_lossy().into_owned(),
                args: pane_argv(req),
                rows,
                cols,
                ..PtyConfig::default()
            };
            // curses needs a real terminal type regardless of the user's shell environment
            cfg.env.insert("TERM".into(), "xterm-256color".into());
            let mut live = CompositePane::spawn(id, cfg)?;
            Self::attach_history(&mut live, &self.config.history_settings());
            self.panes.insert(id, live);
            self.reflow_default_viewport();
        }
        if let Some(p) = self.panes.get_mut(&id) {
            p.set_title(title);
            p.set_status_message(Some(format!("DOS pane #{id}: starting QEMU (qemu-curses)")));
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dos::backend::mock::MockBackend;
    use crate::dos::backend::{DosBackend, SessionSpec};
    use crate::dos::launch::tests::{FakeProbe, fx};

    fn exe() -> std::path::PathBuf {
        "/opt/bilterm/bilterm".into()
    }

    #[test]
    fn opens_a_titled_pane_in_a_big_enough_workspace() {
        let f = fx();
        let probe = FakeProbe::good();
        let mut wm = WindowManager::new_headless(50, 200);
        let before = wm.pane_count();
        let id = wm.spawn_dos_pane(&RunRequest::default(), &f.env(&probe), &exe()).unwrap();
        assert_eq!(wm.pane_count(), before + 1);
        let p = wm.pane(id).unwrap();
        assert!(p.title().starts_with("DOS:default"), "{}", p.title());
        let (r, c) = p.vt().size();
        assert!(r >= MIN_ROWS && c >= MIN_COLS, "{r}x{c}");
    }

    #[test]
    fn refuses_tiny_workspace_and_leaves_no_pane() {
        let f = fx();
        let probe = FakeProbe::good();
        let mut wm = WindowManager::new_headless(20, 60);
        let before = wm.pane_count();
        let e = wm.spawn_dos_pane(&RunRequest::default(), &f.env(&probe), &exe()).unwrap_err();
        assert!(e.to_string().contains("80x25"), "{e}");
        assert_eq!(wm.pane_count(), before);
    }

    #[test]
    fn refuses_missing_image_before_creating_a_pane() {
        let f = fx();
        let probe = FakeProbe::good();
        let mut wm = WindowManager::new_headless(50, 200);
        let before = wm.pane_count();
        let req = RunRequest { name: Some("nope".into()), ..RunRequest::default() };
        let e = wm.spawn_dos_pane(&req, &f.env(&probe), &exe()).unwrap_err();
        assert!(e.to_string().contains("dos setup"), "{e}");
        assert_eq!(wm.pane_count(), before);
    }

    #[test]
    fn colon_command_lowers_through_apply() {
        let mut wm = WindowManager::new_headless(50, 200);
        let a = crate::colon_command::parse_colon_command(":dos run --name x").unwrap().unwrap();
        // no image named x exists in the test environment's data dir: refusal is reported in
        // the status bar and no pane is added
        let before = wm.pane_count();
        wm.apply(a).unwrap();
        assert_eq!(wm.pane_count(), before);
    }

    /// Fixture: the mock backend's canned 80x25 screen, drawn into a headless pane as ANSI text,
    /// stands in for QEMU's curses output (the pane itself is an ordinary PTY pane).
    #[test]
    fn mock_screen_renders_in_a_headless_pane() {
        let mut m = MockBackend::new();
        m.launch(&SessionSpec::default()).unwrap();
        let screen = m.read_text().unwrap();
        let mut wm = WindowManager::new_headless(50, 200);
        let probe = FakeProbe::good();
        let f = fx();
        let id = wm.spawn_dos_pane(&RunRequest::default(), &f.env(&probe), &exe()).unwrap();
        let mut ansi = String::from("\x1b[2J\x1b[H");
        ansi.push_str(&screen.lines().iter().map(|l| l.trim_end().to_string()).collect::<Vec<_>>().join("\r\n"));
        wm.pane_mut(id).unwrap().process_pty_output(ansi.as_bytes());
        let text = wm.pane(id).unwrap().vt().contents();
        assert!(text.contains("C:\\>"), "{text}");
    }
}
