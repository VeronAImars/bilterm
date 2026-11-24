# Plan: Session-Host P2 (Attach Protocol) and P3+ Follow-ons

Status: PLAN ONLY (docs, no code). T-A0 spike DONE (section 12 has results); section 11 decisions OWNER-ACCEPTED. Parent: `docs/plans/session-host-architecture.md` (sections 4, 7, 7.1, 8, "P1 decisions").
Baseline: `main` @ 9960146 (P1 merged: `bilterm --headless --host-ptys`).

Conventions: claims marked **UNVERIFIED** were not confirmed (T-A0 has since resolved most of them; see the result table in section 12, which supersedes the inline tags) by reading code or running anything and must be checked by the PR that depends on them. Everything else was read in the files cited.

## 1. Grounding: what exists today

| Piece | Symbol / file | Relevance |
|---|---|---|
| Host mode | `WindowManager::new_host`, `is_host`, `host_state` (`src/wm/host_mode.rs`); `host_ptys` field | Panes own real PTYs; `host_state()` yields pane ids, pids, sizes. |
| Host pump | `AutomationControlPlane::host_tick` (`src/api/host_tick.rs`, `HOST_TICK` 25 ms) -> `wm.poll_all_ptys()` | Natural place to detect "pane changed" and emit attach updates. |
| Host record + framing | `HostState`, `encode_frame`/`decode_frame`, `"BTHS" \| u16 ver \| u32 len \| JSON`, `MAX_HOST_FRAME_BYTES` 1 MiB (`src/daemon/host.rs`) | P1 already decided this frame for "attach in P2". Reuse the header; P2 adds a message-type layer inside. |
| RPC server | `serve_loop` (`src/api/server.rs`): one task owns `AutomationControlPlane`; connections send `RpcRequest = (line, reply_tx)` over a bounded `mpsc(64)`; `host_tick` is a `select!` arm | The single-owner model is the integration point: attach sessions must also funnel through that loop, not touch the `WindowManager` from other tasks. |
| Socket security | `RpcAcceptor for UnixListener` checks `peer_cred().uid() == secure_fs::current_euid()`; socket `chmod 0600`; stale-socket probe; `MAX_RPC_LINE_BYTES` 1 MiB; `MAX_SOCKET_PATH_BYTES` 103; `SocketFileGuard` | Attach reuses this accept path verbatim. |
| Existing (unused) wire enum | `DaemonRequest`/`DaemonResponse` (`src/daemon/mod.rs`): `Attach`, `Detach`, `ResizeClient`, `Handshake`, `AttachedClient`, `negotiate_session_viewport` | Defined and unit-tested but **no socket consumes it** (grep: only `handle_request` in `daemon/mod.rs`). It models per-session clients, not per-pane streams, and is serialized as serde enums. P2 should reuse the semantics (client_id, viewport negotiation) but not its serde shape. See Open Question Q3. |
| Snapshots | `GridSnapshot { rows, cols, grid_rows: Vec<RowSnapshot{cells,dirty,wrapped,line_size}>, cursor, alternate_screen, scrollback_offset, total_scrollback_rows, generation }`, `CellSnapshot` (`src/vt/snapshot.rs`) | Not `serde` today (derives `Clone, Debug, PartialEq, Eq` only). `RowSnapshot.dirty` and `generation` already exist: raw material for diffs. |
| Output ring | `OutputHub`/per-pane ring with `seq`, `replay_after`, `history` (`src/api/output_ring.rs`), 256 KiB | Raw byte ring; useful for `pane.read`, **not** a substitute for a grid snapshot on reattach (doc section 4). |
| Per-client view precedent | `ClientViewRegistry`/`ClientViewState` (`src/api/client_view.rs`) used by WebSocket (`src/api/ws.rs`, `WsContext{rpc_tx, hub,...}`) | Prior art for "per-connection viewport/focus + hub subscription"; the WS path already does a snapshot-then-stream for web clients. Study before designing diffs (T-A1). |
| Graphics | `TerminalGraphicPlacement{id,protocol,row,col,cols,rows,pixel_width,pixel_height,rgba_bytes}`; caps `MAX_GRAPHIC_PLACEMENTS` 64, `MAX_GRAPHIC_TOTAL_BYTES` 16 MiB, `MAX_GRAPHIC_DIM_PX` 512 (`src/vt/graphics.rs`); `VtParser::graphic_placements()` | Placements are *decoded RGBA*, already bounded. Snapshot-encode is feasible without re-parsing; cost is up to 16 MiB per pane per snapshot. |
| Clipboard / OSC 52 | `pending_osc52` + `push_osc52` bounded buffers (`src/vt/bounded.rs`, `scan_osc_dcs.rs`); `CompositePane::take_last_osc52_sequence` (`src/pane/scroll.rs`); `set_system_clipboard`/`get_system_clipboard` (`src/clipboard.rs`, process-global); registry `MAX_CLIPBOARD_TOTAL_BYTES` 16 MiB; RPC `clipboard.*` (`src/api/rpc_clipboard.rs`) | In a host, `set_system_clipboard` would write the *host's* environment clipboard (wrong machine/session). Must be intercepted and proxied (P3). |
| Actions | `Action` (`src/action.rs`) incl. `PaneResize{dw,dh}`, `DetachClient`, `ForceQuit`; `WindowManager::apply` | Input and window ops already lower here. |
| Input via RPC | `pane.exec`, `pane.send_keys`, `pane.send`, `pane.feed_output` in `rpc_dispatch.rs` (they call `pane_mut(id)` directly, honour `is_read_only`) | Precedent for raw-byte input; P2 needs an Action-level equivalent (see section 6). |
| Lifecycle | `daemon/supervisor.rs` (lock files, `ensure_started`, `spawn_daemon_in`, `stop_daemon`), `registry.rs` (`list_active`, heartbeat), `liveness.rs` | P4 builds on these; they are per-daemon, not a restarting supervisor. |
| Files/limits | `ARCH-R01` (`handle_wm_key_event` <= 80 lines), `ARCH-R02` (private WM/CompositePane fields), `ARCH-R04` (~800-1000 lines/file soft, 1200 hard test backstop) | Constrain module splits below. |

