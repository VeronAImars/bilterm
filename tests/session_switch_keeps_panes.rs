//! Leaving a session must never end the programs running in it.
//!
//! Dropping a `CompositePane` drops its `PtySession`, which kills the child. Session switch, new
//! session and window move used to replace or overwrite the pane map in some paths, so the
//! outgoing session's shells (and whatever ran in them: agents, ssh links) died. These tests pin
//! the rule: the outgoing session is parked alive in `background_sessions`, and only an explicit
//! delete (or re-creating the active session under its own name) ends it.

use std::sync::Mutex;

use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::pty::PtyEvent;
use bilterm::wm::split_tree::SplitDirection;
use bilterm::wm::{WindowManager, WorkspaceLayoutMode};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with session files and state under a private temp dir; tests in this file are serial.
fn isolated<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // Serialized by ENV_LOCK: every test in this file goes through here.
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

fn screen(wm: &WindowManager, id: usize) -> String {
    wm.pane(id).unwrap().vt().snapshot().contents()
}

/// Writes `<name>.toml` the way an earlier run would have left it: on disk, not live.
fn save_on_disk(dir: &std::path::Path, name: &str) {
    let other = headless(dir);
    other.save_session(name).unwrap();
}

/// `{"name": <name>}`, the parameter object of the `workspace.*` RPC methods.
fn name_param(name: &str) -> JsonValue {
    JsonValue::obj([("name", JsonValue::String(name.to_string()))])
}

#[cfg(unix)]
mod live_programs {
    use super::*;

    fn real(dir: &std::path::Path) -> WindowManager {
        let mut wm = WindowManager::spawn_default(24, 80).expect("local shell for pane 1");
        wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
        wm
    }

    fn shell_pid(wm: &WindowManager, id: usize) -> u32 {
        wm.pane(id).unwrap().transport().local_pty().unwrap().child_pid().expect("child pid")
    }

