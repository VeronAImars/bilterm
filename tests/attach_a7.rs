//! P2 T-A7: `bilterm attach` client against a REAL `--headless --host-ptys --attach` host with a
//! real shell pane, over the real unix socket (`docs/plans/session-host-p2-attach.md`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bilterm::attach::client::link::{connect_unix, Link, LinkError};
use bilterm::attach::client::state::{Applied, ClientState};
use bilterm::attach::msg::{ClientMsg, ErrorCode, Role};
use tokio::net::UnixStream;

struct Host {
    child: Child,
    dir: PathBuf,
    sock: PathBuf,
    name: String,
}

impl Host {
    fn start(tag: &str) -> Self {
        let name = format!("a7{tag}");
        let dir = std::env::temp_dir().join(format!("bt-a7-{}", bilterm::secure_fs::unique_suffix()));
        bilterm::secure_fs::create_private_dir_all(&dir).unwrap();
        let sock = dir.join(format!("host-{name}.attach.sock"));
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
            assert!(Instant::now() < deadline, "host attach socket never appeared");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { child, dir, sock, name }
    }

    fn kill9(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

type Errors = Vec<(ErrorCode, String)>;

/// Receives host messages, applying and acking them, until `pred(state, errors)` or timeout.
async fn pump_until(
    link: &mut Link<UnixStream>,
    st: &mut ClientState,
    errors: &mut Errors,
    what: &str,
    mut pred: impl FnMut(&mut ClientState, &Errors) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if pred(st, errors) {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timeout waiting for {what}; screen: {:?}", st.current_pane().and_then(|p| st.pane_text(p)));
        let msg = tokio::time::timeout(left, link.recv()).await.unwrap_or_else(|_| panic!("timeout in recv for {what}")).expect("recv");
        match st.apply(msg).expect("host protocol") {
            Applied::Ack { pane, seq } => link.send(&ClientMsg::Ack { pane, seq }).await.unwrap(),
            Applied::Error(c, m) => errors.push((c, m)),
            Applied::Bye(r) => panic!("unexpected bye {r:?} waiting for {what}"),
            _ => {}
        }
    }
}

fn has_line(st: &mut ClientState, exact: &str) -> bool {
    let Some(p) = st.current_pane() else { return false };
    st.pane_text(p).is_some_and(|rows| rows.iter().any(|r| r == exact))
}

fn has_text(st: &mut ClientState, needle: &str) -> bool {
    let Some(p) = st.current_pane() else { return false };
    st.pane_text(p).is_some_and(|rows| rows.iter().any(|r| r.contains(needle)))
}

fn ready(st: &mut ClientState, _: &Errors) -> bool {
    st.current_pane().is_some_and(|p| st.pane_text(p).is_some())
}

async fn attach(host: &Host, role: Role) -> (Link<UnixStream>, ClientState, Errors) {
    let link = connect_unix(&host.sock, role, 80, 24).await.expect("connect");
    let st = ClientState::new(link.info.client_id);
    (link, st, Vec::new())
}

#[tokio::test(flavor = "multi_thread")]
async fn real_shell_echo_appears_in_client_grid() {
    let host = Host::start("echo");
    let (mut link, mut st, mut errs) = attach(&host, Role::Rw).await;
    assert_eq!(link.info.host_name, host.name);
    pump_until(&mut link, &mut st, &mut errs, "first snapshot", ready).await;
    let pane = st.current_pane().unwrap();
    // The shell computes the number, so the OUTPUT line (not just the typed echo) matches exactly.
    link.send_input(pane, b"echo hello-$((40+2))-attach\r").await.unwrap();
    pump_until(&mut link, &mut st, &mut errs, "shell output line", |s, _| has_line(s, "hello-42-attach")).await;
    pump_until(&mut link, &mut st, &mut errs, "Resized", |s, _| s.host_size.is_some()).await;
    assert_eq!(st.owner, Some(link.info.client_id), "rw client owns the size");
    link.send(&ClientMsg::Detach {}).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn ro_client_cannot_send_input_but_sees_rw_output() {
    let host = Host::start("ro");
    let (mut rw, mut rw_st, mut rw_errs) = attach(&host, Role::Rw).await;
    let (mut ro, mut ro_st, mut ro_errs) = attach(&host, Role::Ro).await;
    pump_until(&mut rw, &mut rw_st, &mut rw_errs, "rw snapshot", ready).await;
    pump_until(&mut ro, &mut ro_st, &mut ro_errs, "ro snapshot", ready).await;
    let pane = ro_st.current_pane().unwrap();

    ro.send_input(pane, b"echo ro-$((1+1))-leak\r").await.unwrap();
    pump_until(&mut ro, &mut ro_st, &mut ro_errs, "read_only error", |_, e| e.iter().any(|(c, _)| *c == ErrorCode::ReadOnly)).await;

    // Resize from ro is refused too.
    ro.send(&ClientMsg::Resize { cols: 100, rows: 30, pixel_w: None, pixel_h: None }).await.unwrap();
    pump_until(&mut ro, &mut ro_st, &mut ro_errs, "second read_only error", |_, e| e.iter().filter(|(c, _)| *c == ErrorCode::ReadOnly).count() >= 2).await;

    // An rw command still works, and the ro client observes it; the refused command never ran.
    rw.send_input(pane, b"echo rw-$((2+3))-ok\r").await.unwrap();
    pump_until(&mut rw, &mut rw_st, &mut rw_errs, "rw output", |s, _| has_line(s, "rw-5-ok")).await;
    pump_until(&mut ro, &mut ro_st, &mut ro_errs, "ro sees rw output", |s, _| has_line(s, "rw-5-ok")).await;
    assert!(!has_text(&mut ro_st, "ro-2-leak"), "ro input must never reach the shell");
    assert!(!has_text(&mut rw_st, "ro-$((1+1))-leak") && !has_text(&mut rw_st, "ro-2-leak"));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_sockets_are_clean_errors() {
    let dir = std::env::temp_dir().join(format!("bt-a7-bad-{}", bilterm::secure_fs::unique_suffix()));
    bilterm::secure_fs::create_private_dir_all(&dir).unwrap();
    // Nothing listening / no such file.
    assert!(matches!(connect_unix(&dir.join("none.sock"), Role::Rw, 80, 24).await, Err(LinkError::Io(_))));
    // A plain file at the path.
    let f = dir.join("file.sock");
    std::fs::write(&f, b"x").unwrap();
    assert!(connect_unix(&f, Role::Rw, 80, 24).await.is_err());
    // A listener that speaks garbage: protocol error, not a hang or panic.
    let sock = dir.join("junk.sock");
    let l = tokio::net::UnixListener::bind(&sock).unwrap();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let (mut s, _) = l.accept().await.unwrap();
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\n\r\nnot an attach host").await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    assert!(matches!(connect_unix(&sock, Role::Rw, 80, 24).await, Err(LinkError::Frame(_))));
    // The CLI reports it with exit code 1 and a message, before touching the terminal.
    let o = Command::new(env!("CARGO_BIN_EXE_bilterm"))
        .args(["attach", "--host", &dir.join("none.sock").display().to_string()])
        .env("BILTERM_RUNTIME_DIR", &dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no attach socket"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = Command::new(env!("CARGO_BIN_EXE_bilterm")).args(["attach", "--ro"]).env("BILTERM_RUNTIME_DIR", &dir).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_death_surfaces_as_eof_and_reconnect_fails() {
    let mut host = Host::start("die");
    let (mut link, mut st, mut errs) = attach(&host, Role::Rw).await;
    pump_until(&mut link, &mut st, &mut errs, "snapshot", ready).await;
    host.kill9();
    let end = loop {
        match tokio::time::timeout(Duration::from_secs(10), link.recv()).await.expect("recv must end after host death") {
            Ok(_) => continue, // drain anything already in flight
            Err(e) => break e,
        }
    };
    assert!(matches!(end, LinkError::Eof | LinkError::Io(_)), "{end}");
    assert!(connect_unix(&host.sock, Role::Rw, 80, 24).await.is_err(), "no host left to reconnect to");
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_host_shutdown_sends_bye() {
    let mut host = Host::start("bye");
    let (mut link, mut st, mut errs) = attach(&host, Role::Rw).await;
    pump_until(&mut link, &mut st, &mut errs, "snapshot", ready).await;
    unsafe { libc::kill(host.child.id() as i32, libc::SIGTERM) };
    let bye = loop {
        match tokio::time::timeout(Duration::from_secs(10), link.recv()).await.expect("bye") {
            Ok(m) => {
                if let Ok(Applied::Bye(r)) = st.apply(m) {
                    break r;
                }
            }
            Err(e) => panic!("connection ended without Bye: {e}"),
        }
    };
    assert_eq!(bye, bilterm::attach::msg::ByeReason::HostShutdown);
    let _ = host.child.wait();
}

// ---- the real `bilterm attach` binary inside a PTY -------------------------------------------

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};

struct PtyClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl PtyClient {
    fn spawn(host_sock: &Path, dir: &Path, extra: &[&str]) -> Self {
        let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_bilterm"));
        cmd.arg("attach");
        for e in extra {
            cmd.arg(e);
        }
        cmd.arg(host_sock);
        cmd.env("BILTERM_RUNTIME_DIR", dir);
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut b = [0u8; 8192];
            while let Ok(n) = reader.read(&mut b) {
                if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self { child, writer, rx, seen: Vec::new(), _master: pair.master }
    }

    fn wait_for(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !String::from_utf8_lossy(&self.seen).contains(needle) {
            assert!(Instant::now() < deadline, "timeout waiting for {needle:?} in client output: {:?}", String::from_utf8_lossy(&self.seen));
            if let Ok(b) = self.rx.recv_timeout(Duration::from_millis(100)) {
                self.seen.extend(b);
            }
        }
    }

    fn wait_exit(&mut self, within: Duration) -> u32 {
        let deadline = Instant::now() + within;
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st.exit_code();
            }
            assert!(Instant::now() < deadline, "client did not exit; output {:?}", String::from_utf8_lossy(&self.seen));
            while let Ok(b) = self.rx.try_recv() {
                self.seen.extend(b);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn cli_client_in_a_pty_types_into_shell_and_detaches_with_code_0() {
    let host = Host::start("pty");
    let mut c = PtyClient::spawn(&host.sock, &host.dir, &[]);
    c.wait_for("rw"); // status line rendered
    c.writer.write_all(b"echo cli-$((6*7))-pty\r").unwrap();
    c.wait_for("cli-42-pty");
    c.writer.write_all(&[0x1d, b'd']).unwrap(); // Ctrl-] d
    assert_eq!(c.wait_exit(Duration::from_secs(10)), 0);
}

#[test]
fn cli_client_exits_1_within_bounded_time_when_host_dies() {
    let mut host = Host::start("ptydie");
    let mut c = PtyClient::spawn(&host.sock, &host.dir, &["--ro"]);
    c.wait_for("ro");
    let t = Instant::now();
    host.kill9();
    // 3 reconnect attempts with 250/500/1000 ms backoff.
    assert_eq!(c.wait_exit(Duration::from_secs(15)), 1);
    assert!(t.elapsed() < Duration::from_secs(10), "reconnect must stay bounded: {:?}", t.elapsed());
}