Known gaps found while reading (relevant to scope):
1. `GridSnapshot` is not serializable and has no diff form. **Fact.**
2. `DaemonRequest` has no consumer. **Fact.**
3. `host_tick` pumps PTYs only; it has no "dirty panes since X" notion beyond `RowSnapshot.dirty`/`generation`. **Fact** (read `host_tick.rs`); whether `dirty` is reliably cleared per consumer is **UNVERIFIED** (T-A1 spike must check, since multiple clients need independent dirty tracking).
4. `negotiate_session_viewport` takes `&[&AttachedClient]` and returns a min/clamp size; its exact policy vs. the doc's "latest non-read-only client wins" differs. **UNVERIFIED** which one it implements; read `daemon/mod.rs:163` in T-A3.

## 2. Goals / non-goals

Goals
- G-P2.1 `bilterm attach <session>` (TUI) renders a host's panes and forwards input; killing the client never kills shells; reattach shows the current screen, cursor, alt-screen state and scrollback position, not a blank.
- G-P2.2 Multiple simultaneous clients on one host with defined resize/input semantics.
- G-P2.3 Every client-originated operation lowers to `crate::action::Action` via `WindowManager::apply` (ARCH rule; no side doors).
- G-P2.4 All queues, frames, and per-client state are bounded; a slow or hostile client cannot grow host memory or stall PTY pumping.
- G-P2.5 Versioned, forward-tolerant wire from day one (matches doc 7.1 "wire/snapshot versions from day one").
- G-P2.6 Deterministic headless tests (no real PTY, no real socket where avoidable).

Non-goals (P2)
- GUI client (P3), clipboard/graphics fidelity (P3 sub-steps below), supervisor/limits (P4), live handover (P6), remote/TLS attach, per-pane processes.
- Changing JSON-RPC (`ctl`, MCP, HTTP/WS) behaviour. Attach is an additional transport on the same listener (or a sibling socket; Q1).
- Auto-spawning a host (stays opt-in per P1 decision).
- Reconnect of a client after **host** crash (shells are gone; client reports and exits).

## 3. Wire protocol

### 3.1 Transport and framing
- Transport: the host's existing Unix socket (`ListenEndpoint::Unix`), accepted by the existing `RpcAcceptor` (uid check, 0600). TCP is **not** offered for attach in P2 (JSON-RPC TCP stays token-gated; attach carries raw input, so a no-TLS remote path is a non-goal). Windows: no AF_UNIX path assumed; attach on Windows is out of scope until a named-pipe decision (Q6).
- Connection discrimination (one listener, two protocols): the first bytes decide. JSON-RPC lines begin with `{`/`[` (existing `looks_like_http` already sniffs HTTP vs JSON); attach connections begin with the 4-byte magic `BTHS`. Sniff 4 bytes, route. **UNVERIFIED:** that `serve_jsonrpc_connection` can be handed an already-peeked stream without re-plumbing; if awkward, use a sibling socket `host-<name>.attach.sock` (same dir, same 0600, same uid check). Decision deferred to T-A2 with a recommendation for the sibling socket because it keeps the JSON-RPC path byte-identical (lower regression risk).
- Frame: reuse the P1 header and extend it with a message type:

```
magic "BTHS" (4) | proto_version u16 | len u32 | JSON payload (len bytes)
```
  P1 frames carry a `HostState`; attach frames carry `{"t":"<type>", ...}`. Distinguish by a new header byte rather than by payload sniffing: bump the header to `magic | proto_version u16 | msg_type u8 | flags u8 | len u32 | payload`. `flags` bit0 = payload is binary (bulk cell data), bit1 = compressed (reserved, must be 0 in v1). The P1 on-disk state record keeps its current 10-byte header (it is a file, not the stream); only the stream format gains `msg_type`/`flags`. This keeps `decode_frame` for the state file untouched.
- Limits: `MAX_ATTACH_FRAME_BYTES` = 4 MiB for server->client snapshot frames (a 500x200 grid with graphics refs fits; **UNVERIFIED** sizing, T-A1 measures real worst case), 64 KiB for client->server frames (input/resize are tiny). Length is validated before allocation (as `decode_frame` does). Over-limit => connection closed with an `Error{code:"frame_too_large"}` best-effort frame.
- Encoding: JSON for control messages (matches existing tooling, debuggable with `socat`). Bulk cell data uses a compact JSON-array-of-run-length-rows in v1; a binary body (flags bit0) is reserved for v2 if measurement shows JSON snapshot cost matters (answers parent Q "binary vs JSON" provisionally: JSON v1, measured).

### 3.2 Versioning and handshake
1. Client sends `Hello { proto_min, proto_max, client_kind: "tui"|"gui"|"ctl", client_name, caps: [..] }`.
2. Host replies `HelloOk { proto, host_version, host_name, host_pid, caps, sessions: [{name, panes}], client_id }` or `Refuse { reason, host_proto_min, host_proto_max }` then closes. `client_id` is host-assigned (never client-chosen; the existing `DaemonRequest::Attach{client_id}` lets the client pick, which is a spoofing/collision hazard in multi-client mode).
3. Version rule (mirrors doc 7.2): the host picks `min(client_max, host_max)`; refuses when that is `< max(client_min, host_min)`. Unknown fields are ignored. v1 rule for an unknown `msg_type`: => `Error{unknown_message}` and the message is skipped (length-prefixed so it can be skipped), connection continues. Only an unsupported *proto_version* closes.
4. `caps` strings gate optional features (`diff`, `graphics`, `clipboard_proxy`, `pixel_size`), so P3 additions do not need a proto bump.
5. Handshake must complete within 5 s and within 4 KiB of client bytes before any attach state is allocated (pre-auth resource bounding).

### 3.3 Auth / permissions
- Authentication is filesystem + uid, exactly as the existing Unix JSON-RPC path: socket dir created via `secure_fs::private_runtime_dir`/`create_private_dir_all` (0700), socket 0600, `peer_cred` uid must equal `current_euid()`. No bearer token on the Unix path (consistent with today; `--token-file` applies to TCP).
- Session names and any path derived from them go through `secure_fs::validate_safe_name` (as `host_state_path_in` does).
- Per-client role, assigned at `Attach` (client requests, host may downgrade): `rw` (can send input, can own size), `ro` (observe only: input and resize rejected with `Error{read_only}`). Read-only panes (`CompositePane::is_read_only`, the "PROT" lock used by `pane.exec`) are honoured for `rw` clients too.
- Same-uid is the trust boundary: any same-uid process could already `ctl pane.exec`. Attach does not widen the boundary; it must not be weaker than `pane.exec`. State this in `docs/API.md` (T-D1).
- `peer_cred` on macOS: **UNVERIFIED** that tokio's `peer_cred()` yields uid there (existing code assumes it; no new risk, but P2 tests should not claim macOS coverage).

