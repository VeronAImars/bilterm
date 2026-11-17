//! Per-client diffing by row hashes (plan section 12 #1, T-A4).
//!
//! `RowSnapshot.dirty` is refuted for multi-client use, so each client keeps only the row hashes
//! (plus cursor/mode scalars) of the last state it was sent: `O(rows)` memory, independent of PTY
//! throughput. The host builds one [`PaneFrame`] (a cloned `GridSnapshot`) per pane per tick and
//! every client diffs against it. `generation` is only the cheap "anything changed" gate.

use std::sync::OnceLock;

use super::wire_grid::{row_hash, WireCursor, WireDelta, WireGrid, WireRow};
use crate::vt::snapshot::{CursorSnapshot, GridSnapshot};

/// One pane's state for the current tick, shared by all clients.
#[derive(Debug)]
pub struct PaneFrame {
    snap: GridSnapshot,
    hashes: OnceLock<Vec<u64>>,
}

impl PaneFrame {
    pub fn new(snap: GridSnapshot) -> Self {
        Self { snap, hashes: OnceLock::new() }
    }

    pub fn snapshot(&self) -> &GridSnapshot {
        &self.snap
    }

    pub fn generation(&self) -> u64 {
        self.snap.generation
    }

    /// Row hashes, computed once and only if some client actually needs them.
    pub fn hashes(&self) -> &[u64] {
        self.hashes.get_or_init(|| self.snap.grid_rows.iter().map(row_hash).collect())
    }

    pub fn to_wire_grid(&self) -> WireGrid {
        WireGrid::from(&self.snap)
    }
}

/// What a client was last sent for one pane. Size is `O(rows)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientPaneView {
    pub generation: u64,
    rows: u16,
    cols: u16,
    hashes: Vec<u64>,
    cursor: CursorSnapshot,
    alt: bool,
    sb: (usize, usize),
}

pub enum DiffOutcome {
    /// Nothing the client can see has changed.
    Unchanged,
    Delta(WireDelta),
    /// Dimensions changed: only a full `Snapshot` is valid.
    NeedSnapshot,
}

impl ClientPaneView {
    /// The view a client has after being sent a full snapshot of `frame`.
    pub fn of(frame: &PaneFrame) -> Self {
        let s = frame.snapshot();
        Self {
            generation: s.generation,
            rows: s.rows,
            cols: s.cols,
            hashes: frame.hashes().to_vec(),
            cursor: s.cursor,
            alt: s.alternate_screen,
            sb: (s.scrollback_offset, s.total_scrollback_rows),
        }
    }

    /// Bytes of per-client state held (for bound assertions).
    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.hashes.len() * 8
    }

    /// Diffs `frame` against what this client has; on `Delta` the view advances to `frame`.
    pub fn advance(&mut self, frame: &PaneFrame) -> DiffOutcome {
        let s = frame.snapshot();
        if s.rows != self.rows || s.cols != self.cols {
            return DiffOutcome::NeedSnapshot;
        }
        // Cheap gate: generation equal and all scalar state equal => skip hashing entirely.
        let sb = (s.scrollback_offset, s.total_scrollback_rows);
        if s.generation == self.generation && s.cursor == self.cursor && s.alternate_screen == self.alt && sb == self.sb
        {
            return DiffOutcome::Unchanged;
        }
        let hashes = frame.hashes();
        let mut rows = Vec::new();
        for (y, (h, row)) in hashes.iter().zip(&s.grid_rows).enumerate() {
            if self.hashes.get(y) != Some(h) {
                rows.push((y as u16, WireRow::from_row(row)));
            }
        }
        let cursor = (s.cursor != self.cursor).then(|| WireCursor::from(&s.cursor));
        let alt = (s.alternate_screen != self.alt).then_some(s.alternate_screen);
        let sb_d = (sb != self.sb).then_some(sb);
        let changed = !rows.is_empty() || cursor.is_some() || alt.is_some() || sb_d.is_some();
        if !changed {
            // Client-visible state is identical: keep the old generation so `base_gen` stays what
            // the client was actually told.
            return DiffOutcome::Unchanged;
        }
        self.generation = s.generation;
        self.hashes.clear();
        self.hashes.extend_from_slice(hashes);
        self.cursor = s.cursor;
        self.alt = s.alternate_screen;
        self.sb = sb;
        DiffOutcome::Delta(WireDelta { rows, cursor, alt, sb: sb_d })
    }
}
