//! One attach connection: bounded handshake (`Hello` -> `HelloOk`/`Refuse`) then a small control
//! loop (`Attach`, `Detach`, `Ping`, `Input`/`Act`/`Resize` for `rw` clients) that, once attached,
//! also streams screen data from the hub through a bounded outbound queue (T-A6).
//! Generic over the stream so tests drive it with in-memory duplex pipes. Never panics on bad
//! peers: every failure becomes a best-effort `Error`/`Refuse`/`Bye` and a close.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use tokio::time::timeout;

use super::frame::{Direction, Frame, FrameError, FrameReader, HANDSHAKE_MAX_PAYLOAD};
use super::handshake::{HOST_PROTO_MAX, HOST_PROTO_MIN, negotiate};
use super::hub::AttachHub;
use super::input::{HostRequest, InputError, RequestSink, TokenBucket, decode_input_b64};
use super::limits::validate_dims;
use super::limits::{
    HANDSHAKE_MAX_BYTES, HANDSHAKE_TIMEOUT, INPUT_BURST_BYTES, INPUT_RATE_BYTES_PER_SEC, MAX_ACT_PER_SEC, MAX_MISSED_PINGS,
    PING_INTERVAL,
};
use super::msg::{ByeReason, ClientMsg, ErrorCode, HostMsg, MsgError, Role};
use super::outq::OutQueue;
use super::registry::ClientSlot;
use super::session::{AckError, ClientSession};
use super::size_owner::SizeError;
use super::stream::{ConnStream, StreamEnd};
use crate::colon_command::parse_colon_command;

/// Consecutive recoverable protocol errors tolerated before the connection is dropped.
pub const MAX_CONSECUTIVE_ERRORS: u32 = 16;
/// Largest single socket read.
const READ_CHUNK: usize = 4096;

/// What the host says about itself in `HelloOk`.
#[derive(Clone, Debug)]
pub struct HostInfo {
    pub name: String,
    pub pid: u32,
    pub version: String,
    pub caps: Vec<String>,
    /// Bounded queue into the control plane for accepted `Input`/`Act`; `None` = not supported.
    pub requests: Option<RequestSink>,
    /// Streaming rendezvous with the control plane; `None` = control-only (no screen data).
    pub hub: Option<Arc<AttachHub>>,
}

#[derive(Clone, Copy, Debug)]
pub struct ConnConfig {
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
    pub write_timeout: Duration,
    /// Sustained `Input` bytes per second (decoded) and burst size.
    pub input_rate: usize,
    pub input_burst: usize,
    /// Sustained `Act` messages per second (burst equals one second's worth).
    pub act_per_sec: u32,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: HANDSHAKE_TIMEOUT,
            idle_timeout: PING_INTERVAL * MAX_MISSED_PINGS,
            write_timeout: Duration::from_secs(5),
            input_rate: INPUT_RATE_BYTES_PER_SEC,
            input_burst: INPUT_BURST_BYTES,
            act_per_sec: MAX_ACT_PER_SEC,
        }
    }
}

/// Why a connection ended (logging and tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnEnd {
    Eof,
    Detached,
    HandshakeTimeout,
    HandshakeInvalid,
    Refused,
    Protocol,
    Idle,
    HostShutdown,
    WriteFailed,
    /// Closed by the lag policy (`Bye{lagged_resync_failed}`) or an operator `attach.kick` (`Bye{kicked}`).
    Kicked,
}

struct Rx {
    reader: FrameReader,
    ready: VecDeque<Frame>,
    read_total: usize,
}

impl Rx {
    /// Next complete frame; `Ok(None)` on clean EOF. Cancel-safe (only `read` awaits).
    async fn next_frame<R: AsyncRead + Unpin>(&mut self, r: &mut R) -> Result<Option<Frame>, FrameError> {
        let mut buf = [0u8; READ_CHUNK];
        loop {
            if let Some(f) = self.ready.pop_front() {
                return Ok(Some(f));
            }
            let n = match r.read(&mut buf).await {
                Ok(0) | Err(_) => return Ok(None),
                Ok(n) => n,
            };
            self.read_total = self.read_total.saturating_add(n);
            let frames = self.reader.push_all(&buf[..n])?;
            self.ready.extend(frames);
        }
    }
}

