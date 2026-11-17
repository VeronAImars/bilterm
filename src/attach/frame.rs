//! Attach stream framing.
//!
//! ```text
//! magic "BTHS" (4) | proto_version u16 BE | msg_type u8 | flags u8 | len u32 BE | payload (len bytes)
//! ```
//!
//! `flags` bit0 = binary payload (reserved for v2); every other bit (including "compressed",
//! bit1) must be zero in v1. The length is validated against the direction's cap *before* any
//! payload is buffered or allocated. Nothing here panics on malformed input.

use super::limits::{HANDSHAKE_MAX_BYTES, MAX_ATTACH_FRAME_BYTES, MAX_CLIENT_FRAME_BYTES};

pub const MAGIC: [u8; 4] = *b"BTHS";
pub const HEADER_LEN: usize = 12;
/// Protocol version this build speaks.
pub const ATTACH_PROTO_VERSION: u16 = 1;
pub const FLAG_BINARY: u8 = 0b0000_0001;
pub const FLAG_COMPRESSED: u8 = 0b0000_0010;
/// Flag bits a v1 peer may set.
pub const KNOWN_FLAGS: u8 = FLAG_BINARY;

/// Which way a frame travels; picks the payload cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ClientToHost,
    HostToClient,
}

impl Direction {
    pub const fn max_payload(self) -> usize {
        match self {
            Self::ClientToHost => MAX_CLIENT_FRAME_BYTES,
            Self::HostToClient => MAX_ATTACH_FRAME_BYTES,
        }
    }
}

/// Payload cap for the pre-auth handshake (whole first frame must fit in 4 KiB).
pub const HANDSHAKE_MAX_PAYLOAD: usize = HANDSHAKE_MAX_BYTES - HEADER_LEN;

/// Message type codes. Client -> host are `0x01..=0x7f`, host -> client `0x80..=0xff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgType {
    Hello = 0x01,
    Attach = 0x02,
    Detach = 0x03,
    Input = 0x04,
    Act = 0x05,
    Resize = 0x06,
    Ack = 0x07,
    Ping = 0x08,
    ClipboardReply = 0x09,
    HelloOk = 0x81,
    Refuse = 0x82,
    Layout = 0x83,
    Snapshot = 0x84,
    Delta = 0x85,
    Scrollback = 0x86,
    Bell = 0x87,
    Title = 0x88,
    Exit = 0x89,
    ClipboardSet = 0x8a,
    Resized = 0x8b,
    Error = 0x8c,
    Pong = 0x8d,
    Bye = 0x8e,
}

impl MsgType {
    pub const ALL: [MsgType; 23] = [
        Self::Hello, Self::Attach, Self::Detach, Self::Input, Self::Act, Self::Resize, Self::Ack,
        Self::Ping, Self::ClipboardReply, Self::HelloOk, Self::Refuse, Self::Layout, Self::Snapshot,
        Self::Delta, Self::Scrollback, Self::Bell, Self::Title, Self::Exit, Self::ClipboardSet,
        Self::Resized, Self::Error, Self::Pong, Self::Bye,
    ];

    /// `None` for codes this build does not know (the caller answers `unknown_message`).
    pub fn from_u8(code: u8) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| *t as u8 == code)
    }

    pub const fn direction(self) -> Direction {
        if (self as u8) < 0x80 { Direction::ClientToHost } else { Direction::HostToClient }
    }
}

/// One decoded frame. `type_code` may be unknown; see [`Frame::msg_type`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub proto_version: u16,
    pub type_code: u8,
    pub flags: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Known message type for `dir`, or `None` (unknown or wrong-direction code: skip + report).
    pub fn msg_type(&self, dir: Direction) -> Option<MsgType> {
        MsgType::from_u8(self.type_code).filter(|t| t.direction() == dir)
    }
}

/// Why a frame was rejected or could not be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    BadMagic,
    /// Unsupported proto version (0 or newer than [`ATTACH_PROTO_VERSION`]). The only error that
    /// closes the connection for a protocol reason; carries the offending version.
    UnsupportedVersion(u16),
    /// Reserved flag bits set (the full flags byte).
    BadFlags(u8),
    /// Declared/actual payload length above the cap.
    TooLarge { len: usize, max: usize },
    /// Reader fed after a previous error.
    Poisoned,
}

impl FrameError {
    /// Wire error code to send best-effort before closing, where one applies.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "frame_too_large",
            Self::UnsupportedVersion(_) => "unsupported_version",
            _ => "bad_request",
        }
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "bad attach frame magic"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported attach protocol version {v} (this build speaks {ATTACH_PROTO_VERSION})")
            }
            Self::BadFlags(b) => write!(f, "reserved attach frame flags set: {b:#04x}"),
            Self::TooLarge { len, max } => write!(f, "attach frame payload {len} exceeds {max} bytes"),
            Self::Poisoned => write!(f, "attach frame reader previously failed"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Parsed, validated header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub proto_version: u16,
    pub type_code: u8,
    pub flags: u8,
    pub len: usize,
}

