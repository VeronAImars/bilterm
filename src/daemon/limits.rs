//! Memory limits for supervised hosts (P4 / C2): policy and decisions, no process spawning.
//!
//! Two enforcement backends sit behind one decision ([`choose_backend`]):
//!
//! * [`LimitBackend::SystemdScope`] (Linux, preferred): the supervisor spawns the host through
//!   `systemd-run --user --scope -p MemoryMax=.. -p MemorySwapMax=0 -p OOMPolicy=kill`, so the
//!   kernel enforces a hard cap over the host and all its PTY children and kills the whole scope.
//!   `MemoryHigh` alone is never enough (it only throttles), so a high limit requires a max.
//! * [`LimitBackend::Watchdog`] (fallback, any Unix): the supervisor samples RSS and drives the
//!   [`Watchdog`] state machine: SIGTERM at the max, SIGKILL after a grace period. It is
//!   best effort (a fast allocator can overshoot between polls); the cgroup limit is the hard one.
//!
//! Everything here is pure or takes an injected clock/RSS so it is table-testable. A host killed
//! by a limit (or externally, e.g. by systemd-oomd) is an ordinary crash for the restart policy;
//! [`classify_exit`] only adds the *cause* so `daemon list` can say "limit".

/// Smallest accepted limit; anything below cannot even start a host.
pub const MIN_LIMIT_BYTES: u64 = 1 << 20;
/// Grace between SIGTERM and SIGKILL in the watchdog.
pub const DEFAULT_GRACE_MS: u64 = 5_000;
/// Default watchdog sampling interval.
pub const DEFAULT_POLL_MS: u64 = 2_000;

/// Requested memory limits for a host. `None` = unlimited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryLimits {
    /// Hard limit: the host is killed above it.
    pub max_bytes: Option<u64>,
    /// Soft limit: throttle (systemd) or warn (watchdog). Never a limit on its own.
    pub high_bytes: Option<u64>,
}

impl MemoryLimits {
    /// Builds limits from user strings (`512M`, `2G`, plain bytes) shared by every surface.
    pub fn from_args(max: Option<&str>, high: Option<&str>) -> Result<Self, String> {
        let p = |label: &str, v: Option<&str>| v.map(|s| parse_bytes(s).map_err(|e| format!("{label}: {e}"))).transpose();
        Self { max_bytes: p("memory-max", max)?, high_bytes: p("memory-high", high)? }.validate()
    }

    /// Rejects a high limit without a max (it would only throttle), and high >= max.
    pub fn validate(self) -> Result<Self, String> {
        match (self.max_bytes, self.high_bytes) {
            (None, Some(_)) => Err("memory-high alone only throttles and never kills; set --memory-max too".to_string()),
            (Some(m), Some(h)) if h >= m => Err("memory-high must be below memory-max".to_string()),
            _ => Ok(self),
        }
    }

    /// True when nothing is enforced.
    pub fn is_unlimited(&self) -> bool {
        self.max_bytes.is_none()
    }
}

/// Parses `1048576`, `512M`, `2G`, `1.5GiB`, `64k` (binary multiples, case-insensitive).
pub fn parse_bytes(s: &str) -> Result<u64, String> {
    let t = s.trim().to_ascii_lowercase();
    let t = t.strip_suffix("ib").or_else(|| t.strip_suffix('b')).unwrap_or(&t);
    let (num, mult) = match t.chars().last() {
        Some('k') => (&t[..t.len() - 1], 1u64 << 10),
        Some('m') => (&t[..t.len() - 1], 1 << 20),
        Some('g') => (&t[..t.len() - 1], 1 << 30),
        Some('t') => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    let value: f64 = num.trim().parse().map_err(|_| format!("invalid size '{s}' (expected e.g. 512M, 2G)"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("invalid size '{s}' (must be positive)"));
    }
    let bytes = value * mult as f64;
    if bytes >= u64::MAX as f64 {
        return Err(format!("size '{s}' is too large"));
    }
    let bytes = bytes as u64;
    if bytes < MIN_LIMIT_BYTES {
        return Err(format!("size '{s}' is below the {MIN_LIMIT_BYTES}-byte minimum"));
    }
    Ok(bytes)
}

/// How a limit is enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitBackend {
    /// Kernel cgroup limit through a transient systemd user scope (Linux).
    SystemdScope,
    /// Windows Job Object (`JOB_OBJECT_LIMIT_JOB_MEMORY` + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
    JobObject,
    /// Userspace RSS polling by the supervisor (cooperative, best effort).
    Watchdog,
}

