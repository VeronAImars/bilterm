//! Remote-origin badge: RPC/MCP/CLI/colon parity and rendering (title, ascii pack).
#![cfg(unix)]

use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::cli::{CliEnv, parse};
use bilterm::colon_command::parse_colon_command;
use bilterm::ui::WorkspaceWidget;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

fn js(s: &str) -> JsonValue {
    JsonValue::String(s.to_string())
}

fn obj(entries: &[(&str, JsonValue)]) -> JsonValue {
    JsonValue::obj(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Vec<_>>())
}

fn plane() -> AutomationControlPlane {
    let mut cp = AutomationControlPlane::new_headless(24, 80);
    cp.dispatch_method("pane.split", &obj(&[("direction", js("v"))])).unwrap();
    cp.wm.resize_all(Rect::new(0, 0, 80, 24)).unwrap();
    cp
}

fn screen(cp: &AutomationControlPlane) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    WorkspaceWidget::new(&cp.wm).render(area, &mut buf);
    (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

fn remote_of(cp: &mut AutomationControlPlane, id: i64) -> String {
    let r = cp.dispatch_method("pane.list", &obj(&[])).unwrap();
    let JsonValue::Array(rows) = r.get("panes").unwrap().clone() else { panic!() };
    let row = rows.iter().find(|p| p.get("id").and_then(JsonValue::as_i64) == Some(id)).unwrap();
    row.get("remote").unwrap().to_json_string()
}

#[test]
fn list_status_and_rpc_agree() {
    let mut cp = plane();
    assert!(remote_of(&mut cp, 1).contains("\"remote\":false"));
    cp.wm.pane_mut(1).unwrap().set_remote_target(Some("root@web1".into()));
    let one = remote_of(&mut cp, 1);
    assert!(one.contains("\"kind\":\"ssh\"") && one.contains("[R:web1]"), "{one}");
    let r = cp.dispatch_method("remote.status", &obj(&[])).unwrap();
    assert_eq!(r.get("remote_panes").and_then(JsonValue::as_i64), Some(1));
    let r = cp.dispatch_method("wm.command", &obj(&[("command", js(":remote status 1"))])).unwrap();
    assert!(r.to_json_string().contains("web1"), "{}", r.to_json_string());
    let ws = cp.dispatch_method("workspace.status", &obj(&[])).unwrap();
    assert_eq!(ws.get("remote_panes").and_then(JsonValue::as_i64), Some(1));
}

#[test]
fn cli_and_mcp_map_to_remote_status() {
    let env = CliEnv::default();
    let c = parse(&["pane".to_string(), "remote".to_string(), "p2".to_string()], &env).unwrap();
    assert_eq!(c.method, "remote.status");
    assert_eq!(AutomationControlPlane::agent_control_method(&obj(&[("action", js("remote_status"))])), Some("remote.status"));
    assert!(parse_colon_command(":remote status").is_some());
}

#[test]
fn title_shows_text_tag_in_unicode_and_ascii() {
    let mut cp = plane();
    assert!(!screen(&cp).contains("[R:"));
    cp.wm.pane_mut(1).unwrap().set_remote_target(Some("web1".into()));
    assert!(screen(&cp).contains("[R:web1]"), "{}", screen(&cp));
    cp.wm.config_mut().glyphs = bilterm::ui::glyphs::GlyphPack::Ascii;
    let s = screen(&cp);
    assert!(s.contains("[R:web1]") && !s.contains('\u{21C4}'));
}
