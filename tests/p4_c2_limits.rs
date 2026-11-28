//! P4 / C2 + C3: memory limits (the watchdog test runs on Linux and macOS; scope test is Linux). Surface parity for `--memory-max/--memory-high`, and live tests
//! that a hog host is killed under a small limit by (a) a systemd user scope and (b) the
//! userspace RSS watchdog. The scope test skips cleanly when systemd user scopes are unavailable.

mod common;

use bilterm::action::{Action, DaemonSupervisorOp};
use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::daemon::limits::MemoryLimits;

fn limits(max: &str, high: Option<&str>) -> MemoryLimits {
    MemoryLimits::from_args(Some(max), high).unwrap()
}

#[test]
fn memory_flags_parse_identically_on_colon_cli_and_rpc() {
    use bilterm::colon_command::parse_colon_command;
    let want = limits("2G", Some("1G"));

    let colon = parse_colon_command(":daemon start w --memory-max 2G --memory-high=1G").unwrap().unwrap();
    assert_eq!(colon, Action::DaemonSupervisor(DaemonSupervisorOp::Start { name: Some("w".into()), restart: None, limits: Some(want), host_ptys: false }));

    let args: Vec<String> = ["start", "w", "--memory-max=2G", "--memory-high", "1G"].iter().map(|s| s.to_string()).collect();
    match bilterm::cli::daemon_cmd::parse_daemon_args(&args, None).unwrap() {
        bilterm::cli::daemon_cmd::DaemonCmd::Start { limits: l, .. } => assert_eq!(l, want),
        other => panic!("{other:?}"),
    }

    // No flags: unlimited everywhere.
    let none = parse_colon_command(":daemon start w").unwrap().unwrap();
    assert_eq!(none, Action::DaemonSupervisor(DaemonSupervisorOp::Start { name: Some("w".into()), restart: None, limits: None, host_ptys: false }));
}

#[test]
fn high_without_max_or_bad_sizes_are_rejected_on_every_surface() {
    use bilterm::colon_command::parse_colon_command;
    assert!(parse_colon_command(":daemon start w --memory-high 1G").unwrap().unwrap_err().contains("memory-max"));
    assert!(parse_colon_command(":daemon start w --memory-max bogus").unwrap().is_err());
    assert!(parse_colon_command(":daemon start w --memory-max").unwrap().is_err());

    let cli = |a: &[&str]| bilterm::cli::daemon_cmd::parse_daemon_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), None);
    assert!(cli(&["start", "--memory-high", "1G"]).unwrap_err().contains("memory-max"));
    assert!(cli(&["start", "--memory-max", "1G", "--memory-high", "2G"]).unwrap_err().contains("below"));
    assert!(cli(&["start", "--memory-max"]).unwrap_err().contains("needs a size"));

    let mut cp = AutomationControlPlane::new_headless(24, 80);
    let p = |k: &str, v: &str| JsonValue::obj([("name", JsonValue::String("w".into())), (k, JsonValue::String(v.into()))]);
    assert!(cp.dispatch_method("daemon.start", &p("memory_high", "1G")).unwrap_err().contains("memory-max"));
    assert!(cp.dispatch_method("daemon.start", &p("memory_max", "nope")).unwrap_err().contains("memory-max"));
    let mcp = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let tools = cp.handle_mcp_line(mcp).expect("tools/list");
    assert!(tools.contains("memory_max") && tools.contains("memory_high"), "MCP bilterm_daemon schema lacks limit params");
}

#[test]
fn job_object_backend_roundtrips_in_supervisor_record_and_selection() {
    use bilterm::daemon::limits::{choose_backend_with_job, classify_exit_with_code, BackendPref, ExitCause, LimitBackend, Probe};
    use bilterm::daemon::restart_policy::{LastExit, RestartMode};
    use bilterm::daemon::supervise::{SupState, SupervisorRecord};

    let sel = choose_backend_with_job(BackendPref::Auto, &Probe::default(), true);
    assert_eq!(sel.backend, LimitBackend::JobObject);
    assert_eq!(LimitBackend::parse(sel.backend.as_str()), Some(LimitBackend::JobObject));

    let lim = limits("64M", Some("48M"));
    assert_eq!(
        classify_exit_with_code(None, Some(1), LimitBackend::JobObject, &lim, 56 << 20, false),
        Some(ExitCause::Limit)
    );

    let rec = SupervisorRecord {
        name: "winhost".into(),
        supervisor_pid: 42,
        daemon_pid: None,
        state: SupState::Backoff,
        mode: RestartMode::OnCrash,
        restart_count: 1,
        last_exit: Some(LastExit { code: Some(1), signal: None, at_ms: 500 }),
        next_restart_at_ms: Some(1500),
        updated_at_ms: 500,
        limit_max_bytes: lim.max_bytes,
        limit_high_bytes: lim.high_bytes,
        limit_backend: Some(LimitBackend::JobObject),
        last_exit_cause: Some(ExitCause::Limit),
    };
    assert_eq!(SupervisorRecord::from_json(&rec.to_json()), Ok(rec));
}

