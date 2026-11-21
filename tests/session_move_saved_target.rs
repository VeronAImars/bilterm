//! Moving a window into a session that exists only on disk must keep that session's windows
//! (drawer plan R3) and must be atomic: on failure the source session is untouched.

use std::sync::Mutex;

use bilterm::wm::split_tree::SplitDirection;
use bilterm::wm::WindowManager;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn isolated<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        std::env::set_var("XDG_STATE_HOME", dir.path());
    }
    f(dir.path())
}

fn headless(dir: &std::path::Path) -> WindowManager {
    let mut wm = WindowManager::new_headless(24, 80);
    wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
    wm
}

fn mark(wm: &mut WindowManager, id: usize, text: &str) {
    wm.pane_mut(id).unwrap().process_pty_output(format!("{text}\r\n").as_bytes());
}

/// Saves a 2-window session `name` to disk only.
fn save_two_window_session(dir: &std::path::Path, name: &str) {
    let mut other = headless(dir);
    other.split_focused(SplitDirection::Vertical).unwrap();
    assert_eq!(other.pane_count(), 2);
    other.save_session(name).unwrap();
}

/// Source "default" with two windows (so moving one leaves it non-empty).
fn source_with_two(dir: &std::path::Path) -> WindowManager {
    let mut wm = headless(dir);
    mark(&mut wm, 1, "SRC_ONE");
    wm.split_focused(SplitDirection::Vertical).unwrap();
    wm
}

fn saved_pane_count(dir: &std::path::Path, name: &str) -> usize {
    let mut probe = headless(dir);
    probe.restore_session(name).unwrap()
}

#[test]
fn saved_only_target_keeps_all_windows_with_switch() {
    isolated(|dir| {
        save_two_window_session(dir, "other");
        let mut wm = source_with_two(dir);
        wm.move_window_to_session(Some(1), "other", true).unwrap();
        assert_eq!(wm.config().history_session, "other");
        assert_eq!(wm.pane_count(), 3, "saved windows were lost");
        assert_eq!(wm.window_manage().background_sessions["default"].panes.len(), 1);
    });
}

#[test]
fn saved_only_target_keeps_all_windows_without_switch() {
    isolated(|dir| {
        save_two_window_session(dir, "other");
        let mut wm = source_with_two(dir);
        wm.move_window_to_session(Some(1), "other", false).unwrap();
        assert_eq!(wm.config().history_session, "default");
        assert_eq!(wm.pane_count(), 1);
        assert_eq!(wm.window_manage().background_sessions["other"].panes.len(), 3);
        // Saving the target afterwards must not shrink it on disk.
        wm.switch_or_create_session("other").unwrap();
        wm.save_session("other").unwrap();
        assert_eq!(saved_pane_count(dir, "other"), 3);
    });
}

#[test]
fn failed_hydration_leaves_source_intact() {
    isolated(|dir| {
        save_two_window_session(dir, "newer");
        let path = dir.join("sessions").join("newer.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let version = text.lines().find(|l| l.starts_with("format_version")).unwrap();
        std::fs::write(&path, text.replace(version, "format_version = 9999")).unwrap();

        let mut wm = source_with_two(dir);
        assert!(wm.move_window_to_session(Some(1), "newer", true).is_err());
        assert_eq!(wm.config().history_session, "default");
        assert_eq!(wm.pane_count(), 2, "source lost its window");
        assert!(wm.pane(1).unwrap().vt().snapshot().contents().contains("SRC_ONE"));
        assert!(wm.window_manage().background_sessions.is_empty());
    });
}

#[test]
fn live_target_behavior_unchanged() {
    isolated(|dir| {
        let mut wm = headless(dir);
        wm.create_new_session(Some("b"), false).unwrap();
        wm.split_focused(SplitDirection::Vertical).unwrap();
        // active "b" has 2 panes; "default" is parked with 1.
        wm.move_window_to_session(Some(2), "default", false).unwrap();
        assert_eq!(wm.pane_count(), 1);
        assert_eq!(wm.window_manage().background_sessions["default"].panes.len(), 2);
    });
}
