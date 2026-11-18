//! T-A6 tests: streaming glue over in-memory duplex pipes with a headless control plane, plus
//! failure injection (slow, dead, flooding, mid-frame disconnect). Paused tokio time throughout.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};
use tokio::sync::{mpsc, watch};

use super::conn::{ConnConfig, ConnEnd, HostInfo, serve_connection};
use super::frame::{Frame, FrameReader};
use super::hub::AttachHub;
use super::limits::*;
use super::msg::*;
use super::registry::SharedRegistry;
use crate::api::AutomationControlPlane;

struct Peer {
    s: DuplexStream,
    fr: FrameReader,
    q: VecDeque<Frame>,
}

impl Peer {
    async fn send(&mut self, m: &ClientMsg) {
        let _ = self.s.write_all(&m.encode().unwrap()).await;
    }
    async fn recv(&mut self) -> Option<HostMsg> {
        let mut b = [0u8; 8192];
        loop {
            if let Some(f) = self.q.pop_front() {
                return Some(HostMsg::decode(&f).unwrap());
            }
            let n = tokio::time::timeout(Duration::from_secs(2), self.s.read(&mut b)).await.ok()?.ok()?;
            if n == 0 {
                return None;
            }
            self.q.extend(self.fr.push_all(&b[..n]).unwrap());
        }
    }
    /// Next message matching `f`, skipping others.
    async fn until(&mut self, f: impl Fn(&HostMsg) -> bool) -> HostMsg {
        for _ in 0..200 {
            let m = self.recv().await.expect("stream ended before expected message");
            if f(&m) {
                return m;
            }
        }
        panic!("expected message not seen");
    }
}

struct Rig {
    plane: AutomationControlPlane,
    hub: Arc<AttachHub>,
    host: Arc<HostInfo>,
    reg: SharedRegistry,
    sd: watch::Sender<bool>,
}

fn rig() -> Rig {
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    let hub = Arc::new(AttachHub::new());
    plane.set_attach_hub(hub.clone());
    let (tx, rx) = mpsc::channel(IN_QUEUE_FRAMES);
    plane.set_attach_requests(rx);
    let host = Arc::new(HostInfo {
        name: "h".into(),
        pid: 1,
        version: "t".into(),
        caps: vec![],
        requests: Some(tx),
        hub: Some(hub.clone()),
    });
    let reg = SharedRegistry::new(MAX_ATTACH_CLIENTS);
    let (sd, _) = watch::channel(false);
    Rig { plane, hub, host, reg, sd }
}

impl Rig {
    fn connect(&self, pipe: usize) -> (Peer, tokio::task::JoinHandle<ConnEnd>) {
        let (a, b) = duplex(pipe);
        let slot = self.reg.reserve().unwrap();
        let (host, rx) = (self.host.clone(), self.sd.subscribe());
        let t = tokio::spawn(async move { serve_connection(b, slot, host, rx, ConnConfig::default()).await });
        (Peer { s: a, fr: FrameReader::with_max_payload(MAX_ATTACH_FRAME_BYTES), q: VecDeque::new() }, t)
    }

