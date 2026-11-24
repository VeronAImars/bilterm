# Built-in editor and scratchpad: milestones

An editor window and a scratchpad for quick edits. Both are ordinary windows: they tile, float,
move, close, belong to sessions and are saved with them. Plain UTF-8 text and markdown only (no rich
media). Linked from [`../FEATURE_MAP_AND_ROADMAP.md`](../FEATURE_MAP_AND_ROADMAP.md).

## Design (as built in E1)

- **Model**: `src/editor/` is pane-independent. `buffer.rs` (lines, cursor, selection, undo/redo,
  search), `io.rs` (safe load/save, scratch paths), `highlight.rs` (markdown classes), `render.rs`
  (ANSI paint, wrap, scroll, mouse mapping), `keys.rs`, `op.rs` (`EditorOp`).
- **Window**: `CompositePane::editor` holds the state; the pane has no PTY. The editor paints
  itself into the pane's terminal grid, so the TUI and the GUI render it through the existing
  pane-render path. The floating prompt is hidden while the editor has the keyboard; `Ctrl+]` hands
  the keyboard to the colon prompt and back (same escape hatch as the browser canvas).
- **Window manager**: `src/wm/editor_pane.rs` (open, scratch, ops, autosave tick, close guard).
  `Action::Editor(EditorOp)` is the only entry point, so `WindowManager::apply` runs everything.
- **Files** (`SEC-R01`): regular files only (directories, FIFOs, devices refused), UTF-8 text only
  (NUL or invalid UTF-8 refused), 4 MiB cap, atomic replace through a sibling temp file that keeps
  the original mode, symlinks followed to their regular target, CRLF and trailing newline
  preserved. Save-as onto an existing file and saving a file that changed on disk both need
  confirmation (`y/n` in the TUI, `force` over the API). File verbs over the API need
  `[api] allow_config_write`.
- **Scratchpad**: unnamed buffer `scratch` (or `:scratch <name>`), autosaved (debounced, flushed on
  close) to `<state>/bilterm/scratch/<name>.md` via `secure_fs` (0700 dir, 0600 atomic files),
  restored on the next `:scratch` and by session restore, promoted to a file by save-as (the scratch
  file is then removed).
- **Sessions**: `SavedPaneState.editor` (`SavedEditor::Scratch{name}` / `File{path,wrap}`); no shell
  is spawned for an editor window on restore.

## Surfaces (UNI-R01)

| Capability | Colon | Key (editor focused) | RPC | MCP `bilterm_editor` | CLI `bilterm editor` |
|---|---|---|---|---|---|
| Open file / new buffer | `:edit [path]` | palette | `editor.open` | `open` | `open [PATH]` |
| Scratchpad | `:scratch [name]` | `Ctrl+X _` (leader; `Ctrl+Alt+E` alias) | `editor.scratch` | `scratch` | `scratch [NAME]` |
| Save / save-as | `:editor save`, `saveas <path> [--force]` | `Ctrl+S`, `Ctrl+O` | `editor.save`, `editor.save_as` | `save`, `save_as` | `save`, `saveas` |
| Soft wrap | `:editor wrap [on\|off]` | `Ctrl+L` | `editor.wrap` | `wrap` | `wrap` |
| Find | `:editor find\|findprev <text>` | `Ctrl+F`, `Ctrl+N`, `Ctrl+P` | `editor.find` | `find` | `find`, `findprev` |
| Undo / redo | `:editor undo`, `redo` | `Ctrl+Z`, `Ctrl+Y` | `editor.undo`, `editor.redo` | `undo`, `redo` | `undo`, `redo` |
| Go to line | `:editor goto <n>` | `Ctrl+G` | `editor.goto` | `goto` | `goto` |
| Copy / cut / paste, select all | n/a | `Ctrl+C`, `Ctrl+K`, `Ctrl+V` (and bracketed paste), `Ctrl+A`, Shift+arrows | `editor.insert` | `insert` | `insert` |
| Revert, text, status, close | `:editor revert\|text\|status\|close` | n/a | `editor.revert\|text\|status\|close` | same names | same names |
| Mouse | n/a | click places cursor, drag selects, wheel scrolls | n/a | n/a | n/a |

Uniformity exceptions: copy/cut/select-all/paste keys have no colon form (they act on the live
selection); `editor.insert` is the programmatic paste. `Ctrl+X`/`Ctrl+B` are the leader keys, so cut
is `Ctrl+K` (nano style).

## Milestones

### E1 Core editor and scratchpad (this PR)

Done: open/edit/save/save-as with the safety rules above; cursor movement, selection, copy/paste,
undo/redo (coalesced typing), search, go to line; dirty indicator (window title `EDIT name *` and
status row); soft wrap toggle; markdown highlighting (headings, fences, quotes, lists, rules, code
spans, bold, italic, links); scratchpad autosave and restore; session persistence; close guard
(first close of a dirty file buffer is refused); all surfaces above; tests (`src/editor/tests.rs`,
`tests/editor_surface_parity.rs`).

Known gaps carried to later milestones: unsaved edits of a file buffer are not kept across a
restart (only scratchpads are); no multi-cursor, no line-number gutter, no replace.

### EDIT-07 Shared artifacts

`:artifact [name]` opens a window whose text is shared through `ContextBus` key `artifact:<name>`
(names are lowercased; Windows device names are rejected) and autosaved privately. Pushes use
`ContextBus::set_if_revision` against the last revision the window saw: a concurrent agent write is
never overwritten, the window shows CONFLICT, keeps the local text and `:editor revert` takes the
shared version. Bus failures (over 64 KiB, bus full) show as NOT SHARED and are not retried until
the buffer changes. Bus text is sanitised (controls and Unicode format characters dropped).
`:editor broadcast [note]` only works on artifact windows; `artifact` and `broadcast` need
`[api] allow_config_write` for API callers. Open question: an entry that expires is re-seeded from
the local text on the next edit.

### E2 Markdown preview and highlight polish

Rendered preview split (headings, emphasis, lists, tables, fenced code with a language hint),
nested emphasis and reference links, table alignment, line-number gutter option, replace
(`:editor replace`), indent-aware list continuation on Enter, theme-aware colours instead of fixed
SGR classes.

### E3 Multi-buffer, tabs and external-change diff

Several buffers per window with tabs, buffer list and switch (`:editor buffers|next|prev`), recovery
copies for dirty file buffers across restart, diff view when the file changed on disk (keep mine /
take theirs / merge), file watcher instead of the stat check at save, large-file handling above
4 MiB (read-only paging).

### E4 GUI polish

Native text rendering path in the GUI (font metrics, ligature-safe cursor, IME and composed text),
smooth scrolling, pixel-accurate mouse selection, GUI chord table review (`Cmd` on macOS), clipboard
integration parity with the terminal surfaces.

### E5 Per-OS verification

Run the editor suite and the manual checklist on Linux, macOS (Apple silicon and Intel), Windows
x64 and ARM64: file safety on NTFS (reserved names, case-insensitive overwrite check, read-only
attribute), CRLF round trips, symlink/junction handling, atomic rename semantics, scratch dir
permissions where POSIX modes do not apply, key chords in each terminal (`Ctrl+]`, `Ctrl+Alt+E`).
