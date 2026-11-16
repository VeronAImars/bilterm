//! Arrow keys in the focused navigator drawer reach the window rows of every session, folded or not.
//!
//! Folded sessions list no window rows, and `Right` used to leave the drawer instead of opening one, so
//! the arrows could only step between session rows.
use std::sync::Mutex;

use bilterm::input::handle_wm_key_event;
use bilterm::ui::drawer::navigator::navigator_row_at;
use bilterm::ui::shell::ShellLayout;
use bilterm::wm::{DrawerRowKind, SplitDirection, WindowManager};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

static ENV_LOCK: Mutex<()> = Mutex::new(());
const AREA: Rect = Rect { x: 0, y: 0, width: 160, height: 44 };

fn isolated<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("XDG_DATA_HOME", dir.path());
    }
    f(dir.path())
}

fn fixture(dir: &std::path::Path) -> WindowManager {
    let mut wm = WindowManager::new_headless(AREA.height, AREA.width);
    wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
    wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.create_new_session(Some("other"), false).unwrap();
    wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.switch_or_create_session("default").unwrap();
    for line in [":set drawer.style navigator", ":drawer right", ":drawer fixed", ":drawer focus"] {
        let a = bilterm::colon_command::parse_colon_command(line).unwrap().unwrap();
        wm.apply(a).unwrap();
    }
    wm.resize_all(AREA).unwrap();
    wm
}

fn press(wm: &mut WindowManager, code: KeyCode) {
    handle_wm_key_event(wm, KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
}

fn label(wm: &WindowManager) -> String {
    let rows = wm.drawer_rows();
    let r = &rows[wm.drawer().selected_index];
    match &r.kind {
        DrawerRowKind::SessionHeader(n) => format!("S:{n}"),
        DrawerRowKind::Pane(id) => format!("P:{id}@{}", r.navigator.as_ref().and_then(|m| m.session_name.clone()).unwrap_or_default()),
        k => format!("{k:?}"),
    }
}

fn select(wm: &mut WindowManager, want: &str) {
    for _ in 0..wm.drawer_rows().len() {
        if label(wm) == want {
            return;
        }
        press(wm, KeyCode::Down);
    }
    panic!("row {want} not reachable with Down; stopped on {}", label(wm));
}

fn walk_down(wm: &mut WindowManager, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            press(wm, KeyCode::Down);
            label(wm)
        })
        .collect()
}

#[test]
fn down_walks_window_rows_of_active_and_background_sessions() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        let first = label(&wm);
        let mut seen = vec![first];
        seen.extend(walk_down(&mut wm, 5));
        for want in ["P:1@default", "P:2@default", "P:1@other", "P:2@other", "S:other", "S:default"] {
            assert!(seen.iter().any(|l| l == want), "{want} missing from {seen:?}");
        }
    });
}

#[test]
fn right_opens_a_folded_session_and_down_lands_on_its_first_window() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        assert_eq!(wm.drawer_rows().len(), 2, "all sessions folded");
        select(&mut wm, "S:other");
        press(&mut wm, KeyCode::Right);
        assert!(wm.drawer().focused, "Right on a session row must keep the keyboard in the drawer");
        assert_eq!(label(&wm), "S:other");
        assert_eq!(wm.drawer_rows().len(), 4, "other opened");
        assert_eq!(walk_down(&mut wm, 2), ["P:1@other", "P:2@other"]);
        press(&mut wm, KeyCode::Up);
        assert_eq!(label(&wm), "P:1@other");
    });
}

#[test]
fn right_on_an_open_session_steps_onto_its_first_window_and_left_climbs_back() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        select(&mut wm, "S:default");
        press(&mut wm, KeyCode::Right);
        assert_eq!(label(&wm), "P:1@default");
        press(&mut wm, KeyCode::Down);
        press(&mut wm, KeyCode::Left);
        assert_eq!(label(&wm), "S:default", "Left on a window selects its session");
        press(&mut wm, KeyCode::Left);
        assert_eq!(wm.drawer_rows().len(), 4, "Left on an open session folds it");
        assert!(wm.drawer().focused);
    });
}

#[test]
fn enter_on_a_background_session_window_switches_without_ending_programs() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        select(&mut wm, "S:other");
        press(&mut wm, KeyCode::Right);
        walk_down(&mut wm, 2);
        assert_eq!(label(&wm), "P:2@other");
        press(&mut wm, KeyCode::Enter);
        assert_eq!(wm.config().history_session, "other");
        assert_eq!(wm.active_pane_id(), 2);
        assert!(wm.window_manage().background_sessions.contains_key("default"), "outgoing session stays parked alive");
        assert_eq!(wm.window_manage().background_sessions["default"].panes.len(), 2);
    });
}

