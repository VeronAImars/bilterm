//! Long-lived `bilterm supervisor` that owns the host `Child` (P4 / C1).
//!
//! Replaces the old fire-and-forget `daemon-reaper` thread, which discarded the exit status. The
//! supervisor spawns the daemon, `wait()`s on it (pid-reuse safe: it holds the `Child`), classifies
//! the exit, applies [`super::restart_policy`], and publishes a [`SupervisorRecord`] to
//! `<runtime>/daemons/<name>.sup` so `daemon list` can show restarts, the last exit and `failed`.
//!
//! Safety (controlling-processes-safely): the supervisor never signals anything. `daemon stop`
//! leaves a `<name>.stop` marker (so a deliberate stop, even `--force`/SIGKILL, is not a crash)
//! and signals the lock-verified daemon pid. The pids in the `.sup` record are display-only.
//!
//! The loop is generic over [`HostRunner`] so tests drive it with a fake clock and fake host.

use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::api::json::JsonValue;
use crate::daemon::limits::{self, ExitCause, LimitBackend, MemoryLimits, Watchdog, WdAction};
use crate::daemon::restart_policy::{decide, Decision, LastExit, RestartConfig, RestartMode, RestartState};
use crate::secure_fs::{ensure_private_dir, validate_safe_name, write_private_file};

/// Lifecycle state of a supervised host as published by its supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupState {
    /// Host process is up.
    Running,
    /// Host crashed; waiting to respawn.
    Backoff,
    /// Crash-loop breaker tripped; the host stays down until `daemon start`.
    Failed,
    /// Host exited and the policy leaves it down (clean exit, `never`, or `daemon stop`).
    Stopped,
}

impl SupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Backoff => "restarting",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "restarting" => Self::Backoff,
            "failed" => Self::Failed,
            "stopped" => Self::Stopped,
            _ => return None,
        })
    }
}

/// What a supervisor publishes about its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupervisorRecord {
    pub name: String,
    pub supervisor_pid: u32,
    pub daemon_pid: Option<u32>,
    pub state: SupState,
    pub mode: RestartMode,
    pub restart_count: u32,
    pub last_exit: Option<LastExit>,
    pub next_restart_at_ms: Option<u64>,
    pub updated_at_ms: u64,
    /// Memory limits this supervisor enforces (`None` = unlimited).
    pub limit_max_bytes: Option<u64>,
    pub limit_high_bytes: Option<u64>,
    /// How the limit is enforced; `None` when unlimited.
    pub limit_backend: Option<LimitBackend>,
    /// Why the last exit happened, when known (`limit`).
    pub last_exit_cause: Option<ExitCause>,
}

