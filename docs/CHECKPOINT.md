# Session Checkpoint (2026-11-15, night)

See `docs/plans/session-checkpoint.md` (newest section on top) for full details.
- **`origin/main`**: `87a2f21`. No open PRs.
- **Merged this session** (verify PASS 206/206 at the end): #91 launcher GUI-default + TUI fallback, #92 `:version` window, #95 workspace.select keeps panes, #96 TRON light-cycle border (`:cycles`), #97 ending a session no longer quits bilterm, #98 focus chords (Ctrl+Alt+X/Z; Ctrl+Tab family as kitty/GUI aliases), #99 drawer: arrows reach windows, Enter unfolds, double-tap/double-click switches, #100 `session_on_empty end|keep`.
- **Not done**: no rc built or installed from this batch (needs owner's word); nothing run in a real TUI/GUI; empty-session gaps (inspector/RPC view, bulk-close and move-window refill); herdr pane-boundary report awaits repro.
- **Flaky under parallel verify load**: phase62_copy_mode_ergonomics test_02, timing tests.
- **Cleanup**: merged worktrees under /datanapps/worktrees/bilterm-* can be removed.
- No background agents running.
