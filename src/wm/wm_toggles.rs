//! `WindowManager` modal / menubar / broadcast toggles (moved out of `wm/mod.rs` to keep it under
//! the ARCH-R04 line limit; no behaviour change).

use super::{Modal, WindowManager};

impl WindowManager {
    /// Toggles the `<F1>` interactive Help Modal.
    pub fn toggle_help(&mut self) {
        if matches!(self.modal, Modal::Help { .. }) {
            self.modal = Modal::None;
        } else {
            self.modal = Modal::Help { scroll: 0 };
        }
    }

    /// Toggles the `:version` details window.
    pub fn toggle_version_info(&mut self) {
        self.modal = if matches!(self.modal, Modal::Version { .. }) { Modal::None } else { Modal::Version { scroll: 0 } };
    }

    pub fn is_version_info_visible(&self) -> bool {
        matches!(self.modal, Modal::Version { .. })
    }

    pub fn version_info_scroll(&self) -> usize {
        match self.modal {
            Modal::Version { scroll } => scroll,
            _ => 0,
        }
    }

    /// Toggles the `<F9>` / `:inspector` Live 3270 Terminal Inspector modal (`BEL-01`).
    pub fn toggle_inspector(&mut self) {
        let next = !self.is_inspector_visible();
        self.set_inspector_visible(next);
    }

    /// Toggles the top 1-row Borland Turbo Vision Menu Bar (`:menubar`, `ECO-03`).
    pub fn toggle_menubar(&mut self) {
        self.config.show_top_menubar = !self.config.show_top_menubar;
        if !self.config.show_top_menubar && matches!(self.modal, Modal::Menubar { .. }) {
            self.modal = Modal::None;
        }
        self.reflow_default_viewport();
    }

    /// Sets or toggles Synchronized Multi-Pane Input Broadcast (`:broadcast`, `ECO-04`).
    pub fn set_broadcast_input(&mut self, enabled: bool) {
        self.broadcast_input = enabled;
        self.active_pane_mut().set_status_message(Some(format!(
            "Multi-Pane Input Broadcast: {}",
            if enabled { "ON (📡 BCAST)" } else { "OFF" }
        )));
    }

    /// Toggles the interactive Fuzzy Command Palette modal (`Ctrl+P` / `Alt+Space` / `:palette`, `v0.6.0`).
    pub fn toggle_palette(&mut self) {
        if self.command_window.pinned && self.command_window.active {
            self.command_window.focused = true;
            return;
        }
        if matches!(self.modal, Modal::Palette { .. }) {
            self.modal = Modal::None;
        } else {
            self.modal = Modal::Palette {
                query: String::new(),
                selected: 0,
            };
        }
    }
}
