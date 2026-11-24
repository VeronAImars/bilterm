//! Built-in text, markdown, and shared-artifact editor model (`EDIT-01..07`).
//!
//! [`EditorState`] is pane-independent: the window manager owns the pane, feeds it keys and
//! paints [`EditorState::render`] into the pane's terminal grid, so the editor is an ordinary
//! window (tile, float, move, close, session membership) on the TUI and the GUI alike.
//! Plain UTF-8 text and markdown only; see [`io`] for the file-safety rules.

pub mod artifact;
pub mod buffer;
pub mod highlight;
pub mod io;
mod keys;
pub mod menu;
mod menu_render;
pub mod op;
mod render;

use std::path::{Path, PathBuf};

pub use artifact::{DEFAULT_ARTIFACT, artifact_bus_key};
use buffer::{Pos, TextBuffer};
pub use io::SaveError;
pub use menu::{MenuCmd, MenuState};
pub use op::{EditorOp, MenuOp};

/// What the buffer is backed by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorKind {
    /// A user file (possibly not created yet).
    File,
    /// The scratchpad `name`, autosaved to a private file until promoted with save-as.
    Scratch(String),
    /// A shared cross-pane artifact backed by `ContextBus` (`artifact:<name>`) + private autosave.
    Artifact(String),
}

/// Pending modal input line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Edit,
    Find(String),
    SaveAs(String),
    Goto(String),
    /// `y` overwrites `path` (save-as target or a file changed on disk), anything else cancels.
    ConfirmOverwrite(PathBuf),
}

impl Mode {
    /// The text being typed in a prompt mode.
    pub(crate) fn input_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Find(s) | Self::SaveAs(s) | Self::Goto(s) => Some(s),
            _ => None,
        }
    }
}

pub struct EditorState {
    pub buf: TextBuffer,
    pub kind: EditorKind,
    pub path: Option<PathBuf>,
    pub wrap: bool,
    pub markdown: bool,
    /// `true` when showing the Rendered Markdown / Interactive Checklist reading view.
    pub render_mode: bool,
    /// `true` when disk or an agent updated the backing source while this buffer had unsaved edits.
    pub external_change: bool,
    /// Keys go to the editor (`Ctrl+]` hands them back to the pane prompt).
    pub focused: bool,
    pub mode: Mode,
    /// Menu bar state (`F10`, `Alt+letter`).
    pub menu: MenuState,
    pub message: Option<String>,
    pub(crate) search: String,
    /// Base for relative save-as paths (the pane's working directory).
    pub cwd: PathBuf,
    crlf: bool,
    trailing_newline: bool,
    stamp: Option<io::DiskStamp>,
    scratch_file: Option<PathBuf>,
    autosaved_rev: u64,
    last_autosave: Option<std::time::Instant>,
    /// Last `ContextBus` revision observed or written by an `EditorKind::Artifact` buffer.
    pub(crate) bus_rev: u64,
    /// Buffer revision last pushed to `ContextBus`.
    pub(crate) bus_pushed_rev: u64,
    last_disk_check: Option<std::time::Instant>,
    /// Row-to-source-line map from the latest rendered view pass (used for click-to-toggle).
    pub(crate) rendered_rows: Vec<(usize, bool)>,
    top: usize,
    left: usize,
    view: (u16, u16),
    pub(crate) close_armed: bool,
}

fn is_markdown_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd"))
}

impl EditorState {
    fn blank(kind: EditorKind, text: &str) -> Self {
        Self {
            buf: TextBuffer::from_text(text),
            kind,
            path: None,
            wrap: true,
            markdown: true,
            render_mode: false,
            external_change: false,
            focused: true,
            mode: Mode::Edit,
            menu: MenuState::default(),
            message: None,
            search: String::new(),
            cwd: PathBuf::from("."),
            crlf: false,
            trailing_newline: true,
            stamp: None,
            scratch_file: None,
            autosaved_rev: 0,
            last_autosave: None,
            bus_rev: 0,
            bus_pushed_rev: 0,
            last_disk_check: None,
            rendered_rows: Vec::new(),
            top: 0,
            left: 0,
            view: (24, 80),
            close_armed: false,
        }
    }

