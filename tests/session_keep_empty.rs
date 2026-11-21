//! `:session on-empty keep`: a session whose last window exits or is closed stays open, empty.
//! Pins both modes (`end` is the headless default, `keep-last` the interactive one), the empty canvas, switching to and from
//! an empty session, refilling it, the save/restore round trip, ending it explicitly, and that
//! bilterm does not quit while the only session is empty.

mod common;

use std::sync::Mutex;

use bilterm::action::Action;
use bilterm::config::SessionOnEmpty;
use bilterm::input::AppAction;
use bilterm::pty::PtyEvent;
use bilterm::ui::WorkspaceWidget;
use bilterm::wm::WindowManager;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

static ENV_LOCK: Mutex<()> = Mutex::new(());

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

fn headless(dir: &std::path::Path, mode: SessionOnEmpty) -> WindowManager {
    let mut wm = WindowManager::new_headless(24, 80);
    wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
    wm.config_mut().session_on_empty = mode;
    wm
}

fn colon(wm: &mut WindowManager, cmd: &str) -> AppAction {
    let action = bilterm::colon_command::parse_colon_command(cmd).unwrap().unwrap();
    wm.apply(action).unwrap()
}

fn mark(wm: &mut WindowManager, id: usize, text: &str) {
    wm.pane_mut(id).unwrap().process_pty_output(format!("{text}\r\n").as_bytes());
}

fn exit_pane(wm: &mut WindowManager, id: usize) {
    wm.pane_mut(id).unwrap().ingest_pty_events(vec![PtyEvent::Exited(Some(0))]);
    wm.poll_all_ptys();
}

fn rendered(wm: &WindowManager) -> String {
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    WorkspaceWidget::new(wm).render(area, &mut buf);
    (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn default_is_keep_last_and_the_setting_round_trips() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::End);
        assert_eq!(bilterm::config::BiltermConfig::default().session_on_empty, SessionOnEmpty::KeepLast);
        colon(&mut wm, ":session on-empty keep");
        assert_eq!(wm.config().session_on_empty, SessionOnEmpty::Keep);
        colon(&mut wm, ":session on-empty end");
        assert_eq!(wm.config().session_on_empty, SessionOnEmpty::End);
        colon(&mut wm, ":set session_on_empty keep");
        assert_eq!(wm.config().session_on_empty, SessionOnEmpty::Keep);
        let toml = toml::to_string(wm.config()).unwrap();
        assert!(toml.contains("session_on_empty = \"keep\""), "{toml}");
        assert!(bilterm::colon_command::parse_colon_command(":session on-empty bogus").unwrap().is_err());
    });
}

#[test]
fn end_mode_still_ends_the_session_and_quits_on_the_last() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::End);
        exit_pane(&mut wm, 1);
        assert!(wm.all_panes_exited(), "end mode: the only session quits as before");
        assert!(!wm.is_session_empty());
    });
}

#[test]
fn keep_mode_last_window_exit_leaves_an_empty_session_and_does_not_quit() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        exit_pane(&mut wm, 1);
        assert!(!wm.all_panes_exited(), "the only session is empty, bilterm must keep running");
        assert!(wm.is_session_empty());
        assert_eq!(wm.window_count(), 0);
        assert_eq!(wm.config().history_session, "default");
    });
}

#[test]
fn keep_mode_close_leaves_an_empty_session_end_mode_does_not() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        colon(&mut wm, ":close");
        assert!(wm.is_session_empty());
        // Closing again is harmless and says what to do.
        colon(&mut wm, ":close");
        assert!(wm.is_session_empty());

        let mut wm = headless(dir, SessionOnEmpty::End);
        colon(&mut wm, ":close");
        assert!(!wm.is_session_empty());
        assert_eq!(wm.window_count(), 1, "end mode on the only session changes nothing");
    });
}

#[test]
fn empty_session_renders_a_hint_and_survives_layout_modes_and_zoom() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        colon(&mut wm, ":close");
        let text = rendered(&wm);
        assert!(text.contains("Session empty"), "hint missing:\n{text}");
        assert!(text.contains(":split"), "hint must name the way to open a window");
        for cmd in [":layout stacked", ":layout tiled", ":zoom", ":deck next", ":window outline dashed"] {
            if let Some(Ok(a)) = bilterm::colon_command::parse_colon_command(cmd) {
                let _ = wm.apply(a);
            }
            let _ = rendered(&wm);
        }
        assert!(wm.is_session_empty());
    });
}

