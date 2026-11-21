# Plan: workspaces drawer — stable order, reorder, real move, end-session-without-quit

Status: PLAN + root-cause evidence (2026-11-20). #127 (merged) only removed the "active session hoisted to top" symptom; it did not fix the model. Nothing below is implemented yet.

## Owner requirements
1. Drawer rows never rearrange on their own.
2. Reorder sessions and windows-within-a-session by drag and drop AND by keyboard (all surfaces: colon, CLI, RPC, MCP, palette: UNI-R01).
3. Moving a window between sessions works correctly (drag, keyboard, all surfaces).
4. Closing the last window can close the session without terminating bilterm.

## Root causes (reproduced headlessly on origin/main 3f6c620; throwaway tests, not committed)
R1 — three unrelated orderings, none authoritative.
  - `navigator_drawer_rows` (src/wm/drawer/navigator.rs) emits `daemon_state.session_tree.nodes` order, then `list_sessions()` (session_order.toml, then name), then unsaved sessions by name.
  - `:session move|reorder` (session_manager.rs:408/447) writes only `session_order.toml`.
  - Evidence: after `create_new_session` zeta, alpha: drawer `[zeta, alpha, default]` vs session_order `[default, zeta, alpha]`; `move_session_in_order("alpha","top")` yields order `[alpha, default, zeta]` but the drawer still shows `[zeta, alpha, default]`. So reorder is invisible, and a session changes group (tree -> saved -> unsaved) when it is saved, parked, attached to the tree, or restored, which reads as "rearranging itself".
R2 — window order is not state. `ordered_pane_ids` = `split_tree.leaves()` (live) / pane id sort (parked, navigator.rs push_bg_panes) / BTreeMap id (saved). The live and parked orders differ, split/close/move reshuffles leaves, and there is no Action to reorder windows at all (no Swap/Reorder variant in src/action.rs).
R3 — moving a window into a session that exists only on disk loses that session's windows. `move_window_to_session` (window_manage.rs:395) does `background_sessions.entry(target).or_insert_with(..)` building a `BackgroundSessionState` with EMPTY `panes` (only the saved split_tree/z_order), then the `else` branch resets `split_tree`/`z_stack` to just the moved pane. Repro: other.toml with 2 windows; move window 1 into it with switch=true -> session has 1 pane, expected 3. The next save overwrites other.toml: silent data loss. Parked (live) targets work; the existing test only covers a live target.
R4 — closing the last window of the only session quits. `session_on_empty` defaults to `End`; with no other session `end_active_session_and_move_on` returns None, so `fall_back_to_parked_session` is false and the loops quit via `app.all_panes_exited()` (src/app.rs:248; gui_window.rs:592; gui_font/linux_x11.rs:838; gui_macos.rs:719). `:close` on the only window is already safe (replaces it). `:session on-empty keep` exists but is opt-in, config-only on some surfaces, and the quit decision is duplicated in four loops.
R5 — no drag-and-drop in the drawer: mouse_wm.rs has only window/modal/resize drags; the drawer has `resizing_drag` only. Keyboard reorder exists only as `ops.rs:297` (`move_session_in_order` via menu) and is invisible because of R1.

