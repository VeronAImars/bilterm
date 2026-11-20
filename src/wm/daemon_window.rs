//! Floating `DAEMON SUPERVISOR` manager window (TUI + GUI).
//!
//! CONTRACT (frozen by the orchestrator; agents fill bodies, do not rename or change signatures):
//! state lives in `WindowManager.daemon_window` (private field), UI reads it via `wm.daemon_window()`
//! and `wm.daemon_window_rect(ws)`; input routes through `wm.daemon_window_key(key)` and
//! `wm.daemon_window_click(ws, col, row)`.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;

use crate::action::{Action, DaemonSupervisorOp};
use crate::wm::WindowManager;

/// One row of the floating daemons window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonMenuEntry {
    pub name: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub sessions: usize,
    pub panes: usize,
    pub socket_path: String,
    /// True when this is the daemon the client is currently attached to / switched to.
    pub current: bool,
    /// Resident bytes (None = unknown).
    pub rss_bytes: Option<u64>,
    /// Supervisor restarts since the host was first started.
    pub restarts: u32,
    /// `Some("restarting")` / `Some("failed")` when the supervisor reports the host down.
    pub supervisor_state: Option<&'static str>,
}

impl DaemonMenuEntry {
    /// A daemon that is not running but still has a recorded PID died without cleaning up.
    pub fn is_crashed(&self) -> bool {
        !self.running && self.pid.is_some()
    }
}

/// Buttons in the action footer, in left-to-right render order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonWindowButton {
    Start,
    New,
    Stop,
    Switch,
    Prune,
    Refresh,
    Close,
}

impl DaemonWindowButton {
    pub const ALL: [DaemonWindowButton; 7] = [
        Self::Start,
        Self::New,
        Self::Stop,
        Self::Switch,
        Self::Prune,
        Self::Refresh,
        Self::Close,
    ];

    /// Footer label; UI and hit-testing must both use this so they cannot drift.
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "[s] Start",
            Self::New => "[n] New",
            Self::Stop => "[x] Stop",
            Self::Switch => "[w] Switch",
            Self::Prune => "[p] Prune",
            Self::Refresh => "[r] Refresh",
            Self::Close => "[Esc] Close",
        }
    }
}

/// Label of the host-PTYs checkbox on the prompt row (state glyph is substituted at index 1).
pub const PROMPT_TOGGLE_LABEL: &str = "[ ] host PTYs (Tab)";

/// Inline "new daemon" entry shown on the status row (`[n] New`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonNewPrompt {
    /// Text typed so far; empty means "pick the next free name".
    pub name: String,
    /// The new host owns real PTYs (`--host-ptys`).
    pub host_ptys: bool,
    /// Last validation / refusal message, shown instead of the hint.
    pub error: Option<String>,
}

/// Mouse hit-test result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonWindowHit {
    Outside,
    Frame,
    CloseBadge,
    /// The `[ ] host PTYs` checkbox on the New-daemon prompt row.
    PromptHostPtys,
    Row(usize),
    Button(DaemonWindowButton),
}

/// Layout shared by renderer and hit-testing (single source of truth).
/// Inside `outer`: row 0 = title bar (with `[X]` at the right edge), row 1 = column header,
/// rows 2..h-3 = daemon rows, row h-3 = status banner, row h-2 = button footer, row h-1 = bottom border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaemonWindowLayout {
    pub outer: Rect,
    pub header_y: u16,
    pub rows: Rect,
    pub status_y: u16,
    pub footer_y: u16,
    /// Absolute x of the `[X]` close badge (3 cells wide) on the title row.
    pub close_x: u16,
}

impl DaemonWindowLayout {
    pub fn new(outer: Rect) -> Self {
        let h = outer.height;
        let rows_y = outer.y + 2;
        let status_y = outer.y + h.saturating_sub(3);
        Self {
            outer,
            header_y: outer.y + 1,
            rows: Rect::new(outer.x + 1, rows_y, outer.width.saturating_sub(2), status_y.saturating_sub(rows_y)),
            status_y,
            footer_y: outer.y + h.saturating_sub(2),
            close_x: outer.x + outer.width.saturating_sub(4),
        }
    }

