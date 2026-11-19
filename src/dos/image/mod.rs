//! Persistent C: image for DOS mode (plan sections 5.1 and 5.1a, PR D1c).
//!
//! * [`build`] / [`livecd`] / [`mbr`]: pure-Rust image builder (hand-written MBR + `fatfs` FAT16),
//!   no mtools/sfdisk/dd. Members come from the pinned FreeDOS Live CD via the bounded readers in
//!   [`zipread`] and [`iso9660`].
//! * [`bootsector`]: the GPL-2.0+ FreeDOS FAT16 boot sector is never committed; it is captured once
//!   through a [`crate::dos::backend::DosBackend`] (guest `sys c:`) and cached in the data dir.
//! * [`store`] / [`lock`] / [`run`] / [`sanity`]: reset, snapshots, single-writer lock, dirty
//!   marker and the corruption path.
//! * [`cmd`] / [`provision`]: the `bilterm dos reset|snapshot|restore` CLI and setup provisioning.

pub mod bootsector;
pub mod build;
pub mod cmd;
pub mod extract;
pub mod iso9660;
pub mod livecd;
pub mod lock;
pub mod mbr;
pub mod provision;
pub mod run;
pub mod sanity;
pub mod store;
pub mod util;
pub mod zipread;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests_e2e;

use std::fmt;

pub const MIB: u64 = 1024 * 1024;
/// Default BASE image (plan 5.1): 128 MiB sparse raw.
pub const BASE_BYTES: u64 = 128 * MIB;
/// `--full` image: package set base apps archiver drivers net tools unix.
pub const FULL_BYTES: u64 = 256 * MIB;
/// Hard cap on any image (FAT16 limit; above 1 GiB is UNVERIFIED).
pub const MAX_IMAGE_BYTES: u64 = 2048 * MIB;
/// Smallest image the builder will format.
pub const MIN_IMAGE_BYTES: u64 = 16 * MIB;
/// Image name used when `--name` is not given.
pub const DEFAULT_NAME: &str = "default";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Base,
    Full,
}

impl ImageKind {
    pub fn bytes(self) -> u64 {
        match self {
            Self::Base => BASE_BYTES,
            Self::Full => FULL_BYTES,
        }
    }
    pub fn sets(self) -> &'static [&'static str] {
        match self {
            Self::Base => &["base"],
            Self::Full => &["base", "apps", "archiver", "drivers", "net", "tools", "unix"],
        }
    }
}

#[derive(Debug)]
pub enum ImageError {
    Io(String),
    Invalid(String),
    /// Malformed or hostile archive / disk structure.
    Format(String),
    Backend(String),
    LockHeld {
        pid: u32,
    },
    Missing(String),
    /// Sanity check failed; carries the problems found.
    Corrupt(Vec<String>),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(s) => write!(f, "io error: {s}"),
            Self::Invalid(s) => write!(f, "invalid: {s}"),
            Self::Format(s) => write!(f, "bad data: {s}"),
            Self::Backend(s) => write!(f, "emulator: {s}"),
            Self::LockHeld { pid } => {
                write!(f, "image is in use by another bilterm process (pid {pid})")
            }
            Self::Missing(s) => write!(f, "missing: {s}"),
            Self::Corrupt(p) => write!(f, "image failed sanity check: {}", p.join("; ")),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<std::io::Error> for ImageError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<crate::dos::backend::BackendError> for ImageError {
    fn from(e: crate::dos::backend::BackendError) -> Self {
        Self::Backend(e.to_string())
    }
}

pub type Res<T> = Result<T, ImageError>;
