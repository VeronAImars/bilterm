//! Bounded client->host input queue. Mirrors the host's token bucket (a fraction of it, so we
//! normally pace ourselves below the limit) and backs off when the host says `rate_limited`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::attach::input::TokenBucket;
use crate::attach::limits::{INPUT_BURST_BYTES, INPUT_RATE_BYTES_PER_SEC, MAX_INPUT_BYTES};

/// Pending input beyond this is dropped (and counted) instead of growing without bound.
pub const MAX_PENDING_BYTES: usize = 1024 * 1024;
const BACKOFF_BASE: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(2);

pub struct Outbox {
    q: VecDeque<Vec<u8>>,
    bytes: usize,
    bucket: TokenBucket,
    backoff_until: Option<Instant>,
    strikes: u32,
    pub dropped: usize,
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Outbox {
    pub fn new() -> Self {
        Self {
            q: VecDeque::new(),
            bytes: 0,
            bucket: TokenBucket::new(INPUT_RATE_BYTES_PER_SEC / 4 * 3, INPUT_BURST_BYTES / 4 * 3),
            backoff_until: None,
            strikes: 0,
            dropped: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    pub fn pending_bytes(&self) -> usize {
        self.bytes
    }

    /// Queues bytes, splitting into frames of at most `MAX_INPUT_BYTES`. Returns false when the
    /// queue is full and the input was dropped.
    pub fn push(&mut self, data: &[u8]) -> bool {
        if self.bytes + data.len() > MAX_PENDING_BYTES {
            self.dropped += data.len();
            return false;
        }
        for chunk in data.chunks(MAX_INPUT_BYTES) {
            self.q.push_back(chunk.to_vec());
        }
        self.bytes += data.len();
        true
    }

    /// Next chunk allowed to be sent at `now` (honours backoff and the token bucket).
    pub fn next(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.backoff_until.is_some_and(|t| now < t) {
            return None;
        }
        let len = self.q.front()?.len();
        if !self.bucket.try_take(len, now) {
            return None;
        }
        self.bytes -= len;
        self.q.pop_front()
    }

    /// Host answered `rate_limited`: pause, doubling up to `BACKOFF_MAX`.
    pub fn on_refused(&mut self, now: Instant) {
        let d = (BACKOFF_BASE * 2u32.saturating_pow(self.strikes.min(8))).min(BACKOFF_MAX);
        self.strikes = self.strikes.saturating_add(1);
        self.backoff_until = Some(now + d);
    }

    /// A chunk went out and no refusal followed: relax the backoff.
    pub fn on_sent(&mut self) {
        self.strikes = self.strikes.saturating_sub(1);
    }

    pub fn backing_off(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|t| now < t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_to_input_cap_and_orders() {
        let mut o = Outbox::new();
        assert!(o.push(&vec![b'a'; MAX_INPUT_BYTES * 2 + 5]));
        let t = Instant::now();
        let sizes: Vec<usize> = std::iter::from_fn(|| o.next(t)).map(|c| c.len()).collect();
        assert_eq!(sizes, vec![MAX_INPUT_BYTES, MAX_INPUT_BYTES, 5]);
        assert!(o.is_empty() && o.pending_bytes() == 0);
    }

    #[test]
    fn bounded_and_counts_drops() {
        let mut o = Outbox::new();
        assert!(o.push(&vec![0u8; MAX_PENDING_BYTES]));
        assert!(!o.push(b"x"));
        assert_eq!(o.dropped, 1);
        assert_eq!(o.pending_bytes(), MAX_PENDING_BYTES);
    }

    #[test]
    fn token_bucket_paces_large_paste() {
        let mut o = Outbox::new();
        let burst = INPUT_BURST_BYTES / 4 * 3;
        o.push(&vec![1u8; burst + MAX_INPUT_BYTES * 4]);
        let t0 = Instant::now();
        let mut sent = 0;
        while let Some(c) = o.next(t0) {
            sent += c.len();
        }
        assert!(sent <= burst && sent > burst - MAX_INPUT_BYTES, "sent {sent}");
        assert!(!o.is_empty());
        // After enough time the rest drains.
        let t1 = t0 + Duration::from_secs(1);
        while o.next(t1).is_some() {}
        assert!(o.is_empty());
    }

    #[test]
    fn refusal_backs_off_exponentially_and_recovers() {
        let mut o = Outbox::new();
        o.push(b"abc");
        let t = Instant::now();
        o.on_refused(t);
        assert!(o.backing_off(t) && o.next(t).is_none());
        assert!(o.next(t + Duration::from_millis(150)).is_some());
        for _ in 0..20 {
            o.on_refused(t);
        }
        assert!(o.backing_off(t + Duration::from_millis(1900)));
        assert!(!o.backing_off(t + Duration::from_millis(2100)), "capped at BACKOFF_MAX");
    }
}
