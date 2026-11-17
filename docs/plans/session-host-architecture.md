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
- v2: live host upgrade by PTY master fd handoff (`SCM_RIGHTS`, or re-exec with CLOEXEC cleared, sozu pattern). Specified as milestone P6 in section 7.2. Not available for ConPTY.

## 7. Phased plan

1. **P1 Host with PTYs:** make the daemon able to host real panes (`headless_mode=false` path), behind a flag; no client change; test via `ctl`. Must satisfy the handover constraints in section 7.1.
2. **P2 Attach protocol:** snapshot + diff + input over the session socket; `bilterm attach <session>` TUI client; wire the existing `DaemonRequest` set. Resize/clipboard proxy.
3. **P3 GUI client** on the same protocol; `--gui`/TUI default to "spawn-or-attach host" (config switch, old in-process mode kept as fallback for one release).
4. **P4 Supervisor + limits:** registry, restart policy, per-session cgroup/job/watchdog limits, `daemon list` shows RSS.
5. **P5 (optional)** per-pane helper, remote attach.
6. **P6 Live handover / upgrade** (after P4; section 7.2): old host(s) hand sessions, PTYs and scrollback to new-version host(s) with no shell restarts.

Each phase ships behind a flag with the old path intact; gate on `scripts/verify.sh` plus new attach/reattach integration tests (kill client mid-output; kill host; lag client; resize race).

### 7.1 Constraints P1 must honour (for P6)

- **PTY ownership is explicit and transferable.** Each pane's PTY master (fd on Unix) and child pid live in one owner type (`PtySession` or successor) that can be `into_raw_parts()`-ed out and rebuilt from `(fd, pid, size, termios)` without respawning. `PtySession::Drop` must not kill the child when the session was handed over (explicit `release()`).
- **All host state is serializable.** Everything a host needs to resume (layout, pane metadata, VT grid + scrollback, cursor/modes, graphics placements, clipboard registry, output-ring `seq`, session names/ids, config overlay) is reachable through one `HostSnapshot` type (extends `GridSnapshot`, `src/vt/snapshot.rs`) with `serde`. No state that exists only in closures, thread-locals, raw pointers, `Instant`s (use durations/wall-clock), or channel handles. Threads (VT worker, PTY reader) must be restartable from snapshot.
- **Fds are CLOEXEC by default** and enumerable: the host keeps a registry of fds it owns (PTY masters, listen sockets, lock files) so handover knows exactly what to pass and nothing leaks into children.
- **Quiesce hook:** the host can pause PTY reads and VT feeding at a message boundary and drain the VT worker queue, then resume (needed for both snapshot and rollback).
- **Wire/snapshot versions from day one:** `HostSnapshot` and the client protocol carry `schema_version` and `min_compatible` fields even though only v1 exists.

### 7.2 Milestone P6: live handover (upgrade without losing sessions)

Goal: replace a running host (or all hosts) with a newer binary while shells, PTYs, panes, layout, scrollback and graphics survive; attached clients reconnect transparently.
Non-goals: surviving a host crash; cross-architecture or cross-OS handover; downgrade guarantees beyond N-1; handing over a half-initialised host; Windows ConPTY (see platform notes).

**Mechanism options**

| Option | How | Pros | Cons |
|---|---|---|---|
| A. Side-by-side, fd passing (recommended) | New host starts, connects to old host's private handover socket (`0700` dir, peer-cred checked); old host sends snapshot + PTY master fds via `SCM_RIGHTS` | Old host stays alive until commit, so rollback is trivial; new binary validated before it owns anything; no exec of unknown state | Briefly two processes; sockets must be re-bound (see below) |
| B. Exec-in-place | Old host serializes state to a memfd/pipe, clears CLOEXEC on PTY fds, `execve`s the new binary which adopts inherited fds | No fd passing; pid unchanged | No rollback once exec'd (a bad new binary loses everything); inherited-fd bookkeeping is fragile |

Pick A. B only as a fallback where a unix socket handover is impossible. Children are not reparented: the PTY child keeps running and simply gets a new controlling-session reader; its parent pid becomes init if the old host exits (acceptable; `waitpid` is replaced by pidfd/polling of the adopted pid, so P1 must not depend on being the direct parent for liveness).

**Serialized state.** `HostSnapshot { schema_version: u32, min_compatible: u32, writer_version: String, sessions: [...], panes: [{id, pid, fd_index, size, vt: GridSnapshot + scrollback, ring_seq, ...}], clipboard, config_overlay, ext: map }`. Encoding: versioned serde (CBOR or JSON), length-prefixed frames, fds carried out-of-band in the ancillary data with `fd_index` references.
- Forward compat (new reads old): the new reader MUST accept every `schema_version >= its own min_supported`; missing fields take documented defaults; a migration function per version step.
- Back compat (old reads new, for rollback and N-1): writers only add optional fields, never repurpose or remove; unknown fields are preserved into `ext` and ignored. If the writer's `min_compatible` exceeds the reader's `schema_version`, the reader refuses at HELLO (no partial adoption).
- A snapshot is checksummed and round-trip tested; any mismatch aborts before fds move.

