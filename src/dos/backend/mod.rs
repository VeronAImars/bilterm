//! DOS emulator backend contract (plan section 4.3).
//!
//! Hosts depend only on [`DosBackend`]; QEMU ([`qemu`]) and the deterministic [`mock`] backend
//! are interchangeable. Nothing here is wired to `Action`/colon/RPC yet (that is D3).

pub mod hostkeys;
pub mod keys;
pub mod mock;
pub mod qemu;
pub mod qmp;
pub mod render;
pub mod screen;
pub mod textbuf;

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod textbuf_tests;

use std::fmt;
use std::path::PathBuf;

use super::doctor::EmulatorProbe;
use super::paths::HostOs;
use super::verify::{Version, parse_qemu_version};

pub use keys::DosKey;
pub use screen::{TextCell, TextScreen};

/// Guest VGA text geometry in v1.
pub const GUEST_COLS: u16 = 80;
pub const GUEST_ROWS: u16 = 25;

#[derive(Debug)]
pub enum BackendError {
    /// The operation is not offered by this backend (see [`BackendCaps`]).
    Unsupported(&'static str),
    /// Rejected input (paths, sizes, key names). Never reaches a process.
    Invalid(String),
    /// Emulator missing, too old, or lacks a needed display.
    Probe(String),
    /// Process spawn / lifecycle failure.
    Process(String),
    /// QMP protocol or transport failure.
    Qmp(String),
    NotRunning,
    Io(std::io::Error),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::Invalid(s) => write!(f, "invalid: {s}"),
            Self::Probe(s) => write!(f, "emulator probe failed: {s}"),
            Self::Process(s) => write!(f, "emulator process: {s}"),
            Self::Qmp(s) => write!(f, "qmp: {s}"),
            Self::NotRunning => write!(f, "emulator is not running"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, BackendError>;

/// Backend-neutral session description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSpec {
    /// Persistent C: image (raw). `None` boots from `floppy` only.
    pub c_image: Option<PathBuf>,
    pub floppy: Option<PathBuf>,
    /// Extra raw drives, attached after C:.
    pub extra_drives: Vec<PathBuf>,
    pub mem_mb: u32,
    pub size_cols: u16,
    pub size_rows: u16,
    /// Floppy and extra drives are attached `readonly=on`.
    pub readonly_media: bool,
    /// Throwaway run: C: is attached with `snapshot=on` (writes discarded).
    pub throwaway: bool,
}

impl Default for SessionSpec {
    fn default() -> Self {
        Self {
            c_image: None,
            floppy: None,
            extra_drives: Vec::new(),
            mem_mb: 32,
            size_cols: GUEST_COLS,
            size_rows: GUEST_ROWS,
            readonly_media: true,
            throwaway: false,
        }
    }
}

/// Where the control channel (QMP) listens. Never exposed beyond loopback / a 0700 directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlEndpoint {
    /// 127.0.0.1 on an ephemeral port.
    Tcp {
        port: u16,
    },
    /// Unix socket inside a 0700 per-session directory (unix only).
    Unix {
        path: PathBuf,
    },
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    /// Curses style: the host runs `argv` in an ordinary PTY pane (D2) and calls `attach`.
    Pty,
    /// Text-buffer style: `launch` spawns the process and bilterm renders [`TextScreen`].
    Bridge,
}

/// A fully resolved launch description. Building one spawns nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub kind: LaunchKind,
    pub program: PathBuf,
    /// Arguments only (no program, never joined or passed through a shell).
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub control: ControlEndpoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackendCaps {
    /// The emulator draws the guest screen to a PTY by itself (curses).
    pub text_pane_native: bool,
    /// `read_text` works (VGA text buffer polling).
    pub textbuf: bool,
    pub graphics_dump: bool,
    pub snapshots: bool,
    pub host_folder: bool,
}

/// Result of a side-effect free emulator probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub found: bool,
    pub version: Option<Version>,
    pub meets_floor: bool,
    pub curses: bool,
    pub detail: String,
}

impl Probe {
    /// Usable at all (found and at or above the version floor).
    pub fn usable(&self) -> bool {
        self.found && self.meets_floor
    }
}

/// Probes an emulator through the D1 [`EmulatorProbe`] seam, applying the manifest floor.
pub fn probe_qemu(p: &dyn EmulatorProbe, floor: Version) -> Probe {
    let mut out = Probe {
        found: false,
        version: None,
        meets_floor: false,
        curses: false,
        detail: String::new(),
    };
    if p.locate().is_none() {
        out.detail = "emulator not found".into();
        return out;
    }
    out.found = true;
    out.version = p
        .version_output()
        .ok()
        .as_deref()
        .and_then(parse_qemu_version);
    match out.version {
        Some(v) if v >= floor => out.meets_floor = true,
        Some(v) => out.detail = format!("{v} is below the floor {floor}"),
        None => out.detail = "could not parse emulator version".into(),
    }
    out.curses = p
        .display_help()
        .map(|t| t.lines().any(|l| l.trim() == "curses"))
        .unwrap_or(false);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    QemuCurses,
    QemuTextbuf,
}

