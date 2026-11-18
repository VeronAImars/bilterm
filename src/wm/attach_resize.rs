//! Host-side resize driven by the attach size owner (`Action::AttachResize`, plan T-A3).

use super::WindowManager;
use crate::attach::limits::validate_dims;
use ratatui::layout::Rect;

impl WindowManager {
    /// Resizes the whole workspace (and so every pane VT/PTY) to `cols`x`rows`.
    /// Rejects zero or over-cap dimensions (`500x200`) without touching any state.
    pub(crate) fn attach_resize(&mut self, cols: u16, rows: u16) {
        if let Err(e) = validate_dims(cols, rows) {
            self.active_pane_mut().set_status_message(Some(format!("Attach resize rejected: {e}")));
            return;
        }
        if let Err(e) = self.resize_all(Rect::new(0, 0, cols, rows)) {
            self.active_pane_mut().set_status_message(Some(format!("Attach resize error: {e}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Action;

    fn sizes(wm: &WindowManager) -> Vec<(u16, u16)> {
        wm.host_state("t").panes.iter().map(|p| (p.rows, p.cols)).collect()
    }

    #[test]
    fn attach_resize_resizes_panes_via_apply() {
        let mut wm = WindowManager::new_headless(24, 80);
        wm.apply(Action::AttachResize { cols: 120, rows: 40 }).unwrap();
        let s = sizes(&wm);
        assert_eq!(s.len(), 1);
        assert!(s[0].1 > 80 && s[0].0 > 24, "{s:?}");
        wm.apply(Action::AttachResize { cols: 60, rows: 20 }).unwrap();
        let s = sizes(&wm);
        assert!(s[0].1 <= 60 && s[0].0 <= 20, "{s:?}");
    }

    #[test]
    fn attach_resize_covers_all_panes() {
        let mut wm = WindowManager::new_headless(24, 80);
        wm.apply(Action::SplitVertical).unwrap();
        wm.apply(Action::AttachResize { cols: 100, rows: 30 }).unwrap();
        let s = sizes(&wm);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|&(r, c)| r <= 30 && c <= 100 && r > 0 && c > 0), "{s:?}");
    }

    #[test]
    fn attach_resize_rejects_bad_dims_without_state_change() {
        let mut wm = WindowManager::new_headless(24, 80);
        let before = sizes(&wm);
        for (c, r) in [(0, 10), (10, 0), (501, 10), (10, 201), (u16::MAX, u16::MAX)] {
            wm.apply(Action::AttachResize { cols: c, rows: r }).unwrap();
            assert_eq!(sizes(&wm), before, "{c}x{r}");
        }
    }

    #[test]
    fn size_owner_decision_drives_apply() {
        use crate::attach::size_owner::{Role, SizeOwner};
        use std::time::Duration;
        let mut wm = WindowManager::new_headless(24, 80);
        let mut so = SizeOwner::default();
        let o = so.register(1, Role::Rw, 110, 35, Duration::ZERO).unwrap();
        let (cols, rows) = o.apply.unwrap();
        wm.apply(Action::AttachResize { cols, rows }).unwrap();
        assert!(sizes(&wm)[0].1 > 80);
    }
}
