//! P2 T-A2: attach listener on the session host (`docs/plans/session-host-p2-attach.md`).
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::attach::frame::{Direction, FrameReader};
use bilterm::attach::msg::{ByeReason, ClientKind, ClientMsg, HostMsg};

mod common;
use common::ShortSocketDir;

fn rpc(plane: &mut AutomationControlPlane, method: &str) -> JsonValue {
    let line = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{{}}}}"#);
    JsonValue::parse(&plane.handle_jsonrpc_line(&line)).expect("json")
}

fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < deadline, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn attach_clients_rpc_reports_disabled_without_listener() {
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    let r = rpc(&mut plane, "attach.clients");
    let res = r.get("result").expect("result");
    assert_eq!(res.get("enabled").and_then(JsonValue::as_bool), Some(false));
    assert_eq!(res.get("count").and_then(JsonValue::as_i64), Some(0));
}

#[test]
fn attach_flag_requires_host_ptys() {
    let o = Command::new(env!("CARGO_BIN_EXE_bilterm")).args(["--headless", "--attach"]).output().unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--attach requires --host-ptys"));
}

fn recv(s: &mut UnixStream, fr: &mut FrameReader) -> HostMsg {
    let mut b = [0u8; 4096];
    loop {
        let n = s.read(&mut b).expect("read");
        assert!(n > 0, "eof");
        if let Some(f) = fr.push_all(&b[..n]).unwrap().into_iter().next() {
            return HostMsg::decode(&f).unwrap();
        }
    }
}

#[test]
fn real_host_serves_attach_handshake_and_cleans_up() {
    let guard = ShortSocketDir::new();
    let dir = guard.path().to_path_buf();
    let exe = env!("CARGO_BIN_EXE_bilterm");
    let sock = guard.sock("host-a2.attach.sock");
    let mut child = Command::new(exe)
        .args(["--headless", "--host-ptys", "--attach", "--name", "a2"])
        .env("BILTERM_RUNTIME_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(|| sock.exists(), "attach socket");
    assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);

    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let hello = ClientMsg::Hello {
        proto_min: 1,
        proto_max: 1,
        client_kind: ClientKind::Ctl,
        client_name: "test".into(),
        caps: vec![],
    };
    s.write_all(&hello.encode().unwrap()).unwrap();
    let mut fr = FrameReader::new(Direction::HostToClient);
    match recv(&mut s, &mut fr) {
        HostMsg::HelloOk { proto, host_name, client_id, .. } => assert_eq!((proto, host_name.as_str(), client_id), (1, "a2", 1)),
        m => panic!("{m:?}"),
    }

    let ctl = Command::new(exe)
        .args(["ctl", "--daemon-name", "a2", "attach.clients"])
        .env("BILTERM_RUNTIME_DIR", &dir)
        .output()
        .unwrap();
    let out = JsonValue::parse(String::from_utf8_lossy(&ctl.stdout).trim()).expect("ctl json");
    let res = out.get("result").unwrap_or(&out);
    assert_eq!(res.get("count").and_then(JsonValue::as_i64), Some(1), "{out:?}");
    assert_eq!(res.get("enabled").and_then(JsonValue::as_bool), Some(true));

    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    assert_eq!(recv(&mut s, &mut fr), HostMsg::Bye { reason: ByeReason::HostShutdown });
    child.wait().unwrap();
    assert!(!sock.exists(), "attach socket removed on clean shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
