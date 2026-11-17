# P4 / C0: Per-host memory limits and restart policy - feasibility spike

Status: C0 DONE (spike results + design). Parent plans: `session-host-architecture.md` section 5, `session-host-p2-attach.md` section 5.3 (cross-link only; not edited here).
Date: 2026-11-17. Machine: Ubuntu, kernel 7.0.0-42, systemd 259, cgroup2fs, x86_64, 188 GB RAM, 8 GB swap. macOS: bigmac.local, Darwin arm64, macOS 27.0.1.

Legend: VERIFIED = exercised on real hardware in this spike. UNVERIFIED = reasoned or documented only.

## 1. Results per mechanism

| Mechanism | Status | Evidence |
|---|---|---|
| Linux: cgroup v2 + user-manager delegation of `memory` | VERIFIED | `stat -fc %T /sys/fs/cgroup` = `cgroup2fs`; `user@1000.service/cgroup.controllers` = `cpu memory pids`; `systemctl show user@1000.service -p Delegate` = `Delegate=yes`. An unprivileged `mkdir` under `user@1000.service/` succeeded. |
| Linux: `systemd-run --user --scope -p MemoryMax=100M -p MemorySwapMax=0 python hog` (allocates+touches 10 MiB steps to 400 MiB) | VERIFIED | Hog killed (rc 137, "Killed") before completing. Hard limit works. |
| Linux: scope properties land in the cgroup | VERIFIED | Inside `-p MemoryMax=100M -p MemoryHigh=64M` scope: `memory.max`=104857600, `memory.high`=67108864, `memory.oom.group`=0 (default). Scope path `.../user@1000.service/app.slice/run-pNNN-iNNN.scope`. |
| Linux: `MemoryHigh` soft throttle | VERIFIED, with caveat | With swap available, a 200 MiB hog under `MemoryHigh=100M` completed (rc 0), i.e. pushed to swap and throttled, not killed. With `MemorySwapMax=0` the same shape stalled for >120 s (anonymous memory cannot be reclaimed, so the task is throttled indefinitely, never killed). Conclusion: `MemoryHigh` is not a limit; it is a slowdown. Never pair it with `MemorySwapMax=0` unless `MemoryMax` is also set. |
| Linux: `memory.oom.group=1` (kill the whole scope, not one pid) | UNVERIFIED | Property is documented (`ManagedOOMMemoryPressure`/`OOMPolicy`); not exercised. Needed so a host and all its PTY children die together. C2 must test it (`-p OOMPolicy=kill`). |
| Linux: systemd-oomd interplay | VERIFIED (partial) | `systemd-oomd` is active on the system manager; `user@1000.service` has `ManagedOOMMemoryPressure=kill`. A kill by oomd would not use `memory.max`; supervisor must treat any host death identically (it will see SIGKILL either way). Not provoked in this spike. |
| Linux: `RLIMIT_AS` (setrlimit/ulimit -v 200000) | VERIFIED works, REJECTED | Hog got `MemoryError` at the VA limit. It caps virtual address space, not RSS, so a Rust/mmap-heavy process (and `vt100` scrollback, GPU/graphics maps) trips it early or never. Use only as a last-resort safety net, not the design. |
| Linux: RSS watchdog via `/proc/<pid>/status` VmRSS (or `/proc/<pid>/smaps_rollup` Pss) | UNVERIFIED (trivially available) | Not exercised here; `/proc` read works for own-uid pids. Counts only the host pid, not PTY children, unless summed over the process group/cgroup. |
| macOS: `proc_pid_rusage(pid, RUSAGE_INFO_V4)` via `libc` | VERIFIED | Compiled with `cc` and ran: rc=0, `ri_phys_footprint` = 201 MiB and `ri_resident_size` = 201 MiB after touching 200 MiB. API is in libSystem (`libproc.h`); `libc = "0.2.170"` is already a dependency (check the `libc` crate exposes `proc_pid_rusage`/`rusage_info_v4` on apple targets in C1/C3; otherwise declare via `extern "C"`). Prefer `ri_phys_footprint` (what Activity Monitor/jetsam use) over resident size. |
| macOS: `setrlimit(RLIMIT_AS)` | VERIFIED NOT ENFORCED | `setrlimit` returned -1; a later 300 MiB malloc succeeded. Do not use. `RLIMIT_RSS` is likewise advisory/unenforced on Darwin (UNVERIFIED by test, documented). |
| macOS: cgroup equivalent | none (known) | Only mechanism is a userspace watchdog that polls `proc_pid_rusage` and sends SIGTERM then SIGKILL. launchd `HardResourceLimits`/`ResidentSetSize` not tested; UNVERIFIED and likely not enforced for user agents. |
| Windows: Job object `JOB_OBJECT_LIMIT_PROCESS_MEMORY` / `JOB_OBJECT_LIMIT_JOB_MEMORY` + `KILL_ON_JOB_CLOSE` | UNVERIFIED | No Windows machine tested. Documented behaviour: commit-charge (not working-set) limit; allocation fails rather than process being killed; a completion-port message `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` is delivered. Needs `windows-sys` (not currently a dependency; `libc` is POSIX only). ConPTY children can be assigned to the job at spawn. Lowest priority; C4. |