#[cfg(unix)]
mod live {
    use super::*;
    #[cfg(target_os = "linux")]
    use bilterm::daemon::limits;
    use bilterm::daemon::limits::{ExitCause, LimitBackend};
    use bilterm::daemon::restart_policy::RestartConfig;
    use bilterm::daemon::supervise::{supervise, ProcessRunner, SupState};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// A "host" that ignores its arguments and allocates ~100 MB/s forever. With `lock` it also
    /// takes the daemon flock and publishes its pid, as a real host does (the watchdog only
    /// signals lock-verified pids).
    fn hog_script(dir: &std::path::Path, lock: bool) -> Option<std::path::PathBuf> {
        let py = std::process::Command::new("python3").arg("--version").output().ok()?;
        py.status.success().then_some(())?;
        let lock_code = if lock {
            r#"
import fcntl, json
d = os.path.join(os.environ["BILTERM_RUNTIME_DIR"], "daemons")
os.makedirs(d, exist_ok=True)
f = open(os.path.join(d, "daemon-%s.lock" % name), "w")
fcntl.flock(f, fcntl.LOCK_EX)
f.write(json.dumps({"pid": os.getpid(), "name": name, "version": "hog", "started_at_ms": 1, "socket_path": "/nonexistent.sock"}))
f.flush()
"#
        } else {
            ""
        };
        let src = format!(
            "#!/usr/bin/env python3\nimport os, sys, time\nname = sys.argv[sys.argv.index('--name') + 1]\n{lock_code}\nchunks = []\nwhile True:\n    chunks.append(bytearray(b'x' * (2 << 20)))\n    time.sleep(0.02)\n"
        );
        let path = dir.join(if lock { "hog-lock.py" } else { "hog.py" });
        std::fs::write(&path, src).ok()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
        Some(path)
    }

    fn private_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    #[test]
    fn watchdog_terminates_a_hog_host_and_counts_it_as_a_crash() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = private_dir();
        let Some(exe) = hog_script(dir.path(), true) else {
            eprintln!("skip: python3 unavailable");
            return;
        };
        // SAFETY: serialized by ENV_LOCK; read by ProcessRunner::new / the hog child.
        unsafe {
            std::env::set_var("BILTERM_LIMIT_POLL_MS", "50");
            std::env::set_var("BILTERM_LIMIT_GRACE_MS", "1000");
            std::env::set_var("BILTERM_RUNTIME_DIR", dir.path());
        }
        let lim = limits("64M", Some("48M"));
        let mut runner = ProcessRunner::new(dir.path(), "wdog", exe).with_limits(lim, LimitBackend::Watchdog);
        let rec = supervise(&RestartConfig::default(), "wdog", std::process::id(), &mut runner);
        assert_eq!(rec.state, SupState::Stopped);
        assert_eq!(rec.limit_backend, Some(LimitBackend::Watchdog));
        assert_eq!(rec.limit_max_bytes, Some(64 << 20));
        let exit = rec.last_exit.expect("exit recorded");
        assert!(matches!(exit.signal, Some(15) | Some(9)), "killed by watchdog signal, got {exit:?}");
        assert!(!exit.is_clean());
        assert_eq!(rec.last_exit_cause, Some(ExitCause::Limit));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_scope_kills_a_hog_host_under_a_small_limit() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let probe = limits::probe_systemd_scope();
        if !probe.supported() {
            eprintln!("skip: systemd user scope with memory controller unavailable ({probe:?})");
            return;
        }
        let dir = private_dir();
        let Some(exe) = hog_script(dir.path(), false) else {
            eprintln!("skip: python3 unavailable");
            return;
        };
        unsafe {
            std::env::set_var("BILTERM_LIMIT_POLL_MS", "25");
            std::env::set_var("BILTERM_RUNTIME_DIR", dir.path());
        }
        let lim = limits("64M", None);
        let mut runner = ProcessRunner::new(dir.path(), "scoped", exe).with_limits(lim, LimitBackend::SystemdScope);
        let rec = supervise(&RestartConfig::default(), "scoped", std::process::id(), &mut runner);
        let exit = rec.last_exit.expect("exit recorded");
        assert!(!exit.is_clean(), "hog must not exit cleanly: {exit:?}");
        assert!(exit.signal == Some(9) || exit.code == Some(137), "cgroup OOM kill expected, got {exit:?}");
        assert_eq!(rec.limit_backend, Some(LimitBackend::SystemdScope));
        assert_eq!(rec.last_exit_cause, Some(ExitCause::Limit), "{rec:?}");
    }
}
