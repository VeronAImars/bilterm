//! Live session transfer between session hosts (unix only).
//!
//! The sender (`transfer_sessions`) releases PTYs and offers them; the receiver
//! (`serve_connection`) validates, waits for `Commit`, adopts and answers `Adopted`.
//! Sockets: `host-<name>.xfer.sock` (a running host accepts sessions) and
//! `host-<name>.takeover.sock` (a freshly started `--takeover` host accepts everything and
//! replaces its own pristine session), both `0600` in the private runtime dir, same-uid only.
//!
//! Rollback rule: the receiver's reader starts only after `Commit`, so a sender that never gets
//! `Ready`, or gets `Refuse`/`Failed`/EOF, reclaims every PTY with no byte lost. A silent
//! receiver after `Commit` is treated as failed too (reclaim); the narrow window where the
//! receiver adopted but its `Adopted` reply is lost is documented in the plan.

pub mod wire;

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use wire::{Msg, Offer};

use crate::wm::WindowManager;

const MAX_SOCKET_PATH_BYTES: usize = 103;
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
pub const ADOPT_TIMEOUT: Duration = Duration::from_secs(20);
const SERVE_IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Which receiving role a socket plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketKind {
    /// A running host: adds the sessions next to its own.
    Accept,
    /// A `--takeover` host: replaces its pristine session with the sender's.
    Takeover,
}

pub fn socket_path_in(runtime_dir: &Path, name: &str, kind: SocketKind) -> Result<PathBuf, String> {
    crate::secure_fs::validate_safe_name(name)?;
    let suffix = match kind {
        SocketKind::Accept => "xfer",
        SocketKind::Takeover => "takeover",
    };
    let path = runtime_dir.join(format!("host-{name}.{suffix}.sock"));
    if path.as_os_str().len() >= MAX_SOCKET_PATH_BYTES {
        return Err(format!("socket path '{}' exceeds the {MAX_SOCKET_PATH_BYTES}-byte limit", path.display()));
    }
    Ok(path)
}

/// Non-blocking listener; the owning daemon polls it from its tick.
pub struct TransferListener {
    listener: UnixListener,
    path: PathBuf,
    kind: SocketKind,
}

impl TransferListener {
    pub fn bind(runtime_dir: &Path, name: &str, kind: SocketKind) -> io::Result<Self> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let path = socket_path_in(runtime_dir, name, kind).map_err(io::Error::other)?;
        crate::secure_fs::ensure_private_dir(runtime_dir)?;
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if !meta.file_type().is_socket() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "refusing to replace a non-socket file"));
            }
            let _ = std::fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path, kind })
    }

    pub fn kind(&self) -> SocketKind {
        self.kind
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// One pending same-uid connection, if any (blocking mode, I/O timeouts set).
    pub fn poll_accept(&self) -> Option<UnixStream> {
        let (stream, _) = self.listener.accept().ok()?;
        stream.set_nonblocking(false).ok()?;
        let cred = peer_uid(&stream)?;
        if crate::secure_fs::check_peer_uid(cred, crate::secure_fs::current_euid()).is_err() {
            return None;
        }
        stream.set_read_timeout(Some(SERVE_IO_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(SERVE_IO_TIMEOUT)).ok()?;
        Some(stream)
    }

    /// Blocks up to `wait` for a connection (the `--takeover` start-up phase).
    pub fn accept_blocking(&self, wait: Duration) -> Option<UnixStream> {
        let end = std::time::Instant::now() + wait;
        while std::time::Instant::now() < end {
            if let Some(s) = self.poll_accept() {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for TransferListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn peer_uid(stream: &UnixStream) -> Option<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // `libc::ucred` exists only on Linux/Android, so it must stay inside this branch.
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: valid out pointers of the stated size.
        let rc = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &raw mut len) };
        (rc == 0).then_some(cred.uid)
    }
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
    {
        let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
        // SAFETY: valid out pointers.
        let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) };
        (rc == 0).then_some(uid)
    }
    // Unknown target: no way to verify the peer, so refuse (callers treat `None` as untrusted).
    #[cfg(not(any(
        target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios",
        target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"
    )))]
    {
        let _ = stream;
        None
    }
}

/// What a transfer moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferReport {
    pub sessions: Vec<String>,
    pub panes: usize,
    pub pids: Vec<u32>,
}

/// Sender timeouts (tests shorten them).
#[derive(Clone, Copy, Debug)]
pub struct TransferOpts {
    pub ready_timeout: Duration,
    pub adopt_timeout: Duration,
}

impl Default for TransferOpts {
    fn default() -> Self {
        Self { ready_timeout: READY_TIMEOUT, adopt_timeout: ADOPT_TIMEOUT }
    }
}

