//! `Ctrl+Tab` / `Ctrl+Shift+Tab` move between the windows of the session (deck order), and
//! `Ctrl+Alt+Tab` / `Ctrl+Alt+Shift+Tab` (`:focus all [prev]`) walk every pane and every open side
//! window; bare `Tab` still belongs to the pane.

use std::sync::Mutex;

use bilterm::action::Action;
use bilterm::colon_command::parse_colon_command;
use bilterm::input::handle_wm_key_event;
use bilterm::wm::keyboard_focus::{FocusOp, KeyboardOwner};
use bilterm::wm::{SplitDirection, WindowManager};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

static ENV_LOCK: Mutex<()> = Mutex::new(());
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const CTRL_SHIFT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::SHIFT);
const CTRL_ALT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);
const CTRL_ALT_SHIFT: KeyModifiers = CTRL_ALT.union(KeyModifiers::SHIFT);
const AREA: Rect = Rect { x: 0, y: 0, width: 160, height: 44 };

fn with_temp_xdg<T>(f: impl FnOnce() -> T) -> T {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("bilterm-tab-cycle-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Serialized by ENV_LOCK.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("XDG_STATE_HOME", &dir);
        std::env::set_var("XDG_DATA_HOME", &dir);
    }
    let out = f();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn fixture(panes: usize) -> WindowManager {
    let mut wm = WindowManager::new_headless(AREA.height, AREA.width);
    for _ in 1..panes {
        wm.split_focused(SplitDirection::Vertical).unwrap();
    }
    wm.resize_all(AREA).unwrap();
    wm
}

fn colon(wm: &mut WindowManager, line: &str) {
    let action = parse_colon_command(line).unwrap_or_else(|| panic!("{line} parses")).unwrap_or_else(|e| panic!("{line}: {e}"));
    wm.apply(action).unwrap();
}

fn press(wm: &mut WindowManager, code: KeyCode, mods: KeyModifiers) {
    handle_wm_key_event(wm, KeyEvent::new(code, mods)).unwrap();
}

fn walk(wm: &mut WindowManager, n: usize, code: KeyCode, mods: KeyModifiers) -> Vec<(usize, KeyboardOwner)> {
    (0..n)
        .map(|_| {
            press(wm, code, mods);
            (wm.active_pane_id(), wm.keyboard_owner())
        })
        .collect()
}

#[test]
fn ctrl_tab_visits_every_window_once_in_a_stable_order_and_wraps() {
    with_temp_xdg(|| {
        let mut wm = fixture(3);
        let start = wm.active_pane_id();
        let fwd = walk(&mut wm, 3, KeyCode::Tab, CTRL);
        let ids: Vec<usize> = fwd.iter().map(|(id, _)| *id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "each of the three windows is visited once: {ids:?}");
        assert_eq!(ids[2], start, "the third press wraps to the window the walk began on");
        assert!(fwd.iter().all(|(_, o)| o.is_pane()));
        // The order is the same on a second lap.
        assert_eq!(walk(&mut wm, 3, KeyCode::Tab, CTRL).iter().map(|(i, _)| *i).collect::<Vec<_>>(), ids);
    });
}

#[test]
fn ctrl_shift_tab_walks_backwards_in_every_encoding() {
    with_temp_xdg(|| {
        for (code, mods) in [(KeyCode::BackTab, CTRL_SHIFT), (KeyCode::BackTab, CTRL), (KeyCode::Tab, CTRL_SHIFT)] {
            let mut wm = fixture(3);
            let start = wm.active_pane_id();
            press(&mut wm, KeyCode::Tab, CTRL);
            assert_ne!(wm.active_pane_id(), start);
            press(&mut wm, code, mods);
            assert_eq!(wm.active_pane_id(), start, "{code:?}+{mods:?} undoes Ctrl+Tab");
        }
    });
}

#[test]
fn ctrl_tab_covers_stacked_floating_windows_too() {
    with_temp_xdg(|| {
        let mut wm = fixture(3);
        colon(&mut wm, ":layout stack");
        let ids: Vec<usize> = walk(&mut wm, 3, KeyCode::Tab, CTRL).iter().map(|(i, _)| *i).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "{ids:?}");
    });
}

#[test]
fn ctrl_tab_from_a_side_window_returns_the_keyboard_to_the_next_pane() {
    with_temp_xdg(|| {
        let mut wm = fixture(2);
        colon(&mut wm, ":agents list open");
        colon(&mut wm, ":focus agents-list");
        assert_eq!(wm.keyboard_owner(), KeyboardOwner::AgentsList);
        let before = wm.active_pane_id();
        press(&mut wm, KeyCode::Tab, CTRL);
        assert!(wm.keyboard_owner().is_pane());
        assert_ne!(wm.active_pane_id(), before);
        assert!(wm.agents_list().active, "the list stays open");
    });
}

