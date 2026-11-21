//! `CompositePane` hooks for the built-in editor window (`EDIT-01`) plus the media-canvas
//! accessors, kept out of `src/pane/mod.rs` (`ARCH-R04`).

use super::{CompositePane, browser_canvas, video_canvas};
use crate::editor::EditorState;

impl CompositePane {
    pub fn browser_canvas(&self) -> Option<&browser_canvas::BrowserCanvasState> {
        self.browser_canvas.as_ref()
    }

    pub fn browser_canvas_mut(&mut self) -> Option<&mut browser_canvas::BrowserCanvasState> {
        self.browser_canvas.as_mut()
    }

    pub fn video_canvas(&self) -> Option<&video_canvas::VideoCanvasState> {
        self.video_canvas.as_ref()
    }

    pub fn video_canvas_mut(&mut self) -> Option<&mut video_canvas::VideoCanvasState> {
        self.video_canvas.as_mut()
    }

    /// The editor shown in this window, if it is an editor window.
    pub fn editor(&self) -> Option<&EditorState> {
        self.editor.as_ref()
    }

    pub fn editor_mut(&mut self) -> Option<&mut EditorState> {
        self.editor.as_mut()
    }

    /// `true` while the editor owns the keyboard (the floating prompt is hidden then).
    pub fn editor_focused(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.focused)
    }

    /// Turns this window into an editor window and paints it.
    pub fn set_editor(&mut self, ed: EditorState) {
        self.editor = Some(ed);
        self.paint_editor();
    }

    /// Repaints the editor into the terminal grid and syncs the window title.
    pub fn paint_editor(&mut self) {
        let Some(mut ed) = self.editor.take() else { return };
        let (rows, cols) = self.vt.size();
        let ansi = ed.render(rows, cols);
        self.vt.process(ansi.as_bytes());
        self.set_title(ed.title());
        self.editor = Some(ed);
    }
}
