//! Editor windows (`EDIT-01..06`): `Action::Editor` execution, window creation, scratchpad
//! autosave and the close guard. The editor is an ordinary window: it joins the split tree and
//! z-stack, is saved in sessions, and closes through `remove_pane` like any other.

use std::path::PathBuf;

use super::WindowManager;
use crate::api::JsonValue;
use crate::editor::{EditorKind, EditorOp, EditorState, io};
use crate::error::{BiltermError, Result};
use crate::pane::PaneId;
use crate::wm::split_tree::SplitDirection;

pub const DEFAULT_SCRATCH: &str = "scratch";

impl WindowManager {
    /// Private directory of scratchpad buffers, next to the sessions directory.
    pub(super) fn scratch_dir(&self) -> Result<PathBuf> {
        let sessions = self.resolve_session_dir()?;
        Ok(sessions.parent().map_or_else(|| sessions.join("scratch"), |p| p.join("scratch")))
    }

    /// Editor windows in id order.
    pub fn editor_pane_ids(&self) -> Vec<PaneId> {
        self.panes.iter().filter(|(_, p)| p.editor().is_some()).map(|(id, _)| *id).collect()
    }

    /// The active window when it is an editor, else the lowest-numbered editor window.
    fn editor_target(&self) -> Option<PaneId> {
        let a = self.active_pane_id();
        if self.panes.get(&a).is_some_and(|p| p.editor().is_some()) { Some(a) } else { self.editor_pane_ids().first().copied() }
    }

    /// Opens a new editor window next to the focused one.
    pub fn spawn_editor_pane(&mut self, mut ed: EditorState) -> Result<PaneId> {
        ed.cwd = self.active_pane().resolve_cwd();
        let id = self.split_focused_with(SplitDirection::Vertical, None, true)?;
        let pane = self.panes.get_mut(&id).ok_or_else(|| BiltermError::Config("editor window vanished".into()))?;
        pane.set_editor(ed);
        Ok(id)
    }

    /// Rebuilds the editor of a restored session window (`None` when its backing file is gone).
    pub(super) fn build_saved_editor(&self, saved: &super::SavedEditor) -> Option<EditorState> {
        match saved {
            super::SavedEditor::Scratch { name } => {
                let file = io::scratch_file(&self.scratch_dir().ok()?, name).ok()?;
                Some(EditorState::open_scratch(name, file))
            }
            super::SavedEditor::File { path, wrap } => EditorState::open_file(std::path::Path::new(path)).ok().map(|mut e| {
                e.wrap = *wrap;
                e
            }),
        }
    }

    /// Session description of an editor window (`None` for terminal windows).
    pub(super) fn saved_editor(p: &crate::pane::CompositePane) -> Option<super::SavedEditor> {
        let ed = p.editor()?;
        match (&ed.kind, &ed.path) {
            (EditorKind::Scratch(n), _) => Some(super::SavedEditor::Scratch { name: n.clone() }),
            (_, Some(p)) => Some(super::SavedEditor::File { path: p.to_string_lossy().into_owned(), wrap: ed.wrap }),
            _ => None,
        }
    }

    fn editor_reply(&mut self, ok: bool, message: String, extra: Vec<(&str, JsonValue)>) {
        let mut entries = vec![
            ("status", JsonValue::String(if ok { "ok" } else { "error" }.into())),
            ("message", JsonValue::String(message.clone())),
        ];
        entries.extend(extra);
        self.set_command_data(JsonValue::obj(entries).to_json_string());
        self.active_pane_mut().set_status_message(Some(message));
    }

    /// Executes `Action::Editor`. Outcomes go to the status bar and the structured command data.
    pub fn apply_editor_op(&mut self, op: EditorOp) {
        if self.is_api_origin()
            && op.touches_files()
            && !self.config.api.allow_config_write
        {
            return self.editor_reply(false, "editor file access over the API is disabled; set [api] allow_config_write = true in config.toml".into(), vec![]);
        }
        match op {
            EditorOp::Open { path } => self.editor_open(path),
            EditorOp::Scratch { name } => self.editor_scratch(name.as_deref().unwrap_or(DEFAULT_SCRATCH)),
            EditorOp::Close { force } => self.editor_close(force),
            op => {
                let Some(id) = self.editor_target() else {
                    return self.editor_reply(false, "no editor window (:edit [path] or :scratch)".into(), vec![]);
                };
                self.editor_run(id, op);
            }
        }
    }