impl SupervisorRecord {
    pub fn to_json(&self) -> String {
        let num = |v: Option<i64>| v.map_or(JsonValue::Null, JsonValue::Number);
        let mut m = BTreeMap::new();
        m.insert("name".into(), JsonValue::String(self.name.clone()));
        m.insert("supervisor_pid".into(), JsonValue::Number(i64::from(self.supervisor_pid)));
        m.insert("daemon_pid".into(), num(self.daemon_pid.map(i64::from)));
        m.insert("state".into(), JsonValue::String(self.state.as_str().into()));
        m.insert("mode".into(), JsonValue::String(self.mode.as_str().into()));
        m.insert("restart_count".into(), JsonValue::Number(i64::from(self.restart_count)));
        m.insert("next_restart_at_ms".into(), num(self.next_restart_at_ms.map(|v| v as i64)));
        m.insert("updated_at_ms".into(), JsonValue::Number(self.updated_at_ms as i64));
        m.insert("last_exit".into(), self.last_exit.as_ref().map_or(JsonValue::Null, last_exit_to_json));
        m.insert("limit_max_bytes".into(), num(self.limit_max_bytes.map(|v| v as i64)));
        m.insert("limit_high_bytes".into(), num(self.limit_high_bytes.map(|v| v as i64)));
        let text = |v: Option<&str>| v.map_or(JsonValue::Null, |s| JsonValue::String(s.into()));
        m.insert("limit_backend".into(), text(self.limit_backend.map(LimitBackend::as_str)));
        m.insert("last_exit_cause".into(), text(self.last_exit_cause.map(ExitCause::as_str)));
        JsonValue::Object(m).to_json_string()
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        let v = JsonValue::parse(s).ok_or("invalid json in supervisor record")?;
        let name = v.get("name").and_then(JsonValue::as_str).ok_or("missing name")?.to_string();
        let state = v.get("state").and_then(JsonValue::as_str).and_then(SupState::parse).ok_or("bad state")?;
        let u = |k: &str| v.get(k).and_then(JsonValue::as_i64).filter(|n| *n >= 0);
        Ok(Self {
            name,
            supervisor_pid: u("supervisor_pid").ok_or("missing supervisor_pid")? as u32,
            daemon_pid: u("daemon_pid").map(|n| n as u32),
            state,
            mode: v.get("mode").and_then(JsonValue::as_str).and_then(|m| RestartMode::parse(m).ok()).unwrap_or_default(),
            restart_count: u("restart_count").unwrap_or(0) as u32,
            last_exit: v.get("last_exit").and_then(last_exit_from_json),
            next_restart_at_ms: u("next_restart_at_ms").map(|n| n as u64),
            updated_at_ms: u("updated_at_ms").unwrap_or(0) as u64,
            limit_max_bytes: u("limit_max_bytes").map(|n| n as u64),
            limit_high_bytes: u("limit_high_bytes").map(|n| n as u64),
            limit_backend: v.get("limit_backend").and_then(JsonValue::as_str).and_then(LimitBackend::parse),
            last_exit_cause: v.get("last_exit_cause").and_then(JsonValue::as_str).and_then(ExitCause::parse),
        })
    }
}

/// `{code, signal, at_ms}` with `null` for absent code/signal.
pub fn last_exit_to_json(e: &LastExit) -> JsonValue {
    let opt = |v: Option<i32>| v.map_or(JsonValue::Null, |n| JsonValue::Number(i64::from(n)));
    JsonValue::obj([("code", opt(e.code)), ("signal", opt(e.signal)), ("at_ms", JsonValue::Number(e.at_ms as i64))])
}

/// Inverse of [`last_exit_to_json`]; `None` for `null` or a non-object.
pub fn last_exit_from_json(v: &JsonValue) -> Option<LastExit> {
    v.get("at_ms")?;
    let opt = |k: &str| v.get(k).and_then(JsonValue::as_i64).map(|n| n as i32);
    Some(LastExit { code: opt("code"), signal: opt("signal"), at_ms: v.get("at_ms").and_then(JsonValue::as_i64).unwrap_or(0).max(0) as u64 })
}

fn state_path(runtime_dir: &Path, name: &str, ext: &str) -> io::Result<PathBuf> {
    validate_safe_name(name).map_err(|e| io::Error::new(ErrorKind::InvalidInput, format!("invalid daemon name: {e}")))?;
    let dir = runtime_dir.join("daemons");
    ensure_private_dir(&dir)?;
    Ok(dir.join(format!("{name}.{ext}")))
}

/// `<runtime>/daemons/<name>.sup`.
pub fn supervisor_record_path(runtime_dir: &Path, name: &str) -> io::Result<PathBuf> {
    state_path(runtime_dir, name, "sup")
}

/// `<runtime>/daemons/<name>.stop`: present while a deliberate stop is in flight.
pub fn stop_marker_path(runtime_dir: &Path, name: &str) -> io::Result<PathBuf> {
    state_path(runtime_dir, name, "stop")
}