impl BackendKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::QemuCurses => "qemu-curses",
            Self::QemuTextbuf => "qemu-textbuf",
        }
    }
}

/// Per-OS backend preference order (plan section 4.2 matrix, QEMU rows).
pub fn preference(os: HostOs) -> &'static [BackendKind] {
    match os {
        HostOs::Windows => &[BackendKind::QemuTextbuf, BackendKind::QemuCurses],
        HostOs::Linux | HostOs::MacOs => &[BackendKind::QemuCurses, BackendKind::QemuTextbuf],
        HostOs::Other => &[],
    }
}

/// Picks the first preferred backend the probe supports. `want` overrides the order (`--backend`).
pub fn detect(os: HostOs, probe: &Probe, want: Option<BackendKind>) -> Option<BackendKind> {
    if !probe.usable() {
        return None;
    }
    let order: Vec<BackendKind> = match want {
        Some(k) => vec![k],
        None => preference(os).to_vec(),
    };
    order
        .into_iter()
        .find(|k| *k != BackendKind::QemuCurses || probe.curses)
}

/// The backend contract. Methods other than `id`/`caps` may fail with
/// [`BackendError::Unsupported`] when the matching [`BackendCaps`] flag is false.
pub trait DosBackend: Send {
    fn id(&self) -> &'static str;
    fn caps(&self) -> BackendCaps;
    /// Resolves the launch description (allocating the control endpoint) without spawning.
    fn plan(&mut self, spec: &SessionSpec) -> Result<LaunchPlan>;
    /// Bridge backends: spawn and connect. Pty backends return `Unsupported`; the host runs
    /// the plan in a PTY and then calls [`DosBackend::attach`].
    fn launch(&mut self, spec: &SessionSpec) -> Result<()>;
    /// Connect the control channel of an already running (host spawned) emulator.
    fn attach(&mut self) -> Result<()>;
    fn send_keys(&mut self, keys: &[DosKey]) -> Result<()>;
    fn read_text(&mut self) -> Result<TextScreen>;
    fn resize(&mut self, cols: u16, rows: u16) -> Result<()>;
    /// Stub hooks (D1c/D5 implement them).
    fn snapshot(&mut self, tag: &str) -> Result<()>;
    fn restore(&mut self, tag: &str) -> Result<()>;
    fn is_running(&mut self) -> bool;
    fn shutdown(&mut self) -> Result<()>;
}

/// Lowercase hex from the OS CSPRNG where available (unix `/dev/urandom`); elsewhere a SHA256 of
/// process-unique state. Used for unguessable per-session names, not for key material.
pub fn random_hex(bytes: usize) -> String {
    let bytes = bytes.clamp(1, 32);
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut buf = vec![0u8; bytes];
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut buf))
            .is_ok()
        {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    let h = super::verify::sha256_bytes(crate::secure_fs::unique_suffix().as_bytes());
    h[..bytes * 2].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(found: bool, ok: bool, curses: bool) -> Probe {
        Probe {
            found,
            version: Some(Version(10, 2, 1)),
            meets_floor: ok,
            curses,
            detail: String::new(),
        }
    }

    struct Fake(Option<&'static str>, &'static str, &'static str);
    impl EmulatorProbe for Fake {
        fn locate(&self) -> Option<PathBuf> {
            self.0.map(PathBuf::from)
        }
        fn version_output(&self) -> std::result::Result<String, String> {
            Ok(self.1.into())
        }
        fn display_help(&self) -> std::result::Result<String, String> {
            Ok(self.2.into())
        }
    }

    #[test]
    fn probe_applies_floor() {
        let floor = Version(10, 2, 0);
        let old = probe_qemu(
            &Fake(Some("/q"), "QEMU emulator version 9.2.4", "curses\n"),
            floor,
        );
        assert!(old.found && !old.meets_floor && !old.usable());
        let ok = probe_qemu(
            &Fake(
                Some("/q"),
                "QEMU emulator version 10.2.0",
                "none\n  curses\n",
            ),
            floor,
        );
        assert!(ok.usable() && ok.curses);
        let none = probe_qemu(&Fake(None, "", ""), floor);
        assert!(!none.found);
        let junk = probe_qemu(&Fake(Some("/q"), "junk", ""), floor);
        assert!(!junk.usable() && junk.version.is_none());
    }

    #[test]
    fn detect_walks_per_os_order() {
        let p = probe(true, true, true);
        assert_eq!(
            detect(HostOs::Linux, &p, None),
            Some(BackendKind::QemuCurses)
        );
        assert_eq!(
            detect(HostOs::Windows, &p, None),
            Some(BackendKind::QemuTextbuf)
        );
        let nocurses = probe(true, true, false);
        assert_eq!(
            detect(HostOs::Linux, &nocurses, None),
            Some(BackendKind::QemuTextbuf)
        );
        assert_eq!(
            detect(HostOs::Linux, &nocurses, Some(BackendKind::QemuCurses)),
            None
        );
        assert_eq!(detect(HostOs::Linux, &probe(true, false, true), None), None);
        assert_eq!(detect(HostOs::Other, &p, None), None);
    }

    #[test]
    fn random_hex_is_hex_and_distinct() {
        let a = random_hex(8);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, random_hex(8));
    }
}
