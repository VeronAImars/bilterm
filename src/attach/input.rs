//! Client input path (plan T-A5): strict `Input.data_b64` decoding, token-bucket rate limits and
//! the bounded request type that carries accepted input/`Act` to the host's control plane.
//! Everything here is pure (the clock is passed in) so it is unit-testable without a runtime.

use std::time::Instant;

use super::limits::{MAX_INPUT_B64_LEN, MAX_INPUT_BYTES};
use super::msg::PaneId;
use crate::action::Action;

/// Why an `Input` payload was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// Not canonical RFC 4648 base64 (bad alphabet, padding, length or trailing bits).
    BadBase64,
    /// Encoded text or decoded bytes exceed the per-message cap.
    TooLarge,
    /// Zero decoded bytes.
    Empty,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadBase64 => write!(f, "input is not valid base64"),
            Self::TooLarge => write!(f, "input exceeds {MAX_INPUT_BYTES} bytes"),
            Self::Empty => write!(f, "input is empty"),
        }
    }
}

fn sextet(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Strictly decodes padded base64. The length is checked before any allocation; unlike the
/// lenient graphics decoder, whitespace and stray characters are errors, not skipped.
pub fn decode_input_b64(s: &str) -> Result<Vec<u8>, InputError> {
    let b = s.as_bytes();
    if b.len() > MAX_INPUT_B64_LEN {
        return Err(InputError::TooLarge);
    }
    if b.is_empty() {
        return Err(InputError::Empty);
    }
    if b.len() % 4 != 0 {
        return Err(InputError::BadBase64);
    }
    let pad = b.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return Err(InputError::BadBase64);
    }
    let body = &b[..b.len() - pad];
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.chunks(4) {
        let mut v = [0u8; 4];
        for (i, &c) in chunk.iter().enumerate() {
            v[i] = sextet(c).ok_or(InputError::BadBase64)?;
        }
        out.push((v[0] << 2) | (v[1] >> 4));
        match chunk.len() {
            4 => {
                out.push((v[1] << 4) | (v[2] >> 2));
                out.push((v[2] << 6) | v[3]);
            }
            3 => {
                if v[2] & 0b11 != 0 {
                    return Err(InputError::BadBase64);
                }
                out.push((v[1] << 4) | (v[2] >> 2));
            }
            2 => {
                if v[1] & 0b1111 != 0 {
                    return Err(InputError::BadBase64);
                }
            }
            _ => return Err(InputError::BadBase64),
        }
    }
    if out.len() > MAX_INPUT_BYTES {
        return Err(InputError::TooLarge);
    }
    if out.is_empty() {
        return Err(InputError::Empty);
    }
    Ok(out)
}

/// Classic token bucket: `rate` tokens per second, at most `burst` stored.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Option<Instant>,
}

impl TokenBucket {
    /// Starts full so a paste within `burst` goes through immediately.
    pub fn new(rate_per_sec: usize, burst: usize) -> Self {
        Self { rate: rate_per_sec as f64, burst: burst as f64, tokens: burst as f64, last: None }
    }

    /// Takes `n` tokens at `now`; `false` (and nothing taken) when not enough are available.
    pub fn try_take(&mut self, n: usize, now: Instant) -> bool {
        if let Some(last) = self.last {
            let dt = now.saturating_duration_since(last).as_secs_f64();
            self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        }
        self.last = Some(now);
        let n = n as f64;
        if n > self.tokens {
            return false;
        }
        self.tokens -= n;
        true
    }
}

/// A validated, rate-limited request from an attach client, queued for the control plane.
/// Roles and bounds are enforced before this exists; the plane only applies it.
#[derive(Debug)]
pub enum HostRequest {
    /// Raw bytes for a pane's PTY (lowers to `Action::PaneInputBytes`).
    PaneInput { client: u64, pane: PaneId, bytes: Vec<u8> },
    /// A colon command already run through the normal parser.
    Act { client: u64, action: Action },
}

/// Bounded sender into the control plane (capacity `IN_QUEUE_FRAMES`).
pub type RequestSink = tokio::sync::mpsc::Sender<HostRequest>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn decodes_canonical_and_rejects_malformed() {
        assert_eq!(decode_input_b64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_input_b64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_input_b64("YWJj").unwrap(), b"abc");
        assert_eq!(decode_input_b64("YQ==").unwrap(), b"a");
        for bad in ["a", "aGk", "aG k=", "aGk=\n", "a*k=", "=GVs", "aG=k", "aGk===", "YR==", "YWN="] {
            assert_eq!(decode_input_b64(bad), Err(InputError::BadBase64), "{bad:?}");
        }
        assert_eq!(decode_input_b64(""), Err(InputError::Empty));
        assert_eq!(decode_input_b64("é!!!"), Err(InputError::BadBase64));
    }

    #[test]
    fn size_cap_is_exact() {
        let enc = |n: usize| crate::vt::codec::base64_encode(&vec![b'x'; n]);
        assert_eq!(decode_input_b64(&enc(MAX_INPUT_BYTES)).unwrap().len(), MAX_INPUT_BYTES);
        assert_eq!(decode_input_b64(&enc(MAX_INPUT_BYTES + 1)), Err(InputError::TooLarge));
        assert_eq!(decode_input_b64(&"A".repeat(MAX_INPUT_B64_LEN + 4)), Err(InputError::TooLarge));
    }

    #[test]
    fn roundtrips_all_byte_values() {
        let data: Vec<u8> = (0..=255u8).collect();
        for len in 1..=data.len() {
            let enc = crate::vt::codec::base64_encode(&data[..len]);
            assert_eq!(decode_input_b64(&enc).unwrap(), &data[..len]);
        }
    }

    #[test]
    fn bucket_bursts_then_refills() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(1000, 100);
        assert!(b.try_take(100, t0));
        assert!(!b.try_take(1, t0), "empty");
        assert!(!b.try_take(50, t0 + Duration::from_millis(10)), "10 tokens refilled only");
        assert!(b.try_take(50, t0 + Duration::from_millis(60)));
        assert!(!b.try_take(101, t0 + Duration::from_secs(60)), "never exceeds burst");
        assert!(b.try_take(100, t0 + Duration::from_secs(120)));
    }
}