**Handover protocol** (new host N, old host O)
1. N starts with `--handover-from <socket>`, sends `HELLO {schema range, version}`; O replies `HELLO` or `REFUSE` (version skew: stop here, O unaffected).
2. O **quiesces**: stops accepting new clients and Actions (queue them), pauses PTY reads at a message boundary, drains the VT worker.
3. O sends `SNAPSHOT` (checksummed) then `FDS` (PTY masters via `SCM_RIGHTS`, plus the client listen socket).
4. N validates, rebuilds panes, resumes PTY reads, and sends `ACK {ready}`. N does not yet serve clients on the public socket path.
5. O sends `COMMIT`; N takes over serving (listen socket already passed, so no gap), O closes its copies of the fds and exits after notifying attached clients `RECONNECT`.
6. **Rollback:** any error, timeout, or missing ACK before COMMIT => O resumes reads, replays queued Actions, discards N, stays authoritative. After COMMIT the state is N's; if N dies right after, that is an ordinary host crash. O keeps its fd copies open until N has acked a post-commit health check (short grace window) to narrow that gap.
Bytes the child writes during quiesce stay in the kernel PTY buffer (no loss; bounded by the kernel buffer, so quiesce must be short, with a deadline that triggers rollback).

**Orchestration (supervisor).** `bilterm daemon upgrade [<name>|--all]`. Supervisor spawns the new binary per host and drives steps 1-5 itself or lets hosts do it pairwise; `--all` proceeds sequentially (one host at a time, configurable concurrency) and halts on first failure, leaving remaining hosts on the old version. The supervisor itself is upgraded last, by exec-in-place (it holds only the registry, no PTYs). Registry records `version` per host; `daemon list` shows mixed versions during a rollout.

**Thin-client reconnect.** On `RECONNECT` (or socket EOF) clients retry the same session socket path with backoff, re-handshake (protocol version negotiation; a too-old client is told to upgrade), then receive a fresh full snapshot per section 4 resync rules. Size-owner and subscriptions are re-established by the client. Unsent input is held briefly client-side and replayed; nothing in flight is guaranteed beyond that.

**Failure modes:** version skew (REFUSE at HELLO), new binary crashes mid-adoption (rollback), snapshot too large for deadline (rollback, optionally raise deadline), fd limit hit in N, quiesce timeout, O or N killed mid-transfer (before COMMIT: O wins; after: crash semantics), client reconnect storm (jittered backoff), stale lock files (`liveness.rs` checks pid + start time).

**Tests**
- Handover integration test: spawn host O with N panes running a counter loop and scrollback; handover to N; assert same child pids, no lost or duplicated output (monotone counter), scrollback and cursor equal, attached client reconnects and gets an equal grid.
- Rollback tests: fault injection at each step (N exits after HELLO, after SNAPSHOT, before ACK, ACK timeout) => O keeps serving, panes untouched.
- Version-skew test: matrix of (O writer, N reader) across the supported range using stored golden snapshots from each released `schema_version`; refusal path when `min_compatible` too new; unknown-field preservation round trip.
- Serialization property test: arbitrary host state -> snapshot -> restore -> snapshot is identical.
- `--all` orchestration test with a deliberately failing second host: first upgraded, second rolled back, rest untouched.

**Platform notes.** Linux and macOS: `SCM_RIGHTS` over `AF_UNIX` works for PTY masters; macOS has no `/proc`, so adopt pids via `kill(0)`/`proc_pidinfo`. Windows: ConPTY handles are tied to the owning `conhost`/pseudoconsole and cannot be passed like fds (`DuplicateHandle` across processes can duplicate pipe handles and the HPCON, but the pseudoconsole lifetime is not reliably transferable); treat Windows as unsupported for P6 v1 and fall back to snapshot-only upgrade that restarts child processes (layout and scrollback kept, shells lost), clearly reported by `daemon upgrade`. Verify the DuplicateHandle claim before committing to either answer.

## 8. Open questions

- Graphics after reattach: store-and-encode vs best-effort drop?
- Is diff encoding worth it vs. periodic snapshots at low rate for slow clients?
- Default for new users: auto-spawn host (tmux-like) or opt-in?
- Wire format: reuse JSON-RPC framing or a binary framing (latency)?
- P6: CBOR vs JSON for `HostSnapshot`; how many schema versions to support (N-1 proposed); keep the old host alive past COMMIT for how long?
- Interaction with the existing tmux integration (`src/tmux/`), which already provides persistence for tmux-backed panes.

## 9. Prior art (summary)

tmux: one fat server (one crash/leak kills all sessions). zellij: server per session, server-side VT state, client gets rendered ANSI + control channel. wezterm: mux server with domains, GUI renders natively. abduco/dtach: per-session raw-byte relay, no scrollback, reattach relies on app repaint. Sources and caveats in the research notes (several claims there are unverified: tmux PTY survival, macOS memory enforcement, `--user` cgroup delegation).
