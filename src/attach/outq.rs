//! Bounded per-client outbound queue of encoded frames (plan T-A6). The queue only ever holds
//! what the writer has not yet pushed into the socket; the diff pump refuses to add screen data
//! while it is non-empty, so a slow client never accumulates frames (state keeps coalescing in
//! the per-client row-hash view instead).

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::limits::{OUT_QUEUE_BYTES, OUT_QUEUE_FRAMES};
use super::msg::HostMsg;

/// Control replies (`Pong`, `Error`) may exceed the screen-data window by this factor before the
/// peer is treated as a flooder that never reads.
const HARD_FRAME_FACTOR: usize = 8;

#[derive(Debug, Default)]
pub struct OutQueue {
    q: VecDeque<Vec<u8>>,
    /// Bytes of the front chunk already written.
    off: usize,
    bytes: usize,
}

impl OutQueue {
    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    pub fn len(&self) -> usize {
        self.q.len()
    }

    /// Bytes still to be written.
    pub fn bytes(&self) -> usize {
        self.bytes - self.off
    }

    /// Encodes and queues `msg`. `false` means the hard bound is exceeded (caller must drop the
    /// client); an unencodable message (over the 4 MiB frame cap) is skipped and logged.
    pub fn push(&mut self, msg: &HostMsg) -> bool {
        match msg.encode() {
            Ok(b) => self.push_bytes(b),
            Err(e) => {
                tracing::warn!(error = %e, "attach: dropping unencodable host message");
                true
            }
        }
    }

    fn push_bytes(&mut self, b: Vec<u8>) -> bool {
        if self.q.len() >= OUT_QUEUE_FRAMES * HARD_FRAME_FACTOR || self.bytes + b.len() > OUT_QUEUE_BYTES {
            return false;
        }
        self.bytes += b.len();
        self.q.push_back(b);
        true
    }

    /// Writes some of the front chunk. Cancel-safe (single `write`), so it can sit in a `select!`.
    /// Returns bytes written.
    pub async fn write_some<W: AsyncWrite + Unpin>(&mut self, w: &mut W) -> io::Result<usize> {
        let Some(front) = self.q.front() else {
            return Ok(0);
        };
        let n = w.write(&front[self.off..]).await?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.off += n;
        if self.off == front.len() {
            self.bytes -= front.len();
            self.off = 0;
            self.q.pop_front();
        }
        Ok(n)
    }

    /// Drops every frame not yet started (a half-written frame must complete to keep the stream
    /// framed). Returns how many were dropped.
    pub fn discard_unstarted(&mut self) -> usize {
        let keep = usize::from(self.off > 0);
        let dropped = self.q.len().saturating_sub(keep);
        self.q.truncate(keep);
        self.bytes = self.q.front().map_or(0, Vec::len);
        dropped
    }

    /// Best-effort drain with an overall deadline (used for the final `Bye`/`Error`).
    pub async fn flush<W: AsyncWrite + Unpin>(&mut self, w: &mut W, limit: Duration) -> bool {
        let r = tokio::time::timeout(limit, async {
            while !self.q.is_empty() {
                self.write_some(w).await?;
            }
            w.flush().await
        })
        .await;
        matches!(r, Ok(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::msg::ByeReason;

    #[tokio::test]
    async fn partial_write_and_discard_keep_framing() {
        let mut q = OutQueue::default();
        for n in 0..4 {
            assert!(q.push(&HostMsg::Pong { n }));
        }
        let (mut a, mut b) = tokio::io::duplex(8);
        q.write_some(&mut a).await.unwrap(); // 8-byte pipe: at most part of the first chunk
        let dropped = q.discard_unstarted();
        assert!(dropped >= 3);
        assert!(q.len() <= 1);
        drop(a);
        let _ = &mut b;
    }

    #[test]
    fn hard_cap_rejects_flood() {
        let mut q = OutQueue::default();
        let mut ok = 0;
        while q.push(&HostMsg::Bye { reason: ByeReason::Kicked }) {
            ok += 1;
            assert!(ok <= OUT_QUEUE_FRAMES * HARD_FRAME_FACTOR);
        }
        assert_eq!(ok, OUT_QUEUE_FRAMES * HARD_FRAME_FACTOR);
    }
}
