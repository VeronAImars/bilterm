//! OS process facts for session find: PID, parent, process group, session id, controlling tty,
//! name, command line, cwd and owner. Linux reads `/proc`; macOS runs `ps`/`lsof` with a numeric
//! pid argument only (never a shell, never untrusted text); other platforms degrade to "no facts"
//! (the matcher then only uses the pane's own title/cwd/text). Every free-text field is
//! sanitised and capped before it leaves this module.

use crate::vt::sanitize_display;

/// Cap on a process name or path shown in a row.
pub const FIELD_CAP: usize = 256;
/// Cap on a command line shown or matched.
pub const CMDLINE_CAP: usize = 512;

/// Facts about one process; every field but `pid` may be unknown on a given platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub pgid: Option<u32>,
    pub sid: Option<u32>,
    /// Controlling terminal device path (`/dev/pts/3`, `ttys003`).
    pub tty: Option<String>,
    pub name: String,
    pub cmdline: String,
    pub cwd: Option<String>,
    pub uid: Option<u32>,
    pub user: Option<String>,
}

/// Platform capability line reported in results (`linux-proc`, `macos-ps`, `none`).
pub fn platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux-proc"
    } else if cfg!(target_os = "macos") {
        "macos-ps"
    } else {
        "none"
    }
}

/// Fields of `/proc/<pid>/stat` after the command name: `(comm, ppid, pgrp, session)`.
/// The command name may contain spaces and parentheses, so split at the last `)`.
pub fn parse_stat(stat: &str) -> Option<(String, u32, u32, u32)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = stat[open + 1..close].to_string();
    let mut it = stat[close + 1..].split_whitespace();
    let _state = it.next()?;
    let ppid = it.next()?.parse().ok()?;
    let pgrp = it.next()?.parse().ok()?;
    let session = it.next()?.parse().ok()?;
    Some((comm, ppid, pgrp, session))
}

/// First `Uid:` value (real uid) of `/proc/<pid>/status`.
pub fn parse_status_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|l| l.strip_prefix("Uid:"))?.split_whitespace().next()?.parse().ok()
}

/// `name` for `uid` from `/etc/passwd`-formatted text.
pub fn passwd_name(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        let name = f.next()?;
        let _pw = f.next()?;
        (f.next()?.parse::<u32>().ok()? == uid).then(|| name.to_string())
    })
}

/// Whether NUL-separated `environ` bytes hold marker `spec` (`KEY` or `KEY=VALUE`).
/// Only a boolean leaves this function; values are never returned.
pub fn environ_has(environ: &[u8], spec: &str) -> bool {
    let (key, val) = match spec.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (spec, None),
    };
    if key.is_empty() {
        return false;
    }
    environ.split(|b| *b == 0).any(|entry| {
        let Ok(entry) = std::str::from_utf8(entry) else { return false };
        match entry.split_once('=') {
            Some((k, v)) => k == key && val.is_none_or(|want| v == want),
            None => false,
        }
    })
}

