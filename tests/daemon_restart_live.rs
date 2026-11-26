//! `bilterm daemon restart --live` and `daemon transfer` against REAL daemon processes: the
//! shell keeps its pid, the terminal keeps every byte, and the old process is gone.
#![cfg(unix)]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bilterm::api::JsonValue;

const EXE: &str = env!("CARGO_BIN_EXE_bilterm");

fn run(dir: &Path, args: &[&str]) -> (String, i32) {
    let o = Command::new(EXE).args(args).env("BILTERM_RUNTIME_DIR", dir).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr), o.status.code().unwrap_or(-1))
}

fn ctl(dir: &Path, name: &str, method: &str, params: &str) -> Option<JsonValue> {
    let (out, _) = run(dir, &["ctl", "--daemon-name", name, method, params]);
    JsonValue::parse(out.trim())
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(40);
    while !f() {
        assert!(Instant::now() < end, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn screen(dir: &Path, name: &str) -> String {
    let Some(v) = ctl(dir, name, "pane.read", "{}") else { return String::new() };
    let r = v.get("result").cloned().unwrap_or(v);
    match r.get("lines") {
        Some(JsonValue::Array(a)) => a.iter().filter_map(|l| l.as_str()).map(str::trim_end).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

fn spawn_host(dir: &Path, name: &str) -> std::process::Child {
    Command::new(EXE)
        .args(["--headless", "--host-ptys", "--name", name])
        .env("BILTERM_RUNTIME_DIR", dir)
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn pid_of(dir: &Path, name: &str) -> Option<u32> {
    let (out, code) = run(dir, &["daemon", "status", name, "--json"]);
    if code != 0 {
        return None;
    }
    JsonValue::parse(out.trim())?.get("pid")?.as_i64().map(|n| n as u32)
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn shell_pid(screen: &str) -> Option<u32> {
    let i = screen.rfind("PID=")? + 4;
    let d: String = screen[i..].chars().take_while(char::is_ascii_digit).collect();
    d.parse().ok().filter(|p| *p > 1)
}

#[test]
fn live_restart_keeps_shell_pid_and_terminal_content() {
    let dir = common::short_root().join(format!("bt-lr-{}", bilterm::secure_fs::unique_suffix()));
    bilterm::secure_fs::create_private_dir_all(&dir).unwrap();
    let mut old = spawn_host(&dir, "lr");
    let old_pid = old.id();
    wait_for("old host", || pid_of(&dir, "lr") == Some(old_pid) && ctl(&dir, "lr", "workspace.status", "{}").is_some());

    // 300 numbers fit on the 24x80 screen without scrolling, so the screen holds the whole stream.
    // Disable job control (`set +m`) inside the subshell loop so macOS `/bin/sh` (`bash 3.2`)
    // does not emit `sh: child setpgid (...): Operation not permitted` into the middle of an
    // iteration when its parent daemon exits during live handover.
    let cmd = "set +m; echo PID=$$.; i=0; while [ $i -lt 300 ]; do printf '%s,' $i; i=$((i+1)); sleep 0.03; done; echo FIN''ISHED; cat";
    let params = format!("{{\"command\":{cmd:?}}}");
    assert!(ctl(&dir, "lr", "pane.exec", &params).is_some());
    let mut sp = None;
    wait_for("shell pid", || {
        sp = shell_pid(&screen(&dir, "lr")).filter(|_| screen(&dir, "lr").contains("12,13"));
        sp.is_some()
    });
    let shell = sp.unwrap();
    assert!(alive(shell));

    let (out, code) = run(&dir, &["daemon", "restart", "--live", "lr"]);
    assert_eq!(code, 0, "restart failed: {out}");
    assert!(out.contains("restarted live"), "{out}");
    let _ = old.wait(); // the old host exits by itself
    let new_pid = pid_of(&dir, "lr").expect("new host serving");
    assert_ne!(new_pid, old_pid, "a new process must be serving");
    assert!(!alive(old_pid), "old host must be gone");
    assert!(alive(shell), "the shell must survive the restart");

    wait_for("loop to finish on the new host", || screen(&dir, "lr").contains("FINISHED"));
    let s = screen(&dir, "lr");
    assert_eq!(shell_pid(&s), Some(shell), "same shell pid");
    let after = s.split("FINISHED").next().unwrap();
    let tail = after.split("PID=").last().unwrap();
    let nums: Vec<u32> = tail.split_once('.').unwrap().1.split(',').filter_map(|t| t.trim().parse().ok()).collect();
    assert_eq!(nums, (0..300).collect::<Vec<_>>(), "numbers lost or duplicated across the restart: {s}");

    unsafe { libc::kill(shell as i32, libc::SIGKILL) };
    let _ = run(&dir, &["daemon", "stop", "lr", "--force"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_restart_of_a_missing_or_modelonly_daemon_is_refused_cleanly() {
    let dir = common::short_root().join(format!("bt-lr2-{}", bilterm::secure_fs::unique_suffix()));
    bilterm::secure_fs::create_private_dir_all(&dir).unwrap();
    let (out, code) = run(&dir, &["daemon", "restart", "--live", "nope"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("not running"), "{out}");
    let (out, code) = run(&dir, &["daemon", "restart", "nope"]);
    assert_ne!(code, 0);
    assert!(out.contains("--live"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transfer_between_two_real_hosts_via_cli() {
    let dir = common::short_root().join(format!("bt-tr-{}", bilterm::secure_fs::unique_suffix()));
    bilterm::secure_fs::create_private_dir_all(&dir).unwrap();
    let mut a = spawn_host(&dir, "ta");
    let mut b = spawn_host(&dir, "tb");
    wait_for("hosts", || ctl(&dir, "ta", "workspace.status", "{}").is_some() && ctl(&dir, "tb", "workspace.status", "{}").is_some());
    // Make a second session on A so the active one can move without emptying A.
    assert!(ctl(&dir, "ta", "wm.command", r#"{"command":":session new work"}"#).is_some());
    let params = r#"{"command":"echo PID=$$.; cat"}"#;
    assert!(ctl(&dir, "ta", "pane.exec", params).is_some());
    let mut sp = None;
    wait_for("shell pid on A", || {
        sp = shell_pid(&screen(&dir, "ta"));
        sp.is_some()
    });
    let shell = sp.unwrap();
    let (out, code) = run(&dir, &["daemon", "transfer", "work", "--from", "ta", "--to", "tb"]);
    assert_eq!(code, 0, "{out}");
    assert!(alive(shell));
    // Switch B to the transferred session: its terminal shows the same shell pid.
    assert!(ctl(&dir, "tb", "wm.command", r#"{"command":":session switch work"}"#).is_some());
    wait_for("transferred session on B", || shell_pid(&screen(&dir, "tb")) == Some(shell));
    // Transferring something that does not exist leaves everything alone.
    let (out, code) = run(&dir, &["daemon", "transfer", "ghost", "--from", "ta", "--to", "tb"]);
    assert_eq!(code, 1, "{out}");
    assert!(alive(shell));
    unsafe { libc::kill(shell as i32, libc::SIGKILL) };
    for n in ["ta", "tb"] {
        let _ = run(&dir, &["daemon", "stop", n, "--force"]);
    }
    let _ = (a.kill(), b.kill(), a.wait(), b.wait());
    let _ = std::fs::remove_dir_all(&dir);
}
