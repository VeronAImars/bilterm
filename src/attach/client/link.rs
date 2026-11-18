//! Framed async connection to an attach host: handshake plus bounded send/receive.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use crate::attach::frame::{Direction, Frame, FrameError, FrameReader, ATTACH_PROTO_VERSION};
use crate::attach::limits::HANDSHAKE_TIMEOUT;
use crate::attach::msg::{ClientKind, ClientMsg, ErrorCode, HostMsg, MsgError, Role};

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_CHUNK: usize = 16 * 1024;

/// Why the link failed.
#[derive(Debug)]
pub enum LinkError {
    Io(io::Error),
    /// Peer closed the stream.
    Eof,
    Frame(FrameError),
    Msg(MsgError),
    /// Host answered `Refuse`.
    Refused(String),
    /// Host answered the handshake with `Error`.
    Host(ErrorCode, String),
    /// Anything else unexpected during the handshake.
    Handshake(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Eof => write!(f, "host closed the connection"),
            Self::Frame(e) => write!(f, "{e}"),
            Self::Msg(e) => write!(f, "{e}"),
            Self::Refused(r) => write!(f, "host refused the connection: {r}"),
            Self::Host(c, m) => write!(f, "host error ({c:?}): {m}"),
            Self::Handshake(m) => write!(f, "handshake failed: {m}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// Facts from `HelloOk`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostInfo {
    pub proto: u16,
    pub host_name: String,
    pub host_pid: u32,
    pub host_version: String,
    pub client_id: u64,
}

pub struct Link<S> {
    stream: S,
    reader: FrameReader,
    ready: VecDeque<Frame>,
    pub info: HostInfo,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Link<S> {
    /// Runs `Hello`/`HelloOk` then `Attach` (role + terminal size), all within `HANDSHAKE_TIMEOUT`.
    pub async fn connect(stream: S, role: Role, cols: u16, rows: u16) -> Result<Self, LinkError> {
        let mut raw = Self {
            stream,
            reader: FrameReader::new(Direction::HostToClient),
            ready: VecDeque::new(),
            info: HostInfo { proto: 0, host_name: String::new(), host_pid: 0, host_version: String::new(), client_id: 0 },
        };
        match timeout(HANDSHAKE_TIMEOUT, raw.handshake(role, cols, rows)).await {
            Ok(Ok(())) => Ok(raw),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(LinkError::Handshake("timed out".into())),
        }
    }

    async fn handshake(&mut self, role: Role, cols: u16, rows: u16) -> Result<(), LinkError> {
        self.send(&ClientMsg::Hello {
            proto_min: 1,
            proto_max: ATTACH_PROTO_VERSION,
            client_kind: ClientKind::Tui,
            client_name: "bilterm-attach".into(),
            caps: Vec::new(),
        })
        .await?;
        match self.recv().await? {
            HostMsg::HelloOk { proto, host_version, host_name, host_pid, client_id, .. } => {
                self.info = HostInfo { proto, host_name, host_pid, host_version, client_id };
            }
            HostMsg::Refuse { reason, .. } => return Err(LinkError::Refused(reason)),
            HostMsg::Error { code, message } => return Err(LinkError::Host(code, message)),
            other => return Err(LinkError::Handshake(format!("unexpected {:?}", other.msg_type()))),
        }
        let session = self.info.host_name.clone();
        self.send(&ClientMsg::Attach { session, role, cols, rows, pane_filter: None }).await
    }

    /// Sends one message (bounded by a write timeout). Not cancel-safe: do not use in `select!`.
    pub async fn send(&mut self, msg: &ClientMsg) -> Result<(), LinkError> {
        let bytes = msg.encode().map_err(LinkError::Msg)?;
        match timeout(WRITE_TIMEOUT, async {
            self.stream.write_all(&bytes).await?;
            self.stream.flush().await
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(LinkError::Io(e)),
            Err(_) => Err(LinkError::Io(io::Error::new(io::ErrorKind::TimedOut, "write timed out"))),
        }
    }

    /// Next host message. Cancel-safe (no await between a read completing and its bytes being
    /// buffered), so it may be a `select!` branch.
    pub async fn recv(&mut self) -> Result<HostMsg, LinkError> {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            while let Some(f) = self.ready.pop_front() {
                match HostMsg::decode(&f) {
                    Ok(m) => return Ok(m),
                    // Unknown/newer message types are skipped (forward compatibility).
                    Err(MsgError::UnknownType(_)) => continue,
                    Err(e) => return Err(LinkError::Msg(e)),
                }
            }
            let n = self.stream.read(&mut buf).await.map_err(LinkError::Io)?;
            if n == 0 {
                return Err(LinkError::Eof);
            }
            let frames = self.reader.push_all(&buf[..n]).map_err(LinkError::Frame)?;
            self.ready.extend(frames);
        }
    }
}

/// Connects to a unix attach socket and verifies the server is the same user.
#[cfg(unix)]
pub async fn connect_unix(path: &std::path::Path, role: Role, cols: u16, rows: u16) -> Result<Link<tokio::net::UnixStream>, LinkError> {
    let stream = tokio::net::UnixStream::connect(path).await.map_err(LinkError::Io)?;
    let peer = stream.peer_cred().map_err(LinkError::Io)?;
    crate::secure_fs::check_peer_uid(peer.uid(), crate::secure_fs::current_euid()).map_err(LinkError::Io)?;
    Link::connect(stream, role, cols, rows).await
}

impl<S: AsyncRead + AsyncWrite + Unpin> Link<S> {
    /// Sends raw bytes for `pane` (caller keeps each chunk within `MAX_INPUT_BYTES`).
    pub async fn send_input(&mut self, pane: crate::attach::msg::PaneId, bytes: &[u8]) -> Result<(), LinkError> {
        self.send(&ClientMsg::Input { pane, data_b64: crate::vt::codec::base64_encode(bytes) }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::frame::{encode_frame, MsgType};

    async fn host_reply(mut s: tokio::io::DuplexStream, replies: Vec<HostMsg>) {
        let mut r = FrameReader::new(Direction::ClientToHost);
        let mut b = [0u8; 1024];
        let n = s.read(&mut b).await.unwrap();
        let f = r.push_all(&b[..n]).unwrap().remove(0);
        assert_eq!(f.type_code, MsgType::Hello as u8);
        for m in replies {
            s.write_all(&m.encode().unwrap()).await.unwrap();
        }
        // Keep open until the client has read.
        let _ = s.read(&mut b).await;
    }

    fn hello_ok() -> HostMsg {
        HostMsg::HelloOk {
            proto: 1,
            host_version: "t".into(),
            host_name: "h".into(),
            host_pid: 7,
            caps: vec![],
            sessions: vec![],
            client_id: 3,
        }
    }

    #[tokio::test]
    async fn handshake_ok_then_attach_is_sent() {
        let (a, mut b) = tokio::io::duplex(8192);
        let host = tokio::spawn(async move {
            let mut r = FrameReader::new(Direction::ClientToHost);
            let mut buf = [0u8; 1024];
            let mut got = Vec::new();
            loop {
                let n = b.read(&mut buf).await.unwrap();
                got.extend(r.push_all(&buf[..n]).unwrap());
                if got.len() == 1 {
                    b.write_all(&hello_ok().encode().unwrap()).await.unwrap();
                }
                if got.len() == 2 {
                    return got;
                }
            }
        });
        let link = Link::connect(a, Role::Ro, 80, 23).await.unwrap();
        assert_eq!(link.info.client_id, 3);
        let frames = host.await.unwrap();
        match ClientMsg::decode(&frames[1]).unwrap() {
            ClientMsg::Attach { role, cols, rows, session, .. } => assert_eq!((role, cols, rows, session.as_str()), (Role::Ro, 80, 23, "h")),
            m => panic!("{m:?}"),
        }
    }

    #[tokio::test]
    async fn refuse_and_error_are_surfaced() {
        let (a, b) = tokio::io::duplex(8192);
        tokio::spawn(host_reply(
            b,
            vec![HostMsg::Refuse { reason: "full".into(), host_proto_min: 1, host_proto_max: 1 }],
        ));
        assert!(matches!(Link::connect(a, Role::Rw, 80, 24).await, Err(LinkError::Refused(r)) if r == "full"));
        let (a, b) = tokio::io::duplex(8192);
        tokio::spawn(host_reply(b, vec![HostMsg::Error { code: ErrorCode::BadRequest, message: "x".into() }]));
        assert!(matches!(Link::connect(a, Role::Rw, 80, 24).await, Err(LinkError::Host(ErrorCode::BadRequest, _))));
    }

    #[tokio::test]
    async fn eof_and_garbage_and_unknown_types() {
        let (a, b) = tokio::io::duplex(8192);
        drop(b);
        assert!(Link::connect(a, Role::Rw, 80, 24).await.is_err());

        let (a, mut b) = tokio::io::duplex(8192);
        // Unknown host->client type 0xfe is skipped, then a Pong arrives.
        let mut bytes = encode_frame(0xfe, 0, b"{}", Direction::HostToClient).unwrap();
        bytes.extend(HostMsg::Pong { n: 9 }.encode().unwrap());
        b.write_all(&bytes).await.unwrap();
        let mut l = Link { stream: a, reader: FrameReader::new(Direction::HostToClient), ready: VecDeque::new(), info: HostInfo { proto: 1, host_name: String::new(), host_pid: 0, host_version: String::new(), client_id: 1 } };
        assert_eq!(l.recv().await.unwrap(), HostMsg::Pong { n: 9 });
        b.write_all(b"GARBAGEGARBAGE").await.unwrap();
        assert!(matches!(l.recv().await, Err(LinkError::Frame(_))));
    }
}
