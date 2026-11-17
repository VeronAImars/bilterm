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
    let frame = encode_frame(state);
    if frame.len() - HEADER_LEN > MAX_HOST_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("host state exceeds {MAX_HOST_FRAME_BYTES} bytes"),
        ));
    }
    crate::secure_fs::write_private_file_durable(path, &frame)
}

/// Reads and decodes a host state file. The read is capped at one maximal frame, so a hostile or
/// runaway file can never be pulled fully into memory. Never panics: every failure is a message.
pub fn read_host_state(path: &Path) -> Result<HostState, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let cap = (HEADER_LEN + MAX_HOST_FRAME_BYTES) as u64;
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > cap {
        return Err(HostFrameError::TooLarge(bytes.len()).to_string());
    }
    decode_frame(&bytes).map_err(|e| format!("corrupt host state {}: {e}", path.display()))
}

/// Removes a host state file; a missing file is success. Returns whether a file was removed.
pub fn remove_host_state(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
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
    fn read_rejects_oversized_and_garbage_files_without_panic() {
        let dir = std::env::temp_dir().join(format!("bt-host-{}", crate::secure_fs::unique_suffix()));
        crate::secure_fs::create_private_dir_all(&dir).unwrap();
        let big = dir.join("host-big.state");
        std::fs::write(&big, vec![b'B'; HEADER_LEN + MAX_HOST_FRAME_BYTES + 100]).unwrap();
        assert!(read_host_state(&big).unwrap_err().contains("exceeds"));
        let junk = dir.join("host-junk.state");
        std::fs::write(&junk, b"not a frame at all").unwrap();
        assert!(read_host_state(&junk).unwrap_err().contains("bad host frame magic"));
        let bad_json = dir.join("host-bj.state");
        let mut f = b"BTHS".to_vec();
        f.extend_from_slice(&1u16.to_be_bytes());
        f.extend_from_slice(&3u32.to_be_bytes());
        f.extend_from_slice(b"{x}");
        std::fs::write(&bad_json, f).unwrap();
        assert!(read_host_state(&bad_json).unwrap_err().contains("invalid host frame payload"));
        assert!(read_host_state(&dir.join("missing")).unwrap_err().contains("cannot open"));
        assert!(remove_host_state(&junk) && !remove_host_state(&junk));
        let _ = std::fs::remove_dir_all(&dir);
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