#[test]
fn ctrl_alt_tab_walks_panes_then_open_side_windows_and_wraps() {
    with_temp_xdg(|| {
        let mut wm = fixture(2);
        colon(&mut wm, ":focus pane");
        colon(&mut wm, ":agents list open");
        colon(&mut wm, ":traffic open");
        colon(&mut wm, ":drawer focus");
        colon(&mut wm, ":focus pane");
        let ring = wm.keyboard_focus_ring();
        assert!(ring.len() >= 3, "{ring:?}");
        // Normalise to the lowest pane so the lap is predictable.
        let first = *wm.panes().keys().next().unwrap();
        wm.focus_and_raise(first);
        let laps = walk(&mut wm, 2 + ring.len(), KeyCode::Tab, CTRL_ALT);
        let second = *wm.panes().keys().nth(1).unwrap();
        assert_eq!(laps[0], (second, KeyboardOwner::Pane));
        let windows: Vec<KeyboardOwner> = laps[1..laps.len() - 1].iter().map(|(_, o)| *o).collect();
        assert_eq!(windows, ring, "side windows follow in ring order");
        assert_eq!(laps[laps.len() - 1], (first, KeyboardOwner::Pane), "wraps to the first pane");
        // Backwards from the first pane lands on the last side window.
        press(&mut wm, KeyCode::BackTab, CTRL_ALT_SHIFT);
        assert_eq!(wm.keyboard_owner(), *ring.last().unwrap());
        press(&mut wm, KeyCode::Tab, CTRL_ALT_SHIFT);
        assert_eq!(wm.keyboard_owner(), ring[ring.len() - 2]);
    });
}

#[test]
fn closed_surfaces_are_skipped() {
    with_temp_xdg(|| {
        let mut wm = fixture(1);
        colon(&mut wm, ":agents list open");
        colon(&mut wm, ":traffic open");
        colon(&mut wm, ":focus pane");
        let ring = wm.keyboard_focus_ring();
        assert_eq!(ring.len(), 2, "{ring:?}");
        colon(&mut wm, ":traffic close");
        assert_eq!(wm.keyboard_focus_ring(), vec![KeyboardOwner::AgentsList]);
        let seen: Vec<KeyboardOwner> = walk(&mut wm, 3, KeyCode::Tab, CTRL_ALT).into_iter().map(|(_, o)| o).collect();
        assert_eq!(seen, vec![KeyboardOwner::AgentsList, KeyboardOwner::Pane, KeyboardOwner::AgentsList]);
    });
}

#[test]
fn a_lone_pane_with_nothing_open_has_nothing_to_cycle() {
    with_temp_xdg(|| {
        let mut wm = fixture(1);
        let msg = wm.keyboard_focus_apply(FocusOp::NextAll);
        assert_eq!(msg, "Nothing else is open");
        assert!(wm.keyboard_owner().is_pane());
    });
}

#[test]
fn bare_tab_and_shift_tab_stay_with_the_pane() {
    with_temp_xdg(|| {
        let mut wm = fixture(3);
        colon(&mut wm, ":agents list open");
        colon(&mut wm, ":focus pane");
        let km = wm.keymap();
        for (code, mods) in [(KeyCode::Tab, KeyModifiers::NONE), (KeyCode::BackTab, KeyModifiers::SHIFT), (KeyCode::BackTab, KeyModifiers::NONE)] {
            assert_eq!(km.global_action(KeyEvent::new(code, mods), false), None, "{code:?}+{mods:?} is not a global chord");
        }
        let before = wm.active_pane_id();
        press(&mut wm, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut wm, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(wm.active_pane_id(), before, "bare Tab does not move between windows");
        assert!(wm.keyboard_owner().is_pane(), "bare Tab does not hand the keyboard to a side window");
    });
}

#[test]
fn chords_lower_to_actions_with_colon_twins_and_a_leader_fallback() {
    with_temp_xdg(|| {
        let wm = fixture(1);
        let km = wm.keymap();
        let g = |code, mods| km.global_action(KeyEvent::new(code, mods), false);
        assert_eq!(g(KeyCode::Tab, CTRL), Some(Action::DeckNext));
        assert_eq!(g(KeyCode::BackTab, CTRL_SHIFT), Some(Action::DeckPrev));
        assert_eq!(g(KeyCode::Tab, CTRL_ALT), Some(Action::KeyboardFocus(FocusOp::NextAll)));
        assert_eq!(g(KeyCode::BackTab, CTRL_ALT_SHIFT), Some(Action::KeyboardFocus(FocusOp::PrevAll)));
        assert_eq!(Action::KeyboardFocus(FocusOp::NextAll).colon_form_or_summary(), ":focus all");
        assert_eq!(Action::KeyboardFocus(FocusOp::PrevAll).colon_form_or_summary(), ":focus all prev");
        assert_eq!(Action::DeckNext.colon_form_or_summary(), ":deck next");

        // Any-terminal chords: `Ctrl+X Tab` (deck) and `Ctrl+Alt+X` / `Ctrl+Alt+Z` (focus all next / prev).
        let mut wm = fixture(2);
        colon(&mut wm, ":agents list open");
        colon(&mut wm, ":focus pane");
        let start = wm.active_pane_id();
        press(&mut wm, KeyCode::Char('x'), CTRL);
        press(&mut wm, KeyCode::Tab, KeyModifiers::NONE);
        assert_ne!(wm.active_pane_id(), start);
        press(&mut wm, KeyCode::Char('x'), CTRL_ALT);
        press(&mut wm, KeyCode::Char('x'), CTRL_ALT);
        assert_eq!(wm.keyboard_owner(), KeyboardOwner::AgentsList, "pane, pane, then the list");
        press(&mut wm, KeyCode::Char('z'), CTRL_ALT);
        assert!(wm.keyboard_owner().is_pane());
        assert_eq!(g(KeyCode::Char('x'), CTRL_ALT), Some(Action::KeyboardFocus(FocusOp::NextAll)));
        assert_eq!(g(KeyCode::Char('z'), CTRL_ALT), Some(Action::KeyboardFocus(FocusOp::PrevAll)));
        assert_eq!(g(KeyCode::Char('e'), CTRL_ALT), Some(Action::Editor(bilterm::action::EditorOp::Scratch { name: None })), "Ctrl+Alt+E is the scratchpad now, no longer a focus chord");
    });
}
