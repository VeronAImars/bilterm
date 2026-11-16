//! Empty sessions (`:session on-empty keep`): a session that stays open with zero windows.
//!
//! The workspace code assumes a session always holds a pane (focus, prompt, layout, the 700-odd
//! `active_pane()` callers). An empty session is therefore represented by one inert placeholder
//! pane (`CompositePane::empty_placeholder`): a headless pane that renders the "session empty"
//! hint, hosts the command prompt so `:split` / `:session ...` can be typed, and is never counted
//! as a window. It is hidden from window counts and lists, saved as a session with no panes, and
//! replaced by the first real window that opens.

use crate::config::SessionOnEmpty;
use crate::pane::{CompositePane, PaneId};
use crate::wm::WindowManager;
use crate::wm::split_tree::SplitTree;
use crate::wm::z_stack::ZStack;

/// Hint painted on the empty canvas (below the default floating prompt rows).
const EMPTY_HINT: &str = "\x1b[2J\x1b[7;3H\x1b[1mSession empty\x1b[0m - no windows\x1b[9;3H:split or :new   open a window\x1b[10;3H:session kill   end this session\x1b[11;3H:session on-empty end|keep";
const EMPTY_STATUS: &str = "session empty - :new / :split to open a window";

impl WindowManager {
    /// `true` when the active session is kept open with zero windows.
    pub fn is_session_empty(&self) -> bool {
        self.panes.len() == 1 && self.panes.values().all(|p| p.empty_placeholder)
    }

    /// Number of real windows in the active session (the placeholder of an empty session is not one).
    pub fn window_count(&self) -> usize {
        self.panes.values().filter(|p| !p.empty_placeholder).count()
    }

    /// Real windows of the active session, excluding an empty session's placeholder.
    pub fn window_panes(&self) -> impl Iterator<Item = (&PaneId, &CompositePane)> {
        self.panes.iter().filter(|(_, p)| !p.empty_placeholder)
    }

    /// Builds the placeholder pane for an empty session.
    fn new_empty_placeholder(id: PaneId, rows: u16, cols: u16) -> CompositePane {
        let mut pane = CompositePane::new_headless(id, rows, cols);
        pane.empty_placeholder = true;
        pane.set_title("SESSION EMPTY");
        pane.process_pty_output(EMPTY_HINT.as_bytes());
        pane.set_status_message(Some(EMPTY_STATUS.to_string()));
        pane
    }

    /// Replaces every window of the active session with the empty-session placeholder. Dropping the
    /// old panes ends their programs. A no-op when the session is already empty.
    pub(crate) fn enter_empty_session(&mut self) {
        if self.is_session_empty() {
            return;
        }
        self.forget_all_pane_ids_before_replace();
        let (rows, cols) = self.default_size;
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        self.install_sole_pane(id, Self::new_empty_placeholder(id, rows, cols));
        self.window_manage.selected_windows.clear();
        self.reflow_default_viewport();
        self.drawer.invalidate_saved_sessions();
        self.maybe_autosave_layout();
    }

    /// Placeholder pane for restoring a saved empty session (`apply_saved_session_snapshot`).
    pub(crate) fn placeholder_for_restore(&self, id: PaneId) -> CompositePane {
        let (rows, cols) = self.default_size;
        Self::new_empty_placeholder(id, rows, cols)
    }

    /// Makes `pane` the only pane of the session (drops the rest) with a fresh single-leaf layout.
    pub(crate) fn install_sole_pane(&mut self, id: PaneId, pane: CompositePane) {
        self.panes.clear();
        self.pane_chrome.clear();
        self.panes.insert(id, pane);
        self.split_tree = SplitTree::new(id);
        self.z_stack = ZStack::new(id);
        self.zoomed_pane = None;
    }

    /// Whether the last window going away should leave the session open and empty.
    pub(crate) fn keeps_empty_session(&self) -> bool {
        self.config.session_on_empty == SessionOnEmpty::Keep
    }

    /// `:session on-empty [end|keep]`: sets or reports what happens when a session's last window goes.
    pub fn set_session_on_empty_command(&mut self, policy: Option<SessionOnEmpty>) {
        let msg = match policy {
            Some(p) => {
                self.config.session_on_empty = p;
                match p {
                    SessionOnEmpty::End => {
                        "Session on-empty: end (the last window ends the session and moves to another)".to_string()
                    }
                    SessionOnEmpty::Keep => {
                        "Session on-empty: keep (the last window leaves the session open and empty)".to_string()
                    }
                }
            }
            None => format!(
                "Session on-empty: {} (:session on-empty end|keep)",
                self.config.session_on_empty.label()
            ),
        };
        self.active_pane_mut().set_status_message(Some(msg));
    }
}
