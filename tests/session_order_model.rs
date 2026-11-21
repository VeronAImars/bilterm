//! M2: one authoritative session order plus per-session window order, shared by the drawer,
//! `:session list` and every mutation. No operation may reorder rows by itself.

use bilterm::colon_command::parse_colon_command;
use bilterm::wm::split_tree::SplitDirection;
use bilterm::wm::{DrawerRowKind, WindowManager};

fn isolate_xdg() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bilterm-order-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            std::env::set_var("XDG_STATE_HOME", dir.join("state"));
            std::env::set_var("XDG_DATA_HOME", dir.join("data"));
        }
    });
}

fn new_wm(dir: &tempfile::TempDir) -> WindowManager {
    isolate_xdg();
    let mut wm = WindowManager::new_headless(30, 100);
    let _ = wm.apply(parse_colon_command(":set drawer.style navigator").unwrap().unwrap());
    wm.config_mut().session_dir = Some(dir.path().to_str().unwrap().to_string());
    let _ = wm.apply(parse_colon_command(":drawer").unwrap().unwrap());
    wm
}

fn sessions(wm: &WindowManager) -> Vec<String> {
    wm.drawer_rows()
        .into_iter()
        .filter_map(|r| match r.kind {
            DrawerRowKind::SessionHeader(n) => Some(n),
            _ => None,
        })
        .collect()
}

fn panes_of(wm: &WindowManager, session: &str) -> Vec<usize> {
    wm.drawer_rows()
        .into_iter()
        .filter_map(|r| match (&r.kind, r.navigator.as_ref()) {
            (DrawerRowKind::Pane(id), Some(m)) if m.session_name.as_deref() == Some(session) => Some(*id),
            _ => None,
        })
        .collect()
}

fn cmd(wm: &mut WindowManager, line: &str) {
    let _ = wm.apply(parse_colon_command(line).unwrap().unwrap());
}

#[test]
fn new_sessions_append_and_ops_never_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let first = wm.config().history_session.clone();
    wm.create_new_session(Some("zeta"), false).unwrap();
    wm.create_new_session(Some("alpha"), false).unwrap();
    let base = sessions(&wm);
    assert_eq!(base, [first.as_str(), "zeta", "alpha"]);
    assert_eq!(wm.ordered_sessions_with_active(), base);

    // save (zeta -> disk), park/switch, restore, back and forth.
    wm.switch_to_session_preserving_panes("zeta").unwrap();
    assert_eq!(sessions(&wm), base, "switch");
    wm.save_and_activate_session(None).unwrap();
    assert_eq!(sessions(&wm), base, "save");
    wm.switch_to_session_preserving_panes("alpha").unwrap();
    wm.save_and_activate_session(None).unwrap();
    assert_eq!(sessions(&wm), base, "save alpha");
    wm.switch_to_session_preserving_panes(&first).unwrap();
    assert_eq!(sessions(&wm), base, "switch back");
    wm.restore_session("zeta").unwrap();
    assert_eq!(sessions(&wm), base, "restore");
    assert_eq!(wm.ordered_sessions_with_active(), base);
}

#[test]
fn rename_keeps_slot_and_tree_attach_keeps_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let first = wm.config().history_session.clone();
    wm.create_new_session(Some("zeta"), false).unwrap();
    wm.create_new_session(Some("alpha"), false).unwrap();
    cmd(&mut wm, ":session rename zeta omega");
    assert_eq!(sessions(&wm), [first.as_str(), "omega", "alpha"]);
    // Attaching alpha under its predecessor groups it but keeps the flat order.
    wm.daemon_state_mut().session_tree.branch("omega", "alpha").unwrap();
    assert_eq!(sessions(&wm), [first.as_str(), "omega", "alpha"]);
    // Attaching a later session under an earlier one never reorders the roots.
    wm.daemon_state_mut().session_tree.branch(&first, "omega").unwrap();
    let s = sessions(&wm);
    assert_eq!(s.iter().position(|n| n == &first), Some(0));
    assert_eq!(s.len(), 3);
}

#[test]
fn session_move_is_visible_in_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let first = wm.config().history_session.clone();
    wm.create_new_session(Some("zeta"), false).unwrap();
    wm.create_new_session(Some("alpha"), false).unwrap();
    wm.move_session_in_order(Some("alpha"), "top").unwrap();
    assert_eq!(sessions(&wm), ["alpha", first.as_str(), "zeta"]);
    cmd(&mut wm, ":session move zeta up");
    assert_eq!(sessions(&wm), ["alpha", "zeta", first.as_str()]);
    wm.reorder_sessions(&["zeta"]);
    assert_eq!(sessions(&wm)[0], "zeta");
    assert_eq!(wm.ordered_sessions_with_active(), sessions(&wm));
}

#[test]
fn live_and_parked_window_order_agree() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let first = wm.config().history_session.clone();
    let a = wm.active_pane_id();
    let b = wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.focus_and_raise(a);
    let c = wm.split_focused(SplitDirection::Horizontal).unwrap();
    let live = panes_of(&wm, &first);
    assert_eq!(live.len(), 3);
    // New window lands right after its source.
    let pos = |v: &[usize], id| v.iter().position(|x| *x == id).unwrap();
    assert_eq!(pos(&live, c), pos(&live, a) + 1);
    assert!(live.contains(&b));
    wm.create_new_session(Some("other"), false).unwrap();
    assert_eq!(panes_of(&wm, &first), live, "parked order equals live order");
    wm.switch_to_session_preserving_panes(&first).unwrap();
    assert_eq!(panes_of(&wm, &first), live, "restored from park keeps order");
}

#[test]
fn window_order_round_trips_through_saved_file_and_legacy_loads() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let a = wm.active_pane_id();
    let _b = wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.focus_and_raise(a);
    let _c = wm.split_focused(SplitDirection::Horizontal).unwrap();
    let live = wm.ordered_pane_ids();
    let snap = wm.snapshot_session("rt");
    assert_eq!(snap.window_order, live);
    let text = toml::to_string_pretty(&snap).unwrap();
    // Legacy file: no window_order key at all.
    let mut table: toml::Table = toml::from_str(&text).unwrap();
    assert!(table.remove("window_order").is_some(), "new files carry window_order");
    let legacy = toml::to_string_pretty(&table).unwrap();
    let parsed: bilterm::wm::SavedWorkspaceSession = toml::from_str(&legacy).unwrap();
    assert!(parsed.window_order.is_empty());
    std::fs::write(dir.path().join("legacy.toml"), legacy).unwrap();
    assert_eq!(wm.restore_session("legacy").unwrap(), 3);
    assert_eq!(wm.ordered_pane_ids().len(), 3);
}