/// Validates the first [`HEADER_LEN`] bytes. `Ok(None)` when fewer than that are available.
/// Errors are reported as soon as the offending bytes are present, before any payload is read.
pub fn parse_header(buf: &[u8], max_payload: usize) -> Result<Option<Header>, FrameError> {
    // Report a magic mismatch as early as the differing byte arrives.
    let n = buf.len().min(4);
    if buf[..n] != MAGIC[..n] {
        return Err(FrameError::BadMagic);
    }
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    let proto_version = u16::from_be_bytes([buf[4], buf[5]]);
    if proto_version == 0 || proto_version > ATTACH_PROTO_VERSION {
        return Err(FrameError::UnsupportedVersion(proto_version));
    }
    let flags = buf[7];
    if flags & !KNOWN_FLAGS != 0 {
        return Err(FrameError::BadFlags(flags));
    }
    let len = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
    if len > max_payload {
        return Err(FrameError::TooLarge { len, max: max_payload });
    }
    Ok(Some(Header { proto_version, type_code: buf[6], flags, len }))
}

/// Encodes one frame at the current protocol version, enforcing the direction's cap.
pub fn encode_frame(type_code: u8, flags: u8, payload: &[u8], dir: Direction) -> Result<Vec<u8>, FrameError> {
    encode_frame_with_max(type_code, flags, payload, dir.max_payload())
}

/// As [`encode_frame`] with an explicit payload cap (e.g. [`HANDSHAKE_MAX_PAYLOAD`]).
pub fn encode_frame_with_max(
    type_code: u8,
    flags: u8,
    payload: &[u8],
    max_payload: usize,
) -> Result<Vec<u8>, FrameError> {
    if flags & !KNOWN_FLAGS != 0 {
        return Err(FrameError::BadFlags(flags));
    }
    if payload.len() > max_payload || u32::try_from(payload.len()).is_err() {
        return Err(FrameError::TooLarge { len: payload.len(), max: max_payload });
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&ATTACH_PROTO_VERSION.to_be_bytes());
    out.push(type_code);
    out.push(flags);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Stateless decode of the first frame in `buf`: `Ok(None)` = need more bytes, otherwise the frame
/// and the number of bytes it consumed. The payload is copied only once it is fully present.
pub fn decode_one(buf: &[u8], max_payload: usize) -> Result<Option<(Frame, usize)>, FrameError> {
    let Some(h) = parse_header(buf, max_payload)? else { return Ok(None) };
    let total = HEADER_LEN + h.len;
    let Some(payload) = buf.get(HEADER_LEN..total) else { return Ok(None) };
    let frame = Frame { proto_version: h.proto_version, type_code: h.type_code, flags: h.flags, payload: payload.to_vec() };
    Ok(Some((frame, total)))
}

/// Streaming reassembler with bounded memory: it never buffers more than one frame
/// (`HEADER_LEN + max_payload`) no matter how much the peer sends.
#[derive(Debug)]
pub struct FrameReader {
    max_payload: usize,
    buf: Vec<u8>,
    header: Option<Header>,
    failed: bool,
}

impl FrameReader {
    pub fn new(dir: Direction) -> Self {
        Self::with_max_payload(dir.max_payload())
    }

    pub fn with_max_payload(max_payload: usize) -> Self {
        Self { max_payload, buf: Vec::new(), header: None, failed: false }
    }

    /// Bytes currently buffered (always `<= HEADER_LEN + max_payload`).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    fn wanted(&self) -> usize {
        match self.header {
            Some(h) => HEADER_LEN + h.len,
            None => HEADER_LEN,
        }
    }

    fn complete(&self) -> bool {
        self.header.is_some() && self.buf.len() == self.wanted()
    }

    /// Consumes bytes from `data` up to the end of the current frame, returning how many were
    /// taken. Returns 0 while a completed frame awaits [`Self::next_frame`]. Errors are sticky.
    pub fn feed(&mut self, data: &[u8]) -> Result<usize, FrameError> {
        if self.failed {
            return Err(FrameError::Poisoned);
        }
        let take = self.wanted().saturating_sub(self.buf.len()).min(data.len());
        self.buf.extend_from_slice(&data[..take]);
        if self.header.is_none() {
            match parse_header(&self.buf, self.max_payload) {
                Ok(Some(h)) => {
                    self.header = Some(h);
                    self.buf.reserve(h.len); // already validated <= max_payload
                }
                Ok(None) => {}
                Err(e) => {
                    self.failed = true;
                    self.buf = Vec::new();
                    return Err(e);
                }
            }
        }
        Ok(take)
    }

    /// Takes the completed frame, if any, and resets for the next one.
    pub fn next_frame(&mut self) -> Option<Frame> {
        if !self.complete() {
            return None;
        }
        let h = self.header.take()?;
        let payload = self.buf.split_off(HEADER_LEN);
        self.buf.clear();
        Some(Frame { proto_version: h.proto_version, type_code: h.type_code, flags: h.flags, payload })
    }

    /// Feeds all of `data`, returning every frame completed by it.
    pub fn push_all(&mut self, mut data: &[u8]) -> Result<Vec<Frame>, FrameError> {
        let mut out = Vec::new();
        loop {
            let n = self.feed(data)?;
            data = &data[n..];
            match self.next_frame() {
                Some(f) => out.push(f),
                None if data.is_empty() => return Ok(out),
                None if n == 0 => return Ok(out), // unreachable in practice; guards against spinning
                None => {}
            }
        }
    }
}