/// Atomically (0600) writes the record.
pub fn write_supervisor_record(runtime_dir: &Path, rec: &SupervisorRecord) -> io::Result<()> {
    write_private_file(&supervisor_record_path(runtime_dir, &rec.name)?, rec.to_json().as_bytes())
}

/// Reads and parses the record; `None` when absent or malformed.
pub fn read_supervisor_record(runtime_dir: &Path, name: &str) -> Option<SupervisorRecord> {
    let path = supervisor_record_path(runtime_dir, name).ok()?;
    SupervisorRecord::from_json(&std::fs::read_to_string(path).ok()?).ok()
}

/// Removes the record (e.g. `daemon prune`, or a `failed` record acknowledged by `daemon start`).
pub fn clear_supervisor_record(runtime_dir: &Path, name: &str) {
    if let Ok(p) = supervisor_record_path(runtime_dir, name) {
        let _ = std::fs::remove_file(p);
    }
}

/// Marks a deliberate stop so the supervisor does not treat the resulting exit as a crash.
pub fn request_stop(runtime_dir: &Path, name: &str) -> io::Result<()> {
    write_private_file(&stop_marker_path(runtime_dir, name)?, b"stop")
}

/// True while a stop marker exists.
pub fn stop_requested(runtime_dir: &Path, name: &str) -> bool {
    stop_marker_path(runtime_dir, name).is_ok_and(|p| p.exists())
}

/// Removes the stop marker (called before a new start).
pub fn clear_stop_request(runtime_dir: &Path, name: &str) {
    if let Ok(p) = stop_marker_path(runtime_dir, name) {
        let _ = std::fs::remove_file(p);
    }
}

/// Everything the supervision loop needs from the outside world.
pub trait HostRunner {
    /// Spawns the host, returning its pid.
    fn spawn(&mut self) -> io::Result<u32>;
    /// Blocks until the spawned host exits.
    fn wait(&mut self) -> io::Result<LastExit>;
    fn now_ms(&self) -> u64;
    /// Sleeps `ms`, returning `true` early if a stop was requested meanwhile.
    fn sleep_or_stop(&mut self, ms: u64) -> bool;
    fn stop_requested(&mut self) -> bool;
    /// Publishes the current record (best effort).
    fn publish(&mut self, rec: &SupervisorRecord);
    /// Memory limits and backend this host runs under (default: none).
    fn limits(&self) -> (MemoryLimits, Option<LimitBackend>) {
        (MemoryLimits::default(), None)
    }
    /// Cause of the exit `wait` just returned, when the runner can tell (default: unknown).
    fn last_exit_cause(&mut self) -> Option<ExitCause> {
        None
    }
}

/// Runs the spawn / wait / decide loop until the host is deliberately or finally down. Returns
/// the final record (also published).
pub fn supervise(cfg: &RestartConfig, name: &str, supervisor_pid: u32, env: &mut impl HostRunner) -> SupervisorRecord {
    let mut state = RestartState::default();
    let (limits, backend) = env.limits();
    let mut rec = SupervisorRecord {
        name: name.to_string(),
        supervisor_pid,
        daemon_pid: None,
        state: SupState::Stopped,
        mode: cfg.mode,
        restart_count: 0,
        last_exit: None,
        next_restart_at_ms: None,
        updated_at_ms: env.now_ms(),
        limit_max_bytes: limits.max_bytes,
        limit_high_bytes: limits.high_bytes,
        limit_backend: backend.filter(|_| limits.max_bytes.is_some()),
        last_exit_cause: None,
    };
    loop {
        let exit = match env.spawn() {
            Ok(pid) => {
                rec.daemon_pid = Some(pid);
                rec.state = SupState::Running;
                rec.next_restart_at_ms = None;
                rec.updated_at_ms = env.now_ms();
                env.publish(&rec);
                env.wait().unwrap_or(LastExit { code: None, signal: None, at_ms: env.now_ms() })
            }
            // A host that cannot even spawn is a crash (code -1): it feeds backoff and the breaker.
            Err(_) => LastExit { code: Some(-1), signal: None, at_ms: env.now_ms() },
        };
        rec.last_exit = Some(exit);
        rec.last_exit_cause = env.last_exit_cause();
        rec.daemon_pid = None;
        let stop = env.stop_requested();
        rec.updated_at_ms = exit.at_ms;
        match decide(cfg, &mut state, &exit, stop) {
            Decision::Restart { delay_ms } => {
                rec.restart_count += 1;
                rec.state = SupState::Backoff;
                rec.next_restart_at_ms = Some(exit.at_ms.saturating_add(delay_ms));
                env.publish(&rec);
                if env.sleep_or_stop(delay_ms) {
                    rec.state = SupState::Stopped;
                    rec.next_restart_at_ms = None;
                    rec.updated_at_ms = env.now_ms();
                    env.publish(&rec);
                    return rec;
                }
            }
            Decision::Tripped => {
                rec.state = SupState::Failed;
                rec.next_restart_at_ms = None;
                env.publish(&rec);
                return rec;
            }
            Decision::NoRestart(_) => {
                rec.state = SupState::Stopped;
                rec.next_restart_at_ms = None;
                env.publish(&rec);
                return rec;
            }
        }
    }
}