## Architecture change
A. One authoritative, persisted session model: `SessionOrder` owned by WindowManager: ordered `Vec<SessionId/name>` covering live, parked and saved sessions, plus per-session `window_order: Vec<PaneId>`. The drawer, `:session list`, RPC/MCP and tabs ALL read it; session_tree gives grouping only (parent/collapsed), never order. New sessions append; nothing reorders implicitly (save/park/restore/rename keep the slot; rename rewrites it; delete removes it).
B. Window order = explicit `window_order` per session (live and parked share one type; saved TOML gains `window_order`, absent -> legacy leaf order). split/close/move edit it deterministically (new window right after its source; moved window appended).
C. Hydrate before mutate: `move_window_to_session` loads a saved-only target into a full `BackgroundSessionState` (spawn/headless panes per saved layout, same path as switch) or refuses; never builds an empty state. Make move atomic with rollback (extends the `switch_to_session_preserving_panes` rollback pattern).
D. Single exit policy function `WindowManager::should_quit()` replacing the four `all_panes_exited()` checks; default `session_on_empty = keep-last` (keep the final session open and empty instead of quitting; explicit `:quit`/`:session kill` of the last session still quits, headless `-c` runs keep old behavior). `end` remains configurable.
E. New Actions (all surfaces): `SessionMove{name, to: Pos}` (exists, route through the single model), `WindowMove{id, to: Pos}` (within session), `WindowMoveToSession{id, session, index?}`, `DrawerReorder` ops. Drag in drawer lowers to these (GUI wgpu + TUI mouse), with a drop-indicator row and Esc to cancel; keyboard: `Alt+Shift+Up/Down` reorder the selected row, `Alt+Shift+Left/Right` (or `m`) move window to prev/next session; colon `:session move`, `:window move`, `:window send <session>`; RPC `session.move`, `window.move`; MCP tools; palette entries.

## Milestones (each: tests fail before / pass after; scripts/verify.sh PASS; PR only, owner merges)
- M1 (bug fix, small): R3 hydrate-before-move + data-loss test; R4 `should_quit()` + keep-last default. 
- M2 (model): `SessionOrder` + `window_order`, migrate drawer/list/RPC to it; regression tests that no operation (save, park, switch, restore, rename, tree attach) changes row order; old goldens unchanged.
- M3 (surfaces): Window/Session move Actions + colon/CLI/RPC/MCP/palette, keyboard reorder in the drawer.
- M4 (mouse): drag and drop in TUI and GUI incl. cross-session drop onto a session row; selection stays on the moved row.
- M5 (verification): real TUI + X11 GUI drive, Windows/macOS compile.

## Tests to write first
Order stable across save/park/switch/restore/rename (R1); `session move` visible in drawer (R1); window reorder live and parked agree (R2); move into saved-only session keeps all windows, and on failure the source is untouched (R3); last window of last session exiting keeps bilterm alive and drawer shows an empty session (R4); drag lowers to the same Action as the keyboard path (R5).

## Added requirement (owner, 2026-11-20): move sessions between bilterm daemons; live restart with no state loss
Goal: a session (its windows, live terminals, scrollback, cursor, alt-screen, cwd, running programs) can move from daemon A to daemon B, so that restarting or upgrading bilterm never kills or resets a terminal. State today (verified by grep): NOT implemented. Only the design exists (`docs/plans/session-host-architecture.md` section 7.2, milestone P6: side-by-side handover, PTY master fds over `SCM_RIGHTS`, old host stays alive until COMMIT for rollback; Windows ConPTY unsupported in v1). P6 depends on P4 (supervisor); attach (P2) is merged. `session.move` between daemons also needs the `SessionOrder`/`window_order` model (architecture A/B above) so order survives the move.
Plan:
- H0 spike (read-only on product code): can a `portable-pty` PTY master fd be extracted from a live pane and adopted by another process, and can the child be tracked without being its parent (pidfd/kill(0))? Does the child keep running and the new reader see the same bytes? Linux first, then macOS (bigmac). Decides A (fd passing) vs fallback B (re-exec with CLOEXEC cleared).
- H1 per-session transfer: `session.transfer {name, to_daemon}` (colon, CLI, RPC, MCP, drawer drag onto a daemon row): A sends snapshot (grids, scrollback, window_order, layout) + PTY fds for ONLY that session; B adopts; A drops its copies only after B's COMMIT; rollback leaves A untouched. Same wire for whole-daemon handover (H2 = all sessions + listen socket) which is the live restart/upgrade path (`bilterm daemon restart --live`).
- H3 tests: counter-loop shells survive with the same child pids, no lost or duplicated output, scrollback/cursor equal, attached clients reconnect; version-skew REFUSE leaves A unaffected; kill B mid-transfer -> A intact.
- H4 platform: Windows ConPTY fallback = snapshot-only restore (programs restart; documented), macOS via bigmac.
Constraint on M1-M4: no new code may assume one daemon owns a session forever; keep session identity (`SessionId`) separate from the daemon that hosts it.
