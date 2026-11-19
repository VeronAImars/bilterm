//! DOS mode ("DOS mode -- runs FreeDOS"), feature `dos` (off by default, never bundled).
//!
//! PR D1 scope: pinned manifest, downloader seam, SHA256 verification, per-user paths, and the
//! `bilterm dos setup|remove|doctor` CLI. The emulator backends, persistent C: image and the
//! `Action::Dos*` surface land in later PRs (see `docs/plans/dos-mode.md`).

pub mod cli;
pub mod doctor;
pub mod download;
pub mod manifest;
pub mod paths;
pub mod verify;