    fn editor_open(&mut self, path: Option<String>) {
        let mut ed = match path {
            None => EditorState::new_file(),
            Some(raw) => {
                let cwd = self.active_pane().resolve_cwd();
                let resolved = match io::resolve_path(&raw, &cwd).and_then(|p| io::check_regular(&p).map(|_| p)) {
                    Ok(p) => p,
                    Err(e) => return self.editor_reply(false, e, vec![]),
                };
                if let Some(id) = self.editor_pane_ids().into_iter().find(|id| self.panes[id].editor().and_then(|e| e.path.as_ref()) == Some(&resolved)) {
                    self.focus_and_raise(id);
                    return self.editor_reply(true, format!("editing {}", resolved.display()), vec![("pane", JsonValue::Number(id as i64))]);
                }
                match EditorState::open_file(&resolved) {
                    Ok(e) => e,
                    Err(e) => return self.editor_reply(false, e, vec![]),
                }
            }
        };
        ed.wrap = true;
        self.finish_open(ed);
    }

    fn finish_open(&mut self, ed: EditorState) {
        let name = ed.display_name();
        match self.spawn_editor_pane(ed) {
            Ok(id) => self.editor_reply(true, format!("editing {name}"), vec![("pane", JsonValue::Number(id as i64))]),
            Err(e) => self.editor_reply(false, format!("editor: {e}"), vec![]),
        }
    }

    fn editor_scratch(&mut self, name: &str) {
        if let Err(e) = crate::secure_fs::validate_safe_name(name) {
            return self.editor_reply(false, format!("scratch name: {e}"), vec![]);
        }
        let existing = self.editor_pane_ids().into_iter().find(|id| matches!(&self.panes[id].editor().map(|e| &e.kind), Some(EditorKind::Scratch(n)) if n == name));
        if let Some(id) = existing {
            self.focus_and_raise(id);
            return self.editor_reply(true, format!("scratchpad {name}"), vec![("pane", JsonValue::Number(id as i64))]);
        }
        let file = match self.scratch_dir().map_err(|e| e.to_string()).and_then(|d| io::scratch_file(&d, name)) {
            Ok(f) => f,
            Err(e) => return self.editor_reply(false, e, vec![]),
        };
        self.finish_open(EditorState::open_scratch(name, file));
    }

    fn editor_close(&mut self, force: bool) {
        let Some(id) = self.editor_target() else {
            return self.editor_reply(false, "no editor window".into(), vec![]);
        };
        if force && let Some(e) = self.panes.get_mut(&id).and_then(|p| p.editor_mut()) {
            e.close_armed = true;
        }
        self.focus_and_raise(id);
        if self.close_focused_pane() {
            self.editor_reply(true, "editor closed".into(), vec![]);
        } else {
            let msg = self.active_pane().status_message().unwrap_or("not closed").to_string();
            self.editor_reply(false, msg, vec![]);
        }
    }

