//! `attach.kick` against a REAL `--headless --host-ptys --attach` host over the real sockets
//! (`docs/plans/session-host-p2-attach.md` section 3): Bye{kicked}, exit code 4, registry updated,
//! other clients unaffected, an attach client cannot kick via `Act`.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bilterm::api::JsonValue;
use bilterm::attach::client::link::{connect_unix, Link, LinkError};
use bilterm::attach::client::state::{Applied, ClientState};
use bilterm::attach::client::tui::bye_outcome;
use bilterm::attach::client::EXIT_KICKED;
mod common;
use common::ShortSocketDir;

use bilterm::attach::msg::{ByeReason, ClientMsg, ErrorCode, Role};
use tokio::net::UnixStream;

struct Host {
    child: Child,
    dir: PathBuf,
    _guard: ShortSocketDir,
    sock: PathBuf,
    name: String,
}

impl Host {
    fn start(tag: &str) -> Self {
        let name = format!("kk{tag}");
        let guard = ShortSocketDir::new();
        let dir = guard.path().to_path_buf();
        let sock = guard.sock(&format!("host-{name}.attach.sock"));
        let child = Command::new(env!("CARGO_BIN_EXE_bilterm"))
            .args(["--headless", "--host-ptys", "--attach", "--name", &name])
            .env("BILTERM_RUNTIME_DIR", &dir)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !sock.exists() {
            assert!(
                Instant::now() < deadline,
                "host attach socket never appeared"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            child,
            dir,
            _guard: guard,
            sock,
            name,
        }
    }

    /// `bilterm ctl attach.* ...` against this host; returns the parsed JSON-RPC response.
    fn ctl(&self, method: &str, params: &str) -> JsonValue {
        let o = Command::new(env!("CARGO_BIN_EXE_bilterm"))
            .args(["ctl", "--daemon-name", &self.name, method, params])
            .env("BILTERM_RUNTIME_DIR", &self.dir)
            .output()
            .unwrap();
        JsonValue::parse(String::from_utf8_lossy(&o.stdout).trim()).expect("ctl json")
    }

    fn client_ids(&self) -> Vec<i64> {
        let r = self.ctl("attach.clients", "{}");
        let res = r.get("result").unwrap_or(&r);
        res.get("clients")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .filter_map(|c| c.get("id").and_then(JsonValue::as_i64))
            .collect()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn attach(host: &Host, role: Role) -> (Link<UnixStream>, ClientState) {
    // The socket file appears at bind(), a hair before listen(): retry a refused connect briefly.
    let deadline = Instant::now() + Duration::from_secs(5);
    let link = loop {
        match connect_unix(&host.sock, role, 80, 24).await {
            Err(LinkError::Io(e))
                if e.kind() == std::io::ErrorKind::ConnectionRefused
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            r => break r.expect("connect"),
        }
    };
    let st = ClientState::new(link.info.client_id);
    (link, st)
}

/// Applies host messages until a `Bye`, returning its reason (acks along the way).
async fn until_bye(link: &mut Link<UnixStream>, st: &mut ClientState) -> ByeReason {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let m = tokio::time::timeout(left, link.recv())
            .await
            .expect("timeout waiting for Bye")
            .expect("recv before Bye");
        match st.apply(m).expect("protocol") {
            Applied::Bye(r) => return r,
            Applied::Ack { pane, seq } => {
                let _ = link.send(&ClientMsg::Ack { pane, seq }).await;
            }
            _ => {}
        }
    }
}

fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < deadline, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn kick_rw_client_gets_bye_kicked_exit_4_and_others_survive() {
    let host = Host::start("one");
    let (mut a, mut a_st) = attach(&host, Role::Rw).await;
    let (mut b, mut b_st) = attach(&host, Role::Ro).await;
    let (a_id, b_id) = (a.info.client_id as i64, b.info.client_id as i64);
    wait_for(|| host.client_ids().len() == 2, "two clients");

    let r = host.ctl("attach.kick", &format!(r#"{{"client_id":{a_id}}}"#));
    let res = r.get("result").unwrap_or(&r);
    assert_eq!(
        res.get("count").and_then(JsonValue::as_i64),
        Some(1),
        "{r:?}"
    );

    let reason = until_bye(&mut a, &mut a_st).await;
    assert_eq!(reason, ByeReason::Kicked);
    assert_eq!(bye_outcome(reason).code, EXIT_KICKED);
    assert_eq!(EXIT_KICKED, 4);
    wait_for(
        || host.client_ids() == vec![b_id],
        "registry drops the kicked client",
    );

    // The other client is untouched and still served: it answers a Ping.
    b.send(&ClientMsg::Ping { n: 7 }).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no Pong for surviving client");
        let m = tokio::time::timeout(Duration::from_secs(10), b.recv())
            .await
            .unwrap()
            .expect("surviving client must stay connected");
        if let Applied::Bye(r) = b_st.apply(m.clone()).unwrap() {
            panic!("surviving client got Bye {r:?}");
        }
        if matches!(m, bilterm::attach::msg::HostMsg::Pong { n: 7 }) {
            break;
        }
        if let Applied::Ack { pane, seq } = b_st.apply(m).unwrap() {
            b.send(&ClientMsg::Ack { pane, seq }).await.unwrap();
        }
    }

    // Unknown id: clear error, nothing else changes.
    let r = host.ctl("attach.kick", r#"{"client_id":4242}"#);
    assert!(
        format!("{r:?}").contains("unknown attach client 4242"),
        "{r:?}"
    );
    assert_eq!(host.client_ids(), vec![b_id]);
}

#[tokio::test(flavor = "multi_thread")]
async fn kick_ro_hits_only_read_only_clients_and_colon_form_works() {
    let host = Host::start("ro");
    let (mut rw, mut rw_st) = attach(&host, Role::Rw).await;
    let (mut ro, mut ro_st) = attach(&host, Role::Ro).await;
    wait_for(|| host.client_ids().len() == 2, "two clients");
    let r = host.ctl("wm.command", r#"{"command":":daemon attach kick --ro"}"#);
    assert!(r.get("error").is_none(), "{r:?}");
    assert_eq!(until_bye(&mut ro, &mut ro_st).await, ByeReason::Kicked);
    wait_for(
        || host.client_ids() == vec![rw.info.client_id as i64],
        "only rw remains",
    );
    rw.send(&ClientMsg::Ping { n: 1 }).await.unwrap();
    let _ = &mut rw_st;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_client_cannot_kick_others_via_act() {
    let host = Host::start("act");
    let (mut a, mut a_st) = attach(&host, Role::Rw).await;
    let (b, _b_st) = attach(&host, Role::Rw).await;
    wait_for(|| host.client_ids().len() == 2, "two clients");
    a.send(&ClientMsg::Act {
        cmd: format!(":daemon attach kick {}", b.info.client_id),
    })
    .await
    .unwrap();
    // `a` receives a refusal Error and stays connected; `b` is never kicked.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut refused = false;
    while !refused {
        assert!(
            Instant::now() < deadline,
            "no refusal for attach-origin kick"
        );
        let m = tokio::time::timeout(Duration::from_secs(10), a.recv())
            .await
            .unwrap()
            .unwrap();
        match a_st.apply(m).unwrap() {
            Applied::Error(code, msg) => {
                assert!(
                    code == ErrorCode::BadRequest && msg.contains("cannot kick"),
                    "{code:?} {msg}"
                );
                refused = true;
            }
            Applied::Ack { pane, seq } => a.send(&ClientMsg::Ack { pane, seq }).await.unwrap(),
            Applied::Bye(r) => panic!("unexpected bye {r:?}"),
            _ => {}
        }
    }
    assert_eq!(host.client_ids().len(), 2, "nobody was kicked");
}
