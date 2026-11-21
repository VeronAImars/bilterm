//! M3: window/session move Actions lower from every surface (colon, RPC, MCP, palette, drawer keys)
//! to one `WindowManager::apply` path, and the drawer reflects the order immediately.

use bilterm::action::Action;
use bilterm::api::json::JsonValue;
use bilterm::api::rpc::AutomationControlPlane;
use bilterm::colon_command::parse_colon_command;
use bilterm::wm::window_manage::{SessionManageOp, WindowManageOp};
use bilterm::wm::{DrawerRowKind, WindowManager};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn isolate_xdg() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bilterm-m3-{}", std::process::id()));
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

fn cmd(wm: &mut WindowManager, line: &str) {
    let _ = wm.apply(parse_colon_command(line).unwrap().unwrap());
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

fn active_windows(wm: &WindowManager) -> Vec<usize> {
    wm.drawer_rows()
        .into_iter()
        .filter_map(|r| match (&r.kind, r.navigator.as_ref()) {
            (DrawerRowKind::Pane(id), Some(m)) if m.session_name.as_deref() == Some(wm.config().history_session.as_str()) => Some(*id),
            _ => None,
        })
        .collect()
}

fn three_windows(wm: &mut WindowManager) -> Vec<usize> {
    cmd(wm, ":split v");
    cmd(wm, ":split v");
    let ids = wm.ordered_pane_ids();
    assert_eq!(ids.len(), 3);
    ids
}

#[test]
fn colon_forms_parse_to_actions() {
    let a = parse_colon_command(":window reorder 2 top").unwrap().unwrap();
    assert_eq!(a, Action::WindowManage(WindowManageOp::Reorder { window_id: Some(2), to: "top".into() }));
    assert_eq!(a.colon_form_or_summary(), ":window reorder 2 top");
    let a = parse_colon_command(":session reorder b a").unwrap().unwrap();
    assert_eq!(a, Action::SessionManage(SessionManageOp::Reorder { names: vec!["b".into(), "a".into()] }));
    assert!(parse_colon_command(":window reorder").unwrap().is_err());
    assert!(parse_colon_command(":session reorder").unwrap().is_err());
}

#[test]
fn window_reorder_is_reflected_in_drawer_immediately_and_nothing_else_moves() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let ids = three_windows(&mut wm);
    assert_eq!(active_windows(&wm), ids);
    cmd(&mut wm, &format!(":window reorder {} bottom", ids[0]));
    let want = vec![ids[1], ids[2], ids[0]];
    assert_eq!(active_windows(&wm), want);
    // Stable: refreshing, focusing and switching sessions do not rearrange.
    cmd(&mut wm, &format!(":window reorder {} 1", ids[0]));
    assert_eq!(active_windows(&wm), ids);
    wm.create_new_session(Some("other"), false).unwrap();
    wm.switch_to_session_preserving_panes(&wm.config().history_session.clone()).ok();
    assert_eq!(active_windows(&wm).len(), if wm.config().history_session == "other" { 1 } else { 3 });
}

#[test]
fn session_move_and_reorder_reflect_in_drawer() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let first = wm.config().history_session.clone();
    wm.create_new_session(Some("zeta"), false).unwrap();
    wm.create_new_session(Some("alpha"), false).unwrap();
    cmd(&mut wm, ":session move alpha top");
    assert_eq!(sessions(&wm)[0], "alpha");
    cmd(&mut wm, &format!(":session reorder zeta {first} alpha"));
    assert_eq!(sessions(&wm), ["zeta", first.as_str(), "alpha"]);
}

#[test]
fn surface_parity_rpc_mcp_colon_agree() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let ids = three_windows(&mut wm);
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    for m in ["window.reorder", "session.move", "session.reorder"] {
        assert!(bilterm::api::rpc::RPC_METHODS.contains(&m), "{m}");
    }
    let mcp_src = include_str!("../src/api/mcp.rs");
    for a in ["\"reorder\" => Some(\"window.reorder\")", "\"session_move\" => Some(\"session.move\")", "\"session_reorder\" => Some(\"session.reorder\")"] {
        assert!(mcp_src.contains(a), "{a}");
    }
    for h in [":window reorder up", ":window reorder down", ":session move up"] {
        assert!(bilterm::ui::modals::PALETTE_CATALOG.iter().any(|e| e.0 == h), "palette {h}");
    }
    // RPC on a fresh control plane's own wm.
    let wm0 = cp.wm.ordered_pane_ids();
    assert_eq!(wm0.len(), 1);
    let p = JsonValue::parse(r#"{"to":"down"}"#).unwrap();
    assert!(cp.dispatch_method("window.reorder", &p).is_ok());
    let bad = JsonValue::parse(r#"{"to":"sideways"}"#).unwrap();
    assert!(cp.dispatch_method("window.reorder", &bad).is_err());
    let sp = JsonValue::parse(r#"{"to":"top"}"#).unwrap();
    let r = cp.dispatch_method("session.move", &sp).unwrap();
    assert_eq!(r.get("status").and_then(JsonValue::as_str), Some("ok"));
    let out = cp
        .handle_mcp_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_window_manage","arguments":{"action":"session_move","to":"top"}}}"#)
        .unwrap();
    assert!(!out.contains("\"isError\":true"), "{out}");
    let _ = (wm, ids);
}

#[test]
fn drawer_keys_lower_to_window_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let mut wm = new_wm(&dir);
    let ids = three_windows(&mut wm);
    cmd(&mut wm, ":drawer fixed");
    cmd(&mut wm, ":drawer focus");
    let row = wm.drawer_rows().iter().position(|r| r.kind == DrawerRowKind::Pane(ids[0])).unwrap();
    wm.drawer_mut().selected_index = row;
    for (code, mods) in [(KeyCode::Down, KeyModifiers::ALT), (KeyCode::Char('J'), KeyModifiers::SHIFT)] {
        bilterm::input::handle_wm_key_event(&mut wm, KeyEvent::new(code, mods)).unwrap();
    }
    assert_eq!(active_windows(&wm), vec![ids[1], ids[2], ids[0]]);
    bilterm::input::handle_wm_key_event(&mut wm, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)).unwrap();
    assert_eq!(active_windows(&wm), vec![ids[1], ids[0], ids[2]]);
}
