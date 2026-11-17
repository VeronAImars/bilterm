//! Live Background Daemon Discovery & Metadata Registry (`DAEM-06`, `DAEM-07`, `SEC-R01..04`).
//!
//! Maintains `<runtime_dir>/daemons/<name>.json` records with automatic dead-peer pruning
//! and cross-daemon state reporting.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::json::JsonValue;
use crate::daemon::liveness::DaemonLiveness;
use crate::daemon::restart_policy::LastExit;
use crate::daemon::supervisor::{check_pid_alive, now_ms};
use crate::secure_fs::{ensure_private_dir, validate_safe_name, write_private_file};

/// Metadata record for a live background daemon instance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRegistryEntry {
    /// Unique instance ID, e.g. `"daem_01jcb89z..."`.
    pub id: String,
    /// Validated safe name, e.g. `"default"`, `"worker-1"`.
    pub name: String,
    /// OS process ID.
    pub pid: u32,
    /// `CARGO_PKG_VERSION` of the running daemon binary.
    pub version: String,
    /// Epoch timestamp when the daemon started (milliseconds).
    pub started_at_ms: u64,
    /// Path to the Unix domain socket.
    pub socket_path: PathBuf,
    /// Number of active sessions hosted by this daemon.
    pub sessions_count: usize,
    /// Total panes running across all sessions.
    pub panes_count: usize,
    /// Number of registered agents.
    pub agents_count: usize,
    /// Epoch timestamp of the last heartbeat (milliseconds).
    pub last_heartbeat_ms: u64,
    /// Resident bytes of the daemon, self-reported on each heartbeat (`None` = never sampled or
    /// unsupported platform). Optional so records written before P4/C1 still parse.
    #[serde(default)]
    pub rss_bytes: Option<u64>,
    /// How many times the supervisor respawned this host after a crash.
    #[serde(default)]
    pub restart_count: u32,
    /// How the previous incarnation of this host ended (from the supervisor), if it ever did.
    #[serde(default)]
    pub last_exit: Option<LastExit>,
    /// Hard memory limit in bytes (`None` = unlimited). Reported only; enforcement is P4/C2-C4.
    #[serde(default)]
    pub limit_max_bytes: Option<u64>,
    /// Soft memory limit in bytes (`None` = none). Reported only; enforcement is P4/C2-C4.
    #[serde(default)]
    pub limit_high_bytes: Option<u64>,
}

impl DaemonRegistryEntry {
    /// Returns true if this entry corresponds to the current process.
    pub fn is_current(&self) -> bool {
        self.pid == std::process::id()
    }