### 3.4 Message types (v1)

Client -> host
| Type | Payload | Lowers to |
|---|---|---|
| `Hello` | see 3.2 | handshake, no Action |
| `Attach` | `{session, role, cols, rows, pane_filter?}` | registers client; resize negotiation (3.6); no Action unless size changes (`Action::` resize path, see 6) |
| `Detach` | `{}` | unregisters client. Pure client-state change; the Action analogue is `Action::DetachClient` only when the *user* typed `:detach` in the attached UI. |
| `Input` | `{pane, data_b64}` (<= 8 KiB per frame) or `{key: ...}` | key events -> existing keymap -> `Action`; raw bytes -> new `Action::PaneInputBytes{pane,bytes}` (section 6) |
| `Act` | `{action: <serialized Action>}` | `WindowManager::apply(Action)` (requires `Action: Deserialize` or a stable string form; **UNVERIFIED** whether `Action` is serde-friendly; many variants carry `String`/enums; T-A5 decides between serde and re-using colon-command text, e.g. `{"cmd":":split h"}`, which already round-trips through `colon_command::parse`) |
| `Resize` | `{cols, rows, pixel_w?, pixel_h?}` | size negotiation; resulting PTY resize via existing resize path |
| `Ack` | `{pane, seq}` | flow control (3.5) |
| `Ping` | `{n}` | liveness |
| `ClipboardReply` | `{req_id, data_b64 \| denied}` | P3 |

Host -> client
| Type | Payload |
|---|---|
| `HelloOk` / `Refuse` | see 3.2 |
| `Layout` | window/pane tree + z-order + focus + titles + per-pane size: sourced from `host_state()` plus layout accessors (new read-only snapshot type, ARCH-R02-compliant). Sent on attach and on every layout change. |
| `Snapshot` | `{pane, seq, gen, grid: ..., cursor, alt_screen, scrollback:{offset,total}, graphics?: [...], modes: {mouse, bracketed_paste, app_cursor, ...}}` full state. Always the first frame per pane after Attach and after any resync. |
| `Delta` | `{pane, base_gen, gen, seq, rows: [{y, cells_rle}], cursor?, modes?}` row-level diff against `base_gen`. |
| `Scrollback` | `{pane, from, rows}` response to a client scrollback request (client has no local scrollback in v1; see 3.7). |
| `Bell` / `Title` / `Exit` | pane events: bell, title change, child exit (`Exit{pane, status}`). |
| `ClipboardSet` | `{pane, data_b64, kind}` proxied OSC 52 write (P3). |
| `Resized` | `{cols, rows, owner_client}` effective size. |
| `Error` | `{code, message}` (codes enumerated in an enum, never free-form control). |
| `Pong`, `Bye {reason}` | liveness / orderly close (`host_shutdown`, `kicked`, `lagged_resync_failed`). |

### 3.5 Snapshot vs incremental output, backpressure, bounded queues

Principle (from doc section 4, restated with bounds): the host owns VT state; clients never receive an unbounded byte replay.

Per-client, per-pane `ClientPaneState { acked_gen, sent_gen, pending: Option<Delta-coalescer> }` (host side, bounded constant size):
- **No per-frame queue of deltas.** Pending changes for a client collapse into "rows dirty since `acked_gen`" (a bitset of <= rows bits). A new delta is *generated at send time* from the current grid vs. the client's last acked/sent generation. This makes memory O(rows) per client-pane regardless of PTY throughput (coalescing, the abduco/zellij-style "latest state wins"), and means a slow client sees fewer, larger deltas, never a growing backlog.
- Send cadence: at most one `Delta` per pane per `ATTACH_FRAME_INTERVAL` (default 16-33 ms; piggyback on `HOST_TICK` 25 ms, **UNVERIFIED** whether 25 ms is acceptable latency for interactive typing; echo path may need an immediate flush on input, T-A4 measures).
- Flow control: client `Ack{pane,seq}` after applying. Host allows `MAX_UNACKED_FRAMES` = 4 in flight per pane. When the window is full the host *stops sending* for that pane (state keeps coalescing). If a client is unacked past `LAG_DEADLINE` (default 5 s) it is moved to **resync**: drop its dirty tracking, mark "needs Snapshot", send one Snapshot when its window frees. If it stays unacked past `KICK_DEADLINE` (default 30 s) or its socket write buffer stays full (tokio write would block) for that long: `Bye{lagged}` and close.
- Outbound socket buffering: a bounded `mpsc` per connection (capacity `OUT_QUEUE_FRAMES` = 8 frames **and** `OUT_QUEUE_BYTES` = 16 MiB cap, whichever hits first) between the plane loop and the connection writer task. The plane loop uses `try_send`; on full it marks the client lagged (resync) instead of awaiting. The plane loop **never awaits a client** (a stuck client must not stall `host_tick`/PTY pumping; this is the key liveness invariant, tested in 8).
- Inbound bounds: client->host input rate-limited by a token bucket (default 1 MiB/s, burst 256 KiB; **UNVERIFIED** values, measure vs. paste-heavy use; bracketed paste of large text should be chunked by the client within the burst). Inbound request queue into the plane is the existing bounded `mpsc(64)` pattern; attach connections get their own bounded channel per connection (cap 32) and are drained fairly in `serve_loop`.
- Global caps (per `bounding-untrusted-resource-use`): `MAX_ATTACH_CLIENTS` (default 16 per host), `MAX_PANES_SUBSCRIBED_PER_CLIENT`, handshake timeout, idle timeout (`Ping` every 15 s, drop after 3 missed), max `Input` frame 8 KiB, max pending `Act` per second (e.g. 100), `ClipboardSet` <= 1 MiB per message (reuse `MAX_CLIPBOARD_TOTAL_BYTES` ceiling logic), string fields in `Hello` <= 256 bytes. All constants live in one `attach/limits.rs` with tests asserting each is enforced.
- Snapshot size bound: the grid is bounded by rows*cols (clamped to a max accepted `Resize`, e.g. 1000x500 -> reject larger); graphics bounded by existing 64 placements / 16 MiB; scrollback is **not** included in `Snapshot` (see 3.7), so the worst-case Snapshot is O(visible cells + graphics), not O(scrollback). Unbounded alloc from a client-supplied `rows`/`cols` is an explicit test.

