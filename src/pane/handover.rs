//! Pane side of live session transfer (unix): detach the PTY without killing the shell, export
//! the terminal state, and rebuild a pane around an adopted PTY on the other side.
//!
//! Order matters: the PTY is released first (`PtySession::release` stops the reader and hands
//! back the bytes it had already consumed), those bytes are fed to this pane's VT, and only then
//! is the VT exported. The adopter therefore never needs the in-flight bytes; whatever the child
//! writes afterwards stays in the kernel PTY buffer until the adopter's reader starts.

use serde::{Deserialize, Serialize};

use super::{CompositePane, PaneId};
use crate::error::{BiltermError, Result};
use super::history_store::HistoryStore;
use crate::pty::handover::{AdoptMeta, ReleasedPty};
use crate::pty::PtySession;

/// Serde for byte vectors as lowercase hex (compact enough, no new dependency).
pub mod hexbytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        const H: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(b.len() * 2);
        for &x in b {
            out.push(H[usize::from(x >> 4)] as char);
            out.push(H[usize::from(x & 15)] as char);
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() % 2 != 0 {
            return Err(serde::de::Error::custom("odd hex length"));
        }
        let nib = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(serde::de::Error::custom("bad hex digit")),
        };
        s.as_bytes().chunks(2).map(|p| Ok(nib(p[0])? << 4 | nib(p[1])?)).collect()
    }
}

/// Everything the adopting side needs about one pane besides its PTY master fd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBundle {
    pub id: PaneId,
    pub title: String,
    pub pid: u32,
    /// Size the PTY currently has.
    pub pty_rows: u16,
    pub pty_cols: u16,
    /// Size of the pane's terminal grid.
    pub vt_rows: u16,
    pub vt_cols: u16,
    pub shell_program: String,
    pub shell_args: Vec<String>,
    pub tracked_cwd: Option<std::path::PathBuf>,
    /// The terminal's history and screen as a replayable byte stream.
    #[serde(with = "hexbytes")]
    pub replay: Vec<u8>,
}

impl CompositePane {
    /// True when this pane is a live local PTY that can be transferred.
    pub fn can_transfer(&self) -> bool {
        !self.exited && self.transport.local_pty().is_some() && self.remote_target.is_none()
    }

    /// Detaches the PTY (the shell keeps running) and folds the bytes the old reader had already
    /// consumed into this pane's terminal. `Err` leaves the pane fully working.
    pub fn release_for_transfer(&mut self, patience: std::time::Duration) -> Result<ReleasedPty> {
        if !self.can_transfer() {
            return Err(BiltermError::Pty(format!("pane #{} has no transferable local PTY", self.id)));
        }
        let pty = self.transport.local_pty_mut().ok_or_else(|| BiltermError::Pty("no PTY".into()))?;
        let mut released = pty.release(patience)?;
        let pending = released.take_pending();
        self.ingest_pty_events(pending);
        Ok(released)
    }

    /// Puts a released PTY back (rollback); the pane keeps working as before.
    pub fn reclaim_after_transfer(&mut self, released: ReleasedPty) -> Result<()> {
        let pty = self.transport.local_pty_mut().ok_or_else(|| BiltermError::Pty("no PTY".into()))?;
        released.reclaim(pty)
    }

    /// Serialisable description of this (released) pane.
    pub fn export_bundle(&mut self, meta: AdoptMeta) -> PaneBundle {
        let (vt_rows, vt_cols) = self.vt.size();
        PaneBundle {
            id: self.id,
            title: self.title.clone(),
            pid: meta.pid,
            pty_rows: meta.rows,
            pty_cols: meta.cols,
            vt_rows,
            vt_cols,
            shell_program: self.shell_program.clone(),
            shell_args: self.shell_args.clone(),
            tracked_cwd: self.tracked_cwd.clone(),
            replay: self.vt.export_replay(),
        }
    }

    /// Rebuilds a pane around an adopted PTY: same id, title, terminal grid, scrollback and
    /// modes. When the app was on the alternate screen it is nudged to repaint.
    pub fn from_bundle(b: &PaneBundle, session: PtySession) -> Self {
        let mut pane = Self::new_base(
            b.id,
            b.title.clone(),
            b.vt_rows,
            b.vt_cols,
            crate::vt::scrollback::DEFAULT_INITIAL_LINES,
            Some(session),
            HistoryStore::new_live(b.id),
        );
        pane.shell_program = b.shell_program.clone();
        pane.shell_args = b.shell_args.clone();
        pane.tracked_cwd = b.tracked_cwd.clone();
        pane.vt.process(&b.replay);
        let _ = pane.vt.take_query_responses();
        pane.restore_persisted_history();
        pane
    }

    /// SIGWINCH nudge for apps that live on the alternate screen: their screen is not part of the
    /// replay, so make them repaint (resize by one column and back).
    pub fn nudge_repaint_after_transfer(&mut self) {
        if !self.vt.is_alternate_screen() {
            return;
        }
        if let Some(pty) = self.transport.local_pty() {
            let (r, c) = pty.size();
            if c > 1 && pty.resize(r, c - 1).is_ok() {
                let _ = pty.resize(r, c);
            }
        }
    }
}
