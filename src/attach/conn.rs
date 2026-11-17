//! One attach connection: bounded handshake (`Hello` -> `HelloOk`/`Refuse`) then a small control
//! loop (`Attach`, `Detach`, `Ping`). No screen data, input or resize yet (T-A4/A5/A6, T-A3).
//! Generic over the stream so tests drive it with in-memory duplex pipes. Never panics on bad
//! peers: every failure becomes a best-effort `Error`/`Refuse`/`Bye` and a close.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use tokio::time::timeout;

use super::frame::{Direction, Frame, FrameError, FrameReader, HANDSHAKE_MAX_PAYLOAD};
use super::handshake::{negotiate, HOST_PROTO_MAX, HOST_PROTO_MIN};
use super::limits::{HANDSHAKE_MAX_BYTES, HANDSHAKE_TIMEOUT, MAX_MISSED_PINGS, PING_INTERVAL};
use super::msg::{ByeReason, ClientMsg, ErrorCode, HostMsg, MsgError};
use super::registry::ClientSlot;

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
}

#[derive(Clone, Copy, Debug)]
pub struct ConnConfig {
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
    pub write_timeout: Duration,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: HANDSHAKE_TIMEOUT,
            idle_timeout: PING_INTERVAL * MAX_MISSED_PINGS,
            write_timeout: Duration::from_secs(5),
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
}

struct Io<S> {
    stream: S,
    reader: FrameReader,
    ready: VecDeque<Frame>,
    read_total: usize,
    cfg: ConnConfig,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Io<S> {
    /// Next complete frame; `Ok(None)` on clean EOF.
    async fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let mut buf = [0u8; READ_CHUNK];
        loop {
            if let Some(f) = self.ready.pop_front() {
                return Ok(Some(f));
            }
            let n = match self.stream.read(&mut buf).await {
                Ok(0) | Err(_) => return Ok(None),
                Ok(n) => n,
            };
            self.read_total = self.read_total.saturating_add(n);
            let frames = self.reader.push_all(&buf[..n])?;
            self.ready.extend(frames);
        }
    }

    async fn send(&mut self, msg: &HostMsg) -> bool {
        let Ok(bytes) = msg.encode() else { return false };
        matches!(timeout(self.cfg.write_timeout, async {
            self.stream.write_all(&bytes).await?;
            self.stream.flush().await
        })
        .await, Ok(Ok(())))
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
        reader: FrameReader::with_max_payload(HANDSHAKE_MAX_PAYLOAD),
        ready: VecDeque::new(),
        read_total: 0,
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
    io.reader.set_max_payload(Direction::ClientToHost.max_payload());
    let end = control_loop(&mut io, &slot, &mut shutdown).await;
    if matches!(end, ConnEnd::Idle) {
        io.send(&HostMsg::Bye { reason: ByeReason::IdleTimeout }).await;
    }
    end
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut Io<S>,
    slot: &ClientSlot,
    host: &HostInfo,
) -> Result<(), ConnEnd> {
    let frame = match io.next_frame().await {
        Ok(Some(f)) => f,
        Ok(None) => return Err(ConnEnd::Eof),
        Err(e) => return Err(refuse_frame_error(io, e).await),
    };
    if io.read_total > HANDSHAKE_MAX_BYTES + READ_CHUNK {
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

/// Frame-level failure before/after the handshake: version skew gets a `Refuse`, the rest an `Error`.
async fn refuse_frame_error<S: AsyncRead + AsyncWrite + Unpin>(io: &mut Io<S>, e: FrameError) -> ConnEnd {
    match e {
        FrameError::UnsupportedVersion(_) => {
            io.send(&refuse(e.to_string())).await;
            ConnEnd::Refused
        }
        _ => {
            let code = MsgError::Frame(e.clone()).code();
            io.send_error(code, &e.to_string()).await;
            ConnEnd::Protocol
        }
    }
}

async fn control_loop<S: AsyncRead + AsyncWrite + Unpin>(
    io: &mut Io<S>,
    slot: &ClientSlot,
    shutdown: &mut watch::Receiver<bool>,
) -> ConnEnd {
    let mut errors = 0u32;
    loop {
        let frame = tokio::select! {
            biased;
            _ = shutdown.changed() => {
                io.send(&HostMsg::Bye { reason: ByeReason::HostShutdown }).await;
                return ConnEnd::HostShutdown;
            }
            r = timeout(io.cfg.idle_timeout, io.next_frame()) => match r {
                Err(_) => return ConnEnd::Idle,
                Ok(Ok(Some(f))) => f,
                Ok(Ok(None)) => return ConnEnd::Eof,
                Ok(Err(e)) => return refuse_frame_error(io, e).await,
            },
        };
        match ClientMsg::decode(&frame) {
            Ok(msg) => match handle(io, slot, msg).await {
                Step::Continue => errors = 0,
                Step::Soft(code, text) => {
                    errors += 1;
                    if !io.send_error(code, text).await {
                        return ConnEnd::WriteFailed;
                    }
                }
                Step::End(end) => return end,
            },
            Err(e) => {
                errors += 1;
                if !io.send_error(e.code(), &e.to_string()).await {
                    return ConnEnd::WriteFailed;
                }
            }
        }
        if errors >= MAX_CONSECUTIVE_ERRORS {
            io.send_error(ErrorCode::RateLimited, "too many protocol errors").await;
            return ConnEnd::Protocol;
        }
    }
}

enum Step {
    Continue,
    Soft(ErrorCode, &'static str),
    End(ConnEnd),
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(io: &mut Io<S>, slot: &ClientSlot, msg: ClientMsg) -> Step {
    match msg {
        ClientMsg::Hello { .. } => Step::Soft(ErrorCode::BadRequest, "already greeted"),
        ClientMsg::Attach { session, role, cols, rows, .. } => {
            let mut reg = slot.registry().lock();
            if reg.is_attached(slot.id()) {
                return Step::Soft(ErrorCode::BadRequest, "already attached");
            }
            reg.set_attached(slot.id(), session, role, cols, rows);
            Step::Continue
        }
        ClientMsg::Detach {} => Step::End(ConnEnd::Detached),
        ClientMsg::Ping { n } => {
            if io.send(&HostMsg::Pong { n }).await { Step::Continue } else { Step::End(ConnEnd::WriteFailed) }
        }
        // Flow control and clipboard replies have no state to touch until T-A6 / P3.
        ClientMsg::Ack { .. } | ClientMsg::ClipboardReply { .. } => Step::Continue,
        ClientMsg::Input { .. } | ClientMsg::Act { .. } | ClientMsg::Resize { .. } => {
            Step::Soft(ErrorCode::UnknownMessage, "not supported by this host build yet")
        }
    }
}
