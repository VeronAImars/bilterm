//! `bilterm attach` client (plan task T-A7): a thin TUI that speaks the attach protocol to a
//! session host's `host-<name>.attach.sock`.
//!
//! * [`link`]   async framed connection + `Hello`/`HelloOk`/`Attach` handshake (generic over the stream)
//! * [`state`]  pure model of what the host has told us (layout, per-pane [`WireGrid`], size owner)
//! * [`keys`]   key -> bytes, the detach/pane-switch prefix, paste chunking
//! * [`outbox`] bounded input queue with the host's token bucket mirrored and refusal backoff
//! * [`ui`]     ratatui rendering of the selected pane plus a status line
//! * [`tui`]    the terminal event loop and reconnect policy
//!
//! Keys (prefix is `Ctrl-]`): `d` or `q` detach, `n`/`p` next/previous pane, `Ctrl-]` sends a
//! literal `Ctrl-]`. Everything else goes to the selected pane as `Input`.

pub mod keys;
pub mod link;
pub mod outbox;
pub mod state;
pub mod tui;
pub mod ui;

use std::path::{Path, PathBuf};

use super::msg::Role;

/// Exit codes of `bilterm attach`.
pub const EXIT_DETACHED: i32 = 0;
/// Cannot connect, not a terminal, or the host connection was lost (after bounded reconnects).
pub const EXIT_IO: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
/// Handshake refused or failed (version mismatch, host full, ...).
pub const EXIT_REFUSED: i32 = 3;
/// Host closed us: kicked, lagged beyond recovery, or idle timeout.
pub const EXIT_KICKED: i32 = 4;
pub const EXIT_PROTOCOL: i32 = 5;
/// Host sent `Bye{host_shutdown}`.
pub const EXIT_HOST_SHUTDOWN: i32 = 6;

/// Parsed `bilterm attach` host-client arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientOpts {
    /// Host name or socket path.
    pub target: String,
    pub role: Role,
}

/// True when `args` (argv after the program name) select the host client rather than the legacy
/// layout re-attach: `attach-host ...`, or `attach` with `--host`/`--ro`/`--rw` or a socket path.
pub fn is_host_attach(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("attach-host") => true,
        Some("attach") => args[1..].iter().any(|a| matches!(a.as_str(), "--host" | "--ro" | "--rw") || looks_like_path(a)),
        _ => false,
    }
}

/// As [`is_host_attach`], and additionally `attach NAME` when a host named `NAME` is serving
/// attach in `runtime_dir` (otherwise `attach NAME` stays the legacy layout re-attach).
pub fn is_host_attach_in(args: &[String], runtime_dir: &Path) -> bool {
    if is_host_attach(args) {
        return true;
    }
    matches!(args, [cmd, name] if cmd == "attach" && !name.starts_with('-'))
        && super::listener::attach_socket_path_in(runtime_dir, &args[1]).is_ok_and(|p| p.exists())
}

fn looks_like_path(s: &str) -> bool {
    !s.starts_with('-') && (s.contains('/') || s.ends_with(".sock"))
}

/// Parses the arguments after the command word.
pub fn parse_args(rest: &[String]) -> Result<ClientOpts, String> {
    let mut target: Option<String> = None;
    let mut role = Role::Rw;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ro" => role = Role::Ro,
            "--rw" => role = Role::Rw,
            "--host" => {
                let v = it.next().ok_or("--host needs a host name or socket path")?;
                if target.replace(v.clone()).is_some() {
                    return Err("only one host may be given".into());
                }
            }
            s if s.starts_with('-') => return Err(format!("unknown option '{s}' (usage: bilterm attach <host-name|socket-path> [--ro])")),
            s => {
                if target.replace(s.to_string()).is_some() {
                    return Err("only one host may be given".into());
                }
            }
        }
    }
    let target = target.ok_or("usage: bilterm attach <host-name|socket-path> [--ro]")?;
    Ok(ClientOpts { target, role })
}

/// Maps a host name (validated like daemon names) or an explicit path to the attach socket.
pub fn resolve_socket(target: &str, runtime_dir: &Path) -> Result<PathBuf, String> {
    if looks_like_path(target) {
        return Ok(PathBuf::from(target));
    }
    super::listener::attach_socket_path_in(runtime_dir, target)
}

