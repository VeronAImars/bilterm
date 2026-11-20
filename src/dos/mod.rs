//! DOS mode ("DOS mode -- runs FreeDOS"), feature `dos` (off by default, never bundled).
//!
//! PR D1 scope: pinned manifest, downloader seam, SHA256 verification, per-user paths, and the
//! `bilterm dos setup|remove|doctor` CLI. PR D1b adds the [`backend`] trait with QEMU and mock
//! backends. PR D1c adds [`image`] (persistent C: image builder, lifecycle, lock, corruption path).
//! PR D2a adds [`launch`] (`bilterm dos run`, the qemu-curses PTY pane). The full `Action::Dos*` surface lands in a later PR (see `docs/plans/dos-mode.md`).

pub mod backend;
pub mod cli;
pub mod doctor;
pub mod download;
pub mod image;
pub mod launch;
pub mod manifest;
pub mod paths;
pub mod verify;