struct Io<S> {
    stream: S,
    rx: Rx,
    cfg: ConnConfig,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Io<S> {
    async fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        self.rx.next_frame(&mut self.stream).await
    }

    /// Direct write; only used before the control loop owns the writer (handshake).
    async fn send(&mut self, msg: &HostMsg) -> bool {
        let Ok(bytes) = msg.encode() else {
            return false;
        };
        matches!(
            timeout(self.cfg.write_timeout, async {
                self.stream.write_all(&bytes).await?;
                self.stream.flush().await
            })
            .await,
            Ok(Ok(()))
        )
    }

    async fn send_error(&mut self, code: ErrorCode, message: &str) -> bool {
        self.send(&HostMsg::Error { code, message: message.to_string() }).await
    }
}

/// Runs the whole connection to completion. The slot is released when this returns.
pub async fn serve_connection<S>(
    stream: S,
    slot: ClientSlot,
    host: Arc<HostInfo>,
    mut shutdown: watch::Receiver<bool>,
    cfg: ConnConfig,
) -> ConnEnd
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut io = Io {
        stream,
        rx: Rx { reader: FrameReader::with_max_payload(HANDSHAKE_MAX_PAYLOAD), ready: VecDeque::new(), read_total: 0 },
        cfg,
    };
    match timeout(cfg.handshake_timeout, handshake(&mut io, &slot, &host)).await {
        Err(_) => {
            io.send_error(ErrorCode::BadRequest, "handshake timeout").await;
            return ConnEnd::HandshakeTimeout;
        }
        Ok(Err(end)) => return end,
        Ok(Ok(())) => {}
    }
    // Hello accepted: widen the cap for later headers; buffered/queued bytes are kept so a
    // pipelined follow-up frame is not lost.
    io.rx.reader.set_max_payload(Direction::ClientToHost.max_payload());
    control_loop(&mut io, &slot, &host, &mut shutdown).await
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(io: &mut Io<S>, slot: &ClientSlot, host: &HostInfo) -> Result<(), ConnEnd> {
    let frame = match io.next_frame().await {
        Ok(Some(f)) => f,
        Ok(None) => return Err(ConnEnd::Eof),
        Err(e) => {
            let (msg, end) = frame_error_reply(e);
            io.send(&msg).await;
            return Err(end);
        }
    };
    if io.rx.read_total > HANDSHAKE_MAX_BYTES + READ_CHUNK {
        io.send_error(ErrorCode::LimitExceeded, "handshake too large").await;
        return Err(ConnEnd::HandshakeInvalid);
    }
    let msg = match ClientMsg::decode(&frame) {
        Ok(m) => m,
        Err(e) => {
            io.send_error(e.code(), &e.to_string()).await;
            return Err(ConnEnd::HandshakeInvalid);
        }
    };
    let ClientMsg::Hello { proto_min, proto_max, client_kind, client_name, caps } = msg else {
        io.send_error(ErrorCode::BadRequest, "expected Hello").await;
        return Err(ConnEnd::HandshakeInvalid);
    };
    let proto = match negotiate(proto_min, proto_max) {
        Ok(p) => p,
        Err(r) => {
            io.send(&refuse(r.0)).await;
            return Err(ConnEnd::Refused);
        }
    };
    slot.registry().lock().set_hello(slot.id(), client_kind, client_name, caps);
    let ok = HostMsg::HelloOk {
        proto,
        host_version: host.version.clone(),
        host_name: host.name.clone(),
        host_pid: host.pid,
        caps: host.caps.clone(),
        // Session/pane listing arrives with `Layout` (T-A4); nothing to report from here.
        sessions: Vec::new(),
        client_id: slot.id(),
    };
    if io.send(&ok).await { Ok(()) } else { Err(ConnEnd::WriteFailed) }
}

fn refuse(reason: String) -> HostMsg {
    HostMsg::Refuse { reason, host_proto_min: HOST_PROTO_MIN, host_proto_max: HOST_PROTO_MAX }
}