fn clean(raw: &str, cap: usize) -> String {
    sanitize_display(raw, cap)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::fs;

    /// Largest `environ` prefix scanned for a marker.
    const ENVIRON_CAP: u64 = 256 * 1024;

    pub fn probe(pid: u32) -> Option<ProcInfo> {
        let base = format!("/proc/{pid}");
        let stat = fs::read_to_string(format!("{base}/stat")).ok()?;
        let (comm, ppid, pgid, sid) = parse_stat(&stat)?;
        let cmdline = fs::read(format!("{base}/cmdline"))
            .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
            .unwrap_or_default();
        let cwd = fs::read_link(format!("{base}/cwd")).ok().map(|p| clean(&p.to_string_lossy(), FIELD_CAP));
        let uid = fs::read_to_string(format!("{base}/status")).ok().and_then(|s| parse_status_uid(&s));
        let user = uid.and_then(|u| fs::read_to_string("/etc/passwd").ok().and_then(|p| passwd_name(&p, u)));
        let tty = (0..3).find_map(|fd| {
            let link = fs::read_link(format!("{base}/fd/{fd}")).ok()?;
            let s = link.to_string_lossy().to_string();
            (s.starts_with("/dev/pts/") || s.starts_with("/dev/tty")).then(|| clean(&s, FIELD_CAP))
        });
        Some(ProcInfo {
            pid,
            ppid: Some(ppid),
            pgid: Some(pgid),
            sid: Some(sid),
            tty,
            name: clean(&comm, FIELD_CAP),
            cmdline: clean(cmdline.trim(), CMDLINE_CAP),
            cwd,
            uid,
            user: user.map(|u| clean(&u, 64)),
        })
    }

    pub fn has_env(pid: u32, spec: &str) -> bool {
        use std::io::Read;
        let Ok(f) = fs::File::open(format!("/proc/{pid}/environ")) else { return false };
        let mut buf = Vec::new();
        if f.take(ENVIRON_CAP).read_to_end(&mut buf).is_err() {
            return false;
        }
        environ_has(&buf, spec)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::process::Command;

    fn run(program: &str, args: &[String]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn probe(pid: u32) -> Option<ProcInfo> {
        let p = pid.to_string();
        let head = run("ps", &["-o".into(), "ppid=,pgid=,sess=,uid=,user=,tty=,comm=".into(), "-p".into(), p.clone()])?;
        let mut f = head.split_whitespace();
        let ppid = f.next()?.parse().ok();
        let pgid = f.next()?.parse().ok();
        let sid = f.next().and_then(|s| s.parse().ok());
        let uid = f.next().and_then(|s| s.parse().ok());
        let user = f.next().map(str::to_string);
        let tty = f.next().filter(|t| *t != "??").map(|t| format!("/dev/{t}"));
        let name = f.collect::<Vec<_>>().join(" ");
        let cmdline = run("ps", &["-o".into(), "command=".into(), "-p".into(), p.clone()]).unwrap_or_default();
        let cwd = run("lsof", &["-a".into(), "-d".into(), "cwd".into(), "-Fn".into(), "-p".into(), p])
            .and_then(|o| o.lines().find_map(|l| l.strip_prefix('n').map(str::to_string)));
        Some(ProcInfo {
            pid,
            ppid,
            pgid,
            sid,
            tty: tty.map(|t| clean(&t, FIELD_CAP)),
            name: clean(name.rsplit('/').next().unwrap_or(&name), FIELD_CAP),
            cmdline: clean(cmdline.trim(), CMDLINE_CAP),
            cwd: cwd.map(|c| clean(&c, FIELD_CAP)),
            uid,
            user: user.map(|u| clean(&u, 64)),
        })
    }

    /// Environment markers are not readable without elevated APIs here: never matches.
    pub fn has_env(_pid: u32, _spec: &str) -> bool {
        false
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    /// Windows and other platforms: no process API wired yet; the matcher degrades to pane facts.
    pub fn probe(_pid: u32) -> Option<ProcInfo> {
        None
    }

    pub fn has_env(_pid: u32, _spec: &str) -> bool {
        false
    }
}

/// Facts for `pid`, or `None` when the process is gone or the platform has no probe.
pub fn probe(pid: u32) -> Option<ProcInfo> {
    imp::probe(pid)
}

/// Whether process `pid` carries environment marker `spec` (`KEY` or `KEY=VALUE`).
pub fn has_env(pid: u32, spec: &str) -> bool {
    imp::has_env(pid, spec)
}

/// Foreground process group of the terminal behind `master_fd` (`tcgetpgrp`).
#[cfg(unix)]
pub fn foreground_pgid(master_fd: std::os::fd::RawFd) -> Option<u32> {
    // SAFETY: `tcgetpgrp` only reads the descriptor; an invalid fd returns -1.
    let pg = unsafe { libc::tcgetpgrp(master_fd) };
    u32::try_from(pg).ok().filter(|p| *p > 0)
}

#[cfg(not(unix))]
pub fn foreground_pgid(_master_fd: i32) -> Option<u32> {
    None
}

/// Slave device path of the PTY behind `master_fd` (`/dev/pts/N`), Linux only.
#[cfg(target_os = "linux")]
pub fn pty_path(master_fd: std::os::fd::RawFd) -> Option<String> {
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: buffer and length are valid; ptsname_r writes a NUL-terminated name or errors.
    let rc = unsafe { libc::ptsname_r(master_fd, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: success guarantees a NUL-terminated string inside `buf`.
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(clean(&s.to_string_lossy(), FIELD_CAP))
}

#[cfg(not(target_os = "linux"))]
pub fn pty_path(_master_fd: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parsing_survives_odd_comm() {
        assert_eq!(parse_stat("12 (a) b) S 5 77 88 0 -1"), Some(("a) b".to_string(), 5, 77, 88)));
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) S 2"), None);
    }

    #[test]
    fn status_and_passwd() {
        assert_eq!(parse_status_uid("Name:\tx\nUid:\t1000\t1000\t1000\t1000\n"), Some(1000));
        assert_eq!(passwd_name("root:x:0:0::/root:/bin/sh\nbill:x:1000:1000::/h:/bin/sh\n", 1000), Some("bill".into()));
        assert_eq!(passwd_name("root:x:0:0", 5), None);
    }

    #[test]
    fn environ_markers_return_only_a_boolean() {
        let env = b"A=1\0BILTERM_PANE_ID=4\0EMPTY=\0";
        assert!(environ_has(env, "A"));
        assert!(environ_has(env, "BILTERM_PANE_ID=4"));
        assert!(!environ_has(env, "BILTERM_PANE_ID=5"));
        assert!(environ_has(env, "EMPTY="));
        assert!(!environ_has(env, "B"));
        assert!(!environ_has(env, ""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probes_this_process() {
        let me = std::process::id();
        let info = probe(me).expect("own /proc entry");
        assert_eq!(info.pid, me);
        assert!(info.pgid.is_some() && info.sid.is_some() && !info.name.is_empty());
        assert!(info.cwd.is_some());
        assert!(probe(u32::MAX - 1).is_none());
    }
}
