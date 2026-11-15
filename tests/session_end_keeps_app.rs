//! Ending a session must not end bilterm while another session exists.
//!
//! The report: "kill session kills the entire bilterm". The Exit Guard's `(K)ill session` button
//! quit the app, so every session parked in the background died with it, and there was no
//! command to end just one session. These tests pin the rules: ending the active session moves to
//! another one (live background session, most recently used first, then a saved one); ending a
//! background session ends only its programs; only ending the last session quits.

mod common;

use std::sync::Mutex;

use bilterm::action::Action;
use bilterm::input::AppAction;
use bilterm::pane::ExitGuardChoice;
use bilterm::pty::PtyEvent;
use bilterm::wm::session_end::SessionEndOutcome;
use bilterm::wm::WindowManager;

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

fn colon(wm: &mut WindowManager, cmd: &str) -> AppAction {
    let action = bilterm::colon_command::parse_colon_command(cmd).unwrap().unwrap();
    wm.apply(action).unwrap()
}

fn live(wm: &WindowManager) -> Vec<String> {
    wm.window_manage().background_sessions.keys().cloned().collect()
}

fn session_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    dir.join("sessions").join(format!("{name}.toml"))
}

/// `default`, then `a`, then `b` (active); `a` was left most recently.
fn three_sessions(dir: &std::path::Path) -> WindowManager {
    let mut wm = headless(dir);
    mark(&mut wm, 1, "DEFAULT_PANE");
    wm.create_new_session(Some("a"), false).unwrap();
    mark(&mut wm, 1, "A_PANE");
    wm.create_new_session(Some("b"), false).unwrap();
    mark(&mut wm, 1, "B_PANE");
    wm
}

#[test]
fn killing_the_active_session_moves_to_the_most_recently_used_live_session() {
    isolated(|dir| {
        let mut wm = three_sessions(dir);
        assert_eq!(colon(&mut wm, ":session kill"), AppAction::Handled);

        assert_eq!(wm.config().history_session, "a", "`a` was left last, so it is the most recent");
        assert!(screen(&wm, 1).contains("A_PANE"));
        assert_eq!(live(&wm), vec!["default"], "`b` ended, `default` keeps running");
        assert!(wm.window_manage().background_sessions["default"].panes[&1]
            .vt()
            .snapshot()
            .contents()
            .contains("DEFAULT_PANE"));
        let status = wm.active_pane().status_message().unwrap_or("").to_string();
        assert!(status.contains("Ended session 'b'") && status.contains("'a'"), "{status}");
        // Kill keeps the saved layout: the session can be restored later.
        assert!(session_file(dir, "b").is_file());
    });
}

#[test]
fn killing_the_active_session_falls_back_to_a_saved_session() {
    isolated(|dir| {
        let other = headless(dir);
        other.save_session("saved").unwrap();
        let mut wm = headless(dir);
        wm.config_mut().history_session = "work".to_string();

        let out = wm.end_session(None, false).unwrap();
        assert_eq!(
            out,
            SessionEndOutcome::Switched { ended: "work".into(), now_active: "saved".into() }
        );
        assert_eq!(wm.config().history_session, "saved");
        assert!(live(&wm).is_empty(), "the ended session must not linger in the background");
    });
}

#[test]
fn killing_the_last_session_quits_and_changes_nothing_else() {
    isolated(|dir| {
        let mut wm = headless(dir);
        mark(&mut wm, 1, "ONLY_PANE");
        assert_eq!(colon(&mut wm, ":session kill"), AppAction::Quit);
        assert_eq!(wm.config().history_session, "default");
        assert!(screen(&wm, 1).contains("ONLY_PANE"));
    });
}

#[test]
fn killing_a_background_session_ends_only_that_session() {
    isolated(|dir| {
        let mut wm = three_sessions(dir);
        assert_eq!(colon(&mut wm, ":session kill a"), AppAction::Handled);

        assert_eq!(wm.config().history_session, "b");
        assert!(screen(&wm, 1).contains("B_PANE"), "the active session must be untouched");
        assert_eq!(live(&wm), vec!["default"]);
        assert!(session_file(dir, "a").is_file(), "kill keeps the saved layout");

        // A session that is not running cannot be killed; delete removes its saved layout.
        assert_eq!(colon(&mut wm, ":session kill a"), AppAction::Handled);
        assert!(wm.active_pane().status_message().unwrap_or("").contains("not running"));
        colon(&mut wm, ":session delete a");
        assert!(!session_file(dir, "a").exists());
    });
}

#[test]
fn deleting_the_active_session_moves_on_and_removes_its_saved_layout() {
    isolated(|dir| {
        let mut wm = three_sessions(dir);
        assert!(session_file(dir, "b").is_file());
        assert_eq!(colon(&mut wm, ":session delete b"), AppAction::Handled);

        assert_eq!(wm.config().history_session, "a");
        assert!(!session_file(dir, "b").exists());
        assert!(!wm.list_sessions().contains(&"b".to_string()));
        assert_eq!(live(&wm), vec!["default"]);
    });
}

