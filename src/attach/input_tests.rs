//! Client input path over in-memory duplex streams: role enforcement, base64/size bounds, rate
//! limits, `Act` parsing, queue back-pressure, and an end-to-end drain into a headless plane.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, watch};

use super::conn::{serve_connection, ConnConfig, ConnEnd, HostInfo};
use super::frame::{Direction, Frame, FrameReader};
use super::input::{HostRequest, RequestSink};
use super::limits::*;
use super::msg::*;
use super::registry::SharedRegistry;

struct T {
    s: DuplexStream,
    fr: FrameReader,
    q: VecDeque<Frame>,
    task: tokio::task::JoinHandle<ConnEnd>,
    _tx: watch::Sender<bool>,
}

impl T {
    async fn send(&mut self, m: &ClientMsg) {
        self.s.write_all(&m.encode().unwrap()).await.unwrap();
    }
    async fn recv(&mut self) -> HostMsg {
        let mut b = [0u8; 4096];
        loop {
            if let Some(f) = self.q.pop_front() {
                return HostMsg::decode(&f).unwrap();
            }
            let n = tokio::time::timeout(Duration::from_secs(2), self.s.read(&mut b)).await.unwrap().unwrap();
            assert!(n > 0, "connection closed");
            self.q.extend(self.fr.push_all(&b[..n]).unwrap());
        }
    }
    async fn expect_err(&mut self, want: ErrorCode) {
        match self.recv().await {
            HostMsg::Error { code, .. } => assert_eq!(code, want),
            m => panic!("expected {want:?}, got {m:?}"),
        }
    }
    /// Round-trips a ping: proves the connection is alive and prior messages were processed.
    async fn alive(&mut self) {
        self.send(&ClientMsg::Ping { n: 5 }).await;
        assert_eq!(self.recv().await, HostMsg::Pong { n: 5 });
    }
}

fn cfg() -> ConnConfig {
    ConnConfig {
        handshake_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_millis(500),
        ..ConnConfig::default()
    }
}

async fn start(role: Option<Role>, sink: Option<RequestSink>, cfg: ConnConfig) -> T {
    let reg = SharedRegistry::new(4);
    let (a, b) = duplex(1 << 16);
    let (tx, rx) = watch::channel(false);
    let slot = reg.reserve().unwrap();
    let host = Arc::new(HostInfo { name: "h".into(), pid: 1, version: "t".into(), caps: vec![], requests: sink });
    let task = tokio::spawn(serve_connection(b, slot, host, rx, cfg));
    let mut t = T { s: a, fr: FrameReader::new(Direction::HostToClient), q: VecDeque::new(), task, _tx: tx };
    t.send(&ClientMsg::Hello { proto_min: 1, proto_max: 1, client_kind: ClientKind::Tui, client_name: "c".into(), caps: vec![] }).await;
    assert!(matches!(t.recv().await, HostMsg::HelloOk { .. }));
    if let Some(role) = role {
        t.send(&ClientMsg::Attach { session: "main".into(), role, cols: 80, rows: 24, pane_filter: None }).await;
    }
    t
}

fn input(pane: usize, raw: &[u8]) -> ClientMsg {
    ClientMsg::Input { pane, data_b64: crate::vt::codec::base64_encode(raw) }
}