## 2. What C1 needs from the existing code

- `src/daemon/supervisor.rs`: `spawn_daemon_in` (line ~380) spawns the daemon detached (`process_group(0)`), then spawns a `daemon-reaper` thread that only calls `child.wait()` and discards the status. There is no restart loop, no exit-status capture, and no persistent supervisor: `ensure_started` is a one-shot, called by the CLI. A restart policy therefore needs a long-lived owner of the `Child`. Two options: (a) `bilterm supervisor` subcommand (long-lived, tiny) that holds children and `wait()`s; (b) lazy restart from `ensure_started` on next client call (no process to watch RSS, so cannot do watchdog limits). Recommend (a).
- `src/daemon/registry.rs`: `DaemonRegistryEntry` has pid, version, started_at, socket_path, session/pane/agent counts, `last_heartbeat_ms`. It has NO rss, no restart count, no limits, no exit reason. C1 adds optional serde fields (`#[serde(default)]`, so old JSON still parses): `rss_bytes: Option<u64>`, `restart_count: u32`, `last_exit: Option<{code, signal, at_ms}>`, `limit_max_bytes: Option<u64>`, `limit_high_bytes: Option<u64>`.
- Who writes RSS: the daemon heartbeat (self-report, always correct pid) is simplest on Linux/macOS (read own `/proc/self/statm` or `proc_pid_rusage(getpid())`); the supervisor additionally samples it for watchdog decisions so a wedged daemon cannot lie. Heartbeat already exists, so C1 extends it.
- `cli/daemon_cmd.rs` `daemon list [--json]` adds RSS and restarts columns. Per UNI-R01 the same data must surface via RPC (`daemon.list`/`workspace.status`), ctl, and the `:daemon list` colon command / `Action::Daemon`; do not add a CLI-only column.
- `libc` is already a dependency (Cargo.toml), enough for Linux/macOS C1/C3. Windows needs a new `windows-sys` target dependency (C4 only).
- Crash detection: `lock_liveness` + `check_pid_alive` (liveness.rs / supervisor.rs) already distinguish running vs stale; the supervisor must use `Child::wait` status instead (pid-reuse safe) and use the lock/registry only for adopt-on-start of daemons not spawned by it.

## 3. Recommended design

Layering (policy separate from mechanism, platform behind one trait, per designing-plugin-contracts):

```
trait MemoryLimiter { fn wrap_spawn(&self, cmd: &mut Command, lim: &Limits) -> Applied;  // pre-spawn (cgroup scope, job object)
                      fn sample(&self, pid: u32) -> Option<Rss>;                         // RSS/footprint
                      fn enforce(&self, pid, rss, lim) -> Verdict }                      // watchdog decision
```

