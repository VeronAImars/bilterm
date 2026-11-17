# Design: Supervisor / Session-Host / Thin-Client Architecture

Status: PROPOSAL (no code). Motivation: limit the blast radius of a runaway or crashing terminal, and let shells outlive the UI.

## 1. Where we are (facts, from code on `origin/main` @ 87a2f21)

- `bilterm --daemon` (`src/main.rs`, `src/api/rpc.rs`, `src/daemon/`) builds a **headless** `WindowManager`; its panes are model-only (`headless_mode=true`) and **spawn no PTYs**. It serves JSON-RPC (Unix socket / loopback TCP, optional HTTP/SSE/WS).
- `--gui` and TUI modes each create an **in-process** `WindowManager` with real PTYs (`WindowManager::spawn_default` -> `CompositePane::spawn`). They do not attach to a daemon. Per-pane VT grid, scrollback, history, and graphics all live in the UI process (`CompositePane.vt: VtParser`, fed on the UI thread).
- **Nothing live survives UI exit.** `detach_client` saves layout; `PtySession::Drop` kills the child. "Attach" means restore-from-disk and respawn.
- `DaemonRequest` (Attach/Detach/Resize/Handshake, `src/daemon/mod.rs`) is defined but no socket server consumes it. There is no native cell-grid/diff/input-forwarding protocol. Reusable pieces: `WindowManager::apply(Action)`, named-daemon registry + sockets + lock files (`src/daemon/supervisor.rs`, `liveness.rs`, `registry.rs`), `GridSnapshot` (`src/vt/snapshot.rs`), per-pane `seq` output ring (`src/api/output_ring.rs`), experimental `VtWorkerHandle` (`src/vt/worker.rs`).

Consequence: one pane that balloons (or one GUI crash) takes every pane in that UI process and all their shells with it.

## 2. Goals / non-goals

Goals: (G1) shells survive UI exit/crash; (G2) a memory hog or crash kills one *session*, not everything; (G3) per-session memory limits where the OS allows; (G4) multiple UIs can attach to one session.
Non-goals (v1): per-pane processes; live-migrating PTYs across a host crash; remote/TLS attach (existing `-l` TCP path unchanged).

## 3. Proposed architecture

```
bilterm-supervisor   tiny; owns socket dir + registry; spawns/restarts hosts; sets limits
 |- session-host "work"    process: WindowManager + real PTYs + VT/scrollback for its panes
 |- session-host "agents"  process: separate memory limit / fault domain
 '- session-host "build"
clients: TUI | GUI | ctl | MCP  --attach-->  one session-host socket
```

- **Fault domain = session, not terminal.** A process per terminal multiplies IPC and breaks cross-pane features (broadcast, clipboard registry, layout). Sessions are the natural unit.
- **Session host** = today's headless daemon promoted to own real PTYs (`headless_mode=false`) plus a render/input protocol. It is the only place VT state lives; clients are thin.
- **Supervisor** = stateless-ish: list/spawn/stop/restart hosts, apply limits, report per-host RSS. Keeps `oom_score_adj` low and stays tiny; treat an unexpected host death as a normal event (respawn empty or restore from journal).
- **Multiple "master" daemons** stay possible: a named supervisor is one registry namespace (reuses `--daemon-name`).
- **Optional later:** sandboxed per-pane VT helper for untrusted/agent panes (also a security boundary).

## 4. Client protocol

