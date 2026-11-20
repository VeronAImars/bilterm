//! One shared `daemon list` view model (P4 / C1, UNI-R01).
//!
//! The CLI (`bilterm daemon list`), RPC (`daemon.list`, hence `bilterm ctl` and MCP
//! `bilterm_daemon`), the `:daemon list` colon command (`Action::Daemon(List)`) and the daemon
//! window all build their rows here, so a new column appears on every surface at once.

use std::path::Path;

use crate::api::json::JsonValue;
use crate::daemon::liveness::DaemonLiveness;
use crate::daemon::registry::DaemonRegistry;
use crate::daemon::restart_policy::LastExit;
use crate::daemon::rss::format_bytes;
use crate::daemon::supervise::{last_exit_to_json, SupState};

/// One row of `daemon list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonRow {
    pub name: String,
    /// `running`, `crashed`, `restarting` (supervisor backoff) or `failed` (breaker tripped).
    pub status: &'static str,
    pub pid: Option<u32>,
    pub version: String,
    pub socket: String,
    pub sessions: usize,
    pub panes: usize,
    pub agents: usize,
    pub is_current: bool,
    pub rss_bytes: Option<u64>,
    pub restart_count: u32,
    pub last_exit: Option<LastExit>,
    pub limit_max_bytes: Option<u64>,
    pub limit_high_bytes: Option<u64>,
    /// How the limit is enforced: `systemd-scope` or `watchdog` (`None` = unlimited).
    pub limit_backend: Option<&'static str>,
    /// `limit` when the last exit was a memory-limit kill.
    pub last_exit_cause: Option<&'static str>,
    /// Epoch ms of the next respawn while `restarting`.
    pub next_restart_at_ms: Option<u64>,
    /// Whether the daemon owns real PTYs; `Some(false)` means panes cannot run commands.
    pub host_ptys: Option<bool>,
}

impl DaemonRow {
    /// JSON shared by every surface. Absent optional facts are `null`, never omitted.
    pub fn to_json(&self) -> JsonValue {
        let opt = |v: Option<u64>| v.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64));
        JsonValue::obj([
            ("name", JsonValue::String(self.name.clone())),
            ("status", JsonValue::String(self.status.to_string())),
            ("pid", self.pid.map_or(JsonValue::Null, |p| JsonValue::Number(i64::from(p)))),
            ("version", JsonValue::String(self.version.clone())),
            ("socket_path", JsonValue::String(self.socket.clone())),
            ("sessions_count", JsonValue::Number(self.sessions as i64)),
            ("panes_count", JsonValue::Number(self.panes as i64)),
            ("agents_count", JsonValue::Number(self.agents as i64)),
            ("is_current", JsonValue::Bool(self.is_current)),
            ("rss_bytes", opt(self.rss_bytes)),
            ("restart_count", JsonValue::Number(i64::from(self.restart_count))),
            ("last_exit", self.last_exit.as_ref().map_or(JsonValue::Null, last_exit_to_json)),
            ("limit_max_bytes", opt(self.limit_max_bytes)),
            ("limit_high_bytes", opt(self.limit_high_bytes)),
            ("limit_backend", self.limit_backend.map_or(JsonValue::Null, |s| JsonValue::String(s.to_string()))),
            ("last_exit_cause", self.last_exit_cause.map_or(JsonValue::Null, |s| JsonValue::String(s.to_string()))),
            ("next_restart_at_ms", opt(self.next_restart_at_ms)),
            ("host_ptys", self.host_ptys.map_or(JsonValue::Null, JsonValue::Bool)),
        ])
    }

    /// `pty` (runs commands), `model-only` (no shells; started without `--host-ptys`) or `-`.
    pub fn pty_text(&self) -> &'static str {
        match self.host_ptys {
            Some(true) => "pty",
            Some(false) => "model-only",
            None => "-",
        }
    }

    /// `12.3M` or `-`.
    pub fn rss_text(&self) -> String {
        format_bytes(self.rss_bytes)
    }

    /// Hard memory limit as `2.0G`, or `-` when unlimited.
    pub fn limit_text(&self) -> String {
        format_bytes(self.limit_max_bytes)
    }

    /// `exit 1`, `signal 9`, `signal 9 (limit)` or `-`.
    pub fn last_exit_text(&self) -> String {
        let base = self.last_exit.map_or_else(|| "-".to_string(), |e| e.describe());
        match self.last_exit_cause {
            Some(c) if self.last_exit.is_some() => format!("{base} ({c})"),
            _ => base,
        }
    }
}