#[test]
fn splitting_or_new_refills_an_empty_session_with_a_real_window() {
    isolated(|dir| {
        for cmd in [":new", ":split", ":vsplit"] {
            let mut wm = headless(dir, SessionOnEmpty::Keep);
            colon(&mut wm, ":close");
            colon(&mut wm, cmd);
            assert!(!wm.is_session_empty(), "{cmd}");
            assert_eq!(wm.window_count(), 1, "{cmd}: only the new window, no leftover placeholder");
            assert_eq!(wm.pane_count(), 1);
            assert!(!wm.active_pane().vt().snapshot().contents().contains("Session empty"));
            colon(&mut wm, ":split");
            assert_eq!(wm.window_count(), 2, "{cmd}: the refilled session splits normally");
        }
    });
}

#[test]
fn switching_to_and_from_an_empty_session_keeps_it_alive_and_listed() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        mark(&mut wm, 1, "WORK_PANE");
        wm.create_new_session(Some("scratch"), false).unwrap();
        colon(&mut wm, ":close");
        assert!(wm.is_session_empty());

        colon(&mut wm, ":session switch default");
        assert_eq!(wm.config().history_session, "default");
        assert!(!wm.is_session_empty());
        assert!(wm.active_pane().vt().snapshot().contents().contains("WORK_PANE"));
        assert!(wm.window_manage().background_sessions.contains_key("scratch"), "empty session stays parked");

        colon(&mut wm, ":session switch scratch");
        assert_eq!(wm.config().history_session, "scratch");
        assert!(wm.is_session_empty(), "still empty after the round trip");
        assert!(rendered(&wm).contains("Session empty"));
    });
}

#[test]
fn an_empty_session_is_not_a_window_in_drawer_or_status() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        colon(&mut wm, ":close");
        assert!(wm.ordered_pane_ids().is_empty());
        let rows = wm.drawer_rows();
        assert!(
            rows.iter().all(|r| !matches!(r.kind, bilterm::wm::DrawerRowKind::Pane(_))),
            "no window rows for an empty session"
        );
        assert!(wm.window_panes().next().is_none());
    });
}

#[test]
fn an_empty_session_saves_and_restores_empty() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        mark(&mut wm, 1, "WORK_PANE");
        colon(&mut wm, ":close");
        let snap = wm.snapshot_session("default");
        assert!(snap.empty_session);
        assert!(snap.panes.is_empty());
        let path = wm.save_session("default").unwrap();
        assert!(path.is_file());

        let mut wm2 = headless(dir, SessionOnEmpty::Keep);
        mark(&mut wm2, 1, "STALE");
        assert_eq!(wm2.restore_session("default").unwrap(), 1);
        assert!(wm2.is_session_empty());
        assert_eq!(wm2.window_count(), 0);
        assert!(rendered(&wm2).contains("Session empty"));
        colon(&mut wm2, ":split");
        assert_eq!(wm2.window_count(), 1);
        assert!(!wm2.is_session_empty());
    });
}

#[test]
fn killing_an_empty_session_moves_on_or_quits_on_the_last() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        mark(&mut wm, 1, "WORK_PANE");
        wm.create_new_session(Some("scratch"), false).unwrap();
        colon(&mut wm, ":close");
        assert_eq!(colon(&mut wm, ":session kill"), AppAction::Handled);
        assert_eq!(wm.config().history_session, "default");
        assert!(!wm.window_manage().background_sessions.contains_key("scratch"));

        // The remaining session, emptied and then killed, was the last one: quit.
        colon(&mut wm, ":close");
        assert!(wm.is_session_empty());
        assert_eq!(colon(&mut wm, ":session kill"), AppAction::Quit);
    });
}

#[test]
fn a_background_empty_session_can_be_killed_by_name() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        wm.create_new_session(Some("scratch"), false).unwrap();
        colon(&mut wm, ":close");
        colon(&mut wm, ":session switch default");
        colon(&mut wm, ":session kill scratch");
        assert!(!wm.window_manage().background_sessions.contains_key("scratch"));
    });
}

#[test]
fn rpc_style_status_reports_zero_windows_for_an_empty_session() {
    isolated(|dir| {
        let mut wm = headless(dir, SessionOnEmpty::Keep);
        colon(&mut wm, ":close");
        assert_eq!(wm.window_count(), 0);
        let a = Action::SessionOnEmpty(None);
        assert_eq!(wm.apply(a).unwrap(), AppAction::Handled);
    });
}