    /// Serializes entry to compact JSON.
    pub fn to_json(&self) -> String {
        let mut map = BTreeMap::new();
        map.insert("id".to_string(), JsonValue::String(self.id.clone()));
        map.insert("name".to_string(), JsonValue::String(self.name.clone()));
        map.insert("pid".to_string(), JsonValue::Number(self.pid as i64));
        map.insert(
            "version".to_string(),
            JsonValue::String(self.version.clone()),
        );
        map.insert(
            "started_at_ms".to_string(),
            JsonValue::Number(self.started_at_ms as i64),
        );
        map.insert(
            "socket_path".to_string(),
            JsonValue::String(self.socket_path.to_string_lossy().into_owned()),
        );
        map.insert(
            "sessions_count".to_string(),
            JsonValue::Number(self.sessions_count as i64),
        );
        map.insert(
            "panes_count".to_string(),
            JsonValue::Number(self.panes_count as i64),
        );
        map.insert(
            "agents_count".to_string(),
            JsonValue::Number(self.agents_count as i64),
        );
        map.insert(
            "last_heartbeat_ms".to_string(),
            JsonValue::Number(self.last_heartbeat_ms as i64),
        );
        let opt = |v: Option<u64>| v.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64));
        map.insert("rss_bytes".to_string(), opt(self.rss_bytes));
        map.insert("restart_count".to_string(), JsonValue::Number(i64::from(self.restart_count)));
        map.insert(
            "last_exit".to_string(),
            self.last_exit.as_ref().map_or(JsonValue::Null, crate::daemon::supervise::last_exit_to_json),
        );
        map.insert("limit_max_bytes".to_string(), opt(self.limit_max_bytes));
        map.insert("limit_high_bytes".to_string(), opt(self.limit_high_bytes));
        JsonValue::Object(map).to_json_string()
    }

    /// Deserializes an entry from JSON text.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let val = JsonValue::parse(s).ok_or_else(|| "invalid json in registry entry".to_string())?;
        let id = val
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("missing id")?
            .to_string();
        let name = val
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("missing name")?
            .to_string();
        let pid = val
            .get("pid")
            .and_then(|v| v.as_i64())
            .ok_or("missing or invalid pid")? as u32;
        let version = val
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or("missing version")?
            .to_string();
        let started_at_ms = val
            .get("started_at_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u64;
        let socket_path = val
            .get("socket_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .ok_or("missing socket_path")?;
        let sessions_count = val
            .get("sessions_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;
        let panes_count = val
            .get("panes_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;
        let agents_count = val
            .get("agents_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;
        let last_heartbeat_ms = val
            .get("last_heartbeat_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u64;
        // P4/C1 fields are optional: records written by older builds simply lack them.
        let opt_u64 = |k: &str| val.get(k).and_then(|v| v.as_i64()).filter(|n| *n >= 0).map(|n| n as u64);

        Ok(Self {
            id,
            name,
            pid,
            version,
            started_at_ms,
            socket_path,
            sessions_count,
            panes_count,
            agents_count,
            last_heartbeat_ms,
            rss_bytes: opt_u64("rss_bytes"),
            restart_count: opt_u64("restart_count").unwrap_or(0).min(u64::from(u32::MAX)) as u32,
            last_exit: val.get("last_exit").and_then(crate::daemon::supervise::last_exit_from_json),
            limit_max_bytes: opt_u64("limit_max_bytes"),
            limit_high_bytes: opt_u64("limit_high_bytes"),
        })
    }
}

/// Registry manager for discovering, registering, and pruning daemon metadata files.
pub struct DaemonRegistry;

impl DaemonRegistry {
    /// Resolves `<base_dir>/daemons/<name>.json`.
    pub fn entry_path_in(base_dir: &Path, name: &str) -> io::Result<PathBuf> {
        validate_safe_name(name).map_err(|e| {
            io::Error::new(ErrorKind::InvalidInput, format!("invalid daemon name: {e}"))
        })?;
        let dir = base_dir.join("daemons");
        ensure_private_dir(&dir)?;
        Ok(dir.join(format!("{name}.json")))
    }

    /// Registers or updates metadata for a daemon in `<base_dir>/daemons/<name>.json` atomically (0600).
    pub fn register(base_dir: &Path, entry: &DaemonRegistryEntry) -> io::Result<()> {
        let path = Self::entry_path_in(base_dir, &entry.name)?;
        let json = entry.to_json();
        write_private_file(&path, json.as_bytes())
    }

    /// Updates the heartbeat timestamp and counts for a registered daemon.
    pub fn update_heartbeat(
        base_dir: &Path,
        name: &str,
        sessions: usize,
        panes: usize,
        agents: usize,
    ) -> io::Result<()> {
        let path = Self::entry_path_in(base_dir, name)?;
        let content = fs::read_to_string(&path)?;
        let mut entry = DaemonRegistryEntry::from_json(&content)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;

        entry.sessions_count = sessions;
        entry.panes_count = panes;
        entry.agents_count = agents;
        entry.last_heartbeat_ms = now_ms();

        write_private_file(&path, entry.to_json().as_bytes())
    }

    /// Heartbeat that also records the daemon's resident size (`daemon list` RSS column).
    /// A missing entry is not recreated (the daemon already unregistered).
    pub fn update_runtime_stats(base_dir: &Path, name: &str, rss_bytes: Option<u64>) -> io::Result<()> {
        let path = Self::entry_path_in(base_dir, name)?;
        let content = fs::read_to_string(&path)?;
        let mut entry = DaemonRegistryEntry::from_json(&content)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
        entry.rss_bytes = rss_bytes;
        entry.last_heartbeat_ms = now_ms();
        write_private_file(&path, entry.to_json().as_bytes())
    }

    /// Scans `<base_dir>/daemons/*.json`, verifies process liveness, prunes dead entries,
    /// and returns all active daemons sorted by name.
    pub fn list_active(base_dir: &Path) -> Vec<DaemonRegistryEntry> {
        let dir = base_dir.join("daemons");
        let Ok(read_dir) = fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut active = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(item) = DaemonRegistryEntry::from_json(&content) else {
                continue;
            };

            if Self::entry_is_live(base_dir, &item) {
                active.push(item);
            } else {
                // Prune dead peer (its lock record, if any, stays as crash evidence).
                let _ = fs::remove_file(&path);
                let _ = crate::daemon::process::probe_or_cleanup_stale_socket(&item.socket_path);
            }
        }

        Self::synthesize_from_locks_and_sockets(base_dir, &mut active);
        for e in &mut active {
            Self::overlay_runtime_facts(base_dir, e);
        }
        active.sort_by(|a, b| a.name.cmp(&b.name));
        active
    }

    /// Fills `restart_count` / `last_exit` from the supervisor's record and, for entries nobody
    /// has heartbeated (synthesized from a lock or socket), samples RSS from the live pid.
    fn overlay_runtime_facts(base_dir: &Path, e: &mut DaemonRegistryEntry) {
        if let Some(rec) = crate::daemon::supervise::read_supervisor_record(base_dir, &e.name) {
            e.restart_count = rec.restart_count;
            if rec.last_exit.is_some() {
                e.last_exit = rec.last_exit;
            }
        }
        if e.rss_bytes.is_none() && e.pid > 1 {
            e.rss_bytes = crate::daemon::rss::rss_bytes_of(e.pid);
        }
    }

    /// Hosts whose supervisor reports them down but not finished: `restarting` (crash backoff,
    /// supervisor alive) and `failed` (crash-loop breaker tripped). Sorted by name.
    pub fn list_supervised_down(base_dir: &Path) -> Vec<crate::daemon::supervise::SupervisorRecord> {
        use crate::daemon::supervise::{read_supervisor_record, SupState};
        let Ok(read_dir) = fs::read_dir(base_dir.join("daemons")) else {
            return Vec::new();
        };
        let mut out: Vec<_> = read_dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("sup"))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .filter_map(|name| read_supervisor_record(base_dir, &name))
            .filter(|r| match r.state {
                // The pid probe only gates display of a transient state; nothing is signaled.
                SupState::Backoff => check_pid_alive(r.supervisor_pid),
                SupState::Failed => true,
                SupState::Running | SupState::Stopped => false,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The lock decides when it has a record (held = live, unheld = crashed); a registry entry
    /// with no lock record falls back to the PID probe, which treats zombies as dead.
    fn entry_is_live(base_dir: &Path, item: &DaemonRegistryEntry) -> bool {
        match crate::daemon::liveness::daemon_liveness(base_dir, &item.name) {
            DaemonLiveness::Running(_) => true,
            DaemonLiveness::Crashed(_) => false,
            DaemonLiveness::Stopped => check_pid_alive(item.pid),
        }
    }

    /// Lock records left behind by daemons that died without cleaning up, sorted by name.
    pub fn list_crashed(base_dir: &Path) -> Vec<crate::daemon::supervisor::DaemonLockRecord> {
        let Ok(read_dir) = fs::read_dir(base_dir.join("daemons")) else {
            return Vec::new();
        };
        let mut out: Vec<_> = read_dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("lock"))
            .filter_map(|p| match crate::daemon::liveness::lock_liveness(&p) {
                DaemonLiveness::Crashed(rec) => Some(rec),
                _ => None,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Adds entries for live `daemon-*.lock` / `daemon-singleton.lock` records and live
    /// `daemon-*.sock` sockets that have no registry json yet (dedupes by name).
    fn synthesize_from_locks_and_sockets(base_dir: &Path, active: &mut Vec<DaemonRegistryEntry>) {
        let Ok(read_dir) = fs::read_dir(base_dir.join("daemons")) else {
            return;
        };
        let mut files: Vec<PathBuf> = read_dir.flatten().map(|e| e.path()).collect();
        files.sort();
        // Locks first (they carry a PID), then bare sockets.
        for path in &files {
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !(fname.starts_with("daemon-") && fname.ends_with(".lock")) {
                continue;
            }
            let Ok(content) = fs::read_to_string(path) else {
                continue;
            };
            let Ok(rec) = crate::daemon::supervisor::DaemonLockRecord::from_json(&content) else {
                continue;
            };
            if !matches!(crate::daemon::liveness::lock_liveness(path), DaemonLiveness::Running(_))
                || active.iter().any(|e| e.name == rec.name)
            {
                continue;
            }
            active.push(DaemonRegistryEntry {
                id: format!("daemon-{}-{}", rec.name, rec.pid),
                name: rec.name,
                pid: rec.pid,
                version: rec.version,
                started_at_ms: rec.started_at_ms,
                socket_path: rec.socket_path,
                sessions_count: 1,
                panes_count: 0,
                agents_count: 0,
                last_heartbeat_ms: now_ms(),
                ..Default::default()
            });
        }
        for path in &files {
            let Some(name) = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|f| f.strip_prefix("daemon-"))
                .and_then(|f| f.strip_suffix(".sock"))
            else {
                continue;
            };
            if active.iter().any(|e| e.name == name || e.socket_path == *path) {
                continue;
            }
            if !crate::daemon::process::probe_or_cleanup_stale_socket(path) {
                continue;
            }
            // PID unknown for a bare live socket; reported as 0 (never signaled).
            active.push(DaemonRegistryEntry {
                id: format!("daemon-{name}-sock"),
                name: name.to_string(),
                pid: 0,
                version: String::new(),
                started_at_ms: 0,
                socket_path: path.clone(),
                sessions_count: 1,
                panes_count: 0,
                agents_count: 0,
                last_heartbeat_ms: now_ms(),
                ..Default::default()
            });
        }
    }

    /// Unregisters daemon `name`, removing its `.json` file.
    pub fn unregister(base_dir: &Path, name: &str) -> io::Result<()> {
        let path = Self::entry_path_in(base_dir, name)?;
        if fs::symlink_metadata(&path).is_ok() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Prunes all dead daemons in `<base_dir>/daemons/`, returning the count of pruned entries.
    pub fn prune_dead(base_dir: &Path) -> usize {
        let dir = base_dir.join("daemons");
        let Ok(read_dir) = fs::read_dir(dir) else {
            return 0;
        };

        let mut count = 0;
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(item) = DaemonRegistryEntry::from_json(&content) else {
                continue;
            };

            if !Self::entry_is_live(base_dir, &item) {
                let _ = fs::remove_file(&path);
                let _ = crate::daemon::process::probe_or_cleanup_stale_socket(&item.socket_path);
                count += 1;
            }
        }
        // Acknowledge tripped crash-loop breakers (`failed` rows) as well.
        for rec in Self::list_supervised_down(base_dir) {
            if rec.state == crate::daemon::supervise::SupState::Failed {
                crate::daemon::supervise::clear_supervisor_record(base_dir, &rec.name);
                count += 1;
            }
        }
        count
    }
}