/// Real runner: spawns the daemon as a detached process group and polls the stop marker.
pub struct ProcessRunner {
    runtime_dir: PathBuf,
    name: String,
    exe: PathBuf,
    child: Option<std::process::Child>,
    limits: MemoryLimits,
    backend: LimitBackend,
    /// Systemd scope unit counter (unit names must be unique per spawn).
    spawns: u32,
    poll_ms: u64,
    grace_ms: u64,
    cause: Option<ExitCause>,
}

impl ProcessRunner {
    pub fn new(runtime_dir: &Path, name: &str, exe: PathBuf) -> Self {
        let env_ms = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(d);
        Self {
            runtime_dir: runtime_dir.to_path_buf(),
            name: name.to_string(),
            exe,
            child: None,
            limits: MemoryLimits::default(),
            backend: LimitBackend::Watchdog,
            spawns: 0,
            poll_ms: env_ms("BILTERM_LIMIT_POLL_MS", limits::DEFAULT_POLL_MS),
            grace_ms: env_ms("BILTERM_LIMIT_GRACE_MS", limits::DEFAULT_GRACE_MS),
            cause: None,
        }
    }

    /// Enforces `limits` through `backend` (ignored when `limits` sets no max).
    pub fn with_limits(mut self, limits: MemoryLimits, backend: LimitBackend) -> Self {
        self.limits = limits;
        self.backend = backend;
        self
    }

    /// Lock-verified: the daemon lock is held and names exactly this child's pid.
    fn pid_is_lock_verified(&self, pid: u32) -> bool {
        matches!(
            crate::daemon::liveness::daemon_liveness(&self.runtime_dir, &self.name),
            crate::daemon::liveness::DaemonLiveness::Running(rec) if rec.pid == pid
        )
    }