### 3.6 Resize and multi-client semantics
- Each client has `ClientViewState`-like record: `{client_id, role, cols, rows, last_input_at, kind}` (extends `AttachedClient` semantics in `daemon/mod.rs`).
- **Size owner** = the most recent `rw` client that sent `Input` or `Resize` (abduco-style, matches doc section 4). The host WM runs at the owner's size; `ro` clients and non-owner `rw` clients render the owner-sized grid letterboxed/cropped client-side (they receive the same grid; no per-client re-layout in v1).
- Owner change is announced via `Resized{owner_client}`; a debounce (default 150 ms) prevents resize ping-pong between two clients both resizing.
- Last `rw` client detaches => size stays at the last owner's value (do not shrink the PTYs on detach; shells must not get a SIGWINCH storm just because a client left). UNVERIFIED whether `negotiate_session_viewport` (min-clamp across clients) is intended to apply here; if the owner wants the min policy instead (tmux `aggressive-resize` style) it is a policy switch (Q2).
- Input arbitration: `rw` clients may all send input (interleaving is the user's problem, like tmux shared sessions); `ro` is rejected. Focus (`active_pane`) is host-global in v1 (shared), not per-client; per-client focus is a v2 consideration (the WS `ClientViewState` has per-client focus, which is a precedent if wanted; Q4).
- Every connection gets its own `client_id`; a client reattaching after a drop is a new client (no resumable session token in v1; Q5).

### 3.7 Scrollback
Host keeps scrollback (bounded by existing scrollback budget). Clients render only the live grid in v1; scrolling is a host-side operation expressed as an Action (`scroll` actions already exist as `PaneScroll*`-style Actions through the keymap) and the next Delta carries `scrollback_offset`. A client-side scrollback cache is a later optimization (`Scrollback` message reserved).

## 4. Failure modes

| Failure | Behaviour | Test |
|---|---|---|
| Client crashes / socket EOF | host drops `ClientRecord`; shells untouched; if it was size owner, owner falls back to the next-most-recent `rw` client else unchanged | kill client mid-output |
| Slow client | coalesce, then resync, then kick (3.5); host loop never blocked | lag test |
| Malicious client: huge frame / slowloris handshake / flood of `Act` | length check before alloc, handshake deadline, rate limits, `Bye` | fuzz + limit tests |
| Version skew | `Refuse` with host's range; client prints actionable error (upgrade/restart host) | skew matrix |
| Host crash | clients see EOF, print "host died; shells lost; layout restorable from journal", exit nonzero; do not auto-respawn in P2 | host-kill test |
| Host shutdown (`daemon stop`) | `Bye{host_shutdown}` to all clients before exit | |
| Pane exits | `Exit` event; existing pane-hold/respawn behaviour (`pane_hold.rs`, `RespawnPaneInPlace`) unchanged | |
| Stale socket / second host | existing probe logic in `run_headless_server_with_http` reused | |
| Client sends input for nonexistent/closed pane | `Error{no_such_pane}`, ignored | |
| Terminal resize race (two clients) | debounce + single owner (3.6) | resize race test |
| Clipboard request with no attached rw client | dropped, counted, not queued indefinitely (P3) | |
| Snapshot too big for frame cap | degrade: omit graphics (flag `graphics_omitted`), still send grid | |

## 5. Clipboard, graphics, supervisor (P3+ detail)

### 5.1 Clipboard / OSC 52 proxy
- Host side: where a pane produces an OSC 52 write (`pending_osc52`, `take_last_osc52_sequence`) and where the host would call `set_system_clipboard`, the host must instead emit `ClipboardSet` to the **size-owner / focused rw client** (policy Q7) and must not touch the host-environment clipboard. Introduce a `ClipboardSink` seam (trait object on `WindowManager`, default = current system clipboard; host mode installs a "proxy to clients" sink). This is the same seam style as `designing-testable-seams`; test with a recording sink.
- OSC 52 *read* (query) is a request/response (`ClipboardReq` host->client, `ClipboardReply` client->host); default deny, opt-in per config, timeout 2 s, and it is a data-exfiltration risk: only the owner client, and only if the user enabled it. Exact current handling of OSC 52 reads in `scan_osc_dcs.rs` is **UNVERIFIED** (only the write path `is_forwardable_osc52_write` was read).
- The clipboard registry (`wm/clipboard_registry.rs`, 16 MiB cap) stays host-side as the history store; `clipboard.*` RPC unchanged.
- Client paste into a pane: client reads its local clipboard and sends `Input` wrapped per the pane's bracketed-paste mode (known from `modes` in Snapshot), chunked within the input rate limit.

### 5.2 Graphics after reattach
- Prototype first (parent doc says "needs a prototype before committing"): spike T-B1 measures encoding `TerminalGraphicPlacement` (RGBA, <= 512x512, <= 64, <= 16 MiB total) into Snapshot frames.
- Recommended v1: `Snapshot.graphics` carries placements as `{id, protocol, row, col, cols, rows, w, h}` + payload either inline (small) or as separate `GraphicsChunk` frames (<= 256 KiB each, same 4 MiB frame cap) with a `gfx_epoch` so deltas can reference ids already delivered. Client caches by `(id, epoch)`; unchanged placements are never re-sent (`GraphicsKeep{ids}`).
- Best-effort fallback (when the client lacks `graphics` cap, or exceeds `OUT_QUEUE_BYTES`): drop graphics, keep grid, set `graphics_omitted`. This answers parent Q1: store-and-encode, with best-effort drop under pressure and for ro/slow clients.
- Client rendering: TUI client re-emits Kitty/Sixel to its own terminal only if its terminal advertises support (detection exists in `src/vt/detect.rs`; **UNVERIFIED** it covers Kitty/Sixel capability queries). Otherwise show a placeholder box.
- Memory: reattach with 16 MiB graphics x N panes may spike; the per-client queue byte cap (3.5) is what bounds it; sends are paced, not all-at-once.

### 5.3 Supervisor + memory limits (P4; refine parent section 5)
- Reuse `daemon/supervisor.rs` (`ensure_started`, `spawn_daemon_in`, `acquire_lock`, `stop_daemon`) and `registry.rs` (`list_active`, heartbeat) rather than writing a second registry. Add: restart policy (`never|on-crash`, capped backoff, crash-loop breaker), `daemon list` RSS column, limit application at spawn.
- Linux: `systemd-run --user --scope -p MemoryHigh= -p MemoryMax=` is the planned mechanism. **UNVERIFIED:** that the user manager has the memory controller delegated on the owner's machine and that systemd-oomd interplay is acceptable; first task of P4 is a 1-hour spike script that reports yes/no on the target boxes. Fallback: watchdog polling `/proc/<pid>/status` VmRSS (Linux) and `proc_pid_rusage` (macOS, **UNVERIFIED** API availability from our current dependency set; may need `libc`).
- Windows: Job object limits: **UNVERIFIED**, spike separately, last priority.
- The supervisor must stay tiny and not link the full WM. **UNVERIFIED** whether the current `bilterm` binary can run as supervisor without pulling the TUI deps in at runtime; a `bilterm supervisor` subcommand is acceptable if startup cost and memory are small (measure RSS).
- Config: `[session] memory_high`, `memory_max`, `restart` (default off). Everything lowers to Actions/RPC: `daemon.start/stop` (existing) get `limits` params; `:daemon ...` colon commands map through `Action`.

## 6. Lowering every operation to `Action`

Rule: network handlers only translate; state changes call `WindowManager::apply(Action)` (`src/colon_command/execute_wm.rs`). The attach server module holds **no** `&mut WindowManager` outside the plane loop (it sends a message to the loop and gets a reply, like `RpcRequest`).

| Wire op | Action path |
|---|---|
| `Act{...}` | decoded to `Action`, `apply` |
| `Input` key events | client forwards raw key events (crossterm-normalized, serializable `KeySpec`); host runs them through the existing keymap/`handle_wm_key_event` path (stays <= 80 lines, ARCH-R01; the attach server calls it rather than extending it) |
| `Input` raw bytes (paste, mouse reports, non-key) | new `Action::PaneInputBytes { pane: PaneId, bytes: Vec<u8> }`; handler honours `is_read_only`, bracketed paste, and the same traffic-tap attribution as `pane.exec` (`TrafficEndpoint`, add `Attach(client_id)`). Add to `Action` per AGENTS.md "4 places" workflow (exhaustive matches). |
| `Resize` | new `Action::AttachResize { cols, rows }` (host-wide), wrapping the existing resize path (`resize_all` in `wm/mod.rs`) and size-owner rules; **not** `PaneResize` (that is window geometry within the workspace) |
| `Attach`/`Detach` | client-registry mutation inside the attach server; emits `Action::ClientAttached/Detached` only if the WM needs to know (status line, `:daemon` window). UNVERIFIED need; start without, add if UI shows client counts |
| `ClipboardSet` source | WM-side `ClipboardSink` (5.1), not an Action (it is an output) |
| Layout/pane ops from client UI (split, focus, zoom...) | existing Actions via `Act`/keymap, unchanged |
| `ctl`/RPC parity | `workspace.status` gains `attach: {clients, owner}`; new RPC `attach.clients` (read) and `attach.kick{client_id}` -> Action `AttachKick`. Every attach feature must have an RPC/ctl/colon/MCP-visible form per UNI-R01 (designing-uniform-surfaces). |

## 7. Module layout and ARCH constraints

New directory `src/attach/` (all files target <= ~800 lines, hard 1200):
- `mod.rs` re-exports, constants.
- `limits.rs` all caps (section 3.5) + tests that each is enforced.
- `frame.rs` header/codec (`encode/decode`, streaming decoder with partial reads); reuses `HOST_*` constants where shared.
- `msg.rs` message enums + serde (<= ~500 lines; split `msg_client.rs` / `msg_host.rs` if over).
- `handshake.rs` version negotiation (pure function, table-testable).
- `registry.rs` `ClientRegistry`: roles, size-owner election, debounce (pure logic, injectable clock).
- `session.rs` per-client per-pane `ClientPaneState`, coalescing, ack/lag/resync state machine (pure; no sockets).
- `diff.rs` grid vs. generation -> `Delta` (pure).
- `server.rs` tokio accept/IO glue only; thin; talks to the plane loop through bounded channels.
- `client/` (`mod.rs`, `render.rs`, `input.rs`) for `bilterm attach` (P2 TUI client), reusing the existing ratatui compositor on a `GridSnapshot`.
Host-side snapshot export lives in `src/wm/host_view.rs` (new, sibling of `host_mode.rs`) exposing `HostView`/`LayoutSnapshot`/`pane_snapshot(id)` so WM and `CompositePane` fields stay private (ARCH-R02). `GridSnapshot` gets serde via a **wire mirror type** (`WireGrid`) in `src/attach/msg.rs` with `From<&GridSnapshot>`, so `vt/snapshot.rs` (renderer-facing) does not grow wire concerns.
`src/api/server.rs` is 725 lines; the accept/route hook adds <= ~40 lines there, everything else in `src/attach/`. `rpc_dispatch.rs` is 847 lines: new RPCs go in a new `rpc_attach.rs`. `src/main.rs`: only argument wiring for `attach` subcommand (check current size before adding; **UNVERIFIED** line count).
Docs: update `docs/API.md`, `docs/DEVELOPER_AND_AGENT_GUIDE.md` module map, `docs/plans/session-host-architecture.md` "P2 decisions" section when each PR lands (keeping-docs-in-sync).

## 8. Test plan

Deterministic, socket-free wherever possible; follow `WindowManager::new_headless(cols, rows)` + `CompositePane::new_headless(id, title, cols, rows)` (note `new_headless` panes are model-only; to exercise real output use `pane.feed_output`/`feed_output` on the VT worker, as `pane.feed_output` RPC does, so no PTY is needed).

1. Unit (pure): `frame` round-trip, truncation at every byte boundary, oversize length before allocation, bad magic/version/msg_type, `flags` reserved bits rejected; `handshake` version matrix table; `limits` each cap; `registry` size-owner election with a fake clock (debounce, owner detach fallback); `session` state machine property test: for any interleaving of {feed, send, ack, lag, resync} memory (pending state) stays O(rows) and the client-reconstructed grid equals the host grid after the final ack.
2. Diff/snapshot property tests: random byte streams fed into a headless pane; `apply(Snapshot)` then each `Delta` on a client-side `GridMirror` always equals host `GridSnapshot` (extends existing `GridSnapshot: PartialEq`). Include alt-screen toggles, wide chars (`is_wide_continuation`), DEC line sizes (`line_size`), resize mid-stream.
3. Host integration with in-memory duplex (`tokio::io::duplex`) instead of a socket: spawn `serve_loop`-equivalent with a headless plane; drive attach, input, resize; assert Actions applied via observable WM state. Plus a small number of real-socket tests under the existing secure tempdir pattern (`secure_fs::unique_suffix`, `create_private_dir_all`) asserting 0600 socket mode and foreign-uid rejection is unit-testable only via the acceptor function (uid compare extracted into a pure fn, **UNVERIFIED** it currently is not; T-A2 extracts it).
4. Failure injection: client killed mid-output (drop duplex), slow client (never reads: assert host tick latency unaffected and queue bytes <= cap), frame flood, handshake slowloris (tokio paused time), two clients resizing simultaneously, host dropping with clients attached (`Bye`).
5. Resource-bound tests (per `bounding-untrusted-resource-use`): attach with `cols=u16::MAX,rows=u16::MAX` rejected before allocating; 1000 connection attempts capped at `MAX_ATTACH_CLIENTS`; 8 KiB input cap; graphics-heavy pane keeps per-client queue <= `OUT_QUEUE_BYTES`.
6. Real-PTY smoke (single test, `--host-ptys`, gated so `scripts/verify.sh` stays token-cheap): spawn host, attach, `echo hi`, detach, reattach, assert `hi` in snapshot; shell pid unchanged.
7. Golden: stored v1 frames (hex) in `tests/` or `src/attach/tests/` so future protocol edits cannot silently break compatibility (matches P6 golden philosophy).
Gate for every PR: `scripts/verify.sh` single-line PASS; no hosted CI added (prefer-local-CI preference).

## 9. Task breakdown: independently mergeable PRs

Legend: depends-on in brackets. "Parallel group" = can be dispatched concurrently once deps are merged. Each PR is behind the existing opt-in (`--host-ptys`; attach server only starts with an explicit `--attach` flag, default off, error without `--host-ptys` like P1).

P2
- **T-A0 Spike (docs/notes only, no merge risk):** STATUS: DONE (#109). Measured GridSnapshot worst-case wire size, `RowSnapshot.dirty` semantics with multiple consumers, `host_tick` latency, read `negotiate_session_viewport`, checked `Action` serde-ability. Output: section 12 below and "P2 decisions" in `docs/plans/session-host-architecture.md`.
- **T-A1 `attach::frame` + `limits` + `msg` types + golden vectors:** STATUS: IMPLEMENTED (#111, `src/attach/{frame,limits,msg}.rs`).
- **T-A2 Listener hook + handshake + client registry + `--attach` flag:** STATUS: IMPLEMENTED (#116, `src/attach/{listener,conn,handshake,registry}.rs`, `attach.clients` RPC, `workspace.status.attach`).
- **T-A3 Size owner + `Action::AttachResize` + resize race logic:** STATUS: IMPLEMENTED (#114, `src/attach/registry.rs`, `Action::AttachResize`).
- **T-A4 Host snapshot export (`wm/host_view.rs`) + `WireGrid` + `diff.rs` + `session.rs` state machine:** STATUS: IMPLEMENTED (#117, `src/wm/host_view.rs`, `src/attach/{view,diff,session}.rs`).
- **T-A5 Input path: `Action::PaneInputBytes`, key-event forwarding, rate limits, ro/rw enforcement, traffic attribution:** STATUS: IMPLEMENTED (#122, `src/attach/input.rs`, `Action::PaneInputBytes`).
- **T-A6 Streaming glue + `attach.kick`:** STATUS: IMPLEMENTED (#125 + #131, `src/attach/{hub,outq,stream,host}.rs`, `Action::DaemonSupervisor(DaemonSupervisorOp::AttachKick)`).
- **T-A7 `bilterm attach <session>` TUI client:** STATUS: IMPLEMENTED (#126, `src/attach/client/`, `tests/attach_a7.rs`).
- **T-A8 Docs + real-PTY smoke + `docs/API.md` + guide module map + "P2 decisions" in the architecture doc:** STATUS: IMPLEMENTED (#128 + macOS verification and architecture doc sync: `docs/user-guide/15-attaching-to-a-session-host.md`, `scripts/attach_smoke.py`, `docs/API.md`, `docs/DEVELOPER_AND_AGENT_GUIDE.md`, `docs/plans/session-host-architecture.md`).

P3 (clipboard/graphics/GUI; can start after T-A6)
- **T-B1 Graphics prototype spike** (size/latency numbers, recommend encoding). [T-A6]. Parallel with T-C1.
- **T-B2 `ClipboardSink` seam + host proxy of OSC 52 writes + client `ClipboardSet` handling + read-deny default.** [T-A6]. Parallel group.
- **T-B3 Graphics in Snapshot/`GraphicsChunk`/`GraphicsKeep` + client re-emit or placeholder.** [T-B1, T-A7].
- **T-B4 GUI client on the protocol** (`--gui` attach, spawn-or-attach config switch, in-process fallback kept one release) — parent P3. [T-A7, T-B2]. Large; split GUI render adapter vs. config switch.

P4 (supervisor + limits; independent of attach, only needs P1)
- **T-C0 Limits feasibility spikes** (Linux cgroup delegation / systemd-oomd; macOS rusage; Windows job). [none]. Can start immediately, in parallel with all of P2.
- **T-C1 Restart policy + crash-loop breaker + `daemon list` RSS** on existing `supervisor.rs`/`registry.rs`. [none].
- **T-C2 Linux limits via systemd scope / cgroup + watchdog fallback; config keys.** [T-C0, T-C1].
- **T-C3 macOS watchdog; T-C4 Windows job object.** [T-C0].

Dependency sketch: `A0 -> A1 -> {A2, A3, A4} -> A5 -> A6 -> {A7, B1, B2} -> {A8, B3, B4}`; `C0 -> C1 -> C2/C3/C4` fully parallel with P2.
Maximum useful parallelism: after T-A1 merges, three PRs (A2, A3, A4) plus C-track plus A7-against-fake-server.

## 10. Risks

- Diff correctness (dirty tracking per client) is the highest-risk item; mitigated by the property test and by the always-available full Snapshot resync path (diff is an optimization; correctness never depends on it).
- Latency of 25 ms tick for echo; mitigate with input-triggered immediate flush (measure in A0).
- `Action` serialization (UNVERIFIED) could force a text form; acceptable via colon-command strings.
- Single-owner plane loop: a heavy snapshot encode blocks `host_tick`. Mitigate: encode from a cloned `GridSnapshot` off-loop where possible, cap per-iteration work (budget N panes per tick).
- Two protocols on one socket (sniffing) vs sibling socket: regression risk to `ctl`; default to sibling socket.

## 11. Decisions (OWNER-ACCEPTED: all recommendations accepted)

The owner accepted every recommendation. Where the original question had no explicit recommendation, the plan's proposed default (stated in the body) is the accepted answer. These are binding for T-A1..T-A8.

| Q | Decision (OWNER-ACCEPTED) |
|---|---|
| Q1 | Sibling socket `host-<name>.attach.sock` (same dir, 0600, same uid check). No magic sniffing; JSON-RPC listener stays byte-identical. Section 3.1 "connection discrimination" is replaced by this. |
| Q2 | Resize: latest active `rw` client wins (abduco-style, 150 ms debounce). `negotiate_session_viewport` (min-clamp) is NOT used for attach; min policy stays a possible later config switch. |
| Q3 | Do not reuse the `DaemonRequest`/`DaemonResponse` serde shape or its client-chosen `client_id`. Leave the unused enum untouched in P2; T-A8 decides retire-vs-adapter (no consumer exists, see section 12 #2). |
| Q4 | Focus/active pane is host-global and shared in v1. |
| Q5 | No resumable token; every connection is a new client. |
| Q6 | Unix-only for attach in P2/P3; Windows named pipes deferred. |
| Q7 | Proxied clipboard writes go to the size-owner / focused `rw` client. OSC 52 read is not supported (it is dropped today, section 12 #8); stays default-deny and unimplemented unless re-opened. |
| Q8 | Default limits accepted: 16 clients, 1 MiB/s input (256 KiB burst), 30 s kick. Values still need a paste-heavy sanity check in T-A5. |
| Q9 | JSON v1 for cell data with row run-length encoding; binary body reserved for v2. A0 measurement supports this (section 12 #5). |
| Q10 | `bilterm attach` never auto-spawns a host; strictly opt-in as P1 decided. |
| Q11 | `ro` attach is a first-class role in P2. |
| Q12 | Proxy tmux-backed panes like any other pane (every `CompositePane` has a non-optional `vt: VtParser`, `src/pane/mod.rs:85`); T-A4 adds a test for a tmux-backed pane snapshot. |

## 12. UNVERIFIED claims: T-A0 results

T-A0 was executed by reading code and running throwaway tests (not committed) against `main` @ 7634a14. Status: VERIFIED / REFUTED / STILL-UNVERIFIED.

| # | Claim | Status | Evidence |
|---|---|---|---|
| 1 | `RowSnapshot.dirty` works with multiple independent consumers | **REFUTED** (cannot be used for per-client diffs) | The default engine hard-codes `dirty: true` for every row (`src/vt/mod.rs:636`, `src/vt/backend.rs:594`, `RowSnapshot::new_blank` `src/vt/snapshot.rs:380`). Only the optional ghostty backend reads a real flag and then clears it (`src/vt/ghostty_backend.rs:325` read, `:375` `set_dirty(false)`), i.e. a single destructive consumer. The worker-level `SharedGridSnapshot.dirty` is one `AtomicBool` with a global `mark_clean` (`src/vt/worker.rs:88,152-157`). `GridSnapshot.generation` is a wrapping counter bumped on every `process()` (`src/vt/mod.rs:252`) and is the only usable change signal, and it is coarse (whole grid). **Consequence:** diffs must be computed by the attach layer: keep per-client-per-pane last-sent row hashes (or copies) and compare against the current cloned `GridSnapshot` (`read_snapshot`, `SharedGridSnapshot::read_snapshot`, `src/vt/worker.rs`, cheap clone off the lock). Ignore `RowSnapshot.dirty` on the wire. T-A4 is adjusted below. |
| 2 | `negotiate_session_viewport` policy | **VERIFIED** (min-clamp; not what we want) | `src/daemon/mod.rs:163-176`: takes the minimum effective cols/rows over all clients after dividing by `font_scale_pct`, clamped to cols 20..500 and rows 6..200. This is tmux-style min, not latest-rw-wins. Not used (Q2). Useful constants: the 500x200 clamp is a sane upper bound for accepted `Resize`; adopt as `MAX_ATTACH_COLS/ROWS` (replaces the plan's 1000x500 example). Confirmed also that nothing consumes `DaemonRequest` outside `handle_request` in `daemon/mod.rs`. |
| 3 | `serve_jsonrpc_connection` can take an already-peeked stream | **VERIFIED awkward, now moot** | It takes ownership of a generic `S` and wraps it in its own `BufReader` (`src/api/server.rs:557-570`); peeked bytes would need a prefix-chaining wrapper (tokio `UnixStream` has no `peek`). Sibling socket chosen (Q1). |
| 4 | 25 ms host tick is acceptable echo latency | **VERIFIED (acceptable, bounded)** | Throwaway test: real PTY host pane (`WindowManager::new_host`), write `echo MARK\n`, poll `poll_all_ptys()` until the marker row appears in the snapshot, 20 samples, `SHELL=/bin/sh`. 1 ms poll: mean 1.7 ms, p95 1.8 ms. 25 ms poll (sleep-then-poll, i.e. worst-phase): mean 25.9 ms, p95 26.1 ms. So the tick adds 0..25 ms (avg about 12.5 ms in a free-running interval) and the PTY->VT path itself is about 2 ms. With the owner's interactive bash the end-to-end was about 100 ms at BOTH 1 ms and 25 ms ticks: the floor is shell/readline/rc latency, not bilterm. **Decision:** attach sends piggyback on `HOST_TICK`; no input-triggered flush required for v1 (keep as optional T-A6 optimisation if the attach RTT budget matters). Not measured: client-side render and socket RTT. |
| 5 | Snapshot/frame size numbers | **VERIFIED (measured), rate limits STILL-UNVERIFIED** | Throwaway encoder (cells as `[text,fg,bg,flags]`; no serde exists on `CellSnapshot`). Naive per-cell JSON: 80x24 = 27 KB, 200x50 = 140 KB, 500x200 = 1.4 MB (2.8 MB with RGB colour on every cell). Row run-length JSON (merge adjacent cells with identical attrs): blank/default-colour 80x24 = 2.3 KB, 200x50 = 10.7 KB, 500x200 = 103 KB; worst case (random RGB fg+bg and bold on every cell) 80x24 = 50 KB, 200x50 = 260 KB, 500x200 = 2.6 MB. All fit under `MAX_ATTACH_FRAME_BYTES` = 4 MiB without graphics; the 4 MiB cap and the 500x200 resize ceiling are consistent. Graphics are the only thing that can exceed it (handled by `graphics_omitted`/`GraphicsChunk`). The input token-bucket numbers (1 MiB/s) were not measured; remain a T-A5 item. |
| 6 | `Action` is serde-serializable | **REFUTED** | `#[derive(Clone, Debug, PartialEq, Eq)]` only (`src/action.rs:19`), about 190 variants, many carrying non-serde types (`PaneId`, `PromptMode`, `ExitProtectionPolicy`, imported at `src/action.rs:7`); only a handful of nested op enums derive serde (`WebOp`, `DrawerPreviewOp`, `AppearanceOp`, `DaemonSupervisorOp`, lines 313-371). **Decision:** do NOT derive serde for `Action`. `Act` carries a colon-command string (`{"cmd":":split h"}`) parsed by the existing colon parser then `apply`; typed Action variants added by this plan (`PaneInputBytes`, `AttachResize`, `AttachKick`) are constructed host-side from dedicated wire messages, never deserialized. T-A5 is simplified accordingly. |
| 7 | tokio `peer_cred` yields uid on macOS | **VERIFIED** | Verified on `bigmac.local` (macOS arm64) via `tests/attach_a7.rs` (7 real-socket e2e tests) and `scripts/attach_smoke.py`. |
| 8a | OSC 52 read handling today | **VERIFIED: reads are dropped** | `src/vt/scan_osc_dcs.rs:158-170` queues only when `is_forwardable_osc52_write` holds (`src/vt/codec.rs:253-267`); that requires an alphanumeric/`+/=` base64 payload, so the query form `52;c;?` fails (`?` rejected) and the sequence is silently ignored. No reply is ever generated. So "read unsupported, default deny" (Q7) is current behaviour; P3 needs no read path. |
| 8b | `vt/detect.rs` covers Kitty/Sixel capability detection | **REFUTED** | `src/vt/detect.rs` is URL/hint/OSC 8/search detection only (`detect_visible_urls`, `search_canvas`, ...). No graphics capability query exists. Graphics capability for a client must come from the `Hello.caps` string (client declares `graphics`), not terminal probing, in P2/P3; TUI-client terminal probing is new work in T-B3. |
| 9 | Linux cgroup/oomd, macOS rusage, Windows job object | **STILL-UNVERIFIED** | Out of scope for this docs spike; belongs to T-C0. |
| 10 | Supervisor RSS footprint of the main binary | **STILL-UNVERIFIED** | Not measured here; T-C0/T-C1. |
| 11 | tmux-backed panes expose a VtParser | **VERIFIED structurally** | `CompositePane.vt: VtParser` is non-optional (`src/pane/mod.rs:85,179`) so snapshot export works for every pane kind; tmux `%output` is decoded in `src/tmux/decode.rs` into pane events. End-to-end tmux-pane snapshot not exercised; test added to T-A4. |
| 12 | uid check extractable to a pure fn; `main.rs` line count | **VERIFIED** | The uid compare is inline in `RpcAcceptor for UnixListener::accept` (`src/api/server.rs:341-357`): `peer_uid != current_euid()`; trivially extractable as `fn check_peer_uid(peer: u32, me: u32) -> io::Result<()>`. `src/main.rs` is 718 lines, so an `attach` subcommand arg wiring fits (<= 1000 target). |

### Task-breakdown adjustments from T-A0

- **T-A0: DONE** (this section).
- **T-A1:** size caps fixed: `MAX_ATTACH_COLS=500`, `MAX_ATTACH_ROWS=200` (from the existing viewport clamp); keep 4 MiB / 64 KiB frame limits (measured worst case grid 2.6 MB < 4 MiB). Wire cell format is row run-length JSON.
- **T-A2:** sibling socket only (no sniffing, no `serve_jsonrpc_connection` change); extract `check_peer_uid`; also verify macOS `peer_cred` over ssh.
- **T-A3:** latest-rw-wins only; do not call `negotiate_session_viewport`.
- **T-A4:** `diff.rs` must not use `RowSnapshot.dirty`; implement per-client row-hash (or copy) comparison against a cloned `GridSnapshot`, with `generation` as the cheap "anything changed" gate. Add the tmux-backed pane test. Property test unchanged.
- **T-A5:** `Act` = colon-command string, no `Action` serde; typed wire messages for input/resize/kick; measure input token-bucket against a large paste.
- **T-A6:** no input-triggered flush needed for v1 (tick latency measured acceptable); keep as a follow-up knob.
- **T-B2:** no OSC 52 read path (already dropped by the VT scanner); only write proxying.
- **T-B3:** client graphics capability comes from `Hello.caps`; terminal probing is new work, not reuse of `vt/detect.rs`.
- Remaining STILL-UNVERIFIED (#7, #9, #10) are carried by T-A2 and T-C0/C1 and do not block T-A1.