impl LimitBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemdScope => "systemd-scope",
            Self::JobObject => "job-object",
            Self::Watchdog => "watchdog",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "systemd-scope" => Some(Self::SystemdScope),
            "job-object" | "job" => Some(Self::JobObject),
            "watchdog" => Some(Self::Watchdog),
            _ => None,
        }
    }
}

/// User preference for the backend (`BILTERM_LIMIT_BACKEND=auto|systemd|job-object|watchdog`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendPref {
    #[default]
    Auto,
    Systemd,
    JobObject,
    Watchdog,
}

impl BackendPref {
    pub fn from_env_value(v: Option<&str>) -> Self {
        match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("systemd") | Some("systemd-scope") => Self::Systemd,
            Some("job") | Some("job-object") => Self::JobObject,
            Some("watchdog") => Self::Watchdog,
            _ => Self::Auto,
        }
    }
}

/// Result of probing the machine for systemd user-scope memory limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// `systemd-run --user --scope` ran a command successfully (a user manager is reachable).
    pub scope_runs: bool,
    /// The user manager's cgroup has the `memory` controller delegated.
    pub memory_controller: bool,
}

impl Probe {
    pub fn supported(&self) -> bool {
        self.scope_runs && self.memory_controller
    }
}

/// Backend decision plus a human-readable reason (logged, and shown on fallback).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub backend: LimitBackend,
    pub note: String,
}

/// Picks the backend (Unix default: `job_object_available = false`). A systemd preference that the
/// probe cannot satisfy falls back to the watchdog (never fails the start) and says so in `note`.
pub fn choose_backend(pref: BackendPref, probe: &Probe) -> Selection {
    choose_backend_with_job(pref, probe, false)
}

/// Picks the backend with explicit Windows Job Object availability (`true` on Windows).
pub fn choose_backend_with_job(pref: BackendPref, probe: &Probe, job_object_available: bool) -> Selection {
    match pref {
        BackendPref::Watchdog => Selection { backend: LimitBackend::Watchdog, note: "watchdog requested".to_string() },
        BackendPref::Auto | BackendPref::JobObject if job_object_available => Selection {
            backend: LimitBackend::JobObject,
            note: "Windows Job Object (JOB_OBJECT_LIMIT_JOB_MEMORY + KILL_ON_JOB_CLOSE)".to_string(),
        },
        BackendPref::JobObject => {
            let mut s = choose_backend_with_job(BackendPref::Auto, probe, false);
            s.note = format!("Windows Job Object unavailable on this OS; {}", s.note);
            s
        }
        BackendPref::Auto | BackendPref::Systemd if probe.supported() => {
            Selection { backend: LimitBackend::SystemdScope, note: "systemd user scope available".to_string() }
        }
        _ => {
            let why = match (probe.scope_runs, probe.memory_controller) {
                (false, _) => "systemd user scopes unavailable",
                (true, false) => "memory controller not delegated to the user manager",
                (true, true) => unreachable!("supported probe handled above"),
            };
            Selection { backend: LimitBackend::Watchdog, note: format!("{why}; limits are cooperative (userspace RSS watchdog)") }
        }
    }
}

/// True when a `cgroup.controllers` file lists the memory controller.
pub fn controllers_include_memory(controllers: &str) -> bool {
    controllers.split_whitespace().any(|c| c == "memory")
}

/// Probes the machine. Linux only; elsewhere everything is unsupported (watchdog).
pub fn probe_systemd_scope() -> Probe {
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let path = format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/cgroup.controllers");
        let memory_controller = std::fs::read_to_string(path).is_ok_and(|s| controllers_include_memory(&s));
        if !memory_controller {
            return Probe { scope_runs: false, memory_controller: false };
        }
        let child = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "-p", "MemoryMax=infinity", "--", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return Probe { scope_runs: false, memory_controller } };
        let deadline = Instant::now() + Duration::from_secs(5);
        let scope_runs = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st.success(),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        Probe { scope_runs, memory_controller }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Probe::default()
    }
}