#[tokio::test]
async fn rw_input_and_act_are_queued() {
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let mut t = start(Some(Role::Rw), Some(tx), cfg()).await;
    t.send(&input(2, b"ls\r")).await;
    t.send(&ClientMsg::Act { cmd: ":broadcast on".into() }).await;
    t.send(&ClientMsg::Act { cmd: "broadcast off".into() }).await; // ':' optional, like wm.command
    t.alive().await;
    assert!(matches!(rx.try_recv().unwrap(), HostRequest::PaneInput { pane: 2, bytes, .. } if bytes == b"ls\r"));
    assert!(matches!(rx.try_recv().unwrap(), HostRequest::Act { action: crate::action::Action::BroadcastToggle(Some(true)), .. }));
    assert!(matches!(rx.try_recv().unwrap(), HostRequest::Act { action: crate::action::Action::BroadcastToggle(Some(false)), .. }));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn ro_client_cannot_input_or_act() {
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let mut t = start(Some(Role::Ro), Some(tx), cfg()).await;
    t.send(&input(1, b"rm -rf\r")).await;
    t.expect_err(ErrorCode::ReadOnly).await;
    t.send(&ClientMsg::Act { cmd: ":broadcast on".into() }).await;
    t.expect_err(ErrorCode::ReadOnly).await;
    t.alive().await;
    assert!(rx.try_recv().is_err(), "nothing reached the plane");
}

#[tokio::test]
async fn input_requires_attach() {
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let mut t = start(None, Some(tx), cfg()).await;
    t.send(&input(1, b"x")).await;
    t.expect_err(ErrorCode::BadRequest).await;
    t.send(&ClientMsg::Act { cmd: ":broadcast on".into() }).await;
    t.expect_err(ErrorCode::BadRequest).await;
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn malformed_base64_and_oversize_are_rejected() {
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let mut t = start(Some(Role::Rw), Some(tx), cfg()).await;
    for bad in ["not base64!", "aGk", "aGk=\n", "", "YR=="] {
        t.send(&ClientMsg::Input { pane: 1, data_b64: bad.into() }).await;
        t.expect_err(ErrorCode::BadRequest).await;
    }
    // Over the encoded cap: rejected by message validation before any decode.
    t.send(&ClientMsg::Input { pane: 1, data_b64: "A".repeat(MAX_INPUT_B64_LEN + 4) }).await;
    t.expect_err(ErrorCode::LimitExceeded).await;
    // Exactly at the cap is fine; one byte over (still within the encoded cap) is not.
    t.send(&input(1, &vec![b'a'; MAX_INPUT_BYTES])).await;
    t.send(&input(1, &vec![b'a'; MAX_INPUT_BYTES + 1])).await;
    t.expect_err(ErrorCode::LimitExceeded).await;
    t.alive().await;
    assert!(matches!(rx.try_recv().unwrap(), HostRequest::PaneInput { bytes, .. } if bytes.len() == MAX_INPUT_BYTES));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn input_rate_limit_applies_per_connection() {
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let c = ConnConfig { input_rate: 1, input_burst: 10, ..cfg() };
    let mut t = start(Some(Role::Rw), Some(tx), c).await;
    t.send(&input(1, b"12345678")).await; // 8 of 10 tokens
    t.send(&input(1, b"12345678")).await; // refused: ~2 left
    t.expect_err(ErrorCode::RateLimited).await;
    t.send(&input(1, b"12")).await; // small one still fits
    t.alive().await;
    assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).count(), 2);
}

#[tokio::test]
async fn default_limits_match_plan_and_admit_a_full_burst() {
    let c = ConnConfig::default();
    assert_eq!((c.input_rate, c.input_burst, c.act_per_sec), (1 << 20, 256 << 10, 100));
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let mut t = start(Some(Role::Rw), Some(tx), cfg()).await;
    // 256 KiB burst = 32 full frames; the queue holds IN_QUEUE_FRAMES (32).
    for _ in 0..32 {
        t.send(&input(1, &vec![b'p'; MAX_INPUT_BYTES])).await;
    }
    t.alive().await;
    assert_eq!(std::iter::from_fn(|| rx.try_recv().ok()).count(), 32);
}

#[tokio::test]
async fn act_validation_and_rate_limit() {
    let (tx, mut rx) = mpsc::channel(IN_QUEUE_FRAMES);
    let c = ConnConfig { act_per_sec: 2, ..cfg() };
    let mut t = start(Some(Role::Rw), Some(tx), c).await;
    t.send(&ClientMsg::Act { cmd: ":definitely-not-a-command".into() }).await;
    t.expect_err(ErrorCode::BadRequest).await;
    t.send(&ClientMsg::Act { cmd: "".into() }).await;
    t.expect_err(ErrorCode::BadRequest).await;
    t.send(&ClientMsg::Act { cmd: ":broadcast on".into() }).await;
    t.expect_err(ErrorCode::RateLimited).await; // third Act within the 2-token burst
    t.alive().await;
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn full_queue_is_backpressure_not_a_stall() {
    let (tx, _rx) = mpsc::channel(1);
    let mut t = start(Some(Role::Rw), Some(tx), cfg()).await;
    t.send(&input(1, b"a")).await;
    t.send(&input(1, b"b")).await;
    t.expect_err(ErrorCode::RateLimited).await;
    t.alive().await;
}

#[tokio::test]
async fn host_without_sink_reports_unsupported() {
    let mut t = start(Some(Role::Rw), None, cfg()).await;
    t.send(&input(1, b"a")).await;
    t.expect_err(ErrorCode::UnknownMessage).await;
    t.alive().await;
}

#[tokio::test]
async fn closed_plane_ends_the_connection() {
    let (tx, rx) = mpsc::channel(IN_QUEUE_FRAMES);
    drop(rx);
    let mut t = start(Some(Role::Rw), Some(tx), cfg()).await;
    t.send(&input(1, b"a")).await;
    assert_eq!(t.task.await.unwrap(), ConnEnd::HostShutdown);
}

#[tokio::test]
async fn end_to_end_into_headless_plane() {
    use crate::api::AutomationControlPlane;
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    let (tx, rx) = mpsc::channel(IN_QUEUE_FRAMES);
    plane.set_attach_requests(rx);
    let mut t = start(Some(Role::Rw), Some(tx), cfg()).await;
    assert!(!plane.wm.is_broadcast_input());
    t.send(&ClientMsg::Act { cmd: ":broadcast on".into() }).await;
    t.send(&input(77, b"to a pane that does not exist")).await; // refused host-side, no panic
    t.send(&input(1, b"x")).await; // headless pane: no PTY, still no panic
    t.alive().await;
    assert!(plane.drain_attach_requests());
    assert!(plane.wm.is_broadcast_input(), "Act lowered through the colon parser and WindowManager::apply");
    assert!(!plane.drain_attach_requests(), "queue empty");
}
