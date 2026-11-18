//! Handshake, registry and listener behaviour over in-memory duplex streams, plus one real
//! unix-socket test.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, watch, Mutex};

use super::conn::{serve_connection, ConnConfig, ConnEnd, HostInfo};
use super::frame::{encode_frame, Direction, Frame, FrameReader};
use super::limits::*;
use super::listener::{serve_attach, AttachAcceptor};
use super::msg::*;
use super::registry::SharedRegistry;

fn host() -> Arc<HostInfo> {
    Arc::new(HostInfo { name: "h".into(), pid: 7, version: "t".into(), caps: vec![], requests: None, hub: None })
}

fn fast() -> ConnConfig {
    ConnConfig {
        handshake_timeout: Duration::from_millis(200),
        idle_timeout: Duration::from_millis(300),
        write_timeout: Duration::from_millis(200),
        ..ConnConfig::default()
    }
}

fn hello(min: u16, max: u16) -> Vec<u8> {
    ClientMsg::Hello { proto_min: min, proto_max: max, client_kind: ClientKind::Tui, client_name: "c".into(), caps: vec![] }
        .encode()
        .unwrap()
}

fn attach_msg() -> Vec<u8> {
    ClientMsg::Attach { session: "main".into(), role: Role::Rw, cols: 80, rows: 24, pane_filter: None }.encode().unwrap()
}

async fn read_host<R: AsyncReadExt + Unpin>(r: &mut R, fr: &mut FrameReader, q: &mut VecDeque<Frame>) -> Option<HostMsg> {
    let mut b = [0u8; 4096];
    loop {
        if let Some(f) = q.pop_front() {
            return Some(HostMsg::decode(&f).unwrap());
        }
        let n = tokio::time::timeout(Duration::from_secs(2), r.read(&mut b)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        q.extend(fr.push_all(&b[..n]).unwrap());
    }
}

struct Peer {
    stream: DuplexStream,
    fr: FrameReader,
    q: VecDeque<Frame>,
}

impl Peer {
    async fn send(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes).await;
    }
    async fn recv(&mut self) -> Option<HostMsg> {
        read_host(&mut self.stream, &mut self.fr, &mut self.q).await
    }
}

struct Harness {
    peer: Peer,
    reg: SharedRegistry,
    task: tokio::task::JoinHandle<ConnEnd>,
    tx: watch::Sender<bool>,
}

fn start(cfg: ConnConfig) -> Harness {
    let reg = SharedRegistry::new(4);
    let (a, b) = duplex(1 << 16);
    let (tx, rx) = watch::channel(false);
    let slot = reg.reserve().unwrap();
    let task = tokio::spawn(serve_connection(b, slot, host(), rx, cfg));
    Harness { peer: Peer { stream: a, fr: FrameReader::new(Direction::HostToClient), q: VecDeque::new() }, reg, task, tx }
}

