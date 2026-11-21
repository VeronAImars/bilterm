//! Session find (`:find --pid N --pty P --cwd D --cmd C --text T ...`): locate panes by OS
//! identifying attributes and act on the matches (focus, send text, beep, flash, select).
//!
//! `SessionFindOp` is the one vocabulary every surface lowers to through `Action::SessionFind`
//! and `WindowManager::apply` (ARCH-R01, UNI-R01): colon (`:find ...`, `:find-<act>`), CLI
//! (`bilterm find`), RPC (`find.locate|focus|send|beep|flash|select`), MCP (`bilterm_session_find`),
//! palette and `Ctrl+Alt+D`. It sits beside the fuzzy registry Find (`find_cmd`), which keeps
//! `:find agent|session|history`.

mod exec;
mod matcher;
pub mod probe;
mod query;

pub use matcher::{Match, Outcome, PaneFacts};
pub use query::{FILTER_CAP, FindAct, FindQuery, LIMIT_DEFAULT, LIMIT_MAX, SessionFindOp, Tokens};

use crate::pane::PaneId;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

/// Largest number of panes one `send` may type into.
pub const SEND_TARGET_CAP: usize = 16;
/// Largest text (bytes) one `send` delivers per pane.
pub const SEND_TEXT_CAP: usize = 4096;
/// How long a `flash` highlight lasts.
pub const FLASH_MS: u64 = 800;

/// Session-find state owned by the window manager (one field; private to `wm`).
#[derive(Debug, Default)]
pub struct SessionFindState {
    /// Panes of the most recent search (what a bare `:find-<act>` acts on).
    pub(super) last: Vec<PaneId>,
    /// Panes marked by `select`.
    pub(super) selected: BTreeSet<PaneId>,
    pub(super) flash: BTreeMap<PaneId, Instant>,
    pub(super) host_bell: bool,
}

impl crate::wm::WindowManager {
    /// Whether `id` is inside a flash highlight right now.
    pub fn find_is_flashing(&self, id: PaneId) -> bool {
        self.session_find.flash.get(&id).is_some_and(|until| Instant::now() < *until)
    }

    /// Whether `id` was marked by `:find-select`.
    pub fn find_is_selected(&self, id: PaneId) -> bool {
        self.session_find.selected.contains(&id)
    }

    /// Panes marked by `:find-select`, ascending.
    pub fn find_selection(&self) -> Vec<PaneId> {
        self.session_find.selected.iter().copied().collect()
    }

    /// Drops expired flashes. `true` while a flash runs or just ended (the caller redraws).
    pub fn find_flash_tick(&mut self) -> bool {
        if self.session_find.flash.is_empty() {
            return false;
        }
        let now = Instant::now();
        self.session_find.flash.retain(|_, until| now < *until);
        true
    }

    /// Takes the pending "ring the host terminal bell" request (the app writes BEL).
    pub fn take_host_bell(&mut self) -> bool {
        std::mem::take(&mut self.session_find.host_bell)
    }
}