1. Restart policy lives in the supervisor and is platform-independent: `restart = never | on-crash` (default `never`; "crash" = non-zero exit or signal, NOT a clean `daemon stop`), capped exponential backoff (1s, 2s, 4s ... 60s), crash-loop breaker (e.g. 5 crashes in 120 s -> stop restarting, mark `failed` in registry, surface in `daemon list`). Pure function over a history of `(exit, time)`, unit-testable with a fake clock (engineering-for-determinism).
2. Linux preferred: spawn through `systemd-run --user --scope --collect -p MemoryMax=<max> [-p MemoryHigh=<high>] -p OOMPolicy=kill -p MemorySwapMax=<set unless high-only> --unit bilterm-<name>-<pid>`. The scope contains the host and all PTY children, so one limit covers the fault domain. Do not use `MemoryHigh` alone; document it as throttle only. Detect support at runtime (probe `systemd-run --user --version` and that `user@UID.service/cgroup.controllers` contains `memory`); if absent (containers, no user manager, WSL, non-systemd), fall back to the watchdog and log that limits are cooperative.
3. Watchdog (all Unix, and the Linux fallback): supervisor samples every 2 s (configurable), compares to `memory_max`; at `memory_high` it logs/warns and sets a registry flag; at `memory_max` it SIGTERMs the process group, waits a grace period (5 s), then SIGKILLs. Linux samples `/proc/<pid>/smaps_rollup` Pss summed over the process group (or cgroup `memory.current` when in a scope); macOS samples `ri_phys_footprint` of the host pid (children are not summed unless walked with `proc_listchildpids`; UNVERIFIED, C3 decides). Watchdog enforcement is racy (a fast allocator can overshoot between polls); this is acceptable and must be documented as "best effort" versus the cgroup hard limit.
4. Windows: job object per host, `KILL_ON_JOB_CLOSE` plus `JOB_OBJECT_LIMIT_JOB_MEMORY`, completion port to notify the supervisor; UNVERIFIED and gated behind C4. Until then Windows runs unlimited and the watchdog is not offered.
5. A killed host is a normal event: supervisor records the exit (code/signal, whether the limit was the cause), applies restart policy, and (once P1 journal/restore exists) respawns restoring from the journal; otherwise respawns empty and says so.
6. Supervisor footprint: C1 must measure RSS of a `bilterm supervisor` that does not instantiate `WindowManager`/ratatui (static link size is irrelevant; resident pages are). Target well under 10 MB; `oom_score_adj` lowered (e.g. -500 needs privilege; unprivileged can only raise, so instead make hosts more killable: set host `oom_score_adj` = +200 via `/proc/<pid>/oom_score_adj` after spawn).
7. Config (lowers to Actions/RPC per ARCH rules): `[session] memory_high`, `memory_max` (byte strings like `2G`), `restart` (`never|on-crash`, default `never`), `restart_max_burst`. `daemon.start` RPC and `bilterm daemon start NAME --memory-max 2G --restart on-crash` accept the same; `:daemon` colon command maps through `Action`. All limits default off.

## 4. Task breakdown

- **C0 (this doc)**: DONE. Open items carried forward: `OOMPolicy=kill` behaviour, oomd provocation, Windows, `libc` crate symbol availability on apple targets.
- **C1 Restart policy + RSS in `daemon list`** (no limits yet). STATUS: IMPLEMENTED (`restart_policy.rs`, `supervise.rs`, `rss.rs`, `list_view.rs`; `bilterm supervisor` replaced the reaper thread; `tests/p4_c1_supervisor.rs`). Files: `src/daemon/supervisor.rs`, `registry.rs`, new `src/daemon/restart_policy.rs` (pure), `src/cli/daemon_cmd.rs`, RPC `daemon.list`, `:daemon list`. Steps: (1) pure `RestartPolicy` + crash-loop breaker with table-driven tests; (2) registry optional fields (back-compat test with old JSON); (3) RSS sampler (`/proc/self/statm`; macOS `proc_pid_rusage` via libc) in heartbeat; (4) long-lived `bilterm supervisor` loop that owns `Child` and applies policy, replacing the discard-status reaper thread; (5) measure supervisor RSS; (6) UNI-R01 surface parity. Exit: kill -9 a daemon, observe restart with backoff, breaker trips after N crashes, `daemon list` shows RSS/restarts.
- **C2 Linux limits** [C0, C1]: STATUS: IMPLEMENTED (`src/daemon/limits.rs`, `ProcessRunner::with_limits`, `tests/p4_c2_limits.rs`; `OOMPolicy=kill` scope kill verified live, systemd-oomd not provoked). `MemoryLimiter` trait + systemd-scope impl + runtime capability probe + `/proc` watchdog fallback; config keys; integration test that spawns a hog child under a 100M scope and asserts SIGKILL and supervisor classification "limit"; verify `OOMPolicy=kill` and `oom_score_adj`. Skipped (not failed) when the probe says unsupported.
- **C3 macOS watchdog** [C0, C1]: `proc_pid_rusage` sampler, SIGTERM/grace/SIGKILL enforcement, child walking decision; run on bigmac via ssh (throwaway dir under `~/worktrees`, cleaned up).
- **C4 Windows job object** [C0]: first spike on an actual Windows ARM64/x64 machine to flip UNVERIFIED to VERIFIED, then `windows-sys` job object implementation. Last priority.

## 5. Spike reproduction notes

Linux: `systemd-run --user --scope -q -p MemoryMax=100M -p MemorySwapMax=0 python3 -I hog.py 40` (hog allocates and touches 10 MiB chunks) gave rc 137. macOS: a 20-line C program calling `proc_pid_rusage(getpid(), RUSAGE_INFO_V4, ...)` and `setrlimit(RLIMIT_AS)`; the temp dir on bigmac was removed afterwards. Scratch files lived under `/datanapps/claude-tmp/c0` (not `/tmp`).
