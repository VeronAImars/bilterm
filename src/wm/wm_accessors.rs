//! `WindowManager` API-origin gate, shared-registry accessors and workspace-recording helpers
//! (moved out of `wm/mod.rs` to keep it under the ARCH-R04 line limit; no behaviour change).

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;

use super::{agent_mail, context_bus, prompt_queue, swarm_surface, WindowManager};
use crate::action::{Action, KeysOp};

impl WindowManager {
    /// Marks subsequent actions as API-originated (used while `pane.send_keys` drives the prompt).
    pub fn set_api_origin(&mut self, on: bool) {
        self.api_origin = on;
    }

    /// Marks actions as coming from an attach client's `Act` (`attach.kick` refuses these).
    pub fn set_attach_origin(&mut self, on: bool) {
        self.attach_origin = on;
    }

    pub fn is_attach_origin(&self) -> bool {
        self.attach_origin
    }

    /// Stores the structured JSON payload of the last query command.
    pub fn set_command_data(&mut self, s: String) {
        self.command_data = Some(s);
    }

    /// Takes the structured JSON payload of the last query command, if any.
    pub fn take_command_data(&mut self) -> Option<String> {
        self.command_data.take()
    }

    /// Why `action` must be refused for an API-originated caller (config write gate, `[api]`).
    /// Returns `None` when the action is allowed. Local (non-API) callers are never gated.
    pub fn config_write_denied(&self, action: &Action) -> Option<String> {
        // Phase 63: key-profile switches and keyboard-mode changes are mutations too (`keys.profile`,
        // `keys.window_mode`, `menu.open` RPC and the same colon forms over `wm.command`).
        if matches!(action, Action::Keys(KeysOp::Profile(Some(_))) | Action::WindowMode(_) | Action::MenuOpen { .. }) {
            return (!self.config.api.allow_config_write)
                .then(|| "key and mode changes over the API are disabled; set [api] allow_config_write = true in config.toml".to_string());
        }
        // DOS lifecycle verbs that download, delete or overwrite images are mutations too.
        let dos_mutates = match action {
            Action::DosSetup { .. } | Action::DosRemove { .. } | Action::DosReset { .. } | Action::DosRestore { .. } => true,
            Action::DosSnapshot { args } => args.first().map(String::as_str) != Some("list"),
            _ => false,
        };
        if dos_mutates {
            return (!self.config.api.allow_config_write)
                .then(|| "DOS setup/remove/reset/snapshot/restore over the API are disabled; set [api] allow_config_write = true in config.toml".to_string());
        }
        let writes = matches!(action, Action::ConfigSet(..) | Action::ConfigSave | Action::AppearanceSave);
        if !writes {
            return None;
        }
        if let Action::ConfigSet(key, _) = action
            && (key.trim() == "api" || key.trim().starts_with("api."))
        {
            return Some("the [api] section cannot be changed over the API".to_string());
        }
        if !self.config.api.allow_config_write {
            return Some("config writes over the API are disabled; set [api] allow_config_write = true in config.toml".to_string());
        }
        None
    }

    /// `true` while an API-originated action is executing.
    pub fn is_api_origin(&self) -> bool {
        self.api_origin
    }

    /// Directory the running automation action is confined to, if any.
    pub fn automation_confine_dir(&self) -> Option<&Path> {
        self.automation_confine_dir.as_deref()
    }

    /// Records the first error of the running automation action (surfaced by [`Self::apply_automation`]).
    pub fn record_automation_error(&mut self, err: String) {
        self.automation_error.get_or_insert(err);
    }

    /// Swarm / Mission Control HUD / replay engine state (UNI-R01, Phases 69/70).
    pub fn swarm_surface(&self) -> &swarm_surface::SwarmSurface {
        &self.swarm_surface
    }

    /// Mutable swarm / HUD / replay engine state.
    pub fn swarm_surface_mut(&mut self) -> &mut swarm_surface::SwarmSurface {
        &mut self.swarm_surface
    }

    /// Context bus handle (cheap clone shares the store).
    pub fn context_bus(&self) -> &context_bus::ContextBus {
        &self.context_bus
    }

    /// Context bus handle (interior-mutable; same store as [`Self::context_bus`]).
    pub fn context_bus_mut(&mut self) -> &mut context_bus::ContextBus {
        &mut self.context_bus
    }

    /// Per-pane prompt queues.
    pub fn prompt_queue(&self) -> &prompt_queue::PromptQueue {
        &self.prompt_queue
    }

    /// Mutable per-pane prompt queues.
    pub fn prompt_queue_mut(&mut self) -> &mut prompt_queue::PromptQueue {
        &mut self.prompt_queue
    }

    /// Agent-to-agent messaging registry (`AGT-01..03`, Phase 52).
    pub fn agent_mail(&self) -> &agent_mail::AgentMailRegistry {
        &self.agent_mail
    }

    /// Mutable agent-to-agent messaging registry (`AGT-01..03`, Phase 52).
    pub fn agent_mail_mut(&mut self) -> &mut agent_mail::AgentMailRegistry {
        &mut self.agent_mail
    }

    /// Sets whether agent messaging is permitted (`[api] allow_agent_messaging` / `:agent enable`).
    pub fn set_agent_messaging_enabled(&mut self, enabled: bool) {
        self.agent_mail.set_enabled(enabled);
        self.config.api.allow_agent_messaging = enabled;
    }

    /// Returns `true` if agent messaging is currently permitted.
    pub fn is_agent_messaging_enabled(&self) -> bool {
        self.agent_mail.is_enabled()
    }

    /// Returns `true` if whole-workspace `.cast` compositor frame recording is active.
    pub fn is_workspace_recording(&self) -> bool {
        self.workspace_recorder
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// Returns `true` if an active whole-workspace `.cast` recorder has captured at least one rendered frame.
    pub fn has_recorded_workspace_frames(&self) -> bool {
        self.workspace_recorder
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| r.frame_count() > 0))
            .unwrap_or(false)
    }

    /// Captures a rendered workspace `Buffer` frame into the active `WorkspaceRecorder` (`REC-02`).
    pub fn record_workspace_frame(&self, buf: &Buffer) {
        if let Ok(mut guard) = self.workspace_recorder.lock()
            && let Some(recorder) = guard.as_mut()
        {
            recorder.capture_buffer(buf);
        }
    }

    /// Stops whole-workspace `.cast` recording and writes the `.cast` file to disk (`REC-02`).
    pub fn stop_workspace_recording(&mut self) -> std::io::Result<Option<PathBuf>> {
        let recorder_opt = self
            .workspace_recorder
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        match recorder_opt {
            Some(rec) => rec.stop().map(Some),
            None => Ok(None),
        }
    }
}