    /// An empty unnamed file buffer (`:edit` with no path).
    pub fn new_file() -> Self {
        let mut s = Self::blank(EditorKind::File, "");
        s.markdown = true;
        s
    }

    /// Opens `path` (absolute). A missing file opens as an empty new buffer bound to the path.
    pub fn open_file(path: &Path) -> Result<Self, String> {
        let mut s = Self::blank(EditorKind::File, "");
        match io::load(path)? {
            Some(l) => {
                s.buf = TextBuffer::from_text(&l.text);
                s.crlf = l.crlf;
                s.trailing_newline = l.trailing_newline;
                s.stamp = l.stamp;
            }
            None => s.message = Some(format!("new file {}", path.display())),
        }
        s.markdown = is_markdown_path(path);
        s.path = Some(path.to_path_buf());
        Ok(s)
    }

    /// Opens the scratchpad `name` backed by `file`, restoring its content when it exists.
    pub fn open_scratch(name: &str, file: PathBuf) -> Self {
        let text = io::load(&file).ok().flatten().map(|l| l.text).unwrap_or_default();
        let mut s = Self::blank(EditorKind::Scratch(name.to_string()), &text);
        s.scratch_file = Some(file);
        s
    }

    /// Opens a shared cross-pane artifact backed by `ContextBus` + private autosave `file`.
    pub fn open_artifact(name: &str, file: PathBuf, bus_text: Option<(String, u64)>) -> Self {
        let (text, bus_rev) = match bus_text {
            Some((t, rev)) => (t, rev),
            None => {
                let loaded = io::load(&file).ok().flatten().map(|l| l.text);
                (loaded.unwrap_or_else(|| artifact::artifact_template(name)), 0)
            }
        };
        let mut s = Self::blank(EditorKind::Artifact(name.to_string()), &text);
        s.scratch_file = Some(file);
        s.bus_rev = bus_rev;
        s.bus_pushed_rev = s.buf.revision();
        s
    }

    pub fn is_scratch(&self) -> bool {
        matches!(self.kind, EditorKind::Scratch(_))
    }

    pub fn is_artifact(&self) -> bool {
        matches!(self.kind, EditorKind::Artifact(_))
    }

    pub fn artifact_name(&self) -> Option<&str> {
        match &self.kind {
            EditorKind::Artifact(n) => Some(n.as_str()),
            _ => None,
        }
    }

    pub fn artifact_bus_key(&self) -> Option<String> {
        self.artifact_name().map(artifact_bus_key)
    }

    /// File name, `scratch`, `artifact:<name>` or `[new]` for display.
    pub fn display_name(&self) -> String {
        match (&self.kind, &self.path) {
            (EditorKind::Scratch(n), _) => format!("scratch:{n}"),
            (EditorKind::Artifact(n), _) => format!("artifact:{n}"),
            (_, Some(p)) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
            _ => "[new]".into(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        !self.is_scratch() && !self.is_artifact() && self.buf.is_dirty()
    }

    /// Window title with kind prefix and dirty/external indicator.
    pub fn title(&self) -> String {
        match &self.kind {
            EditorKind::Artifact(n) => format!("ARTIFACT {n}"),
            _ => format!(
                "EDIT {}{}{}",
                self.display_name(),
                if self.is_dirty() { " *" } else { "" },
                if self.external_change { " !" } else { "" }
            ),
        }
    }

    fn disk_text(&self) -> String {
        let mut t = self.buf.text();
        if self.trailing_newline && !t.is_empty() {
            t.push('\n');
        }
        if self.crlf { t.replace('\n', "\r\n") } else { t }
    }

    /// Saves to the bound path. A scratchpad or artifact has none: use [`Self::save_as`].
    pub fn save(&mut self, force: bool) -> Result<String, SaveError> {
        let Some(path) = self.path.clone() else {
            return Err(SaveError::Failed("no file name: use save-as (:editor saveas <path>)".into()));
        };
        let stamp = io::save(&path, &self.disk_text(), false, self.stamp, force)?;
        self.stamp = Some(stamp);
        self.external_change = false;
        self.buf.mark_saved();
        Ok(format!("saved {}", path.display()))
    }

    /// Saves under `path`; an existing target needs `force`. A scratchpad becomes a file buffer.
    pub fn save_as(&mut self, path: &Path, force: bool) -> Result<String, SaveError> {
        let same = self.path.as_deref() == Some(path);
        let stamp = io::save(path, &self.disk_text(), !same, if same { self.stamp } else { None }, force)?;
        self.stamp = Some(stamp);
        self.external_change = false;
        self.buf.mark_saved();
        if matches!(self.kind, EditorKind::Scratch(_)) {
            self.kind = EditorKind::File;
            if let Some(f) = self.scratch_file.take() {
                let _ = std::fs::remove_file(f);
            }
            self.markdown = is_markdown_path(path);
            self.path = Some(path.to_path_buf());
        } else if !self.is_artifact() {
            self.markdown = is_markdown_path(path);
            self.path = Some(path.to_path_buf());
        }
        Ok(format!("saved {}", path.display()))
    }

    /// Scratchpad content changed since the last write.
    pub fn autosave_pending(&self) -> bool {
        self.scratch_file.is_some() && self.buf.revision() != self.autosaved_rev
    }

    /// Pending and the last write is at least a second old (debounce for typing bursts).
    pub fn autosave_due(&self) -> bool {
        self.autosave_pending() && self.last_autosave.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1))
    }