#[test]
fn selection_stays_on_screen_when_the_list_is_longer_than_the_drawer() {
    isolated(|dir| {
        let area = Rect { x: 0, y: 0, width: 100, height: 12 };
        let mut wm = WindowManager::new_headless(area.height, area.width);
        wm.config_mut().session_dir = Some(dir.join("sessions").to_string_lossy().into_owned());
        for _ in 0..12 {
            wm.split_focused(SplitDirection::Vertical).unwrap();
        }
        for line in [":set drawer.style navigator", ":drawer right", ":drawer fixed", ":drawer rows 1", ":drawer focus"] {
            wm.apply(bilterm::colon_command::parse_colon_command(line).unwrap().unwrap()).unwrap();
        }
        wm.resize_all(area).unwrap();
        let ws = ShellLayout::compute(area, wm.config(), wm.pane_count()).workspace;
        let dr = wm.drawer_rect(ws).expect("drawer on screen");
        let total = wm.drawer_rows().len();
        assert!(total > usize::from(dr.height), "fixture must overflow the drawer: {total} rows, drawer {dr:?}");
        for step in 0..(total + 2) {
            let sel = wm.drawer().selected_index;
            let visible = (dr.y..dr.bottom()).any(|y| (dr.x..dr.right()).any(|x| navigator_row_at(&wm, ws, x, y) == Some(sel)));
            assert!(visible, "step {step}: selected row {sel}/{total} is scrolled out of the drawer");
            press(&mut wm, KeyCode::Down);
        }
    });
}

fn rows_len(wm: &WindowManager) -> usize {
    wm.drawer_rows().len()
}

#[test]
fn enter_on_a_session_row_unfolds_it_and_does_not_switch() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        select(&mut wm, "S:other");
        let folded = rows_len(&wm);
        press(&mut wm, KeyCode::Enter);
        assert_eq!(rows_len(&wm), folded + 2, "other unfolded");
        assert_eq!(wm.config().history_session, "default", "a single Enter must not switch");
        assert!(wm.drawer().focused, "the drawer keeps the keyboard");
        assert_eq!(label(&wm), "S:other");
    });
}

#[test]
fn second_enter_within_the_window_switches() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        select(&mut wm, "S:other");
        press(&mut wm, KeyCode::Enter);
        wm.drawer_mut().advance_clock(bilterm::wm::drawer::ENTER_DOUBLE_TAP_WINDOW / 2);
        press(&mut wm, KeyCode::Enter);
        assert_eq!(wm.config().history_session, "other", "double-tap switches");
    });
}

#[test]
fn second_enter_after_the_window_only_toggles_again() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        select(&mut wm, "S:other");
        let folded = rows_len(&wm);
        press(&mut wm, KeyCode::Enter);
        wm.drawer_mut().advance_clock(bilterm::wm::drawer::ENTER_DOUBLE_TAP_WINDOW * 2);
        press(&mut wm, KeyCode::Enter);
        assert_eq!(wm.config().history_session, "default", "slow second Enter must not switch");
        assert_eq!(rows_len(&wm), folded, "folded again");
    });
}

#[test]
fn enter_on_another_session_row_does_not_pair_with_the_previous_tap() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        select(&mut wm, "S:other");
        press(&mut wm, KeyCode::Enter);
        select(&mut wm, "S:default");
        press(&mut wm, KeyCode::Enter);
        assert_eq!(wm.config().history_session, "default");
    });
}

#[test]
fn colon_enter_matches_the_key_and_activate_switches_at_once() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        select(&mut wm, "S:other");
        let run = |wm: &mut WindowManager, l: &str| wm.apply(bilterm::colon_command::parse_colon_command(l).unwrap().unwrap()).unwrap();
        let folded = rows_len(&wm);
        run(&mut wm, ":drawer enter");
        assert_eq!(rows_len(&wm), folded + 2);
        assert_eq!(wm.config().history_session, "default");
        run(&mut wm, ":drawer activate");
        assert_eq!(wm.config().history_session, "other");
    });
}

#[test]
fn enter_on_a_window_row_still_switches_at_once() {
    isolated(|dir| {
        let mut wm = fixture(dir);
        select(&mut wm, "P:2@default");
        press(&mut wm, KeyCode::Enter);
        assert_eq!(wm.active_pane_id(), 2);
        assert!(!wm.drawer().focused, "window Enter hands the keyboard to the pane");
    });
}

#[test]
fn double_click_on_a_session_row_switches() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    isolated(|dir| {
        let mut wm = fixture(dir);
        press(&mut wm, KeyCode::Char('C'));
        let ws = ShellLayout::compute(AREA, wm.config(), wm.pane_count()).workspace;
        let rows = wm.drawer_rows();
        let idx = rows.iter().position(|r| r.kind == DrawerRowKind::SessionHeader("other".into())).unwrap();
        let dr = wm.drawer_rect(ws).unwrap();
        let (x, y) = (dr.x..dr.right())
            .flat_map(|x| (dr.y..dr.bottom()).map(move |y| (x, y)))
            .find(|&(x, y)| navigator_row_at(&wm, ws, x, y) == Some(idx))
            .expect("row on screen");
        for _ in 0..2 {
            let ev = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
            bilterm::input::handle_wm_mouse_event(&mut wm, ev, AREA).unwrap();
        }
        assert_eq!(wm.config().history_session, "other");
    });
}
