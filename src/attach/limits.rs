//! Every attach-protocol cap in one place (`bounding-untrusted-resource-use`).
//!
//! Numbers come from plan sections 3.5, 11 (Q8) and 12 (T-A0 measurements: worst-case 500x200
//! row-RLE JSON grid is 2.6 MB, below the 4 MiB frame cap).

use std::time::Duration;

/// Max payload of a host -> client frame (snapshots).
pub const MAX_ATTACH_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Max payload of a client -> host frame (input/resize/ack are tiny).
pub const MAX_CLIENT_FRAME_BYTES: usize = 64 * 1024;
/// Max raw bytes in one `Input` message (before base64).
pub const MAX_INPUT_BYTES: usize = 8 * 1024;
/// Max length of the base64 text carrying `MAX_INPUT_BYTES` raw bytes (padded).
pub const MAX_INPUT_B64_LEN: usize = MAX_INPUT_BYTES.div_ceil(3) * 4;
/// Largest terminal width accepted in `Attach`/`Resize` (existing viewport clamp).
pub const MAX_ATTACH_COLS: u16 = 500;
/// Largest terminal height accepted in `Attach`/`Resize`.
pub const MAX_ATTACH_ROWS: u16 = 200;
/// Largest pixel dimension accepted in `Resize`.
pub const MAX_ATTACH_PIXELS: u32 = 32_768;
/// Max bytes of any single string field in `Hello`/`Attach`.
pub const MAX_HELLO_STRING_BYTES: usize = 256;
/// Max number of capability strings in `Hello`.
pub const MAX_HELLO_CAPS: usize = 32;
/// Max entries in `Attach.pane_filter`.
pub const MAX_PANE_FILTER: usize = 256;
/// Max bytes of an `Act` colon-command string.
pub const MAX_ACT_CMD_BYTES: usize = 4096;
/// Max bytes of a clipboard payload (matches the 1 MiB `ClipboardSet` ceiling).
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
/// Bytes a client may send before the handshake completes (pre-auth bound).
pub const HANDSHAKE_MAX_BYTES: usize = 4096;
/// Time allowed for the handshake to complete.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// Host-side limits consumed by later tasks; declared here so there is one source of truth.
pub const MAX_ATTACH_CLIENTS: usize = 16;
pub const MAX_UNACKED_FRAMES: usize = 4;
pub const OUT_QUEUE_FRAMES: usize = 8;
pub const OUT_QUEUE_BYTES: usize = 16 * 1024 * 1024;
pub const IN_QUEUE_FRAMES: usize = 32;
pub const LAG_DEADLINE: Duration = Duration::from_secs(5);
pub const KICK_DEADLINE: Duration = Duration::from_secs(30);
pub const PING_INTERVAL: Duration = Duration::from_secs(15);
pub const MAX_MISSED_PINGS: u32 = 3;
pub const INPUT_RATE_BYTES_PER_SEC: usize = 1024 * 1024;
pub const INPUT_BURST_BYTES: usize = 256 * 1024;
pub const MAX_ACT_PER_SEC: u32 = 100;

/// Why a size was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitError {
    ZeroDimension,
    TooWide(u16),
    TooTall(u16),
}

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroDimension => write!(f, "terminal size must be at least 1x1"),
            Self::TooWide(c) => write!(f, "cols {c} exceeds {MAX_ATTACH_COLS}"),
            Self::TooTall(r) => write!(f, "rows {r} exceeds {MAX_ATTACH_ROWS}"),
        }
    }
}

impl std::error::Error for LimitError {}

/// Validates a client-declared terminal size before anything is sized from it.
pub fn validate_dims(cols: u16, rows: u16) -> Result<(), LimitError> {
    if cols == 0 || rows == 0 {
        return Err(LimitError::ZeroDimension);
    }
    if cols > MAX_ATTACH_COLS {
        return Err(LimitError::TooWide(cols));
    }
    if rows > MAX_ATTACH_ROWS {
        return Err(LimitError::TooTall(rows));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_boundaries() {
        assert!(validate_dims(1, 1).is_ok());
        assert!(validate_dims(MAX_ATTACH_COLS, MAX_ATTACH_ROWS).is_ok());
        assert_eq!(validate_dims(0, 5), Err(LimitError::ZeroDimension));
        assert_eq!(validate_dims(5, 0), Err(LimitError::ZeroDimension));
        assert_eq!(validate_dims(MAX_ATTACH_COLS + 1, 5), Err(LimitError::TooWide(MAX_ATTACH_COLS + 1)));
        assert_eq!(validate_dims(5, MAX_ATTACH_ROWS + 1), Err(LimitError::TooTall(MAX_ATTACH_ROWS + 1)));
        assert!(validate_dims(u16::MAX, u16::MAX).is_err());
    }

    #[test]
    fn frame_caps_match_plan() {
        assert_eq!(MAX_ATTACH_FRAME_BYTES, 4 << 20);
        assert_eq!(MAX_CLIENT_FRAME_BYTES, 64 << 10);
        assert_eq!(MAX_INPUT_B64_LEN, 10_924);
        // A 500x200 worst-case measured snapshot (2.6 MB) fits.
        let measured_worst = std::hint::black_box(2_600_000usize);
        assert!(measured_worst < MAX_ATTACH_FRAME_BYTES);
    }
}
