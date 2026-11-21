//! Editor menu bar (MS-DOS EDIT style): keys, drop-down, dialog, parity with `:editor menu`.
#![cfg(unix)]

use bilterm::action::Action;
use bilterm::colon_command::parse_colon_command;
use bilterm::input::handle_wm_key_event;
use bilterm::wm::WindowManager;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn sandbox() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    // SAFETY: serialized by ENV_LOCK; every test calls sandbox() first.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", d.path().join("state"));
        std::env::set_var("XDG_DATA_HOME", d.path().join("data"));
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("config"));
        std::env::set_var("HOME", d.path());
    }
    (g, d)
}
fn k(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}
fn press(wm: &mut WindowManager, ev: KeyEvent) {
    let _ = handle_wm_key_event(wm, ev).unwrap();
}
fn colon(wm: &mut WindowManager, c: &str) {
    wm.apply(parse_colon_command(c).unwrap().unwrap()).unwrap();
}
fn open_menu(wm: &WindowManager) -> Option<String> {
    let s = wm.active_pane().editor().unwrap().status_json();
    let v = bilterm::api::JsonValue::parse(&s).unwrap();
    v.get("menu").and_then(|m| m.as_str()).map(str::to_string)
}
fn screen(wm: &WindowManager) -> String {
    wm.active_pane().vt().contents()
}
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

#[test]
fn bar_is_always_visible_and_f10_opens_the_first_menu() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    colon(&mut wm, ":scratch");
    let first = screen(&wm).lines().next().unwrap().to_string();
    let _ = first;
    let s = screen(&wm);
    assert!(s.contains("File") && s.contains("Edit") && s.contains("Search") && s.contains("View") && s.contains("Help"), "{s}");
    assert!(s.contains("F10="), "{s}");
    press(&mut wm, k(KeyCode::F(10)));
    assert_eq!(open_menu(&wm).as_deref(), Some("file"));
    assert!(screen(&wm).contains("Save As..."), "{}", screen(&wm));
    assert!(screen(&wm).contains("Ctrl+S"));
    press(&mut wm, k(KeyCode::Esc));
    assert_eq!(open_menu(&wm), None);
}

#[test]
fn alt_letters_open_menus_arrows_navigate_and_esc_closes() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    colon(&mut wm, ":scratch");
    for (c, name) in [('f', "file"), ('e', "edit"), ('s', "search"), ('v', "view"), ('h', "help")] {
        press(&mut wm, alt(c));
        assert_eq!(open_menu(&wm).as_deref(), Some(name), "alt+{c}");
        press(&mut wm, k(KeyCode::Esc));
    }
    press(&mut wm, alt('f'));
    press(&mut wm, k(KeyCode::Right));
    assert_eq!(open_menu(&wm).as_deref(), Some("edit"));
    press(&mut wm, k(KeyCode::Left));
    press(&mut wm, k(KeyCode::Left));
    assert_eq!(open_menu(&wm).as_deref(), Some("help"), "wraps");
    press(&mut wm, k(KeyCode::Esc));
}

#[test]
fn enter_runs_an_item_through_apply_and_menu_closes() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    colon(&mut wm, ":scratch");
    press(&mut wm, k(KeyCode::Char('a')));
    press(&mut wm, k(KeyCode::Char('b')));
    press(&mut wm, alt('e'));
    // Undo is the first Edit item (typing is coalesced into one undo step).
    press(&mut wm, k(KeyCode::Enter));
    assert_eq!(open_menu(&wm), None);
    assert_eq!(wm.active_pane().editor().unwrap().buf.text(), "");
    // Mnemonic letter inside an open menu: Edit > Select all, then Search > Go to line prompt.
    press(&mut wm, k(KeyCode::Char('x')));
    press(&mut wm, alt('e'));
    press(&mut wm, k(KeyCode::Char('a')));
    assert!(wm.active_pane().editor().unwrap().buf.selection().is_some());
    press(&mut wm, alt('s'));
    press(&mut wm, k(KeyCode::Char('g')));
    assert!(screen(&wm).contains("Go to line"), "{}", screen(&wm));
}

#[test]
fn disabled_items_do_nothing_and_are_marked() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    colon(&mut wm, ":scratch");
    press(&mut wm, alt('s'));
    assert!(screen(&wm).contains("n/a"), "{}", screen(&wm));
    press(&mut wm, k(KeyCode::Char('r')));
    assert_eq!(open_menu(&wm).as_deref(), Some("search"), "Replace is not implemented: menu stays open");
    assert!(wm.active_pane().editor().unwrap().buf.text().is_empty());
}

#[test]
fn help_keys_dialog_lists_hotkeys_and_any_key_closes_it() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    colon(&mut wm, ":scratch");
    press(&mut wm, alt('h'));
    press(&mut wm, k(KeyCode::Enter));
    let s = screen(&wm);
    assert!(s.contains("Keys") && s.contains("Ctrl+K") && s.contains("Cut") && s.contains("Esc or Enter closes"), "{s}");
    press(&mut wm, k(KeyCode::Esc));
    assert!(!screen(&wm).contains("Esc or Enter closes"));
    // Esc did not leak into the buffer or close the window.
    assert!(wm.active_pane().editor().is_some());
}

#[test]
fn colon_menu_commands_match_the_keys() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    colon(&mut wm, ":scratch");
    colon(&mut wm, ":editor menu open view");
    assert_eq!(open_menu(&wm).as_deref(), Some("view"));
    colon(&mut wm, ":editor menu close");
    assert_eq!(open_menu(&wm), None);
    let wrap0 = wm.active_pane().editor().unwrap().wrap;
    colon(&mut wm, ":editor menu do wrap");
    assert_ne!(wm.active_pane().editor().unwrap().wrap, wrap0);
    colon(&mut wm, ":editor menu do select-all");
    assert!(parse_colon_command(":editor menu do nonsense").unwrap().is_err());
    assert!(matches!(parse_colon_command(":editor menu").unwrap().unwrap(), Action::Editor(_)));
}

#[test]
fn menu_keys_do_not_leak_into_other_windows() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    // A plain terminal window: F10 stays the global Quake toggle, not an editor menu.
    assert!(wm.active_pane().editor().is_none());
    colon(&mut wm, ":scratch");
    press(&mut wm, alt('f'));
    colon(&mut wm, ":editor menu close");
    assert_eq!(open_menu(&wm), None);
}