    async fn tick(&mut self) {
        self.plane.host_tick();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    fn feed(&mut self, pane: usize, bytes: &[u8]) {
        self.plane.feed_output_and_collect_events(pane, bytes).unwrap();
    }
}

async fn attach(rig: &Rig, pipe: usize, role: Role, cols: u16, rows: u16) -> (Peer, tokio::task::JoinHandle<ConnEnd>) {
    let (mut p, t) = rig.connect(pipe);
    p.send(&ClientMsg::Hello { proto_min: 1, proto_max: 1, client_kind: ClientKind::Tui, client_name: "t".into(), caps: vec![] })
        .await;
    assert!(matches!(p.recv().await, Some(HostMsg::HelloOk { .. })));
    p.send(&ClientMsg::Attach { session: "main".into(), role, cols, rows, pane_filter: None }).await;
    tokio::time::sleep(Duration::from_millis(30)).await; // let the connection join the hub
    (p, t)
}

fn is_snapshot_with(text: &'static str) -> impl Fn(&HostMsg) -> bool {
    move |m| matches!(m, HostMsg::Snapshot { .. } | HostMsg::Delta { .. }) && serde_json::to_string(m).unwrap().contains(text)
}

#[tokio::test(start_paused = true)]
async fn attach_streams_layout_snapshot_then_delta() {
    let mut r = rig();
    let (mut p, _t) = attach(&r, 1 << 16, Role::Rw, 80, 24).await;
    r.tick().await;
    let m = p.until(|m| matches!(m, HostMsg::Layout { .. })).await;
    assert!(matches!(m, HostMsg::Layout { .. }));
    let HostMsg::Snapshot { pane, seq, .. } = p.until(|m| matches!(m, HostMsg::Snapshot { .. })).await else { unreachable!() };
    p.send(&ClientMsg::Ack { pane, seq }).await;
    r.feed(1, b"hello-attach");
    r.tick().await;
    p.until(is_snapshot_with("hello-attach")).await;
}

#[tokio::test(start_paused = true)]
async fn input_refusals_are_answered_to_the_client() {
    let mut r = rig();
    let (mut p, _t) = attach(&r, 1 << 16, Role::Rw, 80, 24).await;
    r.tick().await;
    // Unknown pane.
    p.send(&ClientMsg::Input { pane: 99, data_b64: "aGk=".into() }).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    r.tick().await;
    let HostMsg::Error { code, .. } = p.until(|m| matches!(m, HostMsg::Error { .. })).await else { unreachable!() };
    assert_eq!(code, ErrorCode::BadRequest);
    // Pane locked read-only (PROT).
    r.plane.wm.pane_mut(1).unwrap().set_read_only(true);
    p.send(&ClientMsg::Input { pane: 1, data_b64: "aGk=".into() }).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    r.tick().await;
    let HostMsg::Error { code, message } = p.until(|m| matches!(m, HostMsg::Error { .. })).await else { unreachable!() };
    assert_eq!(code, ErrorCode::ReadOnly);
    assert!(message.contains("read-only"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn resize_goes_to_size_owner_and_owner_change_is_broadcast() {
    let mut r = rig();
    let (mut a, _ta) = attach(&r, 1 << 16, Role::Rw, 100, 30).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    r.tick().await;
    let HostMsg::Resized { cols, rows, owner_client } = a.until(|m| matches!(m, HostMsg::Resized { .. })).await else {
        unreachable!()
    };
    assert_eq!((cols, rows), (100, 30));
    let a_id = owner_client.expect("owner");
    // Applied through Action::AttachResize.
    let (pr, pc) = r.plane.wm.host_view().layout().panes.first().map(|p| (p.rows, p.cols)).unwrap();
    assert!(pc > 80 && pr > 24, "{pr}x{pc}");

    // A second rw client takes ownership: both are told.
    let (mut b, _tb) = attach(&r, 1 << 16, Role::Rw, 90, 28).await;
    tokio::time::sleep(Duration::from_millis(400)).await; // past the 150 ms resize debounce
    r.tick().await;
    let m = a.until(|m| matches!(m, HostMsg::Resized { owner_client, .. } if *owner_client != Some(a_id))).await;
    assert!(matches!(m, HostMsg::Resized { cols: 90, rows: 28, .. }), "{m:?}");
    b.until(|m| matches!(m, HostMsg::Resized { cols: 90, rows: 28, .. })).await;

    // ro clients cannot resize; bad dims are refused without touching the host.
    let (mut ro, _tr) = attach(&r, 1 << 16, Role::Ro, 40, 10).await;
    ro.send(&ClientMsg::Resize { cols: 50, rows: 12, pixel_w: None, pixel_h: None }).await;
    let HostMsg::Error { code, .. } = ro.until(|m| matches!(m, HostMsg::Error { .. })).await else { unreachable!() };
    assert_eq!(code, ErrorCode::ReadOnly);
    b.send(&ClientMsg::Resize { cols: 501, rows: 12, pixel_w: None, pixel_h: None }).await;
    let HostMsg::Error { code, .. } = b.until(|m| matches!(m, HostMsg::Error { .. })).await else { unreachable!() };
    assert_eq!(code, ErrorCode::LimitExceeded);
}

#[tokio::test(start_paused = true)]
async fn slow_client_is_resynced_then_kicked_and_host_tick_stays_cheap() {
    let mut r = rig();
    // 256-byte pipe and a peer that never reads.
    let (_p, t) = attach(&r, 256, Role::Rw, 80, 24).await;
    let mut worst = Duration::ZERO;
    for i in 0..80u32 {
        r.feed(1, format!("line {i} {}\r\n", "x".repeat(60)).as_bytes());
        let t0 = std::time::Instant::now();
        r.plane.host_tick();
        worst = worst.max(t0.elapsed());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(worst < Duration::from_millis(500), "host tick must not depend on the slow client: {worst:?}");
    assert_eq!(t.await.unwrap(), ConnEnd::Kicked);
    assert!(!r.hub.has_clients());
    assert!(r.reg.lock().is_empty(), "slot released");
}

#[tokio::test(start_paused = true)]
async fn dead_client_is_reaped_and_hub_forgets_it() {
    let mut r = rig();
    let (p, t) = attach(&r, 1 << 16, Role::Rw, 80, 24).await;
    r.tick().await;
    assert!(r.hub.has_clients());
    drop(p);
    for _ in 0..4 {
        r.feed(1, b"still running\r\n");
        r.tick().await;
    }
    let end = t.await.unwrap();
    assert!(matches!(end, ConnEnd::Eof | ConnEnd::WriteFailed), "{end:?}");
    assert!(!r.hub.has_clients());
    r.tick().await; // publishing with nobody attached is a no-op, not a panic
}

#[tokio::test(start_paused = true)]
async fn mid_frame_disconnect_does_not_wedge_the_host() {
    let mut r = rig();
    let (mut p, t) = attach(&r, 512, Role::Rw, 80, 24).await;
    r.tick().await;
    let mut b = [0u8; 64];
    let _ = p.s.read(&mut b).await; // read a fragment of a larger frame, then vanish
    drop(p);
    r.feed(1, b"x");
    r.tick().await;
    let end = t.await.unwrap();
    assert!(matches!(end, ConnEnd::Eof | ConnEnd::WriteFailed), "{end:?}");
    assert!(r.reg.lock().is_empty());
}

#[tokio::test(start_paused = true)]
async fn ping_flood_without_reading_is_bounded() {
    let r = rig();
    let (mut p, t) = attach(&r, 64, Role::Rw, 80, 24).await;
    for n in 0..2000 {
        p.send(&ClientMsg::Ping { n }).await;
    }
    let end = t.await.unwrap();
    assert!(matches!(end, ConnEnd::WriteFailed | ConnEnd::Kicked | ConnEnd::Eof), "{end:?}");
}

#[tokio::test(start_paused = true)]
async fn ack_from_the_future_is_rejected_and_detach_cleans_up() {
    let mut r = rig();
    let (mut p, t) = attach(&r, 1 << 16, Role::Rw, 80, 24).await;
    r.tick().await;
    p.until(|m| matches!(m, HostMsg::Snapshot { .. })).await;
    p.send(&ClientMsg::Ack { pane: 1, seq: 9_999 }).await;
    let HostMsg::Error { code, .. } = p.until(|m| matches!(m, HostMsg::Error { .. })).await else { unreachable!() };
    assert_eq!(code, ErrorCode::BadRequest);
    p.send(&ClientMsg::Detach {}).await;
    assert_eq!(t.await.unwrap(), ConnEnd::Detached);
    assert!(!r.hub.has_clients());
}

#[tokio::test(start_paused = true)]
async fn host_shutdown_sends_bye_to_streaming_clients() {
    let mut r = rig();
    let (mut p, t) = attach(&r, 1 << 16, Role::Ro, 80, 24).await;
    r.tick().await;
    let _ = r.sd.send(true);
    p.until(|m| matches!(m, HostMsg::Bye { reason: ByeReason::HostShutdown })).await;
    assert_eq!(t.await.unwrap(), ConnEnd::HostShutdown);
}

#[tokio::test(start_paused = true)]
async fn oversized_attach_is_refused_before_registering() {
    let r = rig();
    let (mut p, _t) = attach(&r, 1 << 16, Role::Rw, u16::MAX, u16::MAX).await;
    let HostMsg::Error { code, .. } = p.until(|m| matches!(m, HostMsg::Error { .. })).await else { unreachable!() };
    assert_eq!(code, ErrorCode::LimitExceeded);
    assert!(!r.hub.has_clients());
}

#[test]
fn discard_unsent_restores_flow_and_resends_snapshot() {
    use super::diff::PaneFrame;
    use super::layout::{WireLayout, WirePaneInfo};
    use super::session::{ClientSession, HostFrames};
    use crate::vt::VtParser;
    let vt = VtParser::new(24, 80, 100);
    let frames = HostFrames {
        layout: WireLayout {
            panes: vec![WirePaneInfo { id: 1, title: "p".into(), rows: 24, cols: 80, read_only: false }],
            z_order: vec![1],
            active: 1,
        },
        panes: [(1usize, PaneFrame::new(vt.snapshot()))].into_iter().collect(),
    };
    let mut s = ClientSession::new(Role::Rw, None);
    let p = s.pump(&frames, Duration::ZERO);
    assert!(p.msgs.iter().any(|m| matches!(m, HostMsg::Snapshot { .. })));
    assert_eq!(s.inflight(1), 1);
    s.discard_unsent();
    assert_eq!(s.inflight(1), 0);
    let p = s.pump(&frames, Duration::from_secs(1));
    assert!(p.msgs.iter().any(|m| matches!(m, HostMsg::Layout { .. })));
    assert!(p.msgs.iter().any(|m| matches!(m, HostMsg::Snapshot { .. })));
}