    /// Polls the child, sampling RSS. The watchdog backend escalates SIGTERM -> SIGKILL; the
    /// systemd backend only tracks the peak (the kernel enforces). Returns the exit and whether
    /// the watchdog signaled the host.
    fn wait_limited(&mut self, child: &mut std::process::Child, max: u64) -> io::Result<(std::process::ExitStatus, Option<i32>, u64)> {
        let pid = child.id();
        let mut dog = Watchdog::new(max, self.limits.high_bytes, self.grace_ms);
        let t0 = std::time::Instant::now();
        let mut next_sample = 0u64;
        let (mut peak, mut sent) = (0u64, None);
        loop {
            if let Some(st) = child.try_wait()? {
                return Ok((st, sent, peak));
            }
            let now = t0.elapsed().as_millis() as u64;
            if now >= next_sample {
                next_sample = now + self.poll_ms;
                let rss = limits::group_rss_bytes(pid);
                peak = peak.max(rss.unwrap_or(0));
                if self.backend == LimitBackend::Watchdog {
                    let signalable = self.pid_is_lock_verified(pid);
                    match dog.observe(now, rss, signalable) {
                        WdAction::Term => {
                            tracing::warn!(daemon = %self.name, pid, rss = ?rss, max, "memory limit exceeded: SIGTERM");
                            sent = Some(send_signal(pid, 15));
                        }
                        WdAction::Kill => {
                            tracing::warn!(daemon = %self.name, pid, "still alive after grace: SIGKILL");
                            sent = Some(send_signal(pid, 9));
                        }
                        WdAction::WarnHigh => tracing::warn!(daemon = %self.name, pid, rss = ?rss, "memory-high crossed"),
                        WdAction::None => {}
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50.min(self.poll_ms)));
        }
    }
}

/// Sends `sig` to exactly `pid` (callers have lock-verified it); returns `sig`.
fn send_signal(pid: u32, sig: i32) -> i32 {
    #[cfg(unix)]
    if pid > 1 && pid <= i32::MAX as u32 {
        // SAFETY: plain kill(2) on a pid the caller verified against the daemon lock.
        unsafe {
            libc::kill(pid as libc::pid_t, sig);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    sig
}

impl HostRunner for ProcessRunner {
    fn spawn(&mut self) -> io::Result<u32> {
        use std::process::{Command, Stdio};
        let sock = crate::daemon::process::session_daemon_socket_path_in(&self.runtime_dir, &self.name)?;
        // The previous host (if any) is dead: a leftover socket file must not block the new bind.
        let _ = crate::daemon::process::probe_or_cleanup_stale_socket(&sock);
        let listen = format!("unix:{}", sock.display());
        let host_args = ["--daemon".to_string(), "--name".to_string(), self.name.clone(), "-l".to_string(), listen];
        let limited = self.limits.max_bytes.is_some();
        let mut cmd = if limited && self.backend == LimitBackend::SystemdScope {
            self.spawns += 1;
            let unit = format!("bilterm-{}-{}-{}", self.name, std::process::id(), self.spawns);
            let argv = limits::scope_command(&unit, &self.limits, &self.exe.to_string_lossy(), &host_args);
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        } else {
            let mut c = Command::new(&self.exe);
            c.args(&host_args);
            c
        };
        cmd.env("BILTERM_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        let pid = child.id();
        if limited {
            limits::bias_oom_score(pid);
        }
        self.cause = None;
        self.child = Some(child);
        Ok(pid)
    }

    fn wait(&mut self) -> io::Result<LastExit> {
        let Some(mut child) = self.child.take() else {
            return Err(io::Error::other("no child"));
        };
        let (status, wd_signal, peak) = match self.limits.max_bytes {
            Some(max) => self.wait_limited(&mut child, max)?,
            None => (child.wait()?, None, 0),
        };
        #[cfg(unix)]
        let mut signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let mut signal = None;
        let mut code = status.code();
        self.cause = limits::classify_exit(signal, &self.limits, peak, wd_signal.is_some());
        // A host that exits cleanly after our SIGTERM was still killed by the limit: it is a
        // crash for the restart policy, never a clean exit.
        if let Some(sig) = wd_signal.filter(|_| code == Some(0) || signal.is_none()) {
            code = None;
            signal = Some(sig);
        }
        Ok(LastExit { code, signal, at_ms: crate::daemon::supervisor::now_ms() })
    }

    fn limits(&self) -> (MemoryLimits, Option<LimitBackend>) {
        (self.limits, self.limits.max_bytes.map(|_| self.backend))
    }

    fn last_exit_cause(&mut self) -> Option<ExitCause> {
        self.cause.take()
    }

    fn now_ms(&self) -> u64 {
        crate::daemon::supervisor::now_ms()
    }

    fn sleep_or_stop(&mut self, ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            if stop_requested(&self.runtime_dir, &self.name) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        stop_requested(&self.runtime_dir, &self.name)
    }

    fn stop_requested(&mut self) -> bool {
        stop_requested(&self.runtime_dir, &self.name)
    }

    fn publish(&mut self, rec: &SupervisorRecord) {
        let _ = write_supervisor_record(&self.runtime_dir, rec);
    }
}

/// `bilterm supervisor --name N [--runtime-dir D] [--restart never|on-crash] [--memory-max BYTES]
/// [--memory-high BYTES]`; returns the exit code.
pub fn run_cli(args: &[String]) -> i32 {
    let mut name = "default".to_string();
    let mut dir: Option<PathBuf> = None;
    let mut mode = RestartMode::Never;
    let (mut mem_max, mut mem_high) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = |it: &mut std::slice::Iter<String>| it.next().cloned();
        match a.as_str() {
            "--name" => name = v(&mut it).unwrap_or_default(),
            "--runtime-dir" => dir = v(&mut it).map(PathBuf::from),
            "--restart" => match v(&mut it).as_deref().map(RestartMode::parse) {
                Some(Ok(m)) => mode = m,
                Some(Err(e)) => return fail(&e),
                None => return fail("--restart needs a value"),
            },
            "--memory-max" => mem_max = v(&mut it),
            "--memory-high" => mem_high = v(&mut it),
            other => return fail(&format!("unknown supervisor option '{other}'")),
        }
    }
    if let Err(e) = validate_safe_name(&name) {
        return fail(&format!("invalid daemon name '{name}': {e}"));
    }
    let runtime_dir = match dir.map_or_else(crate::daemon::supervisor::resolve_runtime_dir, Ok) {
        Ok(d) => d,
        Err(e) => return fail(&format!("runtime dir: {e}")),
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return fail(&format!("current_exe: {e}")),
    };
    let mem = match MemoryLimits::from_args(mem_max.as_deref(), mem_high.as_deref()) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    let mut runner = ProcessRunner::new(&runtime_dir, &name, exe);
    if !mem.is_unlimited() {
        let sel = limits::select_backend_for_host();
        tracing::info!(daemon = %name, backend = sel.backend.as_str(), note = %sel.note, "memory limits");
        runner = runner.with_limits(mem, sel.backend);
    }
    let cfg = RestartConfig::with_mode(mode);
    let rec = supervise(&cfg, &name, std::process::id(), &mut runner);
    i32::from(rec.state == SupState::Failed)
}

fn fail(msg: &str) -> i32 {
    eprintln!("bilterm supervisor: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted host: each spawn consumes the next scripted exit (or spawn failure).
    struct Fake {
        now: u64,
        script: VecDeque<Result<LastExit, ()>>,
        next_pid: u32,
        pending: Option<LastExit>,
        stop_after_sleeps: Option<usize>,
        stop_flag: bool,
        sleeps: Vec<u64>,
        published: Vec<SupervisorRecord>,
        spawns: u32,
    }

    impl Fake {
        fn new(script: Vec<Result<LastExit, ()>>) -> Self {
            Self { now: 1_000, script: script.into(), next_pid: 100, pending: None, stop_after_sleeps: None, stop_flag: false, sleeps: vec![], published: vec![], spawns: 0 }
        }
    }

    impl HostRunner for Fake {
        fn spawn(&mut self) -> io::Result<u32> {
            self.spawns += 1;
            match self.script.pop_front().expect("script exhausted: loop did not stop") {
                Ok(e) => {
                    self.pending = Some(e);
                    self.next_pid += 1;
                    Ok(self.next_pid)
                }
                Err(()) => Err(io::Error::other("spawn failed")),
            }
        }
        fn wait(&mut self) -> io::Result<LastExit> {
            let mut e = self.pending.take().unwrap();
            self.now += 5_000; // host ran 5 s
            e.at_ms = self.now;
            Ok(e)
        }
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn sleep_or_stop(&mut self, ms: u64) -> bool {
            self.sleeps.push(ms);
            self.now += ms;
            self.stop_after_sleeps.is_some_and(|n| self.sleeps.len() >= n)
        }
        fn stop_requested(&mut self) -> bool {
            self.stop_flag
        }
        fn publish(&mut self, rec: &SupervisorRecord) {
            self.published.push(rec.clone());
        }
    }

    fn killed() -> Result<LastExit, ()> {
        Ok(LastExit { code: None, signal: Some(9), at_ms: 0 })
    }
    fn code(c: i32) -> Result<LastExit, ()> {
        Ok(LastExit { code: Some(c), signal: None, at_ms: 0 })
    }
    fn on_crash() -> RestartConfig {
        RestartConfig::with_mode(RestartMode::OnCrash)
    }

    #[test]
    fn never_policy_records_exit_and_does_not_respawn() {
        let mut f = Fake::new(vec![killed()]);
        let rec = supervise(&RestartConfig::default(), "w", 7, &mut f);
        assert_eq!(f.spawns, 1);
        assert_eq!(rec.state, SupState::Stopped);
        assert_eq!(rec.restart_count, 0);
        assert_eq!(rec.last_exit.unwrap().signal, Some(9));
        assert_eq!(rec.supervisor_pid, 7);
        assert!(f.sleeps.is_empty());
    }

    #[test]
    fn on_crash_restarts_with_backoff_then_clean_exit_stops() {
        let mut f = Fake::new(vec![killed(), code(1), code(0)]);
        let rec = supervise(&on_crash(), "w", 7, &mut f);
        assert_eq!(f.spawns, 3);
        assert_eq!(f.sleeps, vec![1_000, 2_000]);
        assert_eq!(rec.restart_count, 2);
        assert_eq!(rec.state, SupState::Stopped);
        assert!(rec.last_exit.unwrap().is_clean());
        // Backoff state was published with the next-restart deadline and the crash exit.
        let b = f.published.iter().find(|r| r.state == SupState::Backoff).unwrap();
        assert!(b.next_restart_at_ms.unwrap() > b.last_exit.unwrap().at_ms);
        assert_eq!(b.daemon_pid, None);
    }

    #[test]
    fn crash_loop_breaker_marks_failed_after_n_crashes() {
        let mut f = Fake::new((0..10).map(|_| killed()).collect());
        let rec = supervise(&on_crash(), "w", 7, &mut f);
        assert_eq!(rec.state, SupState::Failed);
        assert_eq!(f.spawns, 5, "5th crash in the window trips the breaker");
        assert_eq!(rec.restart_count, 4);
        assert_eq!(f.sleeps, vec![1_000, 2_000, 4_000, 8_000]);
        assert_eq!(f.published.last().unwrap().state, SupState::Failed);
    }

    #[test]
    fn deliberate_stop_is_never_a_crash_even_for_sigkill() {
        let mut f = Fake::new(vec![killed()]);
        f.stop_flag = true;
        let rec = supervise(&on_crash(), "w", 7, &mut f);
        assert_eq!((f.spawns, rec.state, rec.restart_count), (1, SupState::Stopped, 0));
    }

    #[test]
    fn stop_during_backoff_ends_without_respawn() {
        let mut f = Fake::new(vec![killed()]);
        f.stop_after_sleeps = Some(1);
        let rec = supervise(&on_crash(), "w", 7, &mut f);
        assert_eq!((f.spawns, rec.state), (1, SupState::Stopped));
        assert_eq!(rec.restart_count, 1);
    }

    #[test]
    fn spawn_failures_count_as_crashes_and_trip_the_breaker() {
        let mut f = Fake::new((0..10).map(|_| Err(())).collect());
        let rec = supervise(&on_crash(), "w", 7, &mut f);
        assert_eq!(rec.state, SupState::Failed);
        assert_eq!(f.spawns, 5);
        assert_eq!(rec.last_exit.unwrap().code, Some(-1));
    }

    #[test]
    fn old_record_without_limit_fields_parses() {
        let old = r#"{"name":"w","supervisor_pid":4,"daemon_pid":null,"state":"stopped","mode":"never","restart_count":0,"next_restart_at_ms":null,"updated_at_ms":1,"last_exit":null}"#;
        let r = SupervisorRecord::from_json(old).unwrap();
        assert_eq!((r.limit_max_bytes, r.limit_backend, r.last_exit_cause), (None, None, None));
    }

    /// A limit kill (cause reported by the runner) is an ordinary crash for the restart policy.
    #[test]
    fn limit_kill_restarts_like_any_crash_and_records_cause() {
        struct Limited(Fake);
        impl HostRunner for Limited {
            fn spawn(&mut self) -> io::Result<u32> { self.0.spawn() }
            fn wait(&mut self) -> io::Result<LastExit> { self.0.wait() }
            fn now_ms(&self) -> u64 { self.0.now_ms() }
            fn sleep_or_stop(&mut self, ms: u64) -> bool { self.0.sleep_or_stop(ms) }
            fn stop_requested(&mut self) -> bool { self.0.stop_requested() }
            fn publish(&mut self, rec: &SupervisorRecord) { self.0.publish(rec) }
            fn limits(&self) -> (MemoryLimits, Option<LimitBackend>) {
                (MemoryLimits { max_bytes: Some(1 << 28), high_bytes: None }, Some(LimitBackend::Watchdog))
            }
            fn last_exit_cause(&mut self) -> Option<ExitCause> { Some(ExitCause::Limit) }
        }
        let mut f = Limited(Fake::new(vec![killed(), code(0)]));
        let rec = supervise(&on_crash(), "w", 7, &mut f);
        assert_eq!(f.0.spawns, 2);
        assert_eq!(rec.restart_count, 1);
        assert_eq!((rec.limit_max_bytes, rec.limit_backend), (Some(1 << 28), Some(LimitBackend::Watchdog)));
        let b = f.0.published.iter().find(|r| r.state == SupState::Backoff).unwrap();
        assert_eq!(b.last_exit_cause, Some(ExitCause::Limit));
    }

    #[test]
    fn record_json_roundtrip_and_files() {
        let rec = SupervisorRecord {
            name: "w".into(),
            supervisor_pid: 4,
            daemon_pid: Some(5),
            state: SupState::Backoff,
            mode: RestartMode::OnCrash,
            restart_count: 3,
            last_exit: Some(LastExit { code: None, signal: Some(9), at_ms: 77 }),
            next_restart_at_ms: Some(99),
            updated_at_ms: 80,
            limit_max_bytes: Some(1 << 30),
            limit_high_bytes: Some(1 << 29),
            limit_backend: Some(LimitBackend::SystemdScope),
            last_exit_cause: Some(ExitCause::Limit),
        };
        assert_eq!(SupervisorRecord::from_json(&rec.to_json()), Ok(rec.clone()));
        let dir = tempfile::tempdir().unwrap();
        write_supervisor_record(dir.path(), &rec).unwrap();
        assert_eq!(read_supervisor_record(dir.path(), "w"), Some(rec));
        clear_supervisor_record(dir.path(), "w");
        assert_eq!(read_supervisor_record(dir.path(), "w"), None);
        assert!(!stop_requested(dir.path(), "w"));
        request_stop(dir.path(), "w").unwrap();
        assert!(stop_requested(dir.path(), "w"));
        clear_stop_request(dir.path(), "w");
        assert!(!stop_requested(dir.path(), "w"));
        assert!(supervisor_record_path(dir.path(), "../x").is_err());
    }
}
