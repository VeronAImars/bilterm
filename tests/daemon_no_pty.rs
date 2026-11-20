//! A daemon started without `--host-ptys` has model-only panes (no shell). Command-sending RPCs
//! must say so instead of reporting success, `host_ptys` must be visible on every daemon-listing
//! surface, and a stale `BILTERM_SOCKET` must never silently retarget the `default` daemon.
#![cfg(unix)]

mod common;

use bilterm::api::AutomationControlPlane;
use bilterm::cli::{stale_env_socket, CliEnv};
use common::{wait_until, IsolatedEnv, KillOnDrop};
use std::process::{Output, Stdio};
use std::time::Duration;

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// Waits (when `wait`) for daemon `name`'s socket under the isolated runtime tree; else one look.
fn has_sock(env: &IsolatedEnv, name: &str, wait: bool) -> bool {
    fn find(dir: &std::path::Path, file: &str) -> bool {
        std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
            let p = e.path();
            p.file_name().is_some_and(|f| f == file) || (p.is_dir() && find(&p, file))
        })
    }
    let file = format!("daemon-{name}.sock");
    let root = env.dir("runtime");
    if wait {
        wait_until(Duration::from_secs(10), || find(&root, &file))
    } else {
        !find(&root, &file)
    }
}

fn rpc(plane: &mut AutomationControlPlane, method: &str, params: &str) -> Result<bilterm::api::JsonValue, String> {
    plane.dispatch_method(method, &bilterm::api::JsonValue::parse(params).unwrap())
}

#[test]
fn offline_pty_less_plane_rejects_command_senders_but_not_structure() {
    let mut p = AutomationControlPlane::new_headless(24, 80).with_no_pty_daemon("omega");
    for (m, params) in [
        ("pane.exec", r#"{"command":"echo hi"}"#),
        ("pane.send_keys", r#"{"keys":"e Enter"}"#),
        ("pane.send", r#"{"text":"echo hi","enter":true}"#),
    ] {
        let e = rpc(&mut p, m, params).expect_err(m);
        assert!(e.contains("no_host_ptys") && e.contains("'omega'") && e.contains("--host-ptys"), "{m}: {e}");
    }
    // Structural commands still work, but carry a warning so "handled" is not read as "ran".
    let r = rpc(&mut p, "wm.command", r#"{"command":"pane split"}"#).unwrap();
    assert!(r.get("warning").and_then(|w| w.as_str()).is_some_and(|w| w.contains("no_host_ptys")));
    assert_eq!(r.get("host_ptys").and_then(|v| v.as_bool()), Some(false));
    // Plain headless planes (tests, embedders) are unchanged.
    let mut plain = AutomationControlPlane::new_headless(24, 80);
    assert!(rpc(&mut plain, "pane.send_keys", r#"{"keys":"e"}"#).is_ok());
}

#[test]
fn offline_stale_env_socket_detection() {
    let env = |s: &str| CliEnv { pane_id: None, socket: Some(s.to_string()) };
    let msg = stale_env_socket(&env("/definitely/not/here/859194.sock")).expect("missing socket is stale");
    assert!(msg.contains("BILTERM_SOCKET=/definitely/not/here/859194.sock") && msg.contains("stale") && msg.contains("default"), "{msg}");
    assert!(stale_env_socket(&env("unix:/definitely/not/here.sock")).is_some());
    assert!(stale_env_socket(&env("127.0.0.1:7878")).is_none(), "host:port is not a file");
    assert!(stale_env_socket(&CliEnv::default()).is_none());
    let here = std::env::current_exe().unwrap();
    assert!(stale_env_socket(&env(&here.display().to_string())).is_none(), "existing path is not stale");
}

#[test]
fn real_pty_less_daemon_reports_capability_and_refuses_commands() {
    let env = IsolatedEnv::new();
    let started = env.run(&["daemon", "start", "nopty"]);
    assert!(started.status.success(), "{}", text(&started));
    assert!(has_sock(&env, "nopty", true));

    let list = env.run(&["daemon", "list", "--json"]);
    let l = text(&list);
    assert!(l.contains("\"host_ptys\":false"), "{l}");
    let table = text(&env.run(&["daemon", "list"]));
    assert!(table.contains("PTY") && table.contains("model-only"), "{table}");
    let status = text(&env.run(&["daemon", "status", "nopty"]));
    assert!(status.contains("WARNING") && status.contains("--host-ptys"), "{status}");
    let rpc_status = text(&env.run(&["ctl", "--daemon-name", "nopty", "daemon.status"]));
    assert!(rpc_status.contains("\"host_ptys\":false"), "{rpc_status}");

    for args in [
        vec!["ctl", "--daemon-name", "nopty", "pane.send_keys", r#"{"keys":"e Enter"}"#],
        vec!["ctl", "--daemon-name", "nopty", "pane.exec", r#"{"command":"echo hello-omega"}"#],
    ] {
        let o = text(&env.run(&args));
        assert!(o.contains("no_host_ptys") && o.contains("daemon 'nopty'"), "{o}");
    }
    let _ = env.run(&["daemon", "stop", "nopty", "--force"]);
}

#[test]
fn real_host_daemon_advertises_ptys_and_accepts_commands() {
    let env = IsolatedEnv::new();
    let child = env
        .bilterm(&["--headless", "--host-ptys", "--name", "hosty"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(child);
    assert!(has_sock(&env, "hosty", true));
    assert!(wait_until(Duration::from_secs(10), || text(&env.run(&["daemon", "list", "--json"])).contains("\"host_ptys\":true")));
    let o = text(&env.run(&["ctl", "--daemon-name", "hosty", "pane.send_keys", r#"{"keys":"e"}"#]));
    assert!(!o.contains("no_host_ptys") && o.contains("dispatched"), "{o}");
}

#[test]
fn stale_bilterm_socket_is_reported_and_never_falls_back_to_default() {
    let env = IsolatedEnv::new();
    let stale = env.dir("runtime").join("bilterm/instances/859194.sock");
    for args in [vec!["ctl", "workspace.status"], vec!["pane", "list"]] {
        let mut cmd = env.bilterm(&args);
        cmd.env("BILTERM_SOCKET", &stale);
        let o = common::run_with_deadline(cmd, common::PROCESS_DEADLINE);
        let t = text(&o);
        assert!(!o.status.success(), "{args:?}: {t}");
        assert!(t.contains("stale") && t.contains("859194.sock") && t.contains("default"), "{args:?}: {t}");
    }
    // Refusal means the default daemon was not auto-started behind the caller's back.
    assert!(has_sock(&env, "default", false), "default daemon must not have been started");
    // Naming a target explicitly still works despite the stale variable.
    let mut cmd = env.bilterm(&["daemon", "list"]);
    cmd.env("BILTERM_SOCKET", &stale);
    assert!(common::run_with_deadline(cmd, common::PROCESS_DEADLINE).status.success());
}
