//! Platform-independent restart policy for supervised session hosts (P4 / C1).
//!
//! Pure functions over an exit history and an injected clock (`now_ms` is a parameter), so the
//! whole policy is table-testable with no process, thread or sleep. The supervisor loop in
//! [`super::supervise`] is the only caller that touches real processes.
//!
//! * `restart = never` (default): record the exit, never respawn.
//! * `restart = on-crash`: respawn after a crash with capped exponential backoff
//!   (`base`, `2*base`, `4*base`, ... capped at `max_backoff_ms`).
//! * Crash-loop breaker: `max_burst` crashes inside `window_ms` stop the restarts and mark the
//!   host `failed`; the operator clears it with `daemon start`.
//! * A "crash" is a non-zero exit or death by signal. A clean exit (code 0) and any exit after
//!   an explicit `daemon stop` are never crashes and are never restarted.

use serde::{Deserialize, Serialize};

/// Whether the supervisor respawns a host that died.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestartMode {
    /// Record the exit and leave the host down (default).
    #[default]
    Never,
    /// Respawn after a crash (with backoff and the crash-loop breaker).
    OnCrash,
}

impl RestartMode {
    /// Canonical spelling used by the CLI, `:daemon`, RPC and the registry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnCrash => "on-crash",
        }
    }

    /// Parses `never` / `on-crash` (also `on_crash`, `oncrash`).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "never" | "off" | "no" => Ok(Self::Never),
            "on-crash" | "on_crash" | "oncrash" => Ok(Self::OnCrash),
            other => Err(format!("invalid restart policy '{other}' (expected 'never' or 'on-crash')")),
        }
    }
}

/// Tunables of the policy. Defaults follow `docs/plans/session-host-p4-limits.md` section 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestartConfig {
    pub mode: RestartMode,
    /// Delay before the first restart; doubles per further crash inside the window.
    pub base_backoff_ms: u64,
    /// Upper bound of the backoff delay.
    pub max_backoff_ms: u64,
    /// This many crashes inside `window_ms` trip the breaker (the Nth crash is not restarted).
    pub max_burst: u32,
    /// Sliding window of the breaker.
    pub window_ms: u64,
}

impl Default for RestartConfig {
    fn default() -> Self {
        Self { mode: RestartMode::Never, base_backoff_ms: 1_000, max_backoff_ms: 60_000, max_burst: 5, window_ms: 120_000 }
    }
}

impl RestartConfig {
    /// Default tunables with `mode` set.
    pub fn with_mode(mode: RestartMode) -> Self {
        Self { mode, ..Self::default() }
    }
}

/// How a host process ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastExit {
    /// Exit code, when it exited normally.
    pub code: Option<i32>,
    /// Terminating signal number (Unix), when it was killed.
    pub signal: Option<i32>,
    /// Epoch milliseconds when the supervisor observed the exit.
    pub at_ms: u64,
}

impl LastExit {
    /// Exit code 0 and no signal.
    pub fn is_clean(&self) -> bool {
        self.code == Some(0) && self.signal.is_none()
    }

    /// Short human form: `exit 1`, `signal 9`, or `unknown`.
    pub fn describe(&self) -> String {
        match (self.signal, self.code) {
            (Some(s), _) => format!("signal {s}"),
            (None, Some(c)) => format!("exit {c}"),
            _ => "unknown".to_string(),
        }
    }
}

/// Why a host is not being restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoRestart {
    /// `daemon stop` was requested.
    StopRequested,
    /// Exited with code 0.
    CleanExit,
    /// Policy is `never`.
    PolicyNever,
}

/// What the supervisor does after an exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Leave the host down.
    NoRestart(NoRestart),
    /// Respawn after `delay_ms`.
    Restart { delay_ms: u64 },
    /// Crash-loop breaker tripped: stop restarting, mark `failed`.
    Tripped,
}

/// Crash timestamps inside the sliding window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestartState {
    crashes_ms: Vec<u64>,
}

impl RestartState {
    /// Crashes currently inside the window (after the last [`decide`]).
    pub fn recent_crashes(&self) -> usize {
        self.crashes_ms.len()
    }
}

