//! Session-host state record and wire framing (P1 of `docs/plans/session-host-architecture.md`).
//!
//! A host daemon started with `--host-ptys` owns real PTYs. Everything a *successor* host would
//! need in order to take them over (live-upgrade handover, a later milestone) is captured in a
//! plain, versioned, serializable [`HostState`]. Raw file descriptors are deliberately **not** part
//! of the record: a handover passes them out-of-band (`SCM_RIGHTS`) keyed by [`HostPaneRecord::id`].
//!
//! Wire/disk framing (all integers big-endian): `magic "BTHS"` | `version u16` | `len u32` | `len`
//! bytes of JSON. The explicit version lets a newer host refuse (or migrate) an older record and
//! vice versa; unknown *fields* are tolerated, unknown *versions* are rejected.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Current `HostState` schema version (bump on any incompatible change).
pub const HOST_STATE_VERSION: u16 = 1;
const MAGIC: [u8; 4] = *b"BTHS";
const HEADER_LEN: usize = 10;
/// Upper bound on a frame payload; a larger length prefix is rejected before any allocation.
pub const MAX_HOST_FRAME_BYTES: usize = 1 << 20;

/// Who owns the PTY master fds backing the panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PtyOwnership {
    /// The recording host process spawned and owns them (P1).
    Host,
}

/// One host-owned pane. Contains no fds or handles; see the module docs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPaneRecord {
    pub id: usize,
    pub child_pid: Option<u32>,
    pub rows: u16,
    pub cols: u16,
    pub title: String,
}

/// Serializable snapshot of everything a host owns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    pub version: u16,
    pub host_name: String,
    pub host_pid: u32,
    pub ownership: PtyOwnership,
    pub active_pane: usize,
    pub panes: Vec<HostPaneRecord>,
}

/// Why a frame could not be decoded.
#[derive(Debug, PartialEq, Eq)]
pub enum HostFrameError {
    Truncated,
    BadMagic,
    UnsupportedVersion(u16),
    TooLarge(usize),
    Payload(String),
}

impl std::fmt::Display for HostFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated host frame"),
            Self::BadMagic => write!(f, "bad host frame magic"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported host state version {v} (this build speaks {HOST_STATE_VERSION})")
            }
            Self::TooLarge(n) => write!(f, "host frame payload {n} exceeds {MAX_HOST_FRAME_BYTES} bytes"),
            Self::Payload(e) => write!(f, "invalid host frame payload: {e}"),
        }
    }
}

impl std::error::Error for HostFrameError {}

/// Encodes `state` as one length-prefixed, versioned frame.
pub fn encode_frame(state: &HostState) -> Vec<u8> {
    let payload = serde_json::to_vec(state).unwrap_or_default();
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&state.version.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Decodes one frame; trailing bytes after the frame are ignored (stream framing).
pub fn decode_frame(bytes: &[u8]) -> Result<HostState, HostFrameError> {
    if bytes.len() < HEADER_LEN {
        return Err(HostFrameError::Truncated);
    }
    if bytes[..4] != MAGIC {
        return Err(HostFrameError::BadMagic);
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version == 0 || version > HOST_STATE_VERSION {
        return Err(HostFrameError::UnsupportedVersion(version));
    }
    let len = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
    if len > MAX_HOST_FRAME_BYTES {
        return Err(HostFrameError::TooLarge(len));
    }
    let payload = bytes[HEADER_LEN..].get(..len).ok_or(HostFrameError::Truncated)?;
    let state: HostState = serde_json::from_slice(payload).map_err(|e| HostFrameError::Payload(e.to_string()))?;
    if state.version != version {
        return Err(HostFrameError::UnsupportedVersion(state.version));
    }
    Ok(state)
}

/// `<runtime_dir>/host-<name>.state` (0600, atomic; the name is validated like daemon names).
pub fn host_state_path_in(runtime_dir: &Path, name: &str) -> Result<PathBuf, String> {
    crate::secure_fs::validate_safe_name(name)?;
    Ok(runtime_dir.join(format!("host-{name}.state")))
}

/// Durably writes `state` to `path` through `secure_fs`.
pub fn write_host_state(path: &Path, state: &HostState) -> std::io::Result<()> {
    crate::secure_fs::write_private_file_durable(path, &encode_frame(state))
}

/// Reads and decodes a host state file.
pub fn read_host_state(path: &Path) -> Result<HostState, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    decode_frame(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HostState {
        HostState {
            version: HOST_STATE_VERSION,
            host_name: "h".into(),
            host_pid: 42,
            ownership: PtyOwnership::Host,
            active_pane: 1,
            panes: vec![HostPaneRecord { id: 1, child_pid: Some(7), rows: 24, cols: 80, title: "sh".into() }],
        }
    }

    #[test]
    fn round_trips() {
        assert_eq!(decode_frame(&encode_frame(&sample())).unwrap(), sample());
    }

    #[test]
    fn rejects_bad_input() {
        let good = encode_frame(&sample());
        assert_eq!(decode_frame(&good[..5]), Err(HostFrameError::Truncated));
        assert_eq!(decode_frame(&good[..good.len() - 1]), Err(HostFrameError::Truncated));
        let mut bad = good.clone();
        bad[0] = b'X';
        assert_eq!(decode_frame(&bad), Err(HostFrameError::BadMagic));
        let mut newer = good.clone();
        newer[4..6].copy_from_slice(&(HOST_STATE_VERSION + 1).to_be_bytes());
        assert_eq!(decode_frame(&newer), Err(HostFrameError::UnsupportedVersion(HOST_STATE_VERSION + 1)));
        let mut huge = good.clone();
        huge[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(decode_frame(&huge), Err(HostFrameError::TooLarge(_))));
        let mut zero = good;
        zero[4..6].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(decode_frame(&zero), Err(HostFrameError::UnsupportedVersion(0)));
    }

    #[test]
    fn tolerates_unknown_fields() {
        let json = br#"{"version":1,"host_name":"h","host_pid":1,"ownership":"host","active_pane":1,"panes":[],"future":true}"#;
        let mut f = Vec::new();
        f.extend_from_slice(b"BTHS");
        f.extend_from_slice(&1u16.to_be_bytes());
        f.extend_from_slice(&(json.len() as u32).to_be_bytes());
        f.extend_from_slice(json);
        assert!(decode_frame(&f).is_ok());
    }

    #[test]
    fn state_file_is_private_and_validated() {
        let dir = std::env::temp_dir().join(format!("bt-host-{}", crate::secure_fs::unique_suffix()));
        crate::secure_fs::create_private_dir_all(&dir).unwrap();
        assert!(host_state_path_in(&dir, "../x").is_err());
        let p = host_state_path_in(&dir, "h").unwrap();
        write_host_state(&p, &sample()).unwrap();
        assert_eq!(read_host_state(&p).unwrap(), sample());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
