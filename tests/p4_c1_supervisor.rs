//! P4 / C1: supervisor restart policy, registry back-compat and `daemon list` surface parity.
//!
//! Deterministic parts (policy, loop with fake clock) live in unit tests next to the code; here:
//! old registry JSON still parses, every `daemon list` surface shows the same new columns, and a
//! real `bilterm supervisor` restarts a `kill -9`'d daemon (Unix only, deadline based).

mod common;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use bilterm::action::{Action, DaemonSupervisorOp};
use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::daemon::registry::{DaemonRegistry, DaemonRegistryEntry};
use bilterm::daemon::restart_policy::{LastExit, RestartMode};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Throwaway runtime dir; serialized because it sets process env.
fn sandbox() -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("sandbox");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // SAFETY: serialized by ENV_LOCK; every test in this file goes through sandbox().
    unsafe {
        std::env::set_var("BILTERM_RUNTIME_DIR", dir.path());
    }
    (guard, dir)
}

const OLD_JSON: &str = r#"{"id":"daem_old","name":"old","pid":4242,"version":"0.9.0","started_at_ms":5,"socket_path":"/run/old.sock","sessions_count":1,"panes_count":2,"agents_count":0,"last_heartbeat_ms":9}"#;

#[test]
fn old_registry_json_without_new_fields_still_parses() {
    let e = DaemonRegistryEntry::from_json(OLD_JSON).expect("old record parses");
    assert_eq!((e.pid, e.panes_count), (4242, 2));
    assert_eq!(e.rss_bytes, None);
    assert_eq!(e.restart_count, 0);
    assert_eq!(e.last_exit, None);
    assert_eq!((e.limit_max_bytes, e.limit_high_bytes), (None, None));
    // The serde derive (the documented contract) agrees, thanks to #[serde(default)].
    let s: DaemonRegistryEntry = serde_json::from_str(OLD_JSON).expect("serde parse of old record");
    assert_eq!(s, e);
}

#[test]
fn new_fields_round_trip_and_survive_heartbeat() {
    let (_g, dir) = sandbox();
    let entry = DaemonRegistryEntry {
        id: "daem_rt".into(),
        name: "rt".into(),
        pid: std::process::id(),
        version: "1".into(),
        socket_path: PathBuf::from("/x.sock"),
        rss_bytes: Some(7_000_000),
        restart_count: 3,
        last_exit: Some(LastExit { code: None, signal: Some(9), at_ms: 11 }),
        limit_max_bytes: Some(1 << 30),
        limit_high_bytes: Some(1 << 29),
        ..Default::default()
    };
    assert_eq!(DaemonRegistryEntry::from_json(&entry.to_json()), Ok(entry.clone()));
    DaemonRegistry::register(dir.path(), &entry).unwrap();
    DaemonRegistry::update_runtime_stats(dir.path(), "rt", Some(9_000_000)).unwrap();
    DaemonRegistry::update_heartbeat(dir.path(), "rt", 1, 2, 3).unwrap();
    let got = DaemonRegistry::list_active(dir.path()).into_iter().find(|e| e.name == "rt").expect("listed");
    assert_eq!(got.rss_bytes, Some(9_000_000));
    assert_eq!((got.restart_count, got.limit_max_bytes), (3, Some(1 << 30)));
    assert_eq!(got.last_exit.map(|e| e.signal), Some(Some(9)));
}

fn daemon_rows(v: &JsonValue) -> Vec<JsonValue> {
    v.get("daemons").and_then(JsonValue::as_array).expect("daemons array").to_vec()
}

