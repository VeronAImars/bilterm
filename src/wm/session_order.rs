//! The one authoritative session order and per-session window order (drawer-session-model M2).
//!
//! Session order: a persisted list (`session_order.toml`) of session names. Live, parked, saved and
//! tree-registered sessions are all rows of that one list; `session_tree` supplies grouping only.
//! Sessions not yet in the list append after it (saved ones by name, then live-only ones by name);
//! [`WindowManager::freeze_session_order`] persists that view before any action runs, so nothing
//! (save, park, switch, restore, tree attach) can move a row. Identity is the session name and is
//! independent of the daemon hosting it.
//!
//! Window order: an explicit `Vec<PaneId>` per session (live: `WindowManageState::window_order`,
//! parked: `BackgroundSessionState::window_order`, saved: `window_order` in the TOML). Empty means
//! the legacy split-tree leaf order.

use std::collections::BTreeSet;

use super::WindowManager;
use super::window_manage::BackgroundSessionState;
use crate::pane::PaneId;
use crate::wm::layout_profile::sanitize_profile_name;

/// Resolves a move spec (`up`, `down`, `top`, `bottom`, or a 1-based index) to a new index.
pub(crate) fn resolve_move_index(cur_idx: usize, len: usize, spec: &str) -> Option<usize> {
    let last = len.saturating_sub(1);
    Some(match spec.trim().to_ascii_lowercase().as_str() {
        "up" | "-1" | "prev" | "previous" | "k" => cur_idx.saturating_sub(1),
        "down" | "+1" | "next" | "j" => (cur_idx + 1).min(last),
        "top" | "first" | "start" => 0,
        "bottom" | "last" | "end" => last,
        num => match num.parse::<usize>() {
            Ok(n) if n >= 1 => (n - 1).min(last),
            _ => return None,
        },
    })
}

impl WindowManager {
    /// Reorders window `window_id` (default: the active window) within its own session to `spec`
    /// (`up|down|top|bottom|1..N`). Works for live and parked sessions; the drawer reads the same order.
    pub fn reorder_window(&mut self, window_id: Option<PaneId>, spec: &str) -> Result<String, String> {
        const USAGE: &str = "Usage: :window reorder [id] <up|down|top|bottom|1..N>";
        let id = window_id.unwrap_or_else(|| self.active_pane_id());
        if self.panes.contains_key(&id) {
            let mut ids = self.ordered_pane_ids();
            let cur = ids.iter().position(|&x| x == id).ok_or_else(|| format!("Window {id} has no slot"))?;
            let new = resolve_move_index(cur, ids.len(), spec).ok_or_else(|| USAGE.to_string())?;
            let adjacent = new.abs_diff(cur) == 1;
            let partner = ids.get(new).copied();
            let item = ids.remove(cur);
            ids.insert(new, item);
            if adjacent && let Some(p) = partner {
                // Keep the tiled layout following the order, as the drawer always did for one step.
                self.split_tree.swap_leaves(id, p);
                self.reflow_default_viewport();
            }
            self.window_manage.window_order = ids.clone();
            let lbl = ids.iter().map(|p| format!("#{p}")).collect::<Vec<_>>().join(", ");
            return Ok(format!("Moved Pane #{id} {} (Window order: {lbl})", spec.trim().to_ascii_lowercase()));
        }
        for bg in self.window_manage.background_sessions.values_mut() {
            let mut ids = bg.ordered_window_ids();
            if let Some(cur) = ids.iter().position(|&x| x == id) {
                let new = resolve_move_index(cur, ids.len(), spec).ok_or_else(|| USAGE.to_string())?;
                let item = ids.remove(cur);
                ids.insert(new, item);
                bg.window_order = ids.clone();
                return Ok(format!("Window order: {}", ids.iter().map(|p| format!("#{p}")).collect::<Vec<_>>().join(", ")));
            }
        }
        Err(format!("Window {id} not found"))
    }


    /// The authoritative session order given the persisted list and the saved-session names.
    pub(crate) fn session_order_view(&self, persisted: &[String], saved: &[String]) -> Vec<String> {
        let active = sanitize_profile_name(&self.config.history_session);
        let tree = &self.daemon_state.session_tree;
        let mut candidates: BTreeSet<String> = saved.iter().cloned().collect();
        candidates.extend(self.window_manage.background_sessions.keys().cloned());
        candidates.insert(active);
        for node in &tree.nodes {
            let explicit = node.parent.is_some() || !node.children.is_empty() || node.name != "main";
            if explicit || candidates.contains(&node.name) {
                candidates.insert(node.name.clone());
            }
        }
        let mut out: Vec<String> = Vec::with_capacity(candidates.len());
        for name in persisted {
            if candidates.contains(name) && !out.contains(name) {
                out.push(name.clone());
            }
        }
        // Saved sessions first (already by name), then live-only ones by name.
        for name in saved {
            if candidates.contains(name) && !out.contains(name) {
                out.push(name.clone());
            }
        }
        // Tree-registered sessions keep the tree's own creation order for their first placement.
        for node in &tree.nodes {
            if candidates.contains(&node.name) && !out.contains(&node.name) {
                out.push(node.name.clone());
            }
        }
        for name in &candidates {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        out
    }

    /// The order the drawer renders, from the drawer's cached persisted order and saved list.
    pub(crate) fn session_order_view_cached(&self, saved: &[String]) -> Vec<String> {
        let persisted = self.drawer.cached_session_order(|| self.load_session_order());
        self.session_order_view(&persisted, saved)
    }

    /// Persists the currently visible session order when it differs from the stored one, so later
    /// operations only ever append or remove rows.
    pub(crate) fn freeze_session_order(&mut self) {
        let persisted = self.load_session_order();
        let saved = self.list_sessions();
        let view = self.session_order_view(&persisted, &saved);
        if view != persisted {
            self.save_session_order(view);
        }
    }

    /// Records that `new_id` sits directly after `source` in the active session's window order.
    pub(crate) fn window_order_insert_after(&mut self, source: PaneId, new_id: PaneId) {
        let mut ids = self.ordered_pane_ids();
        ids.retain(|&i| i != new_id);
        let at = ids.iter().position(|&i| i == source).map_or(ids.len(), |p| p + 1);
        ids.insert(at, new_id);
        self.window_manage.window_order = ids;
    }
}

impl BackgroundSessionState {
    /// Windows of this parked session in order: explicit order first, then leaf order, then the rest.
    pub fn ordered_window_ids(&self) -> Vec<PaneId> {
        let mut ids: Vec<PaneId> = Vec::new();
        let leaves = self.split_tree.leaves();
        for id in self.window_order.iter().copied().chain(leaves).chain(self.panes.keys().copied()) {
            if self.panes.get(&id).is_some_and(|p| !p.empty_placeholder) && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// Appends `id` as the last window (a window moved in from another session).
    pub fn append_window(&mut self, id: PaneId) {
        let mut ids = self.ordered_window_ids();
        ids.retain(|&i| i != id);
        ids.push(id);
        self.window_order = ids;
    }
}