#[test]
fn exit_guard_kill_session_keeps_bilterm_running_while_another_session_exists() {
    isolated(|dir| {
        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE");
        wm.create_new_session(Some("b"), false).unwrap();
        wm.open_exit_guard("test");

        assert!(!wm.apply_exit_guard_choice(ExitGuardChoice::KillSession), "kill session quit bilterm");
        assert_eq!(wm.config().history_session, "default");
        assert!(screen(&wm, 1).contains("DEFAULT_PANE"));
        assert!(live(&wm).is_empty());

        // Now `default` is the last session: killing it quits, as before.
        assert!(wm.apply_exit_guard_choice(ExitGuardChoice::KillSession));
        // Quit client is unchanged.
        assert!(wm.apply_exit_guard_choice(ExitGuardChoice::QuitClient));
    });
}

#[test]
fn last_window_exit_falls_back_to_a_saved_session_when_none_is_live() {
    isolated(|dir| {
        let other = headless(dir);
        other.save_session("saved").unwrap();
        let mut wm = headless(dir);
        wm.config_mut().history_session = "work".to_string();

        wm.pane_mut(1).unwrap().ingest_pty_events(vec![PtyEvent::Exited(Some(0))]);
        wm.poll_all_ptys();
        assert!(!wm.all_panes_exited(), "the app would quit although a saved session exists");
        assert_eq!(wm.config().history_session, "saved");
    });
}

#[test]
fn last_window_exit_prefers_the_most_recently_used_live_session() {
    isolated(|dir| {
        let mut wm = three_sessions(dir);
        wm.pane_mut(1).unwrap().ingest_pty_events(vec![PtyEvent::Exited(Some(0))]);
        wm.poll_all_ptys();
        assert_eq!(wm.config().history_session, "a");
        assert_eq!(live(&wm), vec!["default"]);
    });
}

#[test]
fn last_window_exit_of_the_last_session_still_quits() {
    isolated(|dir| {
        let mut wm = headless(dir);
        wm.pane_mut(1).unwrap().ingest_pty_events(vec![PtyEvent::Exited(Some(0))]);
        wm.poll_all_ptys();
        assert!(wm.all_panes_exited(), "with no other session the app quits as before");
    });
}

#[test]
fn closing_the_last_window_ends_the_session_only_when_another_exists() {
    isolated(|dir| {
        let mut wm = headless(dir);
        mark(&mut wm, 1, "DEFAULT_PANE");
        // Only session: `:close` on the last window changes nothing (`:quit` leaves).
        assert_eq!(colon(&mut wm, ":close"), AppAction::Handled);
        assert!(screen(&wm, 1).contains("DEFAULT_PANE"));

        wm.create_new_session(Some("b"), false).unwrap();
        assert_eq!(colon(&mut wm, ":close"), AppAction::Handled);
        assert_eq!(wm.config().history_session, "default");
        assert!(screen(&wm, 1).contains("DEFAULT_PANE"));
        assert!(live(&wm).is_empty());
    });
}

#[test]
fn colon_forms_round_trip() {
    for cmd in [":session kill", ":session kill work", ":session delete work"] {
        let action = bilterm::colon_command::parse_colon_command(cmd).unwrap().unwrap();
        assert_eq!(action.colon_form_or_summary(), cmd);
    }
    assert!(matches!(
        bilterm::colon_command::parse_colon_command(":session end").unwrap().unwrap(),
        Action::SessionEnd { name: None, delete: false }
    ));
    assert!(bilterm::colon_command::parse_colon_command(":session delete").unwrap().is_err());
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

    fn alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Killing the active session ends its shell and nothing else: the background session's
    /// shell keeps running and becomes the active one.
    #[test]
    fn killing_the_active_session_keeps_the_background_shell_alive() {
        isolated(|dir| {
            let mut wm = real(dir);
            let background_pid = shell_pid(&wm, 1);
            wm.create_new_session(Some("doomed"), false).unwrap();
            let doomed_pid = shell_pid(&wm, 1);
            assert_ne!(background_pid, doomed_pid);

            assert_eq!(colon(&mut wm, ":session kill"), AppAction::Handled);
            assert_eq!(wm.config().history_session, "default");
            assert_eq!(shell_pid(&wm, 1), background_pid, "the background shell must become active");

            // The reaper kills a dropped pane's child within 250 ms.
            assert!(
                common::wait_until(std::time::Duration::from_secs(5), || !alive(doomed_pid)),
                "the killed session's shell {doomed_pid} is still running"
            );
            std::thread::sleep(std::time::Duration::from_millis(500));
            assert!(alive(background_pid), "background shell {background_pid} was killed");
        });
    }
}