#[test]
fn daemon_list_columns_are_identical_on_cli_rpc_colon_and_mcp() {
    let (_g, dir) = sandbox();
    let entry = DaemonRegistryEntry {
        id: "daem_par".into(),
        name: "par".into(),
        pid: std::process::id(),
        version: "1".into(),
        socket_path: PathBuf::from("/par.sock"),
        sessions_count: 1,
        rss_bytes: Some(12_345_678),
        restart_count: 2,
        last_exit: Some(LastExit { code: Some(1), signal: None, at_ms: 5 }),
        ..Default::default()
    };
    DaemonRegistry::register(dir.path(), &entry).unwrap();

    // CLI --json
    let (text, code) = bilterm::cli::daemon_cmd::run(&bilterm::cli::daemon_cmd::DaemonCmd::List { json: true }, dir.path());
    assert_eq!(code, 0);
    let cli = daemon_rows(&JsonValue::parse(&text).unwrap());
    // CLI text table
    let (table, _) = bilterm::cli::daemon_cmd::run(&bilterm::cli::daemon_cmd::DaemonCmd::List { json: false }, dir.path());
    assert!(table.contains("RSS") && table.contains("RESTARTS") && table.contains("11.8M") && table.contains("exit 1"), "{table}");

    // RPC (also what `bilterm ctl daemon.list` sends)
    let mut cp = AutomationControlPlane::new_headless(24, 80);
    let rpc = daemon_rows(&cp.dispatch_method("daemon.list", &JsonValue::Null).unwrap());

    // Colon command / Action::Daemon via WindowManager::apply
    let _ = cp.wm.apply(Action::Daemon(DaemonSupervisorOp::List));
    let colon = daemon_rows(&JsonValue::parse(&cp.wm.take_command_data().expect("command data")).unwrap());

    // MCP tool
    let mcp_line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_daemon","arguments":{"action":"list"}}}"#;
    let mcp = cp.handle_mcp_line(mcp_line).expect("mcp response");
    assert!(mcp.contains("rss_bytes") && mcp.contains("restart_count") && mcp.contains("last_exit"), "{mcp}");

    let pick = |rows: &[JsonValue]| rows.iter().find(|r| r.get("name").and_then(JsonValue::as_str) == Some("par")).cloned().expect("par row");
    let (a, b, c) = (pick(&cli), pick(&rpc), pick(&colon));
    assert_eq!(a, b, "CLI and RPC rows differ");
    assert_eq!(b, c, "RPC and colon rows differ");
    assert_eq!(a.get("rss_bytes").and_then(JsonValue::as_i64), Some(12_345_678));
    assert_eq!(a.get("restart_count").and_then(JsonValue::as_i64), Some(2));
    assert_eq!(a.get("last_exit").and_then(|e| e.get("code")).and_then(JsonValue::as_i64), Some(1));
}

#[test]
fn start_restart_flag_parses_identically_on_colon_cli_and_rpc_params() {
    use bilterm::colon_command::parse_colon_command;
    let parsed = parse_colon_command(":daemon start w --restart on-crash").unwrap().unwrap();
    assert_eq!(
        parsed,
        Action::DaemonSupervisor(DaemonSupervisorOp::Start { name: Some("w".into()), restart: Some(RestartMode::OnCrash) })
    );
    assert!(parse_colon_command(":daemon start w --restart bogus").unwrap().is_err());
    let cli = bilterm::cli::daemon_cmd::parse_daemon_args(&["start".into(), "w".into(), "--restart".into(), "on-crash".into()], None);
    assert_eq!(cli, Ok(bilterm::cli::daemon_cmd::DaemonCmd::Start { name: "w".into(), restart: RestartMode::OnCrash }));
    let (_g, _d) = sandbox();
    let mut cp = AutomationControlPlane::new_headless(24, 80);
    let bad = JsonValue::obj([("name", JsonValue::String("w".into())), ("restart", JsonValue::String("bogus".into()))]);
    assert!(cp.dispatch_method("daemon.start", &bad).unwrap_err().contains("invalid restart policy"));
}

#[cfg(unix)]
mod live {
    use super::*;
    use bilterm::daemon::supervise::{read_supervisor_record, SupState};
    use bilterm::daemon::supervisor::{ensure_started_exe, kill_daemon_process, stop_daemon, DaemonStartResult, SignalablePid};