    fn editor_run(&mut self, id: PaneId, op: EditorOp) {
        let Some(pane) = self.panes.get_mut(&id) else { return };
        let Some(ed) = pane.editor_mut() else { return };
        let mut extra: Vec<(&str, JsonValue)> = Vec::new();
        let result: std::result::Result<String, String> = match op {
            EditorOp::Save { force } => {
                if ed.path.is_none() {
                    Err("no file name: use :editor saveas <path>".into())
                } else {
                    ed.save(force).map_err(|e| e.to_string())
                }
            }
            EditorOp::SaveAs { path, force } => io::resolve_path(&path, &ed.cwd).and_then(|p| ed.save_as(&p, force).map_err(|e| e.to_string())),
            EditorOp::Wrap(w) => Ok(format!("soft wrap {}", if ed.set_wrap(w) { "on" } else { "off" })),
            EditorOp::Find { query, backward } => {
                if ed.find(&query, !backward) { Ok(format!("found: {query}")) } else { Err(format!("not found: {query}")) }
            }
            EditorOp::Undo => if ed.buf.undo() { Ok("undo".into()) } else { Err("nothing to undo".into()) },
            EditorOp::Redo => if ed.buf.redo() { Ok("redo".into()) } else { Err("nothing to redo".into()) },
            EditorOp::Goto(n) => {
                ed.goto_line(n);
                Ok(format!("line {n}"))
            }
            EditorOp::Revert => ed.revert(),
            EditorOp::Insert(t) => {
                ed.paste(&t);
                Ok(format!("inserted {} chars", t.chars().count()))
            }
            EditorOp::Text => {
                extra.push(("text", JsonValue::String(ed.buf.text())));
                Ok("text".into())
            }
            EditorOp::Status => Ok("status".into()),
            EditorOp::Open { .. } | EditorOp::Scratch { .. } | EditorOp::Close { .. } => return,
        };
        extra.push(("editor", JsonValue::parse(&ed.status_json()).unwrap_or(JsonValue::Null)));
        extra.push(("pane", JsonValue::Number(id as i64)));
        pane.paint_editor();
        match result {
            Ok(m) => self.editor_reply(true, m, extra),
            Err(m) => self.editor_reply(false, m, extra),
        }
    }

    /// Routes a key to the active editor window. `None` when the window is not an editor or the
    /// key is not the editor's (window-manager shortcuts keep working).
    pub fn editor_key(&mut self, key: crossterm::event::KeyEvent) -> Option<bool> {
        let pane = self.panes.get_mut(&self.z_stack.top()?)?;
        let ed = pane.editor_mut().filter(|e| e.focused)?;
        let consumed = ed.handle_key(key);
        if consumed {
            self.after_editor_change();
        }
        consumed.then_some(true)
    }

    /// Bracketed paste into the focused editor window.
    pub fn editor_paste(&mut self, text: &str) -> bool {
        let Some(pane) = self.z_stack.top().and_then(|id| self.panes.get_mut(&id)) else { return false };
        let Some(ed) = pane.editor_mut().filter(|e| e.focused) else { return false };
        ed.paste(text);
        self.after_editor_change();
        true
    }

    /// Repaints the active editor window.
    pub(crate) fn after_editor_change(&mut self) {
        if let Some(pane) = self.z_stack.top().and_then(|id| self.panes.get_mut(&id)) {
            pane.paint_editor();
        }
    }

    /// Toggles keyboard ownership between the editor and the pane's colon prompt (`Ctrl+]`).
    pub fn editor_toggle_focus(&mut self) -> bool {
        let Some(pane) = self.z_stack.top().and_then(|id| self.panes.get_mut(&id)) else { return false };
        let Some(ed) = pane.editor_mut() else { return false };
        ed.focused = !ed.focused;
        let msg = if ed.focused { "Editor focus: keys go to the buffer. Ctrl+] for the prompt" } else { "Editor focus: prompt (Ctrl+] returns to the buffer)" };
        pane.set_status_message(Some(msg.into()));
        pane.paint_editor();
        true
    }

    /// Periodic work: writes changed scratchpads (0600, atomic).
    pub fn editor_tick(&mut self) {
        for p in self.panes.values_mut() {
            if let Some(ed) = p.editor_mut().filter(|e| e.autosave_due())
                && let Err(e) = ed.autosave()
            {
                ed.message = Some(format!("scratch autosave failed: {e}"));
            }
        }
    }

    /// Flushes a scratchpad before its window goes away.
    pub(super) fn editor_flush(&mut self, id: PaneId) {
        if let Some(ed) = self.panes.get_mut(&id).and_then(|p| p.editor_mut())
            && ed.autosave_pending()
        {
            let _ = ed.autosave();
        }
    }

    /// Close guard: an unsaved file buffer needs a second close (or `:editor close --force`).
    pub(super) fn editor_allow_close(&mut self, id: PaneId) -> bool {
        let Some(p) = self.panes.get_mut(&id) else { return true };
        let Some(ed) = p.editor_mut() else { return true };
        if !ed.is_dirty() || ed.close_armed {
            return true;
        }
        ed.close_armed = true;
        let msg = format!("{} has unsaved changes: save (Ctrl+S) or close again to discard", ed.display_name());
        p.set_status_message(Some(msg));
        false
    }
}
