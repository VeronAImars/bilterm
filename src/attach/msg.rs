//! Attach message types (plan section 3.4) and their JSON payload codec.
//!
//! Payloads are `{"t":"<snake_case type>", ...}`. The header `msg_type` must agree with the `t`
//! tag. Unknown fields are ignored (forward compatibility). Grid-shaped bodies (`Layout`,
//! `Snapshot`, `Delta`, `Scrollback`) keep only their routing fields typed here and carry the rest
//! as opaque JSON; T-A4 introduces the `WireGrid` mirror types.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::frame::{encode_frame, Direction, Frame, FrameError, MsgType};
use super::limits::{self, *};

pub type PaneId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Tui,
    Gui,
    Ctl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Rw,
    Ro,
}

/// Wire error codes (never free-form). Unknown codes from a newer peer decode as `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    FrameTooLarge,
    UnknownMessage,
    ReadOnly,
    BadRequest,
    UnsupportedVersion,
    RateLimited,
    LimitExceeded,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    HostShutdown,
    Kicked,
    LaggedResyncFailed,
    IdleTimeout,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub panes: u32,
}

/// Client -> host messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello { proto_min: u16, proto_max: u16, client_kind: ClientKind, client_name: String, #[serde(default)] caps: Vec<String> },
    Attach { session: String, role: Role, cols: u16, rows: u16, #[serde(default)] pane_filter: Option<Vec<PaneId>> },
    Detach {},
    Input { pane: PaneId, data_b64: String },
    /// Colon-command text (`":split h"`); parsed host-side, never a serialized `Action`.
    Act { cmd: String },
    Resize { cols: u16, rows: u16, #[serde(default)] pixel_w: Option<u32>, #[serde(default)] pixel_h: Option<u32> },
    Ack { pane: PaneId, seq: u64 },
    Ping { n: u64 },
    ClipboardReply { req_id: u64, #[serde(default)] data_b64: Option<String>, #[serde(default)] denied: bool },
}

/// Host -> client messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    HelloOk { proto: u16, host_version: String, host_name: String, host_pid: u32, #[serde(default)] caps: Vec<String>, #[serde(default)] sessions: Vec<SessionInfo>, client_id: u64 },
    Refuse { reason: String, host_proto_min: u16, host_proto_max: u16 },
    Layout { #[serde(flatten)] body: Map<String, Value> },
    Snapshot { pane: PaneId, seq: u64, #[serde(rename = "gen")] generation: u64, #[serde(flatten)] body: Map<String, Value> },
    Delta { pane: PaneId, base_gen: u64, #[serde(rename = "gen")] generation: u64, seq: u64, #[serde(flatten)] body: Map<String, Value> },
    Scrollback { pane: PaneId, from: u64, #[serde(flatten)] body: Map<String, Value> },
    Bell { pane: PaneId },
    Title { pane: PaneId, title: String },
    Exit { pane: PaneId, #[serde(default)] status: Option<i32> },
    ClipboardSet { pane: PaneId, data_b64: String, kind: String },
    Resized { cols: u16, rows: u16, #[serde(default)] owner_client: Option<u64> },
    Error { code: ErrorCode, message: String },
    Pong { n: u64 },
    Bye { reason: ByeReason },
}

/// Why a payload could not be turned into (or from) a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MsgError {
    Frame(FrameError),
    /// Frame type is unknown for this direction (answer `unknown_message`, skip, continue).
    UnknownType(u8),
    /// Header type and JSON `t` tag disagree.
    TypeMismatch,
    /// Binary/compressed payloads are not supported in v1.
    BinaryUnsupported,
    Json(String),
    /// A field exceeded a cap in `limits` (what, detail).
    Invalid(&'static str),
}

impl MsgError {
    /// Wire code for the best-effort `Error` reply.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Frame(FrameError::TooLarge { .. }) => ErrorCode::FrameTooLarge,
            Self::Frame(FrameError::UnsupportedVersion(_)) => ErrorCode::UnsupportedVersion,
            Self::UnknownType(_) => ErrorCode::UnknownMessage,
            Self::Invalid(_) => ErrorCode::LimitExceeded,
            _ => ErrorCode::BadRequest,
        }
    }
}

impl std::fmt::Display for MsgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Frame(e) => write!(f, "{e}"),
            Self::UnknownType(t) => write!(f, "unknown attach message type {t:#04x}"),
            Self::TypeMismatch => write!(f, "attach message type does not match payload tag"),
            Self::BinaryUnsupported => write!(f, "binary attach payloads are not supported in v1"),
            Self::Json(e) => write!(f, "invalid attach payload: {e}"),
            Self::Invalid(w) => write!(f, "attach field out of bounds: {w}"),
        }
    }
}

impl std::error::Error for MsgError {}

impl From<FrameError> for MsgError {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}

impl ClientMsg {
    pub fn msg_type(&self) -> MsgType {
        match self {
            Self::Hello { .. } => MsgType::Hello,
            Self::Attach { .. } => MsgType::Attach,
            Self::Detach {} => MsgType::Detach,
            Self::Input { .. } => MsgType::Input,
            Self::Act { .. } => MsgType::Act,
            Self::Resize { .. } => MsgType::Resize,
            Self::Ack { .. } => MsgType::Ack,
            Self::Ping { .. } => MsgType::Ping,
            Self::ClipboardReply { .. } => MsgType::ClipboardReply,
        }
    }

    /// Bounds-checks every untrusted field. Run on every decoded client message.
    pub fn validate(&self) -> Result<(), MsgError> {
        let s = |v: &str, what| if v.len() > MAX_HELLO_STRING_BYTES { Err(MsgError::Invalid(what)) } else { Ok(()) };
        match self {
            Self::Hello { client_name, caps, .. } => {
                s(client_name, "client_name")?;
                if caps.len() > MAX_HELLO_CAPS {
                    return Err(MsgError::Invalid("caps count"));
                }
                caps.iter().try_for_each(|c| s(c, "cap"))
            }
            Self::Attach { session, cols, rows, pane_filter, .. } => {
                s(session, "session")?;
                limits::validate_dims(*cols, *rows).map_err(|_| MsgError::Invalid("attach size"))?;
                if pane_filter.as_ref().is_some_and(|p| p.len() > MAX_PANE_FILTER) {
                    return Err(MsgError::Invalid("pane_filter"));
                }
                Ok(())
            }
            Self::Input { data_b64, .. } => {
                if data_b64.len() > MAX_INPUT_B64_LEN { Err(MsgError::Invalid("input")) } else { Ok(()) }
            }
            Self::Act { cmd } => {
                if cmd.len() > MAX_ACT_CMD_BYTES { Err(MsgError::Invalid("act cmd")) } else { Ok(()) }
            }
            Self::Resize { cols, rows, pixel_w, pixel_h } => {
                limits::validate_dims(*cols, *rows).map_err(|_| MsgError::Invalid("resize size"))?;
                if pixel_w.is_some_and(|p| p > MAX_ATTACH_PIXELS) || pixel_h.is_some_and(|p| p > MAX_ATTACH_PIXELS) {
                    return Err(MsgError::Invalid("pixel size"));
                }
                Ok(())
            }
            Self::ClipboardReply { data_b64, .. } => {
                if data_b64.as_ref().is_some_and(|d| d.len() > MAX_CLIENT_FRAME_BYTES) {
                    Err(MsgError::Invalid("clipboard"))
                } else {
                    Ok(())
                }
            }
            Self::Detach {} | Self::Ack { .. } | Self::Ping { .. } => Ok(()),
        }
    }

    /// Encodes to a complete frame (client -> host cap enforced).
    pub fn encode(&self) -> Result<Vec<u8>, MsgError> {
        let payload = serde_json::to_vec(self).map_err(|e| MsgError::Json(e.to_string()))?;
        Ok(encode_frame(self.msg_type() as u8, 0, &payload, Direction::ClientToHost)?)
    }

    /// Decodes and validates a client -> host frame.
    pub fn decode(frame: &Frame) -> Result<Self, MsgError> {
        let ty = frame.msg_type(Direction::ClientToHost).ok_or(MsgError::UnknownType(frame.type_code))?;
        let msg: Self = parse_payload(frame)?;
        if msg.msg_type() != ty {
            return Err(MsgError::TypeMismatch);
        }
        msg.validate()?;
        Ok(msg)
    }
}

impl HostMsg {
    pub fn msg_type(&self) -> MsgType {
        match self {
            Self::HelloOk { .. } => MsgType::HelloOk,
            Self::Refuse { .. } => MsgType::Refuse,
            Self::Layout { .. } => MsgType::Layout,
            Self::Snapshot { .. } => MsgType::Snapshot,
            Self::Delta { .. } => MsgType::Delta,
            Self::Scrollback { .. } => MsgType::Scrollback,
            Self::Bell { .. } => MsgType::Bell,
            Self::Title { .. } => MsgType::Title,
            Self::Exit { .. } => MsgType::Exit,
            Self::ClipboardSet { .. } => MsgType::ClipboardSet,
            Self::Resized { .. } => MsgType::Resized,
            Self::Error { .. } => MsgType::Error,
            Self::Pong { .. } => MsgType::Pong,
            Self::Bye { .. } => MsgType::Bye,
        }
    }

    /// Encodes to a complete frame; fails with `FrameTooLarge` above the 4 MiB cap.
    pub fn encode(&self) -> Result<Vec<u8>, MsgError> {
        let payload = serde_json::to_vec(self).map_err(|e| MsgError::Json(e.to_string()))?;
        Ok(encode_frame(self.msg_type() as u8, 0, &payload, Direction::HostToClient)?)
    }

    pub fn decode(frame: &Frame) -> Result<Self, MsgError> {
        let ty = frame.msg_type(Direction::HostToClient).ok_or(MsgError::UnknownType(frame.type_code))?;
        let msg: Self = parse_payload(frame)?;
        if msg.msg_type() != ty {
            return Err(MsgError::TypeMismatch);
        }
        Ok(msg)
    }
}

fn parse_payload<T: serde::de::DeserializeOwned>(frame: &Frame) -> Result<T, MsgError> {
    if frame.flags != 0 {
        return Err(MsgError::BinaryUnsupported);
    }
    serde_json::from_slice(&frame.payload).map_err(|e| MsgError::Json(e.to_string()))
}