    fn started_pid(r: DaemonStartResult) -> u32 {
        match r {
            DaemonStartResult::Started { pid, .. } | DaemonStartResult::AlreadyRunning { pid, .. } => pid,
        }
    }

    fn live_pid(dir: &std::path::Path, name: &str) -> Option<u32> {
        DaemonRegistry::list_active(dir).into_iter().find(|e| e.name == name).map(|e| e.pid)
    }

    #[test]
    fn supervisor_restarts_a_sigkilled_daemon_and_stop_is_not_a_crash() {
        let (_g, dir) = sandbox();
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_bilterm"));
        let name = "p4c1";
        let pid1 = started_pid(ensure_started_exe(&exe, dir.path(), name, RestartMode::OnCrash).expect("start"));
        assert!(pid1 > 1);

        // kill -9 the daemon (lock-verified pid): the supervisor must observe it and respawn.
        kill_daemon_process(SignalablePid::try_from(i64::from(pid1)).unwrap(), true).unwrap();
        let restarted = common::wait_until(common::PROCESS_DEADLINE, || {
            live_pid(dir.path(), name).is_some_and(|p| p != pid1)
                && bilterm::daemon::process::running_daemon_socket_in(dir.path(), name).is_some()
        });
        assert!(restarted, "daemon was not restarted: {:?}", read_supervisor_record(dir.path(), name));
        let pid2 = live_pid(dir.path(), name).unwrap();
        let rec = read_supervisor_record(dir.path(), name).expect("supervisor record");
        assert_eq!(rec.restart_count, 1);
        assert_eq!(rec.last_exit.and_then(|e| e.signal), Some(9));
        assert_eq!(rec.mode, RestartMode::OnCrash);

        // `daemon list` shows the restart and the sampled RSS.
        let row = bilterm::daemon::list_view::collect_rows(dir.path(), false).into_iter().find(|r| r.name == name).unwrap();
        assert_eq!((row.status, row.restart_count), ("running", 1));
        assert_eq!(row.last_exit.and_then(|e| e.signal), Some(9));
        assert!(row.rss_bytes.is_some_and(|b| b > 0), "rss missing: {row:?}");

        // A deliberate stop (even though it ends in a signal) must NOT be restarted.
        assert!(stop_daemon(dir.path(), name, true).unwrap());
        let stopped = common::wait_until(common::PROCESS_DEADLINE, || {
            read_supervisor_record(dir.path(), name).is_some_and(|r| r.state == SupState::Stopped)
        });
        assert!(stopped, "supervisor did not settle after stop: {:?}", read_supervisor_record(dir.path(), name));
        std::thread::sleep(Duration::from_millis(1500)); // longer than the 1 s first backoff
        assert!(live_pid(dir.path(), name).is_none_or(|p| p == pid2 && !bilterm::daemon::supervisor::check_pid_alive(p)));
        assert_eq!(read_supervisor_record(dir.path(), name).unwrap().restart_count, 1, "stop must not count as a restart");
    }

    #[test]
    fn default_policy_never_restarts_and_records_the_exit() {
        let (_g, dir) = sandbox();
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_bilterm"));
        let name = "p4c1n";
        let pid = started_pid(ensure_started_exe(&exe, dir.path(), name, RestartMode::Never).expect("start"));
        kill_daemon_process(SignalablePid::try_from(i64::from(pid)).unwrap(), true).unwrap();
        let done = common::wait_until(common::PROCESS_DEADLINE, || {
            read_supervisor_record(dir.path(), name).is_some_and(|r| r.state == SupState::Stopped && r.last_exit.is_some())
        });
        assert!(done, "exit not recorded: {:?}", read_supervisor_record(dir.path(), name));
        let rec = read_supervisor_record(dir.path(), name).unwrap();
        assert_eq!((rec.restart_count, rec.last_exit.and_then(|e| e.signal)), (0, Some(9)));
        std::thread::sleep(Duration::from_millis(1500));
        assert!(live_pid(dir.path(), name).is_none(), "policy never must not respawn");
        let _ = stop_daemon(dir.path(), name, true);
    }
}