/// Probes once and applies `BILTERM_LIMIT_BACKEND`.
pub fn select_backend_for_host() -> Selection {
    let pref = BackendPref::from_env_value(std::env::var("BILTERM_LIMIT_BACKEND").ok().as_deref());
    // A watchdog preference or Windows Job Object needs no systemd probe.
    let probe = if pref == BackendPref::Watchdog || cfg!(windows) {
        Probe::default()
    } else {
        probe_systemd_scope()
    };
    choose_backend_with_job(pref, &probe, cfg!(windows))
}

/// `systemd-run` argv (program first) that runs `exe args` inside a limited transient scope.
/// `MemoryHigh` is only ever emitted next to `MemoryMax`.
pub fn scope_command(unit: &str, limits: &MemoryLimits, exe: &str, args: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ["systemd-run", "--user", "--scope", "--quiet", "--collect", "--unit"].iter().map(|s| s.to_string()).collect();
    v.push(unit.to_string());
    if let Some(max) = limits.max_bytes {
        v.extend(["-p".to_string(), format!("MemoryMax={max}"), "-p".to_string(), "MemorySwapMax=0".to_string(), "-p".to_string(), "OOMPolicy=kill".to_string()]);
        if let Some(high) = limits.high_bytes {
            v.extend(["-p".to_string(), format!("MemoryHigh={high}")]);
        }
    }
    v.push("--".to_string());
    v.push(exe.to_string());
    v.extend(args.iter().cloned());
    v
}

/// What the watchdog wants done now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WdAction {
    None,
    /// First crossing of the high limit (log only).
    WarnHigh,
    /// Over the max: ask the host to exit.
    Term,
    /// Still alive after the grace period: kill it.
    Kill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WdState {
    Idle,
    Terming { since_ms: u64 },
    Killed,
}

/// Pure SIGTERM -> grace -> SIGKILL state machine over RSS samples and an injected clock.
#[derive(Clone, Debug)]
pub struct Watchdog {
    max: u64,
    high: Option<u64>,
    grace_ms: u64,
    warned: bool,
    state: WdState,
}

impl Watchdog {
    pub fn new(max: u64, high: Option<u64>, grace_ms: u64) -> Self {
        Self { max, high, grace_ms, warned: false, state: WdState::Idle }
    }

    /// Feeds one sample. `signalable` is false while the target pid cannot be verified (lock
    /// record not matching); then no escalation happens and state does not advance, so the same
    /// action is retried at the next sample.
    pub fn observe(&mut self, now_ms: u64, rss: Option<u64>, signalable: bool) -> WdAction {
        match self.state {
            WdState::Killed => WdAction::None,
            WdState::Terming { since_ms } => {
                if signalable && now_ms.saturating_sub(since_ms) >= self.grace_ms {
                    self.state = WdState::Killed;
                    WdAction::Kill
                } else {
                    WdAction::None
                }
            }
            WdState::Idle => {
                let Some(rss) = rss else { return WdAction::None };
                if rss >= self.max {
                    if !signalable {
                        return WdAction::None;
                    }
                    self.state = WdState::Terming { since_ms: now_ms };
                    return WdAction::Term;
                }
                if !self.warned && self.high.is_some_and(|h| rss >= h) {
                    self.warned = true;
                    return WdAction::WarnHigh;
                }
                WdAction::None
            }
        }
    }
}

/// Why the host died, when the supervisor can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCause {
    /// Killed because it hit its memory limit.
    Limit,
}

impl ExitCause {
    pub fn as_str(self) -> &'static str {
        "limit"
    }

    pub fn parse(s: &str) -> Option<Self> {
        (s == "limit").then_some(Self::Limit)
    }
}

/// Classifies an exit. `watchdog_killed`: our watchdog signaled it. Otherwise a SIGKILL death
/// with a recent peak at/above 80% of the max is attributed to the limit (cgroup OOM kill).
/// Either way the exit is a crash for the restart policy; this only adds the cause.
pub fn classify_exit(signal: Option<i32>, limits: &MemoryLimits, peak_rss: u64, watchdog_killed: bool) -> Option<ExitCause> {
    classify_exit_with_code(signal, None, LimitBackend::SystemdScope, limits, peak_rss, watchdog_killed)
}