/// Refuses sockets another user could have planted: must be a socket (not a symlink to one),
/// owned by us, with no group/other access.
#[cfg(unix)]
pub fn check_socket_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("no attach socket at {} (is the host running with --attach?)", path.display()),
        _ => format!("cannot stat {}: {e}", path.display()),
    })?;
    if !meta.file_type().is_socket() {
        return Err(format!("{} is not a unix socket", path.display()));
    }
    if meta.uid() != crate::secure_fs::current_euid() {
        return Err(format!("{} is not owned by the current user", path.display()));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!("{} must not be accessible by group/other (mode {:o})", path.display(), meta.mode() & 0o777));
    }
    Ok(())
}

/// Entry point for `bilterm attach ...` / `attach-host ...`; returns the process exit code.
#[cfg(unix)]
pub async fn run_cli(rest: &[String]) -> i32 {
    let opts = match parse_args(rest) {
        Ok(o) => o,
        Err(m) => {
            eprintln!("bilterm: {m}");
            return EXIT_USAGE;
        }
    };
    let dir = match crate::daemon::supervisor::resolve_runtime_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("bilterm: runtime directory error: {e}");
            return EXIT_IO;
        }
    };
    let path = match resolve_socket(&opts.target, &dir) {
        Ok(p) => p,
        Err(m) => {
            eprintln!("bilterm: {m}");
            return EXIT_USAGE;
        }
    };
    if let Err(m) = check_socket_file(&path) {
        eprintln!("bilterm: {m}");
        return EXIT_IO;
    }
    tui::run(&path, opts.role).await
}

#[cfg(not(unix))]
pub async fn run_cli(_: &[String]) -> i32 {
    eprintln!("bilterm: attach is Unix-only for now (plan Q6)");
    EXIT_IO
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dispatch_keeps_legacy_attach_intact() {
        assert!(!is_host_attach(&v(&["attach"])));
        assert!(!is_host_attach(&v(&["attach", "mysession"])));
        assert!(!is_host_attach(&v(&["a", "x"])));
        assert!(is_host_attach(&v(&["attach", "--ro", "h"])));
        assert!(is_host_attach(&v(&["attach", "--host", "h"])));
        assert!(is_host_attach(&v(&["attach", "/run/x/host-a.attach.sock"])));
        assert!(is_host_attach(&v(&["attach-host", "h"])));
    }

    #[test]
    fn bare_name_selects_host_only_when_its_socket_exists() {
        let dir = std::env::temp_dir().join(format!("bt-a7-name-{}", crate::secure_fs::unique_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_host_attach_in(&v(&["attach", "dev"]), &dir));
        std::fs::write(dir.join("host-dev.attach.sock"), b"").unwrap();
        assert!(is_host_attach_in(&v(&["attach", "dev"]), &dir));
        assert!(!is_host_attach_in(&v(&["attach", "other"]), &dir));
        assert!(!is_host_attach_in(&v(&["attach", "dev", "x"]), &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_roles_and_targets() {
        assert_eq!(parse_args(&v(&["h"])).unwrap(), ClientOpts { target: "h".into(), role: Role::Rw });
        assert_eq!(parse_args(&v(&["--ro", "h"])).unwrap().role, Role::Ro);
        assert_eq!(parse_args(&v(&["--host", "h", "--ro"])).unwrap().target, "h");
        assert!(parse_args(&v(&[])).is_err());
        assert!(parse_args(&v(&["a", "b"])).is_err());
        assert!(parse_args(&v(&["--bogus", "a"])).is_err());
        assert!(parse_args(&v(&["--host"])).is_err());
    }

    #[test]
    fn resolves_names_and_paths() {
        let d = Path::new("/run/bt");
        assert_eq!(resolve_socket("dev", d).unwrap(), Path::new("/run/bt/host-dev.attach.sock"));
        assert_eq!(resolve_socket("/x/y.sock", d).unwrap(), Path::new("/x/y.sock"));
        assert!(resolve_socket("../evil", d).is_err() || resolve_socket("../evil", d).unwrap().to_string_lossy().contains('/'));
        assert!(resolve_socket("bad name!", d).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn socket_check_rejects_missing_and_non_sockets() {
        let dir = std::env::temp_dir().join(format!("bt-a7-sock-{}", crate::secure_fs::unique_suffix()));
        crate::secure_fs::create_private_dir_all(&dir).unwrap();
        assert!(check_socket_file(&dir.join("nope.sock")).unwrap_err().contains("no attach socket"));
        let f = dir.join("f.sock");
        std::fs::write(&f, b"x").unwrap();
        assert!(check_socket_file(&f).unwrap_err().contains("not a unix socket"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
