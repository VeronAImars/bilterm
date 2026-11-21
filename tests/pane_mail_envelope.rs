//! Pane-mail envelope: RPC/MCP/CLI/colon parity, local typing exemption, and rendering.
#![cfg(unix)]

use std::time::Instant;

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

fn unread(cp: &mut AutomationControlPlane, sel: &str) -> i64 {
    let r = cp.dispatch_method("pane.mail_state", &obj(&[("selector", js(sel))])).unwrap();
    r.get("panes").and_then(JsonValue::as_array).unwrap()[0].get("unread").and_then(JsonValue::as_i64).unwrap()
}

fn screen(cp: &AutomationControlPlane) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    WorkspaceWidget::new(&cp.wm).render(area, &mut buf);
    (0..area.height).map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

#[test]
fn rpc_send_marks_pane_and_ack_clears() {
    let mut cp = plane();
    assert_eq!(unread(&mut cp, "p1"), 0);
    cp.dispatch_method("pane.send", &obj(&[("selector", js("p1")), ("text", js("hi"))])).unwrap();
    cp.dispatch_method("pane.send", &obj(&[("selector", js("p1")), ("text", js("again"))])).unwrap();
    assert_eq!(unread(&mut cp, "p1"), 2);
    let r = cp.dispatch_method("pane.mail_state", &obj(&[])).unwrap();
    assert_eq!(r.get("unread").and_then(JsonValue::as_i64), Some(2));
    // Listings carry the state.
    let list = cp.dispatch_method("pane.list", &obj(&[])).unwrap();
    let p1 = &list.get("panes").and_then(JsonValue::as_array).unwrap()[0];
    assert_eq!(p1.get("mail").and_then(|m| m.get("sender")).and_then(JsonValue::as_str), Some("api"));
    let st = cp.dispatch_method("workspace.status", &obj(&[])).unwrap();
    assert!(st.to_json_string().contains("\"mail\""));
    cp.dispatch_method("pane.mail_ack", &obj(&[("selector", js("p1"))])).unwrap();
    assert_eq!(unread(&mut cp, "p1"), 0);
}

#[test]
fn send_keys_and_find_send_mark_but_local_typing_does_not() {
    let mut cp = plane();
    let active = cp.wm.active_pane_id();
    // Local typing (no API origin) never marks.
    let key = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('a'), crossterm::event::KeyModifiers::NONE);
    bilterm::input::handle_wm_key_event(&mut cp.wm, key).unwrap();
    assert!(cp.wm.pane_mail_view(active, Instant::now()).is_none());
    let target = if active == 1 { 2 } else { 1 };
    cp.dispatch_method("pane.send_keys", &obj(&[("keys", js("b"))])).unwrap();
    assert_eq!(cp.wm.pane_mail_view(active, Instant::now()).unwrap().unread, 1);
    cp.dispatch_method("find.send", &obj(&[("pane", JsonValue::Number(target as i64)), ("send", js("x"))])).unwrap();
    assert_eq!(cp.wm.pane_mail_view(target, Instant::now()).unwrap().sender, "api");
}

#[test]
fn colon_cli_and_mcp_reach_the_same_state() {
    let mut cp = plane();
    cp.dispatch_method("pane.send", &obj(&[("selector", js("p2")), ("text", js("hi"))])).unwrap();
    let r = cp.dispatch_method("wm.command", &obj(&[("command", js(":mail status 2"))])).unwrap();
    assert!(r.to_json_string().contains("unread"), "{}", r.to_json_string());
    assert!(parse_colon_command(":mail ack all").is_some());
    let env = CliEnv::default();
    let c = parse(&["pane".to_string(), "mail".to_string(), "p2".to_string()], &env).unwrap();
    assert_eq!(c.method, "pane.mail_state");
    let c = parse(&["pane".to_string(), "mail-ack".to_string(), "--all".to_string()], &env).unwrap();
    assert_eq!((c.method.as_str(), c.params.get("all").and_then(JsonValue::as_bool)), ("pane.mail_ack", Some(true)));
    let m = AutomationControlPlane::agent_control_method(&obj(&[("action", js("pane_mail_state"))]));
    assert_eq!(m, Some("pane.mail_state"));
    assert_eq!(AutomationControlPlane::agent_control_method(&obj(&[("action", js("pane_mail_ack"))])), Some("pane.mail_ack"));
    cp.dispatch_method("wm.command", &obj(&[("command", js(":mail ack 2"))])).unwrap();
    assert_eq!(unread(&mut cp, "p2"), 0);
}

#[test]
fn renders_closed_then_open_envelope_and_respects_config() {
    let mut cp = plane();
    cp.wm.config_mut().ui.pane_mail_flash = false; // deterministic frame
    assert!(!screen(&cp).contains('\u{2709}'));
    cp.dispatch_method("pane.send", &obj(&[("selector", js("p1")), ("text", js("hi"))])).unwrap();
    let s = screen(&cp);
    assert!(s.contains('\u{2709}'), "closed envelope while unread");
    cp.dispatch_method("pane.mail_ack", &obj(&[("selector", js("p1"))])).unwrap();
    let s = screen(&cp);
    assert!(!s.contains('\u{2709}') && s.contains('\u{1F583}'), "open envelope once read");
    cp.wm.config_mut().ui.pane_mail_indicator = false;
    assert!(!screen(&cp).contains('\u{1F583}'));
}

#[test]
fn ascii_glyph_pack_uses_bracket_fallback() {
    let mut cp = plane();
    cp.wm.config_mut().ui.pane_mail_flash = false;
    cp.wm.config_mut().glyphs = bilterm::ui::glyphs::GlyphPack::Ascii;
    cp.dispatch_method("pane.send", &obj(&[("selector", js("p1")), ("text", js("hi"))])).unwrap();
    assert!(screen(&cp).contains("[M]"));
}