    /// `(x_start, width)` of the host-PTYs checkbox on the New-daemon prompt row (right-aligned).
    pub fn prompt_toggle_span(&self) -> (u16, u16) {
        let w = PROMPT_TOGGLE_LABEL.chars().count() as u16;
        (self.outer.x + self.outer.width.saturating_sub(w + 2), w)
    }

    /// `(button, x_start, width)` for each footer button, separated by one space, starting at `outer.x + 2`.
    pub fn button_spans(&self) -> Vec<(DaemonWindowButton, u16, u16)> {
        let mut x = self.outer.x + 2;
        let mut out = Vec::new();
        for b in DaemonWindowButton::ALL {
            let w = b.label().chars().count() as u16;
            out.push((b, x, w));
            x += w + 1;
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonWindowState {
    pub active: bool,
    pub focused: bool,
    pub selected: usize,
    pub scroll: usize,
    pub entries: Vec<DaemonMenuEntry>,
    pub status_line: String,
    /// Inline new-daemon name entry (`[n] New`); captures all keys while `Some`.
    pub new_prompt: Option<DaemonNewPrompt>,
    /// Last rendered outer rect (set by the renderer/rect helper; informational).
    pub rect: Option<Rect>,
}

impl DaemonWindowState {
    /// True when key input should be routed to the window.
    pub fn captures_input(&self) -> bool {
        self.active && self.focused
    }

    pub fn selected_entry(&self) -> Option<&DaemonMenuEntry> {
        self.entries.get(self.selected)
    }
}

impl WindowManager {
    pub fn daemon_window(&self) -> &DaemonWindowState {
        &self.daemon_window
    }

    pub fn daemon_window_mut(&mut self) -> &mut DaemonWindowState {
        &mut self.daemon_window
    }

    /// Centered floating rect (about 76x16, clamped to the workspace); `None` when inactive or ws too small.
    pub fn daemon_window_rect(&self, ws: Rect) -> Option<Rect> {
        if !self.daemon_window.active || ws.width < 40 || ws.height < 10 {
            return None;
        }
        let w = 80.min(ws.width.saturating_sub(2));
        let h = 16.min(ws.height.saturating_sub(2));
        Some(Rect::new(
            ws.x + ws.width.saturating_sub(w) / 2,
            ws.y + ws.height.saturating_sub(h) / 2,
            w,
            h,
        ))
    }
}

fn crashed_entry(r: crate::daemon::supervisor::DaemonLockRecord) -> DaemonMenuEntry {
    DaemonMenuEntry {
        name: r.name,
        running: false,
        pid: Some(r.pid),
        socket_path: r.socket_path.to_string_lossy().into_owned(),
        ..Default::default()
    }
}

fn default_entry(runtime_dir: &Path) -> DaemonMenuEntry {
    use crate::daemon::supervisor::{daemon_liveness, DaemonLiveness};
    match daemon_liveness(runtime_dir, "default") {
        DaemonLiveness::Running(r) => DaemonMenuEntry {
            name: "default".into(),
            running: true,
            pid: Some(r.pid),
            socket_path: r.socket_path.to_string_lossy().into_owned(),
            ..Default::default()
        },
        DaemonLiveness::Crashed(r) => crashed_entry(r),
        DaemonLiveness::Stopped => DaemonMenuEntry {
            name: "default".into(),
            socket_path: runtime_dir.join("daemon.sock").to_string_lossy().into_owned(),
            ..Default::default()
        },
    }
}

impl WindowManager {
    pub fn open_daemon_window(&mut self) {
        self.daemon_window.active = true;
        self.daemon_window.focused = true;
        self.refresh_daemon_window();
    }

    /// Opens the window and refreshes it from an explicit runtime dir.
    pub fn open_daemon_window_from(&mut self, runtime_dir: &Path) {
        self.daemon_window.active = true;
        self.daemon_window.focused = true;
        self.refresh_daemon_window_from(runtime_dir);
    }

    pub fn close_daemon_window(&mut self) {
        self.daemon_window.new_prompt = None;
        self.daemon_window.active = false;
        self.daemon_window.focused = false;
    }

    pub fn refresh_daemon_window(&mut self) {
        match crate::daemon::supervisor::resolve_runtime_dir() {
            Ok(dir) => self.refresh_daemon_window_from(&dir),
            Err(e) => self.daemon_window.status_line = format!("runtime dir error: {e}"),
        }
    }

    pub fn refresh_daemon_window_from(&mut self, runtime_dir: &Path) {
        let mut entries: Vec<DaemonMenuEntry> =
            crate::daemon::registry::DaemonRegistry::list_active(runtime_dir)
                .into_iter()
                .map(|d| DaemonMenuEntry {
                    current: d.is_current(),
                    name: d.name,
                    running: true,
                    pid: Some(d.pid),
                    sessions: d.sessions_count,
                    panes: d.panes_count,
                    socket_path: d.socket_path.to_string_lossy().into_owned(),
                    rss_bytes: d.rss_bytes,
                    restarts: d.restart_count,
                    ..Default::default()
                })
                .collect();
        for rec in crate::daemon::registry::DaemonRegistry::list_supervised_down(runtime_dir) {
            if !entries.iter().any(|e| e.name == rec.name) {
                entries.push(DaemonMenuEntry {
                    name: rec.name,
                    restarts: rec.restart_count,
                    supervisor_state: Some(if rec.state == crate::daemon::supervise::SupState::Failed { "failed" } else { "restarting" }),
                    ..Default::default()
                });
            }
        }
        for rec in crate::daemon::registry::DaemonRegistry::list_crashed(runtime_dir) {
            if !entries.iter().any(|e| e.name == rec.name) {
                entries.push(crashed_entry(rec));
            }
        }
        if !entries.iter().any(|e| e.name == "default") {
            entries.push(default_entry(runtime_dir));
        }
        entries.sort_by(|a, b| {
            (a.name != "default", a.name.as_str()).cmp(&(b.name != "default", b.name.as_str()))
        });
        let running = entries.iter().filter(|e| e.running).count();
        let crashed = entries.iter().filter(|e| e.is_crashed()).count();
        let w = &mut self.daemon_window;
        w.status_line = format!(
            "{} daemon{}, {} running{}",
            entries.len(),
            if entries.len() == 1 { "" } else { "s" },
            running,
            if crashed > 0 { format!(", {crashed} crashed") } else { String::new() }
        );
        w.entries = entries;
        w.selected = w.selected.min(w.entries.len().saturating_sub(1));
        w.scroll = w.scroll.min(w.selected);
    }

    fn daemon_visible_rows(&self) -> usize {
        // Same geometry the renderer uses: rows = outer height - 5 (title, header, status, footer, border).
        let (rows, cols) = self.default_size;
        let ws = Rect::new(0, 0, cols.max(40), rows.max(10));
        let h = self.daemon_window_rect(ws).map_or(16, |r| r.height);
        usize::from(h.saturating_sub(5)).max(1)
    }

    fn daemon_select(&mut self, idx: usize) {
        let vis = self.daemon_visible_rows();
        let w = &mut self.daemon_window;
        w.selected = idx.min(w.entries.len().saturating_sub(1));
        if w.selected < w.scroll {
            w.scroll = w.selected;
        } else if w.selected >= w.scroll + vis {
            w.scroll = w.selected + 1 - vis;
        }
    }

    /// Runs a footer/key operation via the Action path, then refreshes.
    fn daemon_window_run(&mut self, button: DaemonWindowButton) {
        if button == DaemonWindowButton::New {
            // First press opens the prompt; pressing again while it is open submits it.
            if self.daemon_window.new_prompt.is_some() {
                self.daemon_new_submit();
            } else {
                self.daemon_new_open();
            }
            return;
        }
        self.daemon_window.new_prompt = None;
        let name = self.daemon_window.selected_entry().map(|e| e.name.clone());
        let op = match (button, name) {
            (DaemonWindowButton::Start, Some(name)) => {
                Some(DaemonSupervisorOp::Start { name: Some(name), restart: None, limits: None, host_ptys: false })
            }
            (DaemonWindowButton::Stop, Some(name)) => {
                Some(DaemonSupervisorOp::Stop { name: Some(name), force: false })
            }
            (DaemonWindowButton::Switch, Some(name)) => Some(DaemonSupervisorOp::Switch { name }),
            (DaemonWindowButton::Prune, _) => Some(DaemonSupervisorOp::Prune),
            (DaemonWindowButton::Close, _) => {
                self.close_daemon_window();
                return;
            }
            _ => None,
        };
        if let Some(op) = op {
            let _ = self.apply(Action::Daemon(op));
        }
        if self.daemon_window.active {
            self.refresh_daemon_window();
        }
    }

    /// Opens the inline name prompt, pre-filled with the next free auto name.
    fn daemon_new_open(&mut self) {
        let suggestion = crate::daemon::supervisor::resolve_runtime_dir()
            .map_or_else(|_| "daemon-1".to_string(), |d| crate::daemon::create::next_free_name(&d));
        self.daemon_window.new_prompt = Some(DaemonNewPrompt { name: suggestion, host_ptys: false, error: None });
    }

    /// Validates the prompt and runs `Action::Daemon(New)`; keeps the prompt open on a refusal.
    fn daemon_new_submit(&mut self) {
        let Some(p) = self.daemon_window.new_prompt.clone() else { return };
        let name = p.name.trim().to_string();
        let name_opt = (!name.is_empty()).then_some(name);
        let check = crate::daemon::supervisor::resolve_runtime_dir()
            .map_err(|e| format!("runtime dir error: {e}"))
            .and_then(|d| crate::daemon::create::resolve_new_name(&d, name_opt.as_deref()));
        if let Err(e) = check {
            if let Some(pr) = self.daemon_window.new_prompt.as_mut() {
                pr.error = Some(e);
            }
            return;
        }
        self.daemon_window.new_prompt = None;
        let _ = self.apply(Action::Daemon(DaemonSupervisorOp::New { name: name_opt, restart: None, limits: None, host_ptys: p.host_ptys }));
        if self.daemon_window.active {
            self.refresh_daemon_window();
        }
    }

    /// Key handling while the new-daemon prompt is open.
    fn daemon_new_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.daemon_window.new_prompt = None,
            KeyCode::Enter => self.daemon_new_submit(),
            KeyCode::Tab | KeyCode::BackTab => self.daemon_new_toggle_host_ptys(),
            KeyCode::Backspace => {
                if let Some(p) = self.daemon_window.new_prompt.as_mut() {
                    p.name.pop();
                    p.error = None;
                }
            }
            KeyCode::Char(c)
                if (c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    && !key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                if let Some(p) = self.daemon_window.new_prompt.as_mut()
                    && p.name.chars().count() < 64
                {
                    p.name.push(c);
                    p.error = None;
                }
            }
            _ => {}
        }
    }

    fn daemon_new_toggle_host_ptys(&mut self) {
        if let Some(p) = self.daemon_window.new_prompt.as_mut() {
            p.host_ptys = !p.host_ptys;
        }
    }

    /// Handles a key while the window captures input. Returns true when consumed.
    pub fn daemon_window_key(&mut self, key: KeyEvent) -> bool {
        if self.daemon_window.new_prompt.is_some() {
            self.daemon_new_key(key);
            return true;
        }
        let sel = self.daemon_window.selected;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.daemon_select(sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.daemon_select(sel + 1),
            KeyCode::Char('n') => self.daemon_window_run(DaemonWindowButton::New),
            KeyCode::Enter | KeyCode::Char('s') => self.daemon_window_run(DaemonWindowButton::Start),
            KeyCode::Char('x' | 'q') | KeyCode::Delete => {
                self.daemon_window_run(DaemonWindowButton::Stop)
            }
            KeyCode::Char('w') => self.daemon_window_run(DaemonWindowButton::Switch),
            KeyCode::Char('p') => self.daemon_window_run(DaemonWindowButton::Prune),
            KeyCode::Char('r') => self.refresh_daemon_window(),
            KeyCode::Esc => self.close_daemon_window(),
            _ => {}
        }
        true
    }

    /// Pure hit test (no side effects).
    pub fn daemon_window_hit(&self, ws: Rect, col: u16, row: u16) -> DaemonWindowHit {
        let Some(outer) = self.daemon_window_rect(ws) else {
            return DaemonWindowHit::Outside;
        };
        let inside = col >= outer.x
            && col < outer.x + outer.width
            && row >= outer.y
            && row < outer.y + outer.height;
        if !inside {
            return DaemonWindowHit::Outside;
        }
        let lay = DaemonWindowLayout::new(outer);
        if row == outer.y && col >= lay.close_x && col < lay.close_x + 3 {
            return DaemonWindowHit::CloseBadge;
        }
        if row == lay.status_y && self.daemon_window.new_prompt.is_some() {
            let (x, w) = lay.prompt_toggle_span();
            if col >= x && col < x + w {
                return DaemonWindowHit::PromptHostPtys;
            }
        }
        if row == lay.footer_y {
            for (b, x, w) in lay.button_spans() {
                if col >= x && col < x + w {
                    return DaemonWindowHit::Button(b);
                }
            }
        }
        if row >= lay.rows.y
            && row < lay.rows.y + lay.rows.height
            && col >= lay.rows.x
            && col < lay.rows.x + lay.rows.width
        {
            let idx = self.daemon_window.scroll + usize::from(row - lay.rows.y);
            if idx < self.daemon_window.entries.len() {
                return DaemonWindowHit::Row(idx);
            }
        }
        DaemonWindowHit::Frame
    }

    /// Handles a left-button press at (col,row).
    pub fn daemon_window_click(&mut self, ws: Rect, col: u16, row: u16) -> DaemonWindowHit {
        let hit = self.daemon_window_hit(ws, col, row);
        match hit {
            DaemonWindowHit::Outside => self.daemon_window.focused = false,
            DaemonWindowHit::Frame => self.daemon_window.focused = true,
            DaemonWindowHit::CloseBadge => self.close_daemon_window(),
            DaemonWindowHit::Row(i) => {
                self.daemon_window.focused = true;
                self.daemon_select(i);
            }
            DaemonWindowHit::PromptHostPtys => {
                self.daemon_window.focused = true;
                self.daemon_new_toggle_host_ptys();
            }
            DaemonWindowHit::Button(b) => {
                self.daemon_window.focused = true;
                self.daemon_window_run(b);
            }
        }
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn wm_with_dir() -> (WindowManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut wm = WindowManager::new_headless(100, 40);
        wm.daemon_window_mut().active = true;
        wm.daemon_window_mut().focused = true;
        wm.refresh_daemon_window_from(dir.path());
        (wm, dir)
    }

    #[test]
    fn refresh_lists_default_stopped() {
        let (wm, _d) = wm_with_dir();
        let w = wm.daemon_window();
        assert_eq!(w.entries.len(), 1);
        assert_eq!(w.entries[0].name, "default");
        assert!(!w.entries[0].running);
        assert_eq!(w.entries[0].pid, None);
        assert_eq!(w.status_line, "1 daemon, 0 running");
    }

    #[test]
    fn selection_clamps_and_esc_closes() {
        let (mut wm, _d) = wm_with_dir();
        wm.daemon_window_mut().entries.push(DaemonMenuEntry { name: "b".into(), ..Default::default() });
        assert!(wm.daemon_window_key(key(KeyCode::Char('j'))));
        wm.daemon_window_key(key(KeyCode::Char('j')));
        assert_eq!(wm.daemon_window().selected, 1);
        wm.daemon_window_key(key(KeyCode::Char('k')));
        wm.daemon_window_key(key(KeyCode::Char('k')));
        assert_eq!(wm.daemon_window().selected, 0);
        wm.daemon_window_key(key(KeyCode::Esc));
        assert!(!wm.daemon_window().active);
    }

    #[test]
    fn hit_tests() {
        let (mut wm, _d) = wm_with_dir();
        let ws = Rect::new(0, 0, 100, 40);
        let outer = wm.daemon_window_rect(ws).unwrap();
        let lay = DaemonWindowLayout::new(outer);
        assert_eq!(wm.daemon_window_hit(ws, 0, 0), DaemonWindowHit::Outside);
        assert_eq!(wm.daemon_window_hit(ws, lay.close_x + 1, outer.y), DaemonWindowHit::CloseBadge);
        assert_eq!(wm.daemon_window_hit(ws, lay.rows.x + 1, lay.rows.y), DaemonWindowHit::Row(0));
        for (b, x, w) in lay.button_spans() {
            assert_eq!(wm.daemon_window_hit(ws, x + w - 1, lay.footer_y), DaemonWindowHit::Button(b));
        }
        wm.daemon_window_click(ws, 0, 0);
        assert!(!wm.daemon_window().focused);
        wm.daemon_window_click(ws, lay.close_x, outer.y);
        assert!(!wm.daemon_window().active);
    }
}