/// Frame-level failure: version skew gets a `Refuse`, the rest an `Error`.
fn frame_error_reply(e: FrameError) -> (HostMsg, ConnEnd) {
    match e {
        FrameError::UnsupportedVersion(_) => (refuse(e.to_string()), ConnEnd::Refused),
        _ => {
            let code = MsgError::Frame(e.clone()).code();
            (HostMsg::Error { code, message: e.to_string() }, ConnEnd::Protocol)
        }
    }
}

/// Granularity of the deadline/idle/pump tick inside the control loop.
fn tick_period(cfg: &ConnConfig) -> Duration {
    (cfg.idle_timeout / 4).clamp(Duration::from_millis(10), Duration::from_millis(250))
}

fn soft(out: &mut OutQueue, code: ErrorCode, text: &str) -> bool {
    out.push(&HostMsg::Error { code, message: text.to_string() })
}

/// Streaming state created by a successful `Attach` when the host has a hub.
struct Streaming {
    conn: ConnStream,
    frames: watch::Receiver<Arc<super::hub::Published>>,
    replies: tokio::sync::mpsc::Receiver<HostMsg>,
}

/// Mutable streaming state kept apart from the two receivers so `select!` can poll them together.
struct StreamState {
    conn: ConnStream,
    latest: Option<Arc<super::hub::Published>>,
}

async fn control_loop<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut Io<S>,
    slot: &ClientSlot,
    host: &HostInfo,
    shutdown: &mut watch::Receiver<bool>,
) -> ConnEnd {
    let Io { stream, rx, cfg } = io;
    let cfg = *cfg;
    let (mut rd, mut wr) = tokio::io::split(stream);
    let mut out = OutQueue::default();
    let mut errors = 0u32;
    let mut limits = Limits {
        input: TokenBucket::new(cfg.input_rate, cfg.input_burst),
        act: TokenBucket::new(cfg.act_per_sec as usize, cfg.act_per_sec as usize),
    };
    let mut streaming: Option<StreamState> = None;
    let mut frames_rx: Option<watch::Receiver<Arc<super::hub::Published>>> = None;
    let mut replies_rx: Option<tokio::sync::mpsc::Receiver<HostMsg>> = None;
    let epoch = tokio::time::Instant::now();
    let mut last_rx = epoch;
    let mut tick = tokio::time::interval(tick_period(&cfg));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let end = loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                out.push(&HostMsg::Bye { reason: ByeReason::HostShutdown });
                break ConnEnd::HostShutdown;
            }
            () = slot.kicked() => {
                out.push(&HostMsg::Bye { reason: ByeReason::Kicked });
                break ConnEnd::Kicked;
            }
            w = out.write_some(&mut wr), if !out.is_empty() => match w {
                Ok(_) => {
                    if let Some(s) = streaming.as_mut() {
                        s.conn.on_progress(epoch.elapsed(), &out);
                    }
                }
                Err(_) => break ConnEnd::WriteFailed,
            },
            r = rx.next_frame(&mut rd) => {
                last_rx = tokio::time::Instant::now();
                let frame = match r {
                    Ok(Some(f)) => f,
                    Ok(None) => break ConnEnd::Eof,
                    Err(e) => {
                        let (msg, end) = frame_error_reply(e);
                        out.push(&msg);
                        break end;
                    }
                };
                let step = match ClientMsg::decode(&frame) {
                    Ok(msg) => handle(&mut out, streaming.as_mut().map(|s| &mut s.conn), slot, host, &mut limits, msg),
                    Err(e) => Step::Soft(e.code(), e.to_string()),
                };
                match step {
                    Step::Continue => errors = 0,
                    Step::Soft(code, text) => {
                        errors += 1;
                        if !soft(&mut out, code, &text) {
                            break ConnEnd::WriteFailed;
                        }
                    }
                    Step::Attached(j) => {
                        errors = 0;
                        let Streaming { conn, frames, replies } = *j;
                        streaming = Some(StreamState { conn, latest: None });
                        frames_rx = Some(frames);
                        replies_rx = Some(replies);
                    }
                    Step::End(end) => break end,
                }
                if errors >= MAX_CONSECUTIVE_ERRORS {
                    soft(&mut out, ErrorCode::RateLimited, "too many protocol errors");
                    break ConnEnd::Protocol;
                }
            }
            c = wait_frames(&mut frames_rx) => {
                if c.is_err() {
                    break ConnEnd::HostShutdown;
                }
                if let (Some(s), Some(f)) = (streaming.as_mut(), frames_rx.as_mut()) {
                    s.latest = Some(f.borrow_and_update().clone());
                    s.conn.mark_fresh();
                    if let Some(end) = stream_step(s, &mut out, epoch.elapsed(), false) {
                        break end;
                    }
                }
            }
            reply = next_reply(&mut replies_rx) => {
                match reply {
                    Some(m) => {
                        if !out.push(&m) {
                            break ConnEnd::WriteFailed;
                        }
                    }
                    None => replies_rx = None,
                }
            }
            _ = tick.tick() => {
                if last_rx.elapsed() >= cfg.idle_timeout {
                    out.push(&HostMsg::Bye { reason: ByeReason::IdleTimeout });
                    break ConnEnd::Idle;
                }
                if let Some(end) = streaming.as_mut().and_then(|s| stream_step(s, &mut out, epoch.elapsed(), true)) {
                    break end;
                }
            }
        }
    };
    // Best effort: deliver the last Error/Bye (bounded, so a dead peer cannot hold the task).
    out.flush(&mut wr, cfg.write_timeout.min(Duration::from_secs(1))).await;
    end
}