/// Sender: moves `names` to the host listening on `target`. On `Err` every session is exactly as
/// before (all PTYs reclaimed).
pub fn transfer_sessions(wm: &mut WindowManager, names: &[String], target: &Path, retire: bool, opts: TransferOpts) -> Result<TransferReport, String> {
    let mut stream = UnixStream::connect(target).map_err(|e| format!("cannot reach receiver at {}: {e}", target.display()))?;
    stream.set_write_timeout(Some(opts.ready_timeout)).map_err(|e| e.to_string())?;
    let staged = wm.stage_transfer(names)?;
    let offer = Offer { version: wire::XFER_VERSION, writer_version: crate::version_info::short(), sessions: staged.sessions.clone(), retire };
    let fds = staged.raw_fds();
    let pids: Vec<u32> = offer.sessions.iter().flat_map(|s| s.panes.iter().map(|p| p.pid)).collect();
    let report = TransferReport { sessions: names.to_vec(), panes: fds.len(), pids };

    let fail = |wm: &mut WindowManager, stream: &UnixStream, staged, why: String| -> Result<TransferReport, String> {
        let _ = stream_shutdown(stream);
        wm.rollback_transfer(staged);
        Err(why)
    };
    if let Err(e) = wire::send_offer(&mut stream, &offer, &fds) {
        return fail(wm, &stream, staged, format!("sending offer failed: {e}"));
    }
    let _ = stream.set_read_timeout(Some(opts.ready_timeout));
    match wire::read_msg(&mut stream) {
        Ok(Msg::Ready) => {}
        Ok(Msg::Refuse { why }) => return fail(wm, &stream, staged, format!("receiver refused: {why}")),
        Ok(other) => return fail(wm, &stream, staged, format!("unexpected reply {other:?}")),
        Err(e) => return fail(wm, &stream, staged, format!("receiver did not become ready: {e}")),
    }
    if let Err(e) = wire::write_msg(&mut stream, &Msg::Commit) {
        return fail(wm, &stream, staged, format!("commit failed: {e}"));
    }
    let _ = stream.set_read_timeout(Some(opts.adopt_timeout));
    match wire::read_msg(&mut stream) {
        Ok(Msg::Adopted) => {
            wm.commit_transfer(staged);
            Ok(report)
        }
        Ok(Msg::Failed { why }) => fail(wm, &stream, staged, format!("receiver failed to adopt: {why}")),
        Ok(other) => fail(wm, &stream, staged, format!("unexpected reply {other:?}")),
        Err(e) => {
            let _ = wire::write_msg(&mut stream, &Msg::Abort);
            fail(wm, &stream, staged, format!("no adoption confirmation: {e}"))
        }
    }
}

fn stream_shutdown(s: &UnixStream) -> io::Result<()> {
    s.shutdown(std::net::Shutdown::Both)
}

/// Receiver: runs the protocol on one accepted connection. Returns the installed session names
/// and whether the sender retires afterwards; `Ok(None)` when the sender aborted before commit.
pub fn serve_connection(wm: &mut WindowManager, mut stream: UnixStream, kind: SocketKind) -> Result<Option<(Vec<String>, bool)>, String> {
    let (offer, fds) = wire::recv_offer(&mut stream).map_err(|e| {
        let _ = wire::write_msg(&mut stream, &Msg::Refuse { why: e.to_string() });
        format!("bad offer: {e}")
    })?;
    let takeover = kind == SocketKind::Takeover;
    if let Err(why) = wm.validate_offer(&offer, fds.len(), takeover) {
        let _ = wire::write_msg(&mut stream, &Msg::Refuse { why: why.clone() });
        return Err(why);
    }
    wire::write_msg(&mut stream, &Msg::Ready).map_err(|e| e.to_string())?;
    match wire::read_msg(&mut stream) {
        Ok(Msg::Commit) => {}
        _ => return Ok(None), // abort, EOF or timeout: fds close, sender reclaims
    }
    match wm.install_transfer(&offer, fds, takeover) {
        Ok(names) => {
            if wire::write_msg(&mut stream, &Msg::Adopted).is_err() {
                wm.uninstall_transfer(&names);
                return Err("sender went away before the adoption was confirmed".into());
            }
            Ok(Some((names, offer.retire)))
        }
        Err(why) => {
            let _ = wire::write_msg(&mut stream, &Msg::Failed { why: why.clone() });
            Err(why)
        }
    }
}

#[cfg(test)]
mod peer_uid_tests {
    use super::*;

    /// Guards the per-OS peer-credential path: a same-process socketpair must report our euid
    /// (or `None` on targets with no credential API, which callers treat as untrusted).
    #[test]
    fn peer_uid_of_socketpair_is_own_euid() {
        let (a, _b) = UnixStream::pair().expect("socketpair");
        if let Some(uid) = peer_uid(&a) {
            assert_eq!(uid, crate::secure_fs::current_euid());
        }
    }
}