/// Classifies an exit across Unix (`SIGKILL`/watchdog) and Windows (`LimitBackend::JobObject`,
/// where hitting `JOB_OBJECT_LIMIT_JOB_MEMORY` fails allocations and causes a non-zero exit with
/// peak memory near `max_bytes`).
pub fn classify_exit_with_code(
    signal: Option<i32>,
    code: Option<i32>,
    backend: LimitBackend,
    limits: &MemoryLimits,
    peak_rss: u64,
    watchdog_killed: bool,
) -> Option<ExitCause> {
    let max = limits.max_bytes?;
    let near_limit = peak_rss >= max / 10 * 8;
    let job_abort = backend == LimitBackend::JobObject && code.is_some_and(|c| c != 0) && near_limit;
    if watchdog_killed || (signal == Some(9) && near_limit) || job_abort {
        Some(ExitCause::Limit)
    } else {
        None
    }
}

/// Windows Job Object guard (P4 / C4): enforces `JOB_OBJECT_LIMIT_JOB_MEMORY`,
/// `JOB_OBJECT_LIMIT_PROCESS_MEMORY`, and `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` over the supervised
/// host and its child processes.
#[cfg(windows)]
pub struct WinJobGuard {
    handle: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl WinJobGuard {
    /// Creates an anonymous Job Object configured with `max_bytes` and assigns `process_handle`.
    pub fn create_and_assign(process_handle: *mut std::ffi::c_void, max_bytes: u64) -> std::io::Result<Self> {
        use std::ffi::c_void;
        use std::ptr::{null, null_mut};
        const JOB_OBJECT_EXTENDED_LIMIT_INFO_CLASS: i32 = 9;
        const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
        const JOB_OBJECT_LIMIT_JOB_MEMORY: u32 = 0x0000_0200;
        const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateJobObjectW(lp_job_attributes: *mut c_void, lp_name: *const u16) -> *mut c_void;
            fn SetInformationJobObject(
                h_job: *mut c_void,
                job_object_information_class: i32,
                lp_job_object_information: *const c_void,
                cb_job_object_information_length: u32,
            ) -> i32;
            fn AssignProcessToJobObject(h_job: *mut c_void, h_process: *mut c_void) -> i32;
        }
        // SAFETY: `CreateJobObjectW(null, null)` creates a fresh anonymous job handle owned by
        // `WinJobGuard`. `SetInformationJobObject` reads a stack `JobObjectExtendedLimitInformation`
        // with exact size, and `AssignProcessToJobObject` attaches the caller-owned `Child` handle.
        unsafe {
            let job = CreateJobObjectW(null_mut(), null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let guard = Self { handle: job };
            let cap = max_bytes.min(usize::MAX as u64) as usize;
            let mut info = JobObjectExtendedLimitInformation::default();
            info.basic_limit_information.limit_flags =
                JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            info.process_memory_limit = cap;
            info.job_memory_limit = cap;
            let ok = SetInformationJobObject(
                guard.handle,
                JOB_OBJECT_EXTENDED_LIMIT_INFO_CLASS,
                (&raw const info).cast::<c_void>(),
                std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
            );
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if AssignProcessToJobObject(guard.handle, process_handle) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(guard)
        }
    }

    /// Queries peak committed memory (`PeakJobMemoryUsed` / `PeakProcessMemoryUsed`) recorded by the kernel.
    pub fn peak_memory_bytes(&self) -> Option<u64> {
        use std::ffi::c_void;
        const JOB_OBJECT_EXTENDED_LIMIT_INFO_CLASS: i32 = 9;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn QueryInformationJobObject(
                h_job: *mut c_void,
                job_object_information_class: i32,
                lp_job_object_information: *mut c_void,
                cb_job_object_information_length: u32,
                lp_return_length: *mut u32,
            ) -> i32;
        }
        let mut info = JobObjectExtendedLimitInformation::default();
        let mut ret: u32 = 0;
        // SAFETY: `self.handle` is a valid job object handle owned by `self`; `info` and `ret` are
        // stack-allocated and sized to `size_of::<JobObjectExtendedLimitInformation>()`.
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle,
                JOB_OBJECT_EXTENDED_LIMIT_INFO_CLASS,
                (&raw mut info).cast::<c_void>(),
                std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
                &mut ret,
            ) != 0
        };
        ok.then_some(info.peak_job_memory_used.max(info.peak_process_memory_used) as u64)
    }
}