/// Pumps the diff session and applies the lag policy. `Some` = the connection must end.
fn stream_step(s: &mut StreamState, out: &mut OutQueue, now: Duration, deadlines: bool) -> Option<ConnEnd> {
    let latest = s.latest.clone()?;
    let mut r = s.conn.pump(&latest, out, now);
    if deadlines && r == StreamEnd::Continue {
        r = s.conn.check_stall(out, now);
    }
    match r {
        StreamEnd::Continue => None,
        StreamEnd::Overflow => Some(ConnEnd::WriteFailed),
        StreamEnd::Kick(reason) => {
            out.push(&HostMsg::Bye { reason });
            Some(ConnEnd::Kicked)
        }
    }
}

async fn wait_frames(s: &mut Option<watch::Receiver<Arc<super::hub::Published>>>) -> Result<(), watch::error::RecvError> {
    match s {
        Some(r) => r.changed().await,
        None => std::future::pending().await,
    }
}

async fn next_reply(s: &mut Option<tokio::sync::mpsc::Receiver<HostMsg>>) -> Option<HostMsg> {
    match s {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Per-connection rate limiters.
struct Limits {
    input: TokenBucket,
    act: TokenBucket,
}

enum Step {
    Continue,
    Soft(ErrorCode, String),
    Attached(Box<Streaming>),
    End(ConnEnd),
}

fn soft_step(code: ErrorCode, text: &str) -> Step {
    Step::Soft(code, text.to_string())
}

fn handle(
    out: &mut OutQueue,
    stream: Option<&mut ConnStream>,
    slot: &ClientSlot,
    host: &HostInfo,
    limits: &mut Limits,
    msg: ClientMsg,
) -> Step {
    match msg {
        ClientMsg::Hello { .. } => soft_step(ErrorCode::BadRequest, "already greeted"),
        ClientMsg::Attach { session, role, cols, rows, pane_filter } => {
            if slot.registry().lock().is_attached(slot.id()) {
                return soft_step(ErrorCode::BadRequest, "already attached");
            }
            if validate_dims(cols, rows).is_err() {
                return soft_step(ErrorCode::LimitExceeded, "terminal size out of bounds");
            }
            let joined = match &host.hub {
                Some(h) => match h.join(slot.id(), role, cols, rows) {
                    Ok(j) => Some(j),
                    Err(_) => {
                        return soft_step(ErrorCode::BadRequest, "cannot register terminal size");
                    }
                },
                None => None,
            };
            slot.registry().lock().set_attached(slot.id(), session, role, cols, rows);
            match joined {
                Some(j) => Step::Attached(Box::new(Streaming {
                    conn: ConnStream::new(ClientSession::new(role, pane_filter.as_deref()), j.guard),
                    frames: j.frames,
                    replies: j.replies,
                })),
                None => Step::Continue,
            }
        }
        ClientMsg::Detach {} => Step::End(ConnEnd::Detached),
        ClientMsg::Ping { n } => {
            if out.push(&HostMsg::Pong { n }) {
                Step::Continue
            } else {
                Step::End(ConnEnd::WriteFailed)
            }
        }
        ClientMsg::ClipboardReply { .. } => Step::Continue,
        ClientMsg::Ack { pane, seq } => match stream {
            Some(s) => match s.session.on_ack(pane, seq) {
                Ok(()) | Err(AckError::UnknownPane) => Step::Continue,
                Err(AckError::FromFuture) => soft_step(ErrorCode::BadRequest, "ack for a frame never sent"),
            },
            None => Step::Continue,
        },
        ClientMsg::Input { pane, data_b64 } => {
            let Some(sink) = &host.requests else {
                return unsupported();
            };
            if let Err(step) = require_rw(slot) {
                return step;
            }
            let bytes = match decode_input_b64(&data_b64) {
                Ok(b) => b,
                Err(InputError::TooLarge) => {
                    return soft_step(ErrorCode::LimitExceeded, "input too large");
                }
                Err(_) => return soft_step(ErrorCode::BadRequest, "invalid input payload"),
            };
            if !limits.input.try_take(bytes.len(), Instant::now()) {
                return soft_step(ErrorCode::RateLimited, "input rate limit exceeded");
            }
            if let Some(h) = &host.hub {
                h.activity(slot.id());
            }
            enqueue(sink, HostRequest::PaneInput { client: slot.id(), pane, bytes })
        }
        ClientMsg::Act { cmd } => {
            let Some(sink) = &host.requests else {
                return unsupported();
            };
            if let Err(step) = require_rw(slot) {
                return step;
            }
            if !limits.act.try_take(1, Instant::now()) {
                return soft_step(ErrorCode::RateLimited, "act rate limit exceeded");
            }
            // Same parser as the keyboard `:` prompt and `wm.command`; typed Actions are never on the wire.
            let text = if cmd.trim_start().starts_with(':') { cmd.trim().to_string() } else { format!(":{}", cmd.trim()) };
            match parse_colon_command(&text) {
                Some(Ok(action)) => enqueue(sink, HostRequest::Act { client: slot.id(), action }),
                _ => soft_step(ErrorCode::BadRequest, "not a valid colon command"),
            }
        }
        ClientMsg::Resize { cols, rows, .. } => {
            let Some(hub) = &host.hub else {
                return unsupported();
            };
            if let Err(step) = require_rw(slot) {
                return step;
            }
            match hub.resize(slot.id(), cols, rows) {
                Ok(()) => Step::Continue,
                Err(SizeError::BadDims(_)) => soft_step(ErrorCode::LimitExceeded, "terminal size out of bounds"),
                Err(SizeError::ReadOnly) => soft_step(ErrorCode::ReadOnly, "read-only client"),
                Err(_) => soft_step(ErrorCode::BadRequest, "cannot resize"),
            }
        }
    }
}

fn unsupported() -> Step {
    soft_step(ErrorCode::UnknownMessage, "not supported by this host build yet")
}

/// Input and `Act` need a completed `Attach` with the `rw` role; `ro` observers are refused.
fn require_rw(slot: &ClientSlot) -> Result<(), Step> {
    match slot.registry().lock().role_of(slot.id()) {
        Some(Role::Rw) => Ok(()),
        Some(Role::Ro) => Err(soft_step(ErrorCode::ReadOnly, "read-only client")),
        None => Err(soft_step(ErrorCode::BadRequest, "attach first")),
    }
}

/// Never awaits the plane: a full queue is back-pressure on this client, not a stall.
fn enqueue(sink: &RequestSink, req: HostRequest) -> Step {
    match sink.try_send(req) {
        Ok(()) => Step::Continue,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => soft_step(ErrorCode::RateLimited, "host busy"),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Step::End(ConnEnd::HostShutdown),
    }
}
