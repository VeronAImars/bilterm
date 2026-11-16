//! Ending a session without ending bilterm (`:session kill|end|close|delete`, the Exit Guard
//! `(K)ill session` button, the last window of a session exiting or being closed).
//!
//! Ending the active session moves the workspace to another session: the most recently used one
//! still live in the background, else a saved one. Only when no other session exists does ending
//! it ask the app to quit. Every other session's programs keep running; ending a background
//! session drops only its own panes.

use std::cmp::Reverse;

use crate::wm::layout_profile::sanitize_profile_name;
use crate::wm::WindowManager;

/// What [`WindowManager::end_session`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEndOutcome {
    /// A background or saved-only session was ended; the active session is untouched.
    EndedOther { ended: String },
    /// The active session was ended and `now_active` took its place.
    Switched { ended: String, now_active: String },
    /// The active session was the last one: the caller quits bilterm.
    LastSession { ended: String },
}

impl WindowManager {
    /// Sessions to move to when `ended` (the active session) ends, best first: sessions live in the
    /// background that still run a program, most recently used first, then saved sessions in list
    /// order that were not ended earlier in this run (else killing sessions in turn would cycle
    /// through their saved layouts and never reach the last one). A headless workspace without its own session dir only considers live sessions, so it
    /// never restores from (or writes to) the user's session directory.
    pub fn session_end_candidates(&self, ended: &str) -> Vec<String> {
        let ended = sanitize_profile_name(ended);
        let mut live: Vec<(&String, u64)> = self
            .window_manage
            .background_sessions
            .iter()
            .filter(|(name, bg)| {
                sanitize_profile_name(name) != ended && bg.panes.values().any(|p| !p.has_exited())
            })
            .map(|(name, bg)| (name, bg.parked_seq))
            .collect();
        live.sort_by_key(|&(_, seq)| Reverse(seq));
        let mut out: Vec<String> = live.into_iter().map(|(name, _)| name.clone()).collect();
        if self.should_persist_sessions_to_disk() {
            for name in self.list_sessions() {
                if name != ended
                    && !self.window_manage.ended_sessions.contains(&name)
                    && !out.contains(&name)
                    && !self.window_manage.background_sessions.contains_key(&name)
                {
                    out.push(name);
                }
            }
        }
        out
    }

    /// Makes the best candidate session active and drops the outgoing (active) session's panes,
    /// which ends its programs. Saved layouts are not touched. Returns the new active session, or
    /// `None` when no other session could be brought up (the active session is then unchanged).
    fn end_active_session_and_move_on(&mut self) -> Option<String> {
        let ended = self.config.history_session.clone();
        for next in self.session_end_candidates(&ended) {
            self.forget_all_pane_ids_before_replace();
            match self.switch_to_session_preserving_panes(&next) {
                Ok(_) => {
                    // `switch_to_session_preserving_panes` parked the outgoing panes; dropping
                    // them here is what ends that session's programs.
                    self.window_manage.background_sessions.remove(&ended);
                    self.window_manage.ended_sessions.insert(sanitize_profile_name(&ended));
                    self.daemon_state.session_panes.remove(&sanitize_profile_name(&ended));
                    self.drawer.invalidate_saved_sessions();
                    return Some(self.config.history_session.clone());
                }
                Err(e) => tracing::warn!(session = %next, error = %e, "session end: candidate failed"),
            }
        }
        None
    }

    /// Ends session `name` (the active one when `None`). With `delete`, its saved layout and
    /// `session_order` slot go too (`:session delete`); without, only its running programs end and
    /// a saved layout stays restorable (`:session kill`). Ending the active session moves to
    /// another session; see [`SessionEndOutcome`].
    pub fn end_session(
        &mut self,
        name: Option<&str>,
        delete: bool,
    ) -> Result<SessionEndOutcome, String> {
        let active = sanitize_profile_name(&self.config.history_session);
        let raw = name.map(str::trim).filter(|n| !n.is_empty());
        let target = raw.map(sanitize_profile_name).unwrap_or_else(|| active.clone());
        if target != active {
            let raw = raw.unwrap_or(&target);
            if delete {
                return self
                    .delete_session(raw)
                    .map(|ended| SessionEndOutcome::EndedOther { ended });
            }
            let mut found = false;
            for key in [raw, target.as_str()] {
                found |= self.window_manage.background_sessions.remove(key).is_some();
            }
            if !found {
                return Err(format!(
                    "Session '{target}' is not running (:session delete {target} removes its saved layout)"
                ));
            }
            self.daemon_state.session_panes.remove(&target);
            self.window_manage.ended_sessions.insert(target.clone());
            self.drawer.invalidate_saved_sessions();
            return Ok(SessionEndOutcome::EndedOther { ended: target });
        }
        let outcome = match self.end_active_session_and_move_on() {
            Some(now_active) => SessionEndOutcome::Switched { ended: target.clone(), now_active },
            None => SessionEndOutcome::LastSession { ended: target.clone() },
        };
        if delete {
            self.forget_session_everywhere(&target);
        }
        Ok(outcome)
    }

    /// Runs [`Self::end_session`] for a command surface: sets the status line (and the automation
    /// error on failure). Returns `true` when bilterm should quit (the last session was ended).
    pub fn end_session_command(&mut self, name: Option<&str>, delete: bool) -> bool {
        let verb = if delete { "Deleted" } else { "Ended" };
        let (msg, quit) = match self.end_session(name, delete) {
            Ok(SessionEndOutcome::EndedOther { ended }) => (format!("{verb} session '{ended}'"), false),
            Ok(SessionEndOutcome::Switched { ended, now_active }) => (
                format!("{verb} session '{ended}'; switched to '{now_active}'"),
                false,
            ),
            Ok(SessionEndOutcome::LastSession { ended }) => {
                (format!("{verb} session '{ended}', the last one: quitting"), true)
            }
            Err(e) => {
                self.record_automation_error_msg(e.clone());
                (e, false)
            }
        };
        self.active_pane_mut().set_status_message(Some(msg));
        quit
    }

    /// When every pane of the active session has exited, ends that session and moves to another
    /// (live first, then saved); with none, the app quits as before because all panes exited.
    /// Returns `true` when it switched.
    pub(crate) fn fall_back_to_parked_session(&mut self) -> bool {
        if self.panes.is_empty() || !self.all_panes_exited() {
            return false;
        }
        let ended = self.config.history_session.clone();
        let Some(next) = self.end_active_session_and_move_on() else {
            return false;
        };
        self.active_pane_mut().set_status_message(Some(format!(
            "Session '{ended}' ended (all its panes exited); switched to '{next}'"
        )));
        true
    }

    /// `:close` on the only window of the active session: ends the session and moves to another
    /// one. On the last session nothing changes (`:quit` leaves bilterm). Returns `true` when it
    /// switched.
    pub(crate) fn close_last_window_of_session(&mut self) -> bool {
        let ended = self.config.history_session.clone();
        let Some(next) = self.end_active_session_and_move_on() else {
            return false;
        };
        self.active_pane_mut().set_status_message(Some(format!(
            "Closed the last window of session '{ended}' (session ended); switched to '{next}'"
        )));
        true
    }
}
