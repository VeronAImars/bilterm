//! Wire format of a live session transfer between two session hosts (unix).
//!
//! Stream layout (big-endian): `"BTXF" | version u16 | nfds u16 | body_len u32 | fnv1a64 u64`
//! (the 20-byte header travels with the first <=16 PTY master fds, `SCM_RIGHTS`), then
//! `body_len` bytes of JSON ([`Offer`]), then the remaining fds in batches of <=16, each batch
//! carried by one `F` byte. Afterwards both sides exchange length-prefixed JSON [`Msg`] frames.
//! The fd order is sessions in order, panes in order (`fd_index` = running pane count).

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

use crate::pane::handover::PaneBundle;
use crate::pty::fdpass::{self, MAX_FDS};
use crate::wm::split_tree::SplitTree;
use crate::wm::z_stack::ZStack;
use crate::pane::PaneId;
use crate::wm::WorkspaceLayoutMode;

pub const XFER_VERSION: u16 = 1;
const MAGIC: [u8; 4] = *b"BTXF";
const HEADER_LEN: usize = 20;
/// Largest offer body accepted (history of many panes); larger is refused before allocation.
pub const MAX_OFFER_BYTES: usize = 256 << 20;
pub const MAX_SESSIONS: usize = 256;
pub const MAX_PANES: usize = 1024;
const MAX_CTL_FRAME: usize = 64 << 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionBundle {
    pub name: String,
    /// Was the active session on the sender.
    pub active: bool,
    pub panes: Vec<PaneBundle>,
    pub split_tree: SplitTree,
    pub z_stack: ZStack,
    pub zoomed_pane: Option<PaneId>,
    pub layout_mode: WorkspaceLayoutMode,
    pub window_order: Vec<PaneId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub version: u16,
    pub writer_version: String,
    pub sessions: Vec<SessionBundle>,
    /// The sender will exit after a successful transfer (live restart).
    #[serde(default)]
    pub retire: bool,
}

impl Offer {
    pub fn pane_count(&self) -> usize {
        self.sessions.iter().map(|s| s.panes.len()).sum()
    }
}

/// Control messages after the offer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    Ready,
    Refuse { why: String },
    Commit,
    Abort,
    Adopted,
    Failed { why: String },
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Sends `offer` with `fds` (one per pane, in offer order).
pub fn send_offer(stream: &mut UnixStream, offer: &Offer, fds: &[RawFd]) -> io::Result<()> {
    let body = serde_json::to_vec(offer).map_err(|e| bad(e.to_string()))?;
    if body.len() > MAX_OFFER_BYTES {
        return Err(bad(format!("offer is {} bytes, limit {MAX_OFFER_BYTES}", body.len())));
    }
    if fds.is_empty() || fds.len() != offer.pane_count() || fds.len() > MAX_PANES {
        return Err(bad("fd count does not match pane count"));
    }
    let mut head = Vec::with_capacity(HEADER_LEN);
    head.extend_from_slice(&MAGIC);
    head.extend_from_slice(&XFER_VERSION.to_be_bytes());
    head.extend_from_slice(&(fds.len() as u16).to_be_bytes());
    head.extend_from_slice(&(body.len() as u32).to_be_bytes());
    head.extend_from_slice(&fnv1a64(&body).to_be_bytes());
    let mut chunks = fds.chunks(MAX_FDS);
    let first = chunks.next().unwrap_or(&[]);
    send_all_with_fds(stream, &head, first)?;
    stream.write_all(&body)?;
    for c in chunks {
        send_all_with_fds(stream, b"F", c)?;
    }
    Ok(())
}

fn send_all_with_fds(stream: &UnixStream, payload: &[u8], fds: &[RawFd]) -> io::Result<()> {
    let n = fdpass::send_fds(stream.as_raw_fd(), payload, fds)?;
    if n < payload.len() {
        // The fds went with the first bytes; the rest is ordinary data.
        (&*stream).write_all(&payload[n..])?;
    }
    Ok(())
}

/// Receives an offer and its fds. Every failure closes whatever arrived (owned fds drop).
pub fn recv_offer(stream: &mut UnixStream) -> io::Result<(Offer, Vec<OwnedFd>)> {
    let mut head = [0u8; HEADER_LEN];
    let (n, mut fds) = fdpass::recv_fds(stream.as_raw_fd(), &mut head)?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    stream.read_exact(&mut head[n..])?;
    if head[..4] != MAGIC {
        return Err(bad("bad transfer magic"));
    }
    let version = u16::from_be_bytes([head[4], head[5]]);
    if version != XFER_VERSION {
        return Err(bad(format!("unsupported transfer version {version} (this build speaks {XFER_VERSION})")));
    }
    let nfds = usize::from(u16::from_be_bytes([head[6], head[7]]));
    let len = u32::from_be_bytes([head[8], head[9], head[10], head[11]]) as usize;
    let sum = u64::from_be_bytes(head[12..20].try_into().map_err(|_| bad("header"))?);
    if nfds == 0 || nfds > MAX_PANES || len > MAX_OFFER_BYTES {
        return Err(bad("transfer header out of range"));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    if fnv1a64(&body) != sum {
        return Err(bad("offer checksum mismatch"));
    }
    while fds.len() < nfds {
        let mut one = [0u8; 1];
        let (m, more) = fdpass::recv_fds(stream.as_raw_fd(), &mut one)?;
        if m == 0 || more.is_empty() {
            return Err(bad("missing fd batch"));
        }
        fds.extend(more);
    }
    if fds.len() != nfds {
        return Err(bad("fd count mismatch"));
    }
    let offer: Offer = serde_json::from_slice(&body).map_err(|e| bad(format!("offer: {e}")))?;
    Ok((offer, fds))
}

pub fn write_msg(stream: &mut UnixStream, m: &Msg) -> io::Result<()> {
    let body = serde_json::to_vec(m).map_err(|e| bad(e.to_string()))?;
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)
}

pub fn read_msg(stream: &mut UnixStream) -> io::Result<Msg> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_CTL_FRAME {
        return Err(bad("control frame too large"));
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| bad(format!("control frame: {e}")))
}
