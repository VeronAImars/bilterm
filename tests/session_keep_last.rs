//! `session_on_empty = keep-last` (interactive default): the last window of the ONLY session
//! exiting keeps bilterm alive with an empty session; explicit quit/kill and `end` still quit.

mod common;

use std::sync::Mutex;

use bilterm::config::SessionOnEmpty;
use bilterm::input::AppAction;
use bilterm::pty::PtyEvent;
use bilterm::wm::WindowManager;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn isolated<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // Serialized by ENV_LOCK.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        std::env::set_var("XDG_STATE_HOME", dir.path());
    }
    f(dir.path())
}

fn wm_with(dir: &std::path::Path, mode: SessionOnEmpty) -> WindowManager {
    let mut wm = WindowManager::new_headless(24, 80);
    wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
    wm.config_mut().session_on_empty = mode;
    wm
}

fn colon(wm: &mut WindowManager, cmd: &str) -> AppAction {
    let action = bilterm::colon_command::parse_colon_command(cmd).unwrap().unwrap();
    wm.apply(action).unwrap()
}

fn exit_pane(wm: &mut WindowManager, id: usize) {
    wm.pane_mut(id).unwrap().ingest_pty_events(vec![PtyEvent::Exited(Some(0))]);
    wm.poll_all_ptys();
}

#[test]
fn defaults_keep_last_interactive_end_headless() {
    assert_eq!(bilterm::config::BiltermConfig::default().session_on_empty, SessionOnEmpty::KeepLast);
    assert_eq!(SessionOnEmpty::from_name("keep-last"), Some(SessionOnEmpty::KeepLast));
    // Headless workspaces (daemons, one-shot runs, most tests) keep quitting.
    assert_eq!(WindowManager::new_headless(24, 80).config().session_on_empty, SessionOnEmpty::End);
}

#[test]
fn last_pane_exit_of_only_session_keeps_app_alive_and_empty() {
    isolated(|dir| {
        let mut wm = wm_with(dir, SessionOnEmpty::KeepLast);
        exit_pane(&mut wm, 1);
        assert!(!wm.should_quit());
        assert!(wm.is_session_empty());
        assert_eq!(wm.window_count(), 0);
    });
}

#[test]
fn end_policy_still_quits_on_last_pane_exit() {
    isolated(|dir| {
        let mut wm = wm_with(dir, SessionOnEmpty::End);
        exit_pane(&mut wm, 1);
        assert!(wm.should_quit());
    });
}

#[test]
fn explicit_quit_and_session_kill_of_last_session_still_quit() {
    isolated(|dir| {
        let mut wm = wm_with(dir, SessionOnEmpty::KeepLast);
        assert!(matches!(colon(&mut wm, ":quit!"), AppAction::Quit));
        let mut wm = wm_with(dir, SessionOnEmpty::KeepLast);
        assert!(matches!(colon(&mut wm, ":session kill"), AppAction::Quit));
    });
}

#[test]
fn close_last_window_keeps_empty_and_refill_works() {
    isolated(|dir| {
        let mut wm = wm_with(dir, SessionOnEmpty::KeepLast);
        colon(&mut wm, ":close");
        assert!(wm.is_session_empty());
        assert!(!wm.should_quit());
        colon(&mut wm, ":split");
        assert!(!wm.is_session_empty());
        assert_eq!(wm.window_count(), 1);
    });
}

#[test]
fn keep_last_with_other_session_ends_and_moves_on() {
    isolated(|dir| {
        let mut wm = wm_with(dir, SessionOnEmpty::KeepLast);
        colon(&mut wm, ":session new other");
        assert_eq!(wm.config().history_session, "other");
        let id = wm.window_panes().next().map(|(i, _)| *i).unwrap();
        exit_pane(&mut wm, id);
        assert_eq!(wm.config().history_session, "default");
        assert!(!wm.is_session_empty());
        assert!(!wm.should_quit());
    });
}

#[test]
fn on_empty_command_round_trips_keep_last() {
    isolated(|dir| {
        let mut wm = wm_with(dir, SessionOnEmpty::End);
        colon(&mut wm, ":session on-empty keep-last");
        assert_eq!(wm.config().session_on_empty, SessionOnEmpty::KeepLast);
    });
}
