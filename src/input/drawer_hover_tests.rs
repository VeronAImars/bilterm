//! Mouse-move tests: a pointer move over a drawer row lowers to `DrawerNavOp::Hover`.

use super::*;
use crate::input::AppAction;
use crate::input::mouse_wm::handle_drawer_mouse_event;
use crate::wm::SplitDirection;
use crate::wm::drawer::DrawerStyle;
use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

const AREA: Rect = Rect { x: 0, y: 0, width: 100, height: 30 };

fn moved(col: u16, row: u16) -> MouseEvent {
    MouseEvent { kind: MouseEventKind::Moved, column: col, row, modifiers: KeyModifiers::NONE }
}

/// Hermetic fixture: `session_dir` is pinned to a private, nonexistent temp path so the drawer never
/// lists the owner's real saved sessions from `$XDG_STATE_HOME` / `$HOME`.
/// Removes the fixture's session dir when the test ends.
struct DirGuard(std::path::PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(style: DrawerStyle) -> (WindowManager, DirGuard) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("bilterm_hover_{}_{n}", std::process::id()));
    let mut wm = WindowManager::new_headless(30, 100);
    wm.config_mut().session_dir = Some(dir.to_string_lossy().into_owned());
    wm.config_mut().drawer.style = style;
    let _ = wm.split_focused(SplitDirection::Horizontal);
    let _ = wm.create_new_session(Some("two"), false);
    wm.toggle_drawer();
    (wm, DirGuard(dir))
}

fn cell_of_row(wm: &WindowManager, idx: usize) -> (u16, u16) {
    let dr = wm.drawer_rect(AREA).unwrap();
    let x = dr.x + 2;
    let y = (dr.y..dr.y + dr.height).find(|&y| drawer_row_at(wm, AREA, x, y) == Some(idx)).expect("row visible");
    (x, y)
}

#[test]
fn pointer_move_sets_and_clears_hover_in_both_styles() {
    for style in [DrawerStyle::Navigator, DrawerStyle::Classic] {
        let (mut wm, _guard) = fixture(style);
        let sel = wm.drawer().selected_index;
        let target = (sel + 1) % 3;
        let (x, y) = cell_of_row(&wm, target);
        let r = handle_drawer_mouse_event(&mut wm, moved(x, y), AREA);
        assert!(matches!(r, Some(AppAction::Handled)), "{style:?}: row change asks for a redraw");
        assert_eq!(wm.drawer().hover_row, Some(target), "{style:?}");
        assert_eq!(wm.drawer().selected_index, sel, "hover never moves the keyboard selection");
        // Same row again: nothing changed.
        let _ = handle_drawer_mouse_event(&mut wm, moved(x, y), AREA);
        assert_eq!(wm.drawer().hover_row, Some(target));
        // Off the drawer clears it.
        let _ = handle_drawer_mouse_event(&mut wm, moved(AREA.width - 1, 15), AREA);
        assert_eq!(wm.drawer().hover_row, None, "{style:?}");
    }
}

#[test]
fn pointer_over_each_visible_row_hovers_that_row() {
    let (mut wm, _guard) = fixture(DrawerStyle::Navigator);
    let mut seen = 0;
    for idx in 0..wm.drawer_rows().len() {
        let dr = wm.drawer_rect(AREA).unwrap();
        let x = dr.x + 2;
        let Some(y) = (dr.y..dr.y + dr.height).find(|&y| drawer_row_at(&wm, AREA, x, y) == Some(idx)) else {
            continue;
        };
        let _ = handle_drawer_mouse_event(&mut wm, moved(x, y), AREA);
        assert_eq!(wm.drawer().hover_row, Some(idx));
        seen += 1;
    }
    assert!(seen >= 3);
}

#[test]
fn keyboard_navigation_after_hover_leaves_only_the_selection() {
    let (mut wm, _guard) = fixture(DrawerStyle::Navigator);
    let (x, y) = cell_of_row(&wm, 2);
    let _ = handle_drawer_mouse_event(&mut wm, moved(x, y), AREA);
    assert_eq!(wm.drawer().hover_row, Some(2));
    let _ = wm.apply(Action::DrawerNav(DrawerNavOp::Next));
    assert_eq!(wm.drawer().hover_row, None);
}

#[test]
fn hover_is_ignored_when_drawer_hidden() {
    let (mut wm, _guard) = fixture(DrawerStyle::Navigator);
    wm.toggle_drawer();
    assert!(!apply_drawer_hover(&mut wm, AREA, 2, 5));
    assert_eq!(wm.drawer().hover_row, None);
}