Recommended v1: **server-owned VT state; full `GridSnapshot` on attach, then cell-row diffs tagged with `seq`; client sends `Action`s / raw input / resize.** Rationale: clean reattach (dtach/abduco forward raw bytes and rely on app repaint - poor for us), at the cost of host memory (bounded by the scrollback budget, PR #101 and the scrollback PR).
- Resync: any client lag/drop => send a fresh snapshot (never replay unboundedly; output ring is 256 KiB).
- Resize: explicit size-owner policy (latest non-read-only client wins, as abduco), others letterbox.
- Clipboard/OSC 52 and system-clipboard writes are *proxied to the attached client*; host never touches the client's clipboard.
- Graphics (sixel/kitty): host stores bounded placements (16 MiB budget) and encodes them in snapshots; forwarding verbatim fails after reattach. Needs a prototype before committing.
- Reuse `Action` as the input channel so the single-entry-point rule (`WindowManager::apply`) and ARCH-R02 (private fields) hold; add a snapshot type rather than public fields.

## 5. Memory limits per session (G3)

| OS | Mechanism | Notes |
|---|---|---|
| Linux | `systemd-run --user --scope -p MemoryHigh= -p MemoryMax=` or cgroup v2 `memory.high`/`memory.max` + `memory.oom.group=1` | `MemoryHigh` = soft throttle, `MemoryMax` = hard kill; `--user` needs memory controller delegation (verify). systemd-oomd may kill a scope before `memory.max` - supervisor must tolerate it. Avoid `RLIMIT_AS` (virtual address space, trips early). |
| macOS | RSS watchdog in supervisor (poll `proc_pid_rusage`, kill/restart); launchd `ResidentSetSize` unverified | No cgroup equivalent. |
| Windows | Job object with `KILL_ON_JOB_CLOSE` + memory limit + completion-port notification | ConPTY must be owned by the host for the child's lifetime; output is a re-rendered stream. |

Limits are config (`[session] memory_high`, `memory_max`), default off.

## 6. Persistence of shells (G1)

- v1: shells live as long as their **session host**; UI exit/crash no longer kills them. Host crash still kills that session's shells (acceptable; layout restored from journal).
- v2 (optional): PTY master fd handoff via `SCM_RIGHTS` (reptyr technique) or re-exec with CLOEXEC cleared (sozu pattern) for host upgrades. Not available for ConPTY.

## 7. Phased plan

1. **P1 Host with PTYs:** make the daemon able to host real panes (`headless_mode=false` path), behind a flag; no client change; test via `ctl`.
2. **P2 Attach protocol:** snapshot + diff + input over the session socket; `bilterm attach <session>` TUI client; wire the existing `DaemonRequest` set. Resize/clipboard proxy.
3. **P3 GUI client** on the same protocol; `--gui`/TUI default to "spawn-or-attach host" (config switch, old in-process mode kept as fallback for one release).
4. **P4 Supervisor + limits:** registry, restart policy, per-session cgroup/job/watchdog limits, `daemon list` shows RSS.
5. **P5 (optional)** fd handoff, per-pane helper, remote attach.

Each phase ships behind a flag with the old path intact; gate on `scripts/verify.sh` plus new attach/reattach integration tests (kill client mid-output; kill host; lag client; resize race).

## 8. Open questions

- Graphics after reattach: store-and-encode vs best-effort drop?
- Is diff encoding worth it vs. periodic snapshots at low rate for slow clients?
- Default for new users: auto-spawn host (tmux-like) or opt-in?
- Wire format: reuse JSON-RPC framing or a binary framing (latency)?
- Interaction with the existing tmux integration (`src/tmux/`), which already provides persistence for tmux-backed panes.

## 9. Prior art (summary)

tmux: one fat server (one crash/leak kills all sessions). zellij: server per session, server-side VT state, client gets rendered ANSI + control channel. wezterm: mux server with domains, GUI renders natively. abduco/dtach: per-session raw-byte relay, no scrollback, reattach relies on app repaint. Sources and caveats in the research notes (several claims there are unverified: tmux PTY survival, macOS memory enforcement, `--user` cgroup delegation).

## P1 decisions (implemented)

- **Flag:** `bilterm --headless --host-ptys [--name N]`; default off, errors without `--headless`. Off = byte-for-byte the old model-only daemon. Test via `bilterm ctl` (`pane.exec`/`pane.read`; `workspace.status` reports `host_ptys`).
- **Auto-spawn vs opt-in:** opt-in. Nothing starts a host implicitly.
- **Wire format (decided, used for the host state record now, for attach in P2):** versioned length-prefixed frame `"BTHS" | version u16 | len u32 | JSON`, 1 MiB cap, unknown fields tolerated, unknown versions rejected (`src/daemon/host.rs`). JSON-RPC is unchanged.
- **Graphics after reattach:** deferred to P2+; P1 has no reattach.
- **Handover-ready state:** `HostState` (pane ids, child pids, sizes, ownership) is plain serializable data written 0600 atomically to `host-<name>.state` in the runtime dir. It holds no fds; a later live-upgrade passes master fds out-of-band (`SCM_RIGHTS`) keyed by pane id. The record is not yet read back (no handover/restore in P1).
- **Remaining for P2+:** attach protocol, client resize/clipboard proxy, supervisor/limits.