    /// Writes the scratchpad content (0600, atomic) when it changed.
    pub fn autosave(&mut self) -> std::io::Result<()> {
        let Some(f) = self.scratch_file.clone() else { return Ok(()) };
        if let Some(dir) = f.parent() {
            io::ensure_scratch_dir(dir)?;
        }
        crate::secure_fs::write_private_file(&f, self.buf.text().as_bytes())?;
        self.autosaved_rev = self.buf.revision();
        self.last_autosave = Some(std::time::Instant::now());
        Ok(())
    }

    /// Reloads the file, discarding edits.
    pub fn revert(&mut self) -> Result<String, String> {
        let Some(path) = self.path.clone() else { return Err("nothing to revert".into()) };
        let l = io::load(&path)?.ok_or("file no longer exists")?;
        self.buf = TextBuffer::from_text(&l.text);
        self.crlf = l.crlf;
        self.trailing_newline = l.trailing_newline;
        self.stamp = l.stamp;
        self.external_change = false;
        Ok(format!("reverted {}", path.display()))
    }

    /// Polls disk for external changes on file-backed buffers (~500ms interval, or immediately when `force_now`).
    /// Auto-reloads clean buffers so agent writes appear live; flags dirty buffers without clobbering.
    pub fn poll_external_file_change(&mut self, force_now: bool) -> bool {
        if !matches!(self.kind, EditorKind::File) {
            return false;
        }
        let Some(path) = self.path.clone() else { return false };
        if !force_now && self.last_disk_check.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(500)) {
            return false;
        }
        self.last_disk_check = Some(std::time::Instant::now());
        let Some(disk_stamp) = io::stat_stamp(&path) else { return false };
        if self.stamp == Some(disk_stamp) {
            return false;
        }
        if !self.is_dirty() {
            if let Ok(Some(l)) = io::load(&path) {
                let prev_cursor = self.buf.cursor;
                let prev_top = self.top;
                self.buf = TextBuffer::from_text(&l.text);
                self.buf.cursor = self.buf.clamp(prev_cursor);
                self.top = prev_top.min(self.buf.line_count().saturating_sub(1));
                self.crlf = l.crlf;
                self.trailing_newline = l.trailing_newline;
                self.stamp = l.stamp;
                self.external_change = false;
                self.message = Some("reloaded from disk".into());
                return true;
            }
            return false;
        }
        if !self.external_change {
            self.external_change = true;
            self.message = Some("changed on disk — :editor revert to reload, Ctrl+S to overwrite".into());
            return true;
        }
        false
    }

    /// Applies an external `ContextBus` update to an `EditorKind::Artifact` buffer.
    pub fn sync_from_bus(&mut self, text: &str, rev: u64) -> bool {
        if !self.is_artifact() || rev <= self.bus_rev {
            return false;
        }
        let norm = text.replace("\r\n", "\n").replace('\r', "\n");
        self.bus_rev = rev;
        if self.buf.text() == norm {
            return false;
        }
        let prev_cursor = self.buf.cursor;
        let prev_top = self.top;
        self.buf = TextBuffer::from_text(&norm);
        self.buf.cursor = self.buf.clamp(prev_cursor);
        self.top = prev_top.min(self.buf.line_count().saturating_sub(1));
        self.bus_pushed_rev = self.buf.revision();
        // TextBuffer::from_text resets revision to 0; invalidate autosaved_rev so the synced
        // bus content is persisted on the next autosave tick.
        self.autosaved_rev = u64::MAX;
        self.external_change = false;
        self.message = Some(format!("synced from shared bus (rev {rev})"));
        true
    }

    /// Toggles the Markdown task checkbox (`- [ ]` <-> `- [x]`) on the current line.
    pub fn toggle_check(&mut self) -> Result<bool, String> {
        let line = self.buf.cursor.line;
        match artifact::toggle_checkbox_on_line(&mut self.buf, line) {
            Some(checked) => {
                let (done, total) = artifact::checklist_progress(&self.buf);
                self.message = Some(format!(
                    "{} ({done}/{total} done)",
                    if checked { "checked" } else { "unchecked" }
                ));
                Ok(checked)
            }
            None => Err("no Markdown checkbox (- [ ] / - [x]) on current line".into()),
        }
    }

    /// Toggles or sets Rendered Markdown / Interactive Checklist reading view.
    pub fn set_render_mode(&mut self, on: Option<bool>) -> bool {
        self.render_mode = on.unwrap_or(!self.render_mode);
        self.left = 0;
        self.render_mode
    }

    /// Safe artifact name (`validate_safe_name`-compliant) derived from this buffer's name.
    pub fn studio_artifact_name(&self) -> String {
        if let Some(n) = self.artifact_name() {
            return n.to_string();
        }
        let slug = artifact::slugify_artifact_name(&self.display_name());
        if slug.is_empty() { DEFAULT_ARTIFACT.to_string() } else { slug }
    }

    pub fn set_wrap(&mut self, on: Option<bool>) -> bool {
        self.wrap = on.unwrap_or(!self.wrap);
        self.left = 0;
        self.wrap
    }

    /// Moves to 1-based `line`.
    pub fn goto_line(&mut self, line: usize) {
        self.buf.anchor = None;
        self.buf.cursor = self.buf.clamp(Pos { line: line.saturating_sub(1), col: 0 });
    }

    /// Selects the next/previous match of `query` (remembered for repeat).
    pub fn find(&mut self, query: &str, forward: bool) -> bool {
        if !query.is_empty() {
            self.search = query.to_string();
        }
        let from = match (forward, self.buf.selection()) {
            (true, Some((_, e))) => e,
            (false, Some((s, _))) => s,
            _ => self.buf.cursor,
        };
        match self.buf.find(&self.search, from, forward) {
            Some((s, e)) => {
                self.buf.anchor = Some(s);
                self.buf.cursor = e;
                true
            }
            None => false,
        }
    }

    pub fn status_json(&self) -> String {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let kind = match &self.kind {
            EditorKind::File => "file",
            EditorKind::Scratch(_) => "scratch",
            EditorKind::Artifact(_) => "artifact",
        };
        let (tasks_done, tasks_total) = artifact::checklist_progress(&self.buf);
        let bus_key = self.artifact_bus_key().map_or("null".into(), |k| format!("\"{}\"", esc(&k)));
        format!(
            "{{\"kind\":\"{kind}\",\"name\":\"{}\",\"path\":{},\"dirty\":{},\"lines\":{},\"line\":{},\"col\":{},\"wrap\":{},\"markdown\":{},\"render_mode\":{},\"external_change\":{},\"tasks_done\":{tasks_done},\"tasks_total\":{tasks_total},\"bus_key\":{bus_key},\"focused\":{},\"menu\":{}}}",
            esc(&self.display_name()),
            self.path.as_ref().map_or("null".into(), |p| format!("\"{}\"", esc(&p.to_string_lossy()))),
            self.is_dirty(),
            self.buf.line_count(),
            self.buf.cursor.line + 1,
            self.buf.cursor.col + 1,
            self.wrap,
            self.markdown,
            self.render_mode,
            self.external_change,
            self.focused,
            self.menu.open.map_or("null".into(), |i| format!("\"{}\"", menu::MENUS[i].title.to_ascii_lowercase()))
        )
    }
}

#[cfg(test)]
mod tests;