    /// The reaper kills a dropped pane's child after at most 250 ms; wait well past that.
    fn alive_after_reaper_window(pid: u32) -> bool {
        std::thread::sleep(std::time::Duration::from_millis(900));
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The reported bug: the target exists only as a saved layout (as after a restart), so the
    /// switch restored it over the live panes and their shells were killed.
    #[test]
    fn switching_to_a_saved_session_keeps_the_outgoing_shell_alive() {
        isolated(|dir| {
            save_on_disk(dir, "other");
            let mut wm = real(dir);
            let pid = shell_pid(&wm, 1);

            let (name, _, created) = wm.switch_or_create_session("other").unwrap();
            assert_eq!((name.as_str(), created), ("other", false));
            assert!(alive_after_reaper_window(pid), "outgoing shell {pid} was killed by the switch");
            assert!(wm.window_manage().background_sessions.contains_key("default"));

            wm.switch_or_create_session("default").unwrap();
            assert_eq!(shell_pid(&wm, 1), pid, "switching back must return the same live shell");
        });
    }

    #[test]
    fn creating_a_session_keeps_the_outgoing_shell_alive() {
        isolated(|dir| {
            let mut wm = real(dir);
            let pid = shell_pid(&wm, 1);

            wm.create_new_session(Some("fresh"), false).unwrap();
            assert!(alive_after_reaper_window(pid), "outgoing shell {pid} was killed by :session new");

            let (_, _, created) = wm.switch_or_create_session("brand-new").unwrap();
            assert!(created);
            let fresh_pid = wm.window_manage().background_sessions["fresh"].panes[&1]
                .transport()
                .local_pty()
                .unwrap()
                .child_pid()
                .unwrap();
            assert!(alive_after_reaper_window(fresh_pid), "shell {fresh_pid} was killed by switch-to-new");
        });
    }

    /// `bilterm workspace select`, RPC `workspace.select` and MCP `workspace_select` all land here.
    #[test]
    fn workspace_select_rpc_keeps_the_outgoing_shell_alive() {
        isolated(|dir| {
            save_on_disk(dir, "other");
            let mut cp = AutomationControlPlane::new_headless(24, 80);
            cp.wm = real(dir);
            let pid = shell_pid(&cp.wm, 1);

            cp.dispatch_method("workspace.select", &name_param("other")).unwrap();
            assert_eq!(cp.wm.config().history_session, "other");
            assert!(alive_after_reaper_window(pid), "outgoing shell {pid} was killed by workspace.select");

            cp.dispatch_method("workspace.select", &name_param("default")).unwrap();
            assert_eq!(shell_pid(&cp.wm, 1), pid, "selecting it again must return the same live shell");
        });
    }
}

#[test]
fn switching_to_a_saved_session_parks_the_outgoing_panes() {
    isolated(|dir| {
        save_on_disk(dir, "other");
        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE_ONE");

        wm.switch_or_create_session("other").unwrap();
        assert_eq!(wm.config().history_session, "other");
        let parked = &wm.window_manage().background_sessions["default"];
        assert!(parked.panes[&1].vt().snapshot().contents().contains("DEFAULT_PANE_ONE"));

        wm.switch_or_create_session("default").unwrap();
        assert!(screen(&wm, 1).contains("DEFAULT_PANE_ONE"), "live screen lost on the way back");
        assert!(wm.window_manage().background_sessions.contains_key("other"));
    });
}

#[test]
fn switching_back_keeps_layout_mode_and_does_not_reuse_live_pane_ids() {
    isolated(|dir| {
        let mut wm = headless(dir);
        wm.split_focused(SplitDirection::Vertical).unwrap();
        let third = wm.split_focused(SplitDirection::Vertical).unwrap();
        mark(&mut wm, 2, "DEFAULT_PANE_TWO");
        wm.set_layout_mode(WorkspaceLayoutMode::Stacked);

        // The new session counts its pane ids from 2 again.
        wm.create_new_session(Some("b"), false).unwrap();
        assert_eq!(wm.layout_mode(), WorkspaceLayoutMode::Tiled);

        wm.switch_or_create_session("default").unwrap();
        assert_eq!(wm.layout_mode(), WorkspaceLayoutMode::Stacked);
        let added = wm.split_focused(SplitDirection::Vertical).unwrap();
        assert_eq!(added, third + 1, "a new pane must not take the id of a live one");
        assert_eq!(wm.pane_count(), 4);
        assert!(screen(&wm, 2).contains("DEFAULT_PANE_TWO"), "pane 2 was overwritten by the split");
    });
}

#[test]
fn moving_a_window_onto_a_taken_id_keeps_both_windows() {
    isolated(|dir| {
        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE_ONE");
        wm.create_new_session(Some("b"), false).unwrap();
        mark(&mut wm, 1, "B_PANE_ONE");

        let msg = wm.move_window_to_session(Some(1), "default", false).unwrap();
        assert!(msg.contains("as window 2"), "{msg}");

        wm.switch_or_create_session("default").unwrap();
        assert_eq!(wm.pane_count(), 2);
        assert!(screen(&wm, 1).contains("DEFAULT_PANE_ONE"), "resident window was overwritten");
        assert!(screen(&wm, 2).contains("B_PANE_ONE"));
        assert_eq!(wm.pane(2).unwrap().id(), 2);
    });
}

#[test]
fn failed_restore_leaves_the_outgoing_session_active() {
    isolated(|dir| {
        save_on_disk(dir, "newer");
        let path = dir.join("sessions").join("newer.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let version = text.lines().find(|l| l.starts_with("format_version")).expect("format_version line");
        std::fs::write(&path, text.replace(version, "format_version = 9999")).unwrap();

        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE_ONE");
        assert!(wm.switch_or_create_session("newer").is_err());
        assert_eq!(wm.config().history_session, "default");
        assert!(screen(&wm, 1).contains("DEFAULT_PANE_ONE"));
        assert!(wm.window_manage().background_sessions.is_empty());
    });
}

#[test]
fn switching_to_the_active_session_changes_nothing() {
    isolated(|dir| {
        let mut wm = headless(dir);
        wm.save_session("default").unwrap();
        mark(&mut wm, 1, "TYPED_AFTER_SAVE");

        let (name, count, created) = wm.switch_or_create_session("default").unwrap();
        assert_eq!((name.as_str(), count, created), ("default", 1, false));
        assert!(screen(&wm, 1).contains("TYPED_AFTER_SAVE"), "the live pane was replaced from disk");
    });
}

#[test]
fn new_session_under_a_live_background_name_is_refused() {
    isolated(|dir| {
        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE_ONE");
        wm.create_new_session(Some("b"), false).unwrap();

        let err = wm.create_new_session(Some("default"), false).unwrap_err().to_string();
        assert!(err.contains("already running in the background"), "{err}");
        assert_eq!(wm.config().history_session, "b");
        let parked = &wm.window_manage().background_sessions["default"];
        assert!(parked.panes[&1].vt().snapshot().contents().contains("DEFAULT_PANE_ONE"));
        // Auto-generated names skip live sessions too.
        assert_ne!(wm.generate_next_session_name(), "default");
    });
}

#[test]
fn deleting_a_background_session_ends_it() {
    isolated(|dir| {
        let mut wm = headless(dir);
        wm.create_new_session(Some("b"), false).unwrap();
        assert!(wm.window_manage().background_sessions.contains_key("default"));

        wm.delete_session("default").unwrap();
        assert!(!wm.window_manage().background_sessions.contains_key("default"));
    });
}

/// The app quits when every active pane has exited; with a session parked that would kill it.
#[test]
fn last_pane_exit_falls_back_to_a_parked_session_instead_of_quitting() {
    isolated(|dir| {
        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE_ONE");
        wm.create_new_session(Some("b"), false).unwrap();

        wm.pane_mut(1).unwrap().ingest_pty_events(vec![PtyEvent::Exited(Some(0))]);
        assert!(wm.all_panes_exited());
        wm.poll_all_ptys();

        assert!(!wm.all_panes_exited(), "the app would quit and kill the parked session");
        assert_eq!(wm.config().history_session, "default");
        assert!(screen(&wm, 1).contains("DEFAULT_PANE_ONE"));
        assert!(wm.active_pane().status_message().unwrap_or("").contains("Session 'b' ended"));
    });
}

#[test]
fn workspace_select_rpc_parks_the_outgoing_session_and_lists_it_as_live() {
    isolated(|dir| {
        save_on_disk(dir, "other");
        let mut cp = AutomationControlPlane::new_headless(24, 80);
        cp.wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
        mark(&mut cp.wm, 1, "DEFAULT_PANE_ONE");

        let res = cp.dispatch_method("workspace.select", &name_param("other")).unwrap();
        assert_eq!(res.get("name").and_then(JsonValue::as_str), Some("other"));
        assert_eq!(cp.wm.config().history_session, "other");
        let parked = &cp.wm.window_manage().background_sessions["default"];
        assert!(parked.panes[&1].vt().snapshot().contents().contains("DEFAULT_PANE_ONE"));

        // The session left behind is listed as live; selecting it by its `wN` id returns its panes.
        let list = cp.dispatch_method("workspace.list", &JsonValue::obj(Vec::<(String, JsonValue)>::new())).unwrap();
        let rows = list.get("workspaces").and_then(JsonValue::as_array).unwrap();
        let row = rows
            .iter()
            .find(|r| r.get("name").and_then(JsonValue::as_str) == Some("default"))
            .expect("the parked session is listed");
        assert_eq!(row.get("live").and_then(JsonValue::as_bool), Some(true));
        let id = row.get("id").and_then(JsonValue::as_str).unwrap().to_string();
        cp.dispatch_method("workspace.select", &name_param(&id)).unwrap();
        assert_eq!(cp.wm.config().history_session, "default");
        assert!(screen(&cp.wm, 1).contains("DEFAULT_PANE_ONE"), "live screen lost on the way back");

        // Selecting never creates a session: that is `workspace.new`.
        let err = cp.dispatch_method("workspace.select", &name_param("no-such-session")).unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert_eq!(cp.wm.config().history_session, "default");
    });
}