/// `{"daemons":[...]}` for a row set.
pub fn rows_json(rows: &[DaemonRow]) -> JsonValue {
    JsonValue::obj([("daemons", JsonValue::Array(rows.iter().map(DaemonRow::to_json).collect()))])
}

/// Running daemons, plus hosts their supervisor reports `restarting` / `failed`, plus (when
/// `include_crashed`) lock records of daemons that died without cleanup. Sorted by name.
pub fn collect_rows(runtime_dir: &Path, include_crashed: bool) -> Vec<DaemonRow> {
    let mut out: Vec<DaemonRow> = DaemonRegistry::list_active(runtime_dir)
        .into_iter()
        .map(|d| DaemonRow {
            is_current: d.is_current(),
            name: d.name,
            status: "running",
            pid: Some(d.pid).filter(|p| *p != 0),
            version: d.version,
            socket: d.socket_path.display().to_string(),
            sessions: d.sessions_count,
            panes: d.panes_count,
            agents: d.agents_count,
            rss_bytes: d.rss_bytes,
            restart_count: d.restart_count,
            last_exit: d.last_exit,
            limit_max_bytes: d.limit_max_bytes,
            limit_high_bytes: d.limit_high_bytes,
            limit_backend: None,
            last_exit_cause: None,
            next_restart_at_ms: None,
            host_ptys: d.host_ptys,
        })
        .collect();
    // Enforcement facts live in the supervisor's record, not the heartbeat.
    for row in &mut out {
        if let Some(rec) = crate::daemon::supervise::read_supervisor_record(runtime_dir, &row.name) {
            row.limit_backend = rec.limit_backend.map(|b| b.as_str());
            row.last_exit_cause = rec.last_exit_cause.map(|c| c.as_str());
        }
    }
    for rec in DaemonRegistry::list_supervised_down(runtime_dir) {
        if out.iter().any(|r| r.name == rec.name) {
            continue;
        }
        let socket = crate::daemon::process::session_daemon_socket_path_in(runtime_dir, &rec.name)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        out.push(DaemonRow {
            name: rec.name,
            status: if rec.state == SupState::Failed { "failed" } else { "restarting" },
            socket,
            restart_count: rec.restart_count,
            last_exit: rec.last_exit,
            limit_max_bytes: rec.limit_max_bytes,
            limit_high_bytes: rec.limit_high_bytes,
            limit_backend: rec.limit_backend.map(|b| b.as_str()),
            last_exit_cause: rec.last_exit_cause.map(|c| c.as_str()),
            next_restart_at_ms: rec.next_restart_at_ms,
            ..Default::default()
        });
    }
    if include_crashed {
        for rec in DaemonRegistry::list_crashed(runtime_dir) {
            if !out.iter().any(|r| r.name == rec.name) {
                out.push(DaemonRow {
                    name: rec.name,
                    status: "crashed",
                    pid: Some(rec.pid),
                    version: rec.version,
                    socket: rec.socket_path.display().to_string(),
                    ..Default::default()
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The row for `name` (any status), falling back to the lock when the registry lags.
pub fn row_for(runtime_dir: &Path, name: &str) -> DaemonRow {
    if let Some(r) = collect_rows(runtime_dir, true).into_iter().find(|r| r.name == name) {
        return r;
    }
    let (status, rec) = match crate::daemon::liveness::daemon_liveness(runtime_dir, name) {
        DaemonLiveness::Running(r) => ("running", Some(r)),
        DaemonLiveness::Crashed(r) => ("crashed", Some(r)),
        DaemonLiveness::Stopped => ("stopped", None),
    };
    match rec {
        Some(r) => DaemonRow {
            name: name.to_string(),
            status,
            pid: Some(r.pid),
            version: r.version,
            socket: r.socket_path.display().to_string(),
            rss_bytes: if status == "running" { crate::daemon::rss::rss_bytes_of(r.pid) } else { None },
            ..Default::default()
        },
        None => DaemonRow { name: name.to_string(), status, ..Default::default() },
    }
}

/// Text table used by the CLI.
pub fn table(rows: &[DaemonRow]) -> String {
    if rows.is_empty() {
        return "No bilterm daemons running (start one with `bilterm daemon start [NAME]`)".to_string();
    }
    let w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    let mut out = format!(
        "{:<w$}  {:<10}  {:>8}  {:<9}  {:>7}  {:>8}  {:>7}  {:<16}  {:<10}  SOCKET",
        "NAME", "STATUS", "PID", "VERSION", "RSS", "RESTARTS", "LIMIT", "LAST-EXIT", "PTY"
    );
    for r in rows {
        let pid = r.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
        out.push_str(&format!(
            "\n{:<w$}  {:<10}  {:>8}  {:<9}  {:>7}  {:>8}  {:>7}  {:<16}  {:<10}  {}",
            r.name,
            r.status,
            pid,
            r.version,
            r.rss_text(),
            r.restart_count,
            r.limit_text(),
            r.last_exit_text(),
            r.pty_text(),
            r.socket
        ));
    }
    out
}

/// One-line status-bar summary used by `:daemon list`.
pub fn summary(rows: &[DaemonRow]) -> String {
    if rows.is_empty() {
        return "No background daemons currently running".to_string();
    }
    let items: Vec<String> = rows
        .iter()
        .map(|d| {
            let pid = d.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
            let flag = if d.status == "running" { String::new() } else { format!(" {}", d.status.to_uppercase()) };
            let flag = if d.host_ptys == Some(false) { format!("{flag}, MODEL-ONLY no PTYs") } else { flag };
            format!("{}(pid {pid}, {} sess, {} panes, rss {}, {} restarts{flag})", d.name, d.sessions, d.panes, d.rss_text(), d.restart_count)
        })
        .collect();
    format!("Active daemons ({}): {}", rows.len(), items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> DaemonRow {
        DaemonRow {
            name: "w".into(),
            status: "running",
            pid: Some(42),
            version: "1.0".into(),
            socket: "/s".into(),
            rss_bytes: Some(5 * 1024 * 1024),
            restart_count: 2,
            last_exit: Some(LastExit { code: None, signal: Some(9), at_ms: 1 }),
            ..Default::default()
        }
    }

    #[test]
    fn json_always_carries_the_new_columns() {
        let j = row().to_json();
        assert_eq!(j.get("rss_bytes").and_then(JsonValue::as_i64), Some(5 * 1024 * 1024));
        assert_eq!(j.get("restart_count").and_then(JsonValue::as_i64), Some(2));
        assert_eq!(j.get("last_exit").and_then(|e| e.get("signal")).and_then(JsonValue::as_i64), Some(9));
        let blank = DaemonRow { name: "x".into(), status: "stopped", ..Default::default() }.to_json();
        for k in ["rss_bytes", "last_exit", "limit_max_bytes", "limit_high_bytes", "limit_backend", "last_exit_cause", "pid"] {
            assert!(matches!(blank.get(k), Some(JsonValue::Null)), "{k} must be present as null");
        }
        assert_eq!(blank.get("restart_count").and_then(JsonValue::as_i64), Some(0));
    }

    #[test]
    fn table_and_summary_show_rss_and_restarts() {
        let t = table(&[row()]);
        assert!(t.contains("RSS") && t.contains("RESTARTS") && t.contains("LAST-EXIT"), "{t}");
        assert!(t.contains("5.0M") && t.contains("signal 9"), "{t}");
        let s = summary(&[row()]);
        assert!(s.contains("rss 5.0M") && s.contains("2 restarts"), "{s}");
        assert!(table(&[]).starts_with("No bilterm daemons running"));
    }
}
