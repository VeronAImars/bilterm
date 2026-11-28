//! P1 session host (`docs/plans/session-host-architecture.md`): opt-in PTY-owning headless daemon.
#![cfg(unix)]

mod common;

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

#[test]
fn host_state_rpc_reports_absent_and_rejects_bad_names() {
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    let r = rpc(&mut plane, "daemon.host_state", r#"{"name":"p1-no-such-host"}"#);
    let res = r.get("result").expect("result");
    assert_eq!(res.get("status").and_then(JsonValue::as_str), Some("absent"));
    let r = rpc(&mut plane, "daemon.host_state", r#"{"name":"../x"}"#);
    assert!(r.get("error").is_some() || r.to_json_string().contains("error"), "{r:?}");
}

fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < deadline, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Real daemon process: state file appears, is inspectable as live, is removed on SIGTERM; after
/// SIGKILL the leftover is reported stale (and a stop cleans it up).
#[test]
fn host_state_file_lifecycle_across_real_daemon() {
    use std::process::{Command, Stdio};
    let dir = common::short_root().join(format!("bt-p1life-{}", bilterm::secure_fs::unique_suffix()));
    bilterm::secure_fs::create_private_dir_all(&dir).unwrap();
    let exe = env!("CARGO_BIN_EXE_bilterm");
    let state = dir.join("host-p1life.state");
    let ctl = |dir: &std::path::Path| {
        let o = Command::new(exe)
            .args(["ctl", "--daemon-name", "p1life", "daemon.host_state"])
            .env("BILTERM_RUNTIME_DIR", dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let spawn = |dir: &std::path::Path| {
        Command::new(exe)
            .args(["--headless", "--host-ptys", "--name", "p1life"])
            .env("BILTERM_RUNTIME_DIR", dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let sig = |pid: u32, s: i32| unsafe {
        libc::kill(pid as i32, s);
    };

    // Clean shutdown removes the file.
    let mut child = spawn(&dir);
    wait_for(|| state.exists(), "state file");
    wait_for(|| ctl(&dir).contains("\"live\""), "live status");
    sig(child.id(), libc::SIGTERM);
    child.wait().unwrap();
    assert!(!state.exists(), "state file must be removed on clean shutdown");

    // Crash leaves a record that is detected as stale.
    let mut child = spawn(&dir);
    wait_for(|| state.exists(), "state file (2nd)");
    sig(child.id(), libc::SIGKILL);
    child.wait().unwrap();
    assert!(state.exists());
    let dead_pid = child.id();
    match bilterm::daemon::host_inspect::inspect_host_state(&dir, "p1life").unwrap() {
        bilterm::daemon::host_inspect::HostInspection::Loaded { stale: Some(_), state, .. } => {
            assert_eq!(state.host_pid, dead_pid)
        }
        other => panic!("expected stale record, got {other:?}"),
    }

    // A restarted host replaces the leftover with its own record, then cleans up on SIGTERM.
    let mut child = spawn(&dir);
    wait_for(|| ctl(&dir).contains("\"live\""), "live status after restart");
    sig(child.id(), libc::SIGTERM);
    child.wait().unwrap();
    assert!(!state.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
