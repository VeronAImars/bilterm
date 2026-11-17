//! P1 session host (`docs/plans/session-host-architecture.md`): opt-in PTY-owning headless daemon.
#![cfg(unix)]

use std::time::{Duration, Instant};

use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::wm::WindowManager;
use ratatui::layout::Rect;

fn rpc(plane: &mut AutomationControlPlane, method: &str, params: &str) -> JsonValue {
    let line = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    JsonValue::parse(&plane.handle_jsonrpc_line(&line)).expect("json")
}

#[test]
fn default_headless_spawns_no_pty() {
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    assert!(!plane.is_host());
    let st = rpc(&mut plane, "workspace.status", "{}");
    assert_eq!(st.get("result").and_then(|r| r.get("host_ptys")).and_then(JsonValue::as_bool), Some(false));
    assert!(plane.wm.panes().values().all(|p| p.is_headless() && p.child_pid().is_none()));
    // New panes stay model-only too.
    plane.wm.split_focused(bilterm::wm::SplitDirection::Vertical).unwrap();
    assert!(plane.wm.panes().values().all(|p| p.is_headless()));
}

#[test]
fn host_panes_run_real_shell_via_ctl_surface() {
    let wm = WindowManager::new_host(24, 80).expect("host");
    let mut plane = AutomationControlPlane::new(wm, Rect::new(0, 0, 80, 24));
    assert!(plane.is_host());
    let st = rpc(&mut plane, "workspace.status", "{}");
    assert_eq!(st.get("result").and_then(|r| r.get("host_ptys")).and_then(JsonValue::as_bool), Some(true));

    // New panes in a host are live too, not model-only.
    plane.wm.split_focused(bilterm::wm::SplitDirection::Vertical).unwrap();
    assert!(plane.wm.panes().values().all(|p| !p.is_headless() && p.child_pid().is_some()));
    assert_eq!(plane.wm.host_state("t").panes.len(), 2);

    let id = plane.wm.active_pane_id();
    let r = rpc(&mut plane, "pane.exec", &format!(r#"{{"pane_id":{id},"command":"echo host-$((20+22))-ok"}}"#));
    assert!(r.get("error").is_none(), "{r:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        plane.host_tick();
        let out = rpc(&mut plane, "pane.read", &format!(r#"{{"pane_id":{id}}}"#)).to_json_string();
        if out.contains("host-42-ok") {
            break;
        }
        assert!(Instant::now() < deadline, "no shell output: {out}");
        std::thread::sleep(Duration::from_millis(25));
    }
}