#[tokio::test]
async fn hello_attach_ping_detach() {
    let mut h = start(fast());
    h.peer.send(&hello(1, 1)).await;
    match h.peer.recv().await.unwrap() {
        HostMsg::HelloOk { proto, host_name, host_pid, client_id, .. } => {
            assert_eq!((proto, host_name.as_str(), host_pid, client_id), (1, "h", 7, 1));
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(h.reg.to_json()["clients"][0]["state"], "ready");
    h.peer.send(&attach_msg()).await;
    h.peer.send(&ClientMsg::Ping { n: 9 }.encode().unwrap()).await;
    assert_eq!(h.peer.recv().await.unwrap(), HostMsg::Pong { n: 9 });
    let j = h.reg.to_json();
    assert_eq!(j["clients"][0]["role"], "rw");
    assert_eq!(j["clients"][0]["cols"], 80);
    // Second attach is a soft error; connection survives.
    h.peer.send(&attach_msg()).await;
    assert!(matches!(h.peer.recv().await.unwrap(), HostMsg::Error { code: ErrorCode::BadRequest, .. }));
    h.peer.send(&ClientMsg::Detach {}.encode().unwrap()).await;
    assert_eq!(h.task.await.unwrap(), ConnEnd::Detached);
    assert_eq!(h.reg.lock().len(), 0, "slot released");
}

#[tokio::test]
async fn version_skew_is_refused_with_host_range() {
    let mut h = start(fast());
    h.peer.send(&hello(50, 60)).await;
    match h.peer.recv().await.unwrap() {
        HostMsg::Refuse { host_proto_min, host_proto_max, .. } => assert_eq!((host_proto_min, host_proto_max), (1, 1)),
        m => panic!("{m:?}"),
    }
    assert_eq!(h.task.await.unwrap(), ConnEnd::Refused);
}

#[tokio::test]
async fn unsupported_header_version_is_refused() {
    let mut h = start(fast());
    let mut bytes = hello(1, 1);
    bytes[4..6].copy_from_slice(&9u16.to_be_bytes());
    h.peer.send(&bytes).await;
    assert!(matches!(h.peer.recv().await.unwrap(), HostMsg::Refuse { .. }));
    assert_eq!(h.task.await.unwrap(), ConnEnd::Refused);
}

#[tokio::test]
async fn non_hello_first_message_closes() {
    let mut h = start(fast());
    h.peer.send(&attach_msg()).await;
    assert!(matches!(h.peer.recv().await.unwrap(), HostMsg::Error { code: ErrorCode::BadRequest, .. }));
    assert_eq!(h.task.await.unwrap(), ConnEnd::HandshakeInvalid);
}

#[tokio::test]
async fn garbage_and_oversize_handshake_never_panic() {
    for junk in [vec![0xffu8; 64], b"GET / HTTP/1.1\r\n\r\n".to_vec(), vec![0u8; 3]] {
        let mut h = start(fast());
        h.peer.send(&junk).await;
        drop(h.peer.stream);
        let end = h.task.await.unwrap();
        assert!(matches!(end, ConnEnd::Protocol | ConnEnd::Eof | ConnEnd::HandshakeTimeout), "{end:?}");
    }
    // Declared payload far above the 4 KiB handshake cap is rejected before allocation.
    let mut h = start(fast());
    let big = encode_frame(super::frame::MsgType::Hello as u8, 0, &vec![b' '; 40_000], Direction::ClientToHost).unwrap();
    h.peer.send(&big[..16]).await;
    assert!(matches!(h.peer.recv().await.unwrap(), HostMsg::Error { code: ErrorCode::FrameTooLarge, .. }));
    assert_eq!(h.task.await.unwrap(), ConnEnd::Protocol);
}

#[tokio::test]
async fn slowloris_handshake_times_out_and_frees_slot() {
    let mut h = start(fast());
    h.peer.send(&hello(1, 1)[..5]).await; // partial header, then silence
    assert_eq!(h.task.await.unwrap(), ConnEnd::HandshakeTimeout);
    assert_eq!(h.reg.lock().len(), 0);
}

#[tokio::test]
async fn unknown_type_is_skipped_and_error_flood_closes() {
    let mut h = start(fast());
    h.peer.send(&hello(1, 1)).await;
    h.peer.recv().await.unwrap();
    // Host->client type code sent by a client: unknown for this direction.
    let bogus = Frame { proto_version: 1, type_code: super::frame::MsgType::Pong as u8, flags: 0, payload: b"{}".to_vec() };
    let mut raw = encode_frame(bogus.type_code, 0, &bogus.payload, Direction::ClientToHost).unwrap();
    h.peer.send(&raw).await;
    assert!(matches!(h.peer.recv().await.unwrap(), HostMsg::Error { code: ErrorCode::UnknownMessage, .. }));
    // Still alive.
    h.peer.send(&ClientMsg::Ping { n: 1 }.encode().unwrap()).await;
    assert_eq!(h.peer.recv().await.unwrap(), HostMsg::Pong { n: 1 });
    raw = encode_frame(bogus.type_code, 0, &bogus.payload, Direction::ClientToHost).unwrap();
    for _ in 0..super::conn::MAX_CONSECUTIVE_ERRORS + 2 {
        h.peer.send(&raw).await;
    }
    assert_eq!(h.task.await.unwrap(), ConnEnd::Protocol);
}

#[tokio::test]
async fn idle_client_gets_bye_and_shutdown_gets_bye() {
    let mut h = start(fast());
    h.peer.send(&hello(1, 1)).await;
    h.peer.recv().await.unwrap();
    assert_eq!(h.peer.recv().await.unwrap(), HostMsg::Bye { reason: ByeReason::IdleTimeout });
    assert_eq!(h.task.await.unwrap(), ConnEnd::Idle);

    let mut h = start(ConnConfig { idle_timeout: Duration::from_secs(30), ..fast() });
    h.peer.send(&hello(1, 1)).await;
    h.peer.recv().await.unwrap();
    h.tx.send(true).unwrap();
    assert_eq!(h.peer.recv().await.unwrap(), HostMsg::Bye { reason: ByeReason::HostShutdown });
    assert_eq!(h.task.await.unwrap(), ConnEnd::HostShutdown);
}

#[tokio::test]
async fn pipelined_partial_frame_after_hello_is_not_lost() {
    let mut h = start(fast());
    let ping = ClientMsg::Ping { n: 5 }.encode().unwrap();
    let mut bytes = hello(1, 1);
    bytes.extend_from_slice(&ping[..7]);
    h.peer.send(&bytes).await;
    h.peer.recv().await.unwrap();
    h.peer.send(&ping[7..]).await;
    assert_eq!(h.peer.recv().await.unwrap(), HostMsg::Pong { n: 5 });
}

/// In-memory acceptor: streams are pushed through a channel.
struct ChanAcceptor(Mutex<mpsc::Receiver<DuplexStream>>);

impl AttachAcceptor for ChanAcceptor {
    type Stream = DuplexStream;
    async fn accept(&self) -> std::io::Result<DuplexStream> {
        match self.0.lock().await.recv().await {
            Some(s) => Ok(s),
            None => std::future::pending().await,
        }
    }
}

#[tokio::test]
async fn client_cap_refuses_surplus_and_recovers() {
    let reg = SharedRegistry::new(MAX_ATTACH_CLIENTS);
    let (conn_tx, conn_rx) = mpsc::channel(64);
    let (sd_tx, sd_rx) = watch::channel(false);
    let srv = tokio::spawn(serve_attach(ChanAcceptor(Mutex::new(conn_rx)), reg.clone(), host(), sd_rx, fast_long()));

    let mut ok = Vec::new();
    for _ in 0..MAX_ATTACH_CLIENTS {
        let (a, b) = duplex(1 << 16);
        conn_tx.send(b).await.unwrap();
        let mut p = Peer { stream: a, fr: FrameReader::new(Direction::HostToClient), q: VecDeque::new() };
        p.send(&hello(1, 1)).await;
        assert!(matches!(p.recv().await.unwrap(), HostMsg::HelloOk { .. }));
        ok.push(p);
    }
    // A flood of extra connections: every one is refused, none registered.
    for _ in 0..200 {
        let (a, b) = duplex(1 << 16);
        conn_tx.send(b).await.unwrap();
        let mut p = Peer { stream: a, fr: FrameReader::new(Direction::HostToClient), q: VecDeque::new() };
        if let Some(m) = p.recv().await {
            assert!(matches!(m, HostMsg::Refuse { .. }), "{m:?}");
        }
    }
    assert_eq!(reg.lock().len(), MAX_ATTACH_CLIENTS);
    // Free one slot; a new client gets in.
    let mut gone = ok.pop().unwrap();
    gone.send(&ClientMsg::Detach {}.encode().unwrap()).await;
    for _ in 0..100 {
        if reg.lock().len() < MAX_ATTACH_CLIENTS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (a, b) = duplex(1 << 16);
    conn_tx.send(b).await.unwrap();
    let mut p = Peer { stream: a, fr: FrameReader::new(Direction::HostToClient), q: VecDeque::new() };
    p.send(&hello(1, 1)).await;
    assert!(matches!(p.recv().await.unwrap(), HostMsg::HelloOk { .. }));
    // Shutdown: live clients are told Bye.
    sd_tx.send(true).unwrap();
    assert_eq!(p.recv().await.unwrap(), HostMsg::Bye { reason: ByeReason::HostShutdown });
    tokio::time::timeout(Duration::from_secs(2), srv).await.unwrap().unwrap();
}

fn fast_long() -> ConnConfig {
    ConnConfig { idle_timeout: Duration::from_secs(30), ..fast() }
}

#[cfg(unix)]
#[tokio::test]
async fn real_unix_socket_is_0600_and_speaks_the_handshake() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("bt-a2-{}", crate::secure_fs::unique_suffix()));
    crate::secure_fs::create_private_dir_all(&dir).unwrap();
    let path = super::listener::attach_socket_path_in(&dir, "t").unwrap();
    assert!(path.ends_with("host-t.attach.sock"));
    assert!(super::listener::attach_socket_path_in(&dir, "../x").is_err());
    // A stale socket file from a crash is replaced; a regular file is refused.
    let (l0, g0) = super::listener::bind_attach_socket(&path).unwrap();
    std::mem::forget(g0);
    drop(l0);
    let (listener, guard) = super::listener::bind_attach_socket(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

    let reg = SharedRegistry::default();
    let (sd_tx, sd_rx) = watch::channel(false);
    let srv = tokio::spawn(serve_attach(listener, reg.clone(), host(), sd_rx, fast_long()));
    let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
    s.write_all(&hello(1, 1)).await.unwrap();
    let mut fr = FrameReader::new(Direction::HostToClient);
    let mut q = VecDeque::new();
    assert!(matches!(read_host(&mut s, &mut fr, &mut q).await.unwrap(), HostMsg::HelloOk { .. }));
    assert_eq!(reg.to_json()["count"], 1);
    sd_tx.send(true).unwrap();
    assert_eq!(read_host(&mut s, &mut fr, &mut q).await.unwrap(), HostMsg::Bye { reason: ByeReason::HostShutdown });
    srv.await.unwrap();
    drop(guard);
    assert!(!path.exists(), "socket removed");
    std::fs::write(&path, b"x").unwrap();
    assert!(super::listener::bind_attach_socket(&path).is_err(), "regular file never overwritten");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn peer_uid_check() {
    assert!(crate::secure_fs::check_peer_uid(5, 5).is_ok());
    let e = crate::secure_fs::check_peer_uid(0, 1000).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
}