/// Decides what to do after `exit`. Mutates `state` only for crashes. `now_ms` is `exit.at_ms`.
pub fn decide(cfg: &RestartConfig, state: &mut RestartState, exit: &LastExit, stop_requested: bool) -> Decision {
    if stop_requested {
        return Decision::NoRestart(NoRestart::StopRequested);
    }
    if exit.is_clean() {
        return Decision::NoRestart(NoRestart::CleanExit);
    }
    if cfg.mode == RestartMode::Never {
        return Decision::NoRestart(NoRestart::PolicyNever);
    }
    let now = exit.at_ms;
    state.crashes_ms.retain(|&t| now.saturating_sub(t) < cfg.window_ms);
    state.crashes_ms.push(now);
    let n = state.crashes_ms.len() as u32;
    if n >= cfg.max_burst.max(1) {
        return Decision::Tripped;
    }
    let shift = (n - 1).min(32);
    let delay_ms = cfg.base_backoff_ms.saturating_mul(1u64 << shift).min(cfg.max_backoff_ms);
    Decision::Restart { delay_ms }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crash(at_ms: u64) -> LastExit {
        LastExit { code: None, signal: Some(9), at_ms }
    }

    fn cfg() -> RestartConfig {
        RestartConfig::with_mode(RestartMode::OnCrash)
    }

    #[test]
    fn mode_parse_roundtrip_and_default() {
        assert_eq!(RestartMode::default(), RestartMode::Never);
        for m in [RestartMode::Never, RestartMode::OnCrash] {
            assert_eq!(RestartMode::parse(m.as_str()), Ok(m));
        }
        assert_eq!(RestartMode::parse("ON_CRASH"), Ok(RestartMode::OnCrash));
        assert!(RestartMode::parse("always").unwrap_err().contains("invalid restart policy"));
        assert_eq!(RestartConfig::default().mode, RestartMode::Never);
    }

    #[test]
    fn table_of_non_restart_cases() {
        let clean = LastExit { code: Some(0), signal: None, at_ms: 10 };
        let code1 = LastExit { code: Some(1), signal: None, at_ms: 10 };
        let rows = [
            (RestartMode::OnCrash, clean, false, Decision::NoRestart(NoRestart::CleanExit)),
            (RestartMode::OnCrash, code1, true, Decision::NoRestart(NoRestart::StopRequested)),
            (RestartMode::OnCrash, crash(10), true, Decision::NoRestart(NoRestart::StopRequested)),
            (RestartMode::Never, code1, false, Decision::NoRestart(NoRestart::PolicyNever)),
            (RestartMode::Never, crash(10), false, Decision::NoRestart(NoRestart::PolicyNever)),
            (RestartMode::Never, clean, false, Decision::NoRestart(NoRestart::CleanExit)),
        ];
        for (mode, exit, stop, want) in rows {
            let mut st = RestartState::default();
            assert_eq!(decide(&RestartConfig::with_mode(mode), &mut st, &exit, stop), want, "{mode:?} {exit:?} stop={stop}");
            assert_eq!(st.recent_crashes(), 0, "non-restart cases never count as crashes");
        }
    }

    #[test]
    fn backoff_doubles_then_caps_then_breaker_trips() {
        let c = RestartConfig { max_burst: 10, window_ms: 1_000_000, ..cfg() };
        let mut st = RestartState::default();
        let delays: Vec<u64> = (0..8)
            .map(|i| match decide(&c, &mut st, &crash(1_000 * (i + 1)), false) {
                Decision::Restart { delay_ms } => delay_ms,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(delays, vec![1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000]);
    }

    #[test]
    fn default_breaker_trips_on_fifth_crash_in_window() {
        let c = cfg();
        let mut st = RestartState::default();
        for i in 0..4u64 {
            assert!(matches!(decide(&c, &mut st, &crash(i * 10_000), false), Decision::Restart { .. }));
        }
        assert_eq!(decide(&c, &mut st, &crash(40_000), false), Decision::Tripped);
    }

    #[test]
    fn crashes_outside_window_expire_and_backoff_resets() {
        let c = cfg();
        let mut st = RestartState::default();
        for i in 0..3u64 {
            decide(&c, &mut st, &crash(i * 1_000), false);
        }
        // Long stable run: all earlier crashes leave the 120 s window.
        assert_eq!(decide(&c, &mut st, &crash(10_000_000), false), Decision::Restart { delay_ms: 1_000 });
        assert_eq!(st.recent_crashes(), 1);
    }

    #[test]
    fn zero_burst_config_does_not_underflow() {
        let c = RestartConfig { max_burst: 0, ..cfg() };
        assert_eq!(decide(&c, &mut RestartState::default(), &crash(1), false), Decision::Tripped);
    }

    #[test]
    fn last_exit_describe() {
        assert_eq!(crash(0).describe(), "signal 9");
        assert_eq!(LastExit { code: Some(3), signal: None, at_ms: 0 }.describe(), "exit 3");
        assert_eq!(LastExit::default().describe(), "unknown");
    }
}
