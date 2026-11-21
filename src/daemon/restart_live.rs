//! `bilterm daemon restart --live` (unix): replace a running session host with a fresh process of
//! the current binary without killing or resetting any terminal.
//!
//! - Start a new daemon with `--takeover`: it listens on `host-<name>.takeover.sock` but takes
//!   neither the lock nor the public sockets yet.
//! - The old host transfers every session to it (`session.transfer {all, takeover, retire}`;
//!   in-process when we ARE the old host, else over its control socket).
//! - The old host exits (releasing the lock and sockets); the new one then acquires them, binds,
//!   registers and serves. Clients reconnect to the same socket path.
//!
//! Any failure before the commit leaves the old host serving and the new process is killed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::transfer::{self, SocketKind, TransferOpts};
use crate::wm::WindowManager;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartReport {
    pub name: String,
    pub old_pid: u32,
    pub new_pid: u32,
    pub sessions: Vec<String>,
    pub panes: usize,
    pub pids: Vec<u32>,
    /// The new daemon answered on the public socket (false when the caller is the old host and
    /// exits right after replying).
    pub new_is_serving: bool,
}

fn spawn_takeover(runtime_dir: &Path, name: &str) -> Result<Child, String> {
    let sock = super::process::session_daemon_socket_path_in(runtime_dir, name).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut args = vec!["--daemon".to_string(), "--name".into(), name.into(), "-l".into(), format!("unix:{}", sock.display()), "--host-ptys".into(), "--takeover".into()];
    if crate::attach::listener::attach_socket_path_in(runtime_dir, name).is_ok_and(|p| p.exists()) {
        args.push("--attach".into());
    }
    let mut cmd = Command::new(exe);
    // The new daemon's stderr goes to a private log so a failed takeover can be diagnosed.
    let log = crate::secure_fs::create_private_file(&runtime_dir.join(format!("host-{name}.takeover.log"))).map_err(|e| e.to_string())?;
    cmd.args(&args).env("BILTERM_RUNTIME_DIR", runtime_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map_err(|e| format!("cannot start the new daemon: {e}"))
}

fn wait_takeover_socket(child: &mut Child, path: &Path) -> Result<(), String> {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!("the new daemon exited early ({st})"));
        }
        if UnixStream::connect(path).is_ok() {
            // The probe connection is closed at once; the receiver treats it as a bad offer.
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("the new daemon did not open its takeover socket in time".into())
}

/// One JSON-RPC call over a unix socket (`session.transfer` to the old host).
pub fn rpc_call(sock: &Path, method: &str, params: &str, wait: Duration) -> Result<crate::api::json::JsonValue, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("cannot reach {}: {e}", sock.display()))?;
    s.set_read_timeout(Some(wait)).map_err(|e| e.to_string())?;
    let line = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{method}\",\"params\":{params}}}\n");
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).map_err(|e| format!("no reply to {method}: {e}"))?;
    let v = crate::api::json::JsonValue::parse(reply.trim()).ok_or("unparseable reply")?;
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(crate::api::json::JsonValue::as_str).unwrap_or("error");
        return Err(msg.to_string());
    }
    v.get("result").cloned().ok_or_else(|| "reply without result".to_string())
}

/// Runs the live restart of daemon `name`. `wm` is the workspace of the calling process when it
/// is a session host (then `name` may be that host itself); a plain CLI passes `None`.
pub fn restart_live(wm: Option<&mut WindowManager>, runtime_dir: &Path, name: &str) -> Result<RestartReport, String> {
    crate::secure_fs::validate_safe_name(name)?;
    let row = super::list_view::row_for(runtime_dir, name);
    let (old_pid, true) = (row.pid.filter(|_| row.status == "running").ok_or_else(|| format!("daemon '{name}' is not running"))?, row.host_ptys == Some(true)) else {
        return Err(format!("daemon '{name}' is not a session host (--host-ptys); there are no PTYs to carry over"));
    };
    let mut child = spawn_takeover(runtime_dir, name)?;
    let new_pid = child.id();
    let take_sock = transfer::socket_path_in(runtime_dir, name, SocketKind::Takeover)?;
    let abort = |child: &mut Child, why: String| -> Result<RestartReport, String> {
        let _ = child.kill();
        let _ = child.wait();
        Err(why)
    };
    if let Err(e) = wait_takeover_socket(&mut child, &take_sock) {
        return abort(&mut child, e);
    }
    let in_process = wm.as_ref().is_some_and(|w| w.host_name() == Some(name));
    let (sessions, panes, pids) = if let (true, Some(wm)) = (in_process, wm) {
        let names = wm.live_session_names();
        match transfer::transfer_sessions(wm, &names, &take_sock, true, TransferOpts::default()) {
            Ok(r) => {
                wm.request_retire();
                (r.sessions, r.panes, r.pids)
            }
            Err(e) => return abort(&mut child, e),
        }
    } else {
        let old_sock = super::process::running_daemon_socket_in(runtime_dir, name).ok_or_else(|| format!("daemon '{name}' has no control socket"))?;
        let params = format!("{{\"all\":true,\"to\":\"{name}\",\"takeover\":true,\"retire\":true}}");
        match rpc_call(&old_sock, "session.transfer", &params, Duration::from_secs(60)) {
            Ok(v) => {
                let list = |k: &str| v.get(k).and_then(|a| a.as_array().map(<[_]>::to_vec)).unwrap_or_default();
                (
                    list("sessions").iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                    v.get("panes").and_then(crate::api::json::JsonValue::as_i64).unwrap_or(0) as usize,
                    list("pids").iter().filter_map(|x| x.as_i64().map(|n| n as u32)).collect(),
                )
            }
            Err(e) => return abort(&mut child, e),
        }
    };
    // Reap the new daemon if it ends while we live; it is detached otherwise.
    let mut reaper_child = child;
    let _ = std::thread::Builder::new().name("takeover-reaper".into()).spawn(move || {
        let _ = reaper_child.wait();
    });
    let new_is_serving = if in_process {
        false
    } else {
        let end = Instant::now() + Duration::from_secs(40);
        loop {
            let r = super::list_view::row_for(runtime_dir, name);
            if r.status == "running" && r.pid == Some(new_pid) {
                break true;
            }
            if Instant::now() >= end {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    Ok(RestartReport { name: name.to_string(), old_pid, new_pid, sessions, panes, pids, new_is_serving })
}