#[cfg(windows)]
impl Drop for WinJobGuard {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CloseHandle(h_object: *mut std::ffi::c_void) -> i32;
        }
        if !self.handle.is_null() {
            // SAFETY: `self.handle` was returned by `CreateJobObjectW` and is closed once on drop.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

/// Sum of resident bytes over every process in process group `pgid` (host plus PTY children).
/// Linux walks `/proc`; macOS uses `proc_listpids`; elsewhere the single pid.
pub fn group_rss_bytes(pgid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut total = 0u64;
        let mut any = false;
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
            if parse_stat_pgrp(&stat) == Some(pgid)
                && let Some(rss) = crate::daemon::rss::rss_bytes_of(pid)
            {
                total = total.saturating_add(rss);
                any = true;
            }
        }
        any.then_some(total)
    }
    #[cfg(target_os = "macos")]
    {
        macos_group_rss_bytes(pgid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        crate::daemon::rss::rss_bytes_of(pgid)
    }
}

/// macOS: sums `ri_phys_footprint` over every pid in process group `pgid` (`proc_listpids`
/// with `PROC_PGRP_ONLY`), so PTY children count toward the limit. Hosts are spawned as their own
/// group leader (`process_group(0)`), so the group is the host's fault domain. Falls back to the
/// leader alone when the listing fails.
#[cfg(target_os = "macos")]
fn macos_group_rss_bytes(pgid: u32) -> Option<u64> {
    const PROC_PGRP_ONLY: u32 = 2; // sys/proc_info.h
    let pids = macos_group_pids(pgid, PROC_PGRP_ONLY);
    let mut total = 0u64;
    let mut any = false;
    for pid in pids {
        if let Some(b) = crate::daemon::rss::rss_bytes_of(pid) {
            total = total.saturating_add(b);
            any = true;
        }
    }
    if any { Some(total) } else { crate::daemon::rss::rss_bytes_of(pgid) }
}

