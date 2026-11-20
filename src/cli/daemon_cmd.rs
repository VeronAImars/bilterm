//! `bilterm daemon list|start|stop|status` and named-daemon targeting (`--daemon-name`,
//! `BILTERM_DAEMON`).
//!
//! These run against the runtime directory directly (lock records + registry), so they work
//! with no daemon running and for daemons started by hand (`bilterm --headless --name X`).

use std::path::Path;

use crate::daemon::list_view::{collect_rows, row_for, rows_json, table};
use crate::daemon::restart_policy::RestartMode;
use crate::daemon::limits::MemoryLimits;
use crate::daemon::supervisor::{ensure_started_limited, stop_daemon, DaemonStartResult};
use crate::secure_fs::validate_safe_name;

/// Switch that selects which named daemon a client command talks to.
pub const DAEMON_NAME_SWITCH: &str = "--daemon-name";
/// Environment fallback for [`DAEMON_NAME_SWITCH`].
pub const DAEMON_NAME_ENV: &str = "BILTERM_DAEMON";

/// One parsed `bilterm daemon ...` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonCmd {
    List { json: bool },
    Start { name: String, restart: RestartMode, limits: MemoryLimits },
    Stop { name: String, force: bool },
    Status { name: String, json: bool },
}

/// Usage text for `bilterm daemon`.
pub fn usage() -> String {
    "\
Usage:
  bilterm daemon list [--json]              List daemons: status, PID, RSS, restarts, last exit
  bilterm daemon start [NAME] [--restart never|on-crash] [--memory-max SIZE] [--memory-high SIZE]
                                            Start daemon NAME in the background (no-op if running);
                                            on-crash respawns it with backoff (1s..60s) and stops after
                                            5 crashes in 120s (shown as 'failed').
                                            --memory-max (e.g. 2G) caps the host: a systemd user scope
                                            (MemoryMax, no swap, OOM kill) on Linux, else a best-effort RSS
                                            watchdog (SIGTERM, then SIGKILL). --memory-high must be below
                                            --memory-max and only throttles/warns
  bilterm daemon stop [NAME] [--force]      Stop daemon NAME (SIGTERM; --force = SIGKILL)
  bilterm daemon status [NAME] [--json]     Show one daemon (exit 0 running, 3 otherwise)

NAME defaults to --daemon-name / BILTERM_DAEMON, else 'default'. Names: [A-Za-z0-9_-], max 64.
Each named daemon listens on its own socket (daemons/daemon-<NAME>.sock in the runtime dir);
target one from a client with `bilterm ctl --daemon-name NAME <METHOD>` or BILTERM_DAEMON=NAME."
        .to_string()
}

/// Removes the first `--daemon-name NAME` / `--daemon-name=NAME` from `args` (stopping at a
/// literal `--`) and returns the validated target, falling back to `env_value` (`BILTERM_DAEMON`).
pub fn take_daemon_target(args: &mut Vec<String>, env_value: Option<&str>) -> Result<Option<String>, String> {
    let mut found = None;
    let prefix = format!("{DAEMON_NAME_SWITCH}=");
    for i in 0..args.len() {
        if args[i] == "--" {
            break;
        }
        if args[i] == DAEMON_NAME_SWITCH {
            if i + 1 >= args.len() {
                return Err(format!("{DAEMON_NAME_SWITCH} needs a NAME"));
            }
            found = Some(args.remove(i + 1));
            args.remove(i);
            break;
        }
        if let Some(v) = args[i].strip_prefix(&prefix) {
            found = Some(v.to_string());
            args.remove(i);
            break;
        }
    }
    let target = found.or_else(|| env_value.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string));
    if let Some(name) = &target {
        validate_safe_name(name).map_err(|e| format!("invalid daemon name '{name}': {e}"))?;
    }
    Ok(target)
}

/// Parses the words after `bilterm daemon`. `target` is the `--daemon-name` / env default name.
pub fn parse_daemon_args(args: &[String], target: Option<&str>) -> Result<DaemonCmd, String> {
    let mut pos = Vec::new();
    let (mut json, mut force) = (false, false);
    let mut restart = RestartMode::Never;
    let (mut mem_max, mut mem_high): (Option<String>, Option<String>) = (None, None);
    let mut args_it = args.iter();
    while let Some(a) = args_it.next() {
        match a.as_str() {
            "--restart" => {
                let v = args_it.next().ok_or_else(|| format!("--restart needs never|on-crash\n{}", usage()))?;
                restart = RestartMode::parse(v)?;
            }
            s if s.starts_with("--restart=") => restart = RestartMode::parse(&s["--restart=".len()..])?,
            "--memory-max" => mem_max = Some(args_it.next().ok_or_else(|| format!("--memory-max needs a size\n{}", usage()))?.clone()),
            "--memory-high" => mem_high = Some(args_it.next().ok_or_else(|| format!("--memory-high needs a size\n{}", usage()))?.clone()),
            s if s.starts_with("--memory-max=") => mem_max = Some(s["--memory-max=".len()..].to_string()),
            s if s.starts_with("--memory-high=") => mem_high = Some(s["--memory-high=".len()..].to_string()),
            "--json" => json = true,
            "--force" | "-f" => force = true,
            "-h" | "--help" | "help" => return Err(usage()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n{}", usage())),
            _ => pos.push(a.as_str()),
        }
    }
    let sub = pos.first().copied().unwrap_or("list");
    if pos.len() > 2 {
        return Err(format!("too many arguments\n{}", usage()));
    }
    let name = || -> Result<String, String> {
        let n = pos.get(1).copied().or(target).unwrap_or("default");
        validate_safe_name(n).map_err(|e| format!("invalid daemon name '{n}': {e}"))?;
        Ok(n.to_string())
    };
    match sub {
        "list" | "ls" if pos.len() <= 1 => Ok(DaemonCmd::List { json }),
        "start" => Ok(DaemonCmd::Start { name: name()?, restart, limits: MemoryLimits::from_args(mem_max.as_deref(), mem_high.as_deref())? }),
        "stop" => Ok(DaemonCmd::Stop { name: name()?, force }),
        "status" => Ok(DaemonCmd::Status { name: name()?, json }),
        other => Err(format!("unknown daemon subcommand '{other}'\n{}", usage())),
    }
}

/// `unix:PATH` endpoint of running daemon `name`, or a message saying it is not running.
pub fn resolve_daemon_endpoint(runtime_dir: &Path, name: &str) -> Result<String, String> {
    crate::daemon::process::running_daemon_socket_in(runtime_dir, name)
        .map(|p| format!("unix:{}", p.display()))
        .ok_or_else(|| format!("daemon '{name}' is not running (start it with `bilterm daemon start {name}`)"))
}

/// Runs `cmd` against `runtime_dir`, returning the text to print and the process exit code.
pub fn run(cmd: &DaemonCmd, runtime_dir: &Path) -> (String, i32) {
    match cmd {
        DaemonCmd::List { json } => {
            let rows = collect_rows(runtime_dir, true);
            if *json {
                (rows_json(&rows).to_json_string(), 0)
            } else {
                (table(&rows), 0)
            }
        }
        DaemonCmd::Status { name, json } => {
            let row = row_for(runtime_dir, name);
            let code = if row.status == "running" { 0 } else { 3 };
            if *json {
                return (row.to_json().to_json_string(), code);
            }
            let text = match (row.status, row.pid) {
                ("running", Some(pid)) => {
                    let note = if row.host_ptys == Some(false) {
                        format!(
                            "\n  WARNING: started without --host-ptys (model-only): panes have no shell and cannot run commands; `bilterm daemon stop {name}` then run `bilterm --headless --host-ptys --name {name}` in the background"
                        )
                    } else {
                        String::new()
                    };
                    format!("daemon '{name}': running (PID {pid}, v{}, socket {}, ptys: {}){note}", row.version, row.socket, row.pty_text())
                }
                ("crashed", Some(pid)) => format!(
                    "daemon '{name}': crashed (PID {pid} exited without cleanup) — `bilterm daemon start {name}` restarts it"
                ),
                ("failed", _) => format!(
                    "daemon '{name}': failed (crash loop, {} restarts, last exit {}) — `bilterm daemon start {name}` clears it",
                    row.restart_count,
                    row.last_exit_text()
                ),
                ("restarting", _) => {
                    format!("daemon '{name}': restarting after a crash ({} restarts, last exit {})", row.restart_count, row.last_exit_text())
                }
                _ => format!("daemon '{name}': not running"),
            };
            (text, code)
        }
        DaemonCmd::Start { name, restart, limits } => match ensure_started_limited(runtime_dir, name, *restart, *limits) {
            Ok(DaemonStartResult::Started { pid, socket_path }) => {
                (format!("daemon '{name}' started (PID {pid}, socket {})", socket_path.display()), 0)
            }
            Ok(DaemonStartResult::AlreadyRunning { pid, socket_path }) => {
                (format!("daemon '{name}' is already running (PID {pid}, socket {})", socket_path.display()), 0)
            }
            Err(e) => (format!("bilterm: failed to start daemon '{name}': {e}"), 1),
        },
        DaemonCmd::Stop { name, force } => match stop_daemon(runtime_dir, name, *force) {
            Ok(true) => (format!("daemon '{name}' stopped"), 0),
            Ok(false) => (format!("daemon '{name}' was not running"), 0),
            Err(e) => (format!("bilterm: failed to stop daemon '{name}': {e}"), 1),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_subcommands_and_defaults() {
        assert_eq!(parse_daemon_args(&v(&[]), None), Ok(DaemonCmd::List { json: false }));
        assert_eq!(parse_daemon_args(&v(&["ls", "--json"]), None), Ok(DaemonCmd::List { json: true }));
        assert_eq!(parse_daemon_args(&v(&["start", "alpha"]), None), Ok(DaemonCmd::Start { name: "alpha".into(), restart: RestartMode::Never, limits: MemoryLimits::default() }));
        assert_eq!(parse_daemon_args(&v(&["start"]), None), Ok(DaemonCmd::Start { name: "default".into(), restart: RestartMode::Never, limits: MemoryLimits::default() }));
        assert_eq!(
            parse_daemon_args(&v(&["start", "--restart", "on-crash", "w"]), None),
            Ok(DaemonCmd::Start { name: "w".into(), restart: RestartMode::OnCrash, limits: MemoryLimits::default() })
        );
        assert_eq!(
            parse_daemon_args(&v(&["start", "--restart=never"]), None),
            Ok(DaemonCmd::Start { name: "default".into(), restart: RestartMode::Never, limits: MemoryLimits::default() })
        );
        assert!(parse_daemon_args(&v(&["start", "--restart", "always"]), None).unwrap_err().contains("invalid restart"));
        assert!(parse_daemon_args(&v(&["start", "--restart"]), None).unwrap_err().contains("needs"));
        assert_eq!(
            parse_daemon_args(&v(&["stop", "beta", "--force"]), None),
            Ok(DaemonCmd::Stop { name: "beta".into(), force: true })
        );
        assert_eq!(
            parse_daemon_args(&v(&["--force", "stop"]), Some("gamma")),
            Ok(DaemonCmd::Stop { name: "gamma".into(), force: true })
        );
        assert_eq!(
            parse_daemon_args(&v(&["status"]), Some("gamma")),
            Ok(DaemonCmd::Status { name: "gamma".into(), json: false })
        );
        // An explicit NAME beats the --daemon-name / env default.
        assert_eq!(
            parse_daemon_args(&v(&["status", "alpha", "--json"]), Some("gamma")),
            Ok(DaemonCmd::Status { name: "alpha".into(), json: true })
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_daemon_args(&v(&["start", "../evil"]), None).unwrap_err().contains("invalid daemon name"));
        assert!(parse_daemon_args(&v(&["restart"]), None).unwrap_err().contains("unknown daemon subcommand"));
        assert!(parse_daemon_args(&v(&["stop", "a", "b"]), None).unwrap_err().contains("too many"));
        assert!(parse_daemon_args(&v(&["list", "--bogus"]), None).unwrap_err().contains("unknown option"));
        assert!(parse_daemon_args(&v(&["list", "extra"]), None).is_err());
        assert!(parse_daemon_args(&v(&["--help"]), None).unwrap_err().starts_with("Usage:"));
    }

    #[test]
    fn takes_daemon_target_from_switch_then_env() {
        let mut a = v(&["ctl", "--daemon-name", "alpha", "daemon.status"]);
        assert_eq!(take_daemon_target(&mut a, Some("envd")), Ok(Some("alpha".into())));
        assert_eq!(a, v(&["ctl", "daemon.status"]));

        let mut a = v(&["pane", "list", "--daemon-name=beta"]);
        assert_eq!(take_daemon_target(&mut a, None), Ok(Some("beta".into())));
        assert_eq!(a, v(&["pane", "list"]));

        let mut a = v(&["ctl", "daemon.status"]);
        assert_eq!(take_daemon_target(&mut a, Some(" envd ")), Ok(Some("envd".into())));
        assert_eq!(take_daemon_target(&mut a, Some("")), Ok(None));
        assert_eq!(take_daemon_target(&mut a, None), Ok(None));
        assert_eq!(a, v(&["ctl", "daemon.status"]));
    }

    #[test]
    fn daemon_target_edge_cases() {
        // Text after a literal `--` is payload, never a switch.
        let mut a = v(&["pane", "send", "p1", "--", "--daemon-name", "x"]);
        assert_eq!(take_daemon_target(&mut a, None), Ok(None));
        assert_eq!(a.len(), 6);

        let mut a = v(&["ctl", "--daemon-name"]);
        assert!(take_daemon_target(&mut a, None).unwrap_err().contains("needs a NAME"));
        let mut a = v(&["ctl", "--daemon-name", "a/b"]);
        assert!(take_daemon_target(&mut a, None).unwrap_err().contains("invalid daemon name"));
        let mut a = v(&["ctl"]);
        assert!(take_daemon_target(&mut a, Some("../x")).is_err());
    }

    #[test]
    fn list_status_and_endpoint_on_empty_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (text, code) = run(&DaemonCmd::List { json: false }, dir.path());
        assert!(text.starts_with("No bilterm daemons running"), "{text}");
        assert_eq!(code, 0);
        assert_eq!(run(&DaemonCmd::List { json: true }, dir.path()).0, r#"{"daemons":[]}"#);
        let (text, code) = run(&DaemonCmd::Status { name: "alpha".into(), json: false }, dir.path());
        assert_eq!((text.as_str(), code), ("daemon 'alpha': not running", 3));
        let (text, code) = run(&DaemonCmd::Stop { name: "alpha".into(), force: false }, dir.path());
        assert_eq!((text.as_str(), code), ("daemon 'alpha' was not running", 0));
        assert!(resolve_daemon_endpoint(dir.path(), "alpha").unwrap_err().contains("bilterm daemon start alpha"));
    }

    #[test]
    fn lists_and_resolves_a_lock_holding_daemon() {
        use crate::daemon::supervisor::{acquire_lock, DaemonLockResult};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("custom.sock");
        let DaemonLockResult::Acquired(lock) = acquire_lock(dir.path(), "alpha", sock.clone()).unwrap() else {
            panic!("expected to acquire the lock");
        };
        assert_eq!(resolve_daemon_endpoint(dir.path(), "alpha"), Ok(format!("unix:{}", sock.display())));
        let (text, code) = run(&DaemonCmd::Status { name: "alpha".into(), json: false }, dir.path());
        assert!(text.contains("running") && text.contains("custom.sock"), "{text}");
        assert_eq!(code, 0);
        let (text, _) = run(&DaemonCmd::List { json: false }, dir.path());
        assert!(text.contains("alpha") && text.contains("running"), "{text}");
        drop(lock);
        assert!(resolve_daemon_endpoint(dir.path(), "alpha").is_err());
    }
}