#[cfg(target_os = "macos")]
fn macos_group_pids(pgid: u32, kind: u32) -> Vec<u32> {
    // First call sizes the buffer (bytes); grow slack for racing forks.
    let need = unsafe { libc::proc_listpids(kind, pgid, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Vec::new();
    }
    let cap = need as usize / std::mem::size_of::<libc::pid_t>() + 16;
    let mut buf = vec![0 as libc::pid_t; cap];
    let got = unsafe { libc::proc_listpids(kind, pgid, buf.as_mut_ptr() as *mut libc::c_void, (cap * std::mem::size_of::<libc::pid_t>()) as libc::c_int) };
    if got <= 0 {
        return Vec::new();
    }
    buf.truncate(got as usize / std::mem::size_of::<libc::pid_t>());
    buf.into_iter().filter(|p| *p > 0).map(|p| p as u32).collect()
}

/// Process group (5th field) of a `/proc/<pid>/stat` line; `comm` may contain spaces/parens.
pub fn parse_stat_pgrp(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1;
    // After comm: state ppid pgrp ...
    rest.split_whitespace().nth(2)?.parse().ok()
}

/// Raises the host's `oom_score_adj` so the kernel prefers it over the supervisor. Best effort.
pub fn bias_oom_score(pid: u32) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::fs::write(format!("/proc/{pid}/oom_score_adj"), b"200");
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn parse_bytes_table() {
        assert_eq!(parse_bytes("1048576"), Ok(MIB));
        assert_eq!(parse_bytes("64m"), Ok(64 * MIB));
        assert_eq!(parse_bytes("2G"), Ok(2 << 30));
        assert_eq!(parse_bytes("1.5GiB"), Ok(3 << 29));
        assert_eq!(parse_bytes(" 512MB "), Ok(512 * MIB));
        assert_eq!(parse_bytes("2048k"), Ok(2 * MIB));
        for bad in ["", "abc", "0", "-1G", "10", "1x", "99999999999999999999T", "nanG"] {
            assert!(parse_bytes(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn high_alone_is_rejected_and_must_be_below_max() {
        assert!(MemoryLimits::from_args(None, Some("1G")).unwrap_err().contains("only throttles"));
        assert!(MemoryLimits::from_args(Some("1G"), Some("1G")).unwrap_err().contains("below"));
        assert!(MemoryLimits::from_args(Some("1G"), Some("2G")).is_err());
        assert_eq!(MemoryLimits::from_args(None, None), Ok(MemoryLimits::default()));
        let l = MemoryLimits::from_args(Some("2G"), Some("1G")).unwrap();
        assert_eq!((l.max_bytes, l.high_bytes), (Some(2 << 30), Some(1 << 30)));
        assert!(MemoryLimits::from_args(Some("bogus"), None).unwrap_err().starts_with("memory-max:"));
    }

    #[test]
    fn backend_decision_table() {
        let full = Probe { scope_runs: true, memory_controller: true };
        let no_ctl = Probe { scope_runs: true, memory_controller: false };
        let none = Probe::default();
        use BackendPref::*;
        assert_eq!(choose_backend(Auto, &full).backend, LimitBackend::SystemdScope);
        assert_eq!(choose_backend(Systemd, &full).backend, LimitBackend::SystemdScope);
        assert_eq!(choose_backend(Watchdog, &full).backend, LimitBackend::Watchdog);
        let s = choose_backend(Auto, &no_ctl);
        assert_eq!(s.backend, LimitBackend::Watchdog);
        assert!(s.note.contains("memory controller") && s.note.contains("cooperative"), "{}", s.note);
        let s = choose_backend(Systemd, &none);
        assert_eq!(s.backend, LimitBackend::Watchdog, "forced systemd still falls back, never fails");
        assert!(s.note.contains("unavailable"));
        assert_eq!(BackendPref::from_env_value(Some("Systemd")), Systemd);
        assert_eq!(BackendPref::from_env_value(Some("job-object")), JobObject);
        assert_eq!(BackendPref::from_env_value(Some("job")), JobObject);
        assert_eq!(BackendPref::from_env_value(Some("watchdog")), Watchdog);
        assert_eq!(BackendPref::from_env_value(Some("x")), Auto);
        assert_eq!(BackendPref::from_env_value(None), Auto);
        assert!(controllers_include_memory("cpu io memory pids"));
        assert!(!controllers_include_memory("cpu io pids"));
        assert_eq!(LimitBackend::parse("watchdog"), Some(LimitBackend::Watchdog));
        assert_eq!(LimitBackend::parse("job-object"), Some(LimitBackend::JobObject));
        assert_eq!(LimitBackend::parse("job"), Some(LimitBackend::JobObject));
        assert_eq!(LimitBackend::JobObject.as_str(), "job-object");

        // Windows Job Object selection table.
        let win_auto = choose_backend_with_job(Auto, &none, true);
        assert_eq!(win_auto.backend, LimitBackend::JobObject);
        assert!(win_auto.note.contains("Windows Job Object"));
        let win_wd = choose_backend_with_job(Watchdog, &none, true);
        assert_eq!(win_wd.backend, LimitBackend::Watchdog);
        let unix_job_fallback = choose_backend_with_job(JobObject, &full, false);
        assert_eq!(unix_job_fallback.backend, LimitBackend::SystemdScope);
        assert!(unix_job_fallback.note.contains("Windows Job Object unavailable"));
    }

    #[test]
    fn scope_command_has_hard_limit_swap_off_and_oom_kill() {
        let l = MemoryLimits { max_bytes: Some(100 * MIB), high_bytes: Some(80 * MIB) };
        let argv = scope_command("u1", &l, "/bin/host", &["--daemon".to_string()]);
        let joined = argv.join(" ");
        assert!(joined.starts_with("systemd-run --user --scope"));
        for want in ["MemoryMax=104857600", "MemorySwapMax=0", "OOMPolicy=kill", "MemoryHigh=83886080", "--unit u1", "-- /bin/host --daemon"] {
            assert!(joined.contains(want), "{want} missing in {joined}");
        }
        // MemoryHigh is never emitted without MemoryMax.
        let high_only = scope_command("u", &MemoryLimits { max_bytes: None, high_bytes: Some(MIB) }, "h", &[]);
        assert!(!high_only.join(" ").contains("MemoryHigh"));
    }

    #[test]
    fn watchdog_term_grace_kill_with_fake_clock() {
        let mut w = Watchdog::new(100, Some(80), 5_000);
        assert_eq!(w.observe(0, Some(10), true), WdAction::None);
        assert_eq!(w.observe(1_000, Some(85), true), WdAction::WarnHigh);
        assert_eq!(w.observe(2_000, Some(90), true), WdAction::None, "warn once");
        assert_eq!(w.observe(3_000, Some(120), true), WdAction::Term);
        assert_eq!(w.observe(4_000, Some(200), true), WdAction::None, "within grace");
        assert_eq!(w.observe(7_999, Some(200), true), WdAction::None);
        assert_eq!(w.observe(8_000, Some(200), true), WdAction::Kill, "grace elapsed");
        assert_eq!(w.observe(9_000, Some(500), true), WdAction::None, "kill sent once");
    }

    #[test]
    fn watchdog_escalates_even_if_rss_drops_and_ignores_unknown_samples() {
        let mut w = Watchdog::new(100, None, 1_000);
        assert_eq!(w.observe(0, None, true), WdAction::None, "unknown rss never acts");
        assert_eq!(w.observe(0, Some(100), true), WdAction::Term, ">= max triggers");
        assert_eq!(w.observe(1_000, Some(1), true), WdAction::Kill, "asked to die: still escalates");
    }

    #[test]
    fn watchdog_never_signals_an_unverified_pid_and_retries() {
        let mut w = Watchdog::new(100, None, 1_000);
        assert_eq!(w.observe(0, Some(500), false), WdAction::None);
        assert_eq!(w.observe(100, Some(500), false), WdAction::None);
        assert_eq!(w.observe(200, Some(500), true), WdAction::Term, "acts once verified");
        assert_eq!(w.observe(5_000, Some(500), false), WdAction::None, "kill waits for verification");
        assert_eq!(w.observe(5_100, Some(500), true), WdAction::Kill);
    }

    #[test]
    fn exit_classification() {
        let l = MemoryLimits { max_bytes: Some(100 * MIB), high_bytes: None };
        assert_eq!(classify_exit(Some(9), &l, 95 * MIB, false), Some(ExitCause::Limit));
        assert_eq!(classify_exit(Some(9), &l, 10 * MIB, false), None, "SIGKILL from elsewhere with low RSS is a plain crash");
        assert_eq!(classify_exit(Some(15), &l, 10 * MIB, true), Some(ExitCause::Limit));
        assert_eq!(classify_exit(Some(9), &MemoryLimits::default(), u64::MAX, true), None, "no limit, no cause");
        assert_eq!(ExitCause::parse("limit"), Some(ExitCause::Limit));
        assert_eq!(
            classify_exit_with_code(None, Some(101), LimitBackend::JobObject, &l, 90 * MIB, false),
            Some(ExitCause::Limit),
            "Windows JobObject allocation failure with peak near max is a limit exit"
        );
        assert_eq!(
            classify_exit_with_code(None, Some(0), LimitBackend::JobObject, &l, 95 * MIB, false),
            None,
            "clean exit is never classified as limit without watchdog kill"
        );
    }

    #[test]
    fn stat_pgrp_parsing_handles_odd_comm() {
        assert_eq!(parse_stat_pgrp("123 (bil term) S 1 456 456 0 -1"), Some(456));
        assert_eq!(parse_stat_pgrp("123 (a) b) S 1 77 77 0"), Some(77));
        assert_eq!(parse_stat_pgrp("garbage"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn own_group_rss_is_plausible() {
        // SAFETY: getpgrp has no preconditions.
        let pgid = unsafe { libc::getpgrp() } as u32;
        assert!(group_rss_bytes(pgid).is_some_and(|b| b > 100 * 1024));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_group_sum_includes_children() {
        use std::os::unix::process::CommandExt;
        // Own group: sh (leader) plus a sleep child; the sum must cover both pids.
        let mut c = std::process::Command::new("sh").args(["-c", "sleep 5 & wait"]).process_group(0).spawn().unwrap();
        let pgid = c.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let pids = macos_group_pids(pgid, 2);
        let sum = group_rss_bytes(pgid);
        let _ = c.kill();
        let _ = c.wait();
        unsafe { libc::killpg(pgid as i32, libc::SIGKILL) };
        assert!(pids.len() >= 2, "expected sh + sleep, got {pids:?}");
        assert!(sum.is_some_and(|b| b > 0));
    }
}
