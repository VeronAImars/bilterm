//! Layout signature + shared resize guard for the GUI event loops (`COR-resize-guard-skips-layout-changes`).
//!
//! `WindowManager::resize_all` depends on more than the viewport rect: pane count and framing,
//! layout mode, display mode, bar flags, quick-terminal visibility and font scale. The GUI
//! backends (Windows, macOS, X11) use [`needs_resize`] so the per-frame resize is skipped only
//! when none of those inputs changed.

use ratatui::layout::Rect;

use super::WindowManager;
use super::WorkspaceLayoutMode;
use crate::ui::design::{AppearanceProfile, DisplayMode};

/// Every `WindowManager` input (besides the viewport rect) that `resize_all` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutKey {
    pane_count: usize,
    is_framed: bool,
    layout_mode: WorkspaceLayoutMode,
    show_top_menubar: bool,
    show_dos_fkey_bar: bool,
    display_mode: DisplayMode,
    explicit_new_look: bool,
    quick_terminal_visible: bool,
    viewport_scale_pct: u16,
    pane_scales_fold: u64,
}

impl WindowManager {
    /// Snapshot of the non-viewport inputs to `resize_all`.
    pub fn layout_key(&self) -> LayoutKey {
        let cfg = self.config();
        let pane_scales_fold = self.panes().keys().fold(0xcbf2_9ce4_8422_2325_u64, |acc, &id| {
            let s = u64::from(self.pane_effective_scale_pct(id));
            (acc ^ (id as u64).wrapping_mul(0x9e37_79b9) ^ s).wrapping_mul(0x0100_0000_01b3)
        });
        LayoutKey {
            pane_count: self.pane_count(),
            is_framed: self.is_framed(),
            layout_mode: self.layout_mode(),
            show_top_menubar: cfg.show_top_menubar,
            show_dos_fkey_bar: cfg.show_dos_fkey_bar,
            display_mode: crate::ui::shell::effective_display_mode(cfg),
            explicit_new_look: cfg.appearance.profile.is_some_and(|p| p != AppearanceProfile::Legacy),
            quick_terminal_visible: self.is_quick_terminal_visible(),
            viewport_scale_pct: self.viewport_scale_pct(),
            pane_scales_fold,
        }
    }
}

/// Whether the GUI loop must call `resize_all` this frame.
pub fn needs_resize(
    font_changed: bool,
    desired: Rect,
    last: Rect,
    key: &LayoutKey,
    last_key: Option<&LayoutKey>,
) -> bool {
    font_changed || desired != last || last_key != Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colon_command::parse_colon_command;

    fn vp() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    #[test]
    fn guard_font_change_forces_resize() {
        let k = WindowManager::new_headless(24, 80).layout_key();
        assert!(needs_resize(true, vp(), vp(), &k, Some(&k)));
    }

    #[test]
    fn guard_skips_when_nothing_changed() {
        let k = WindowManager::new_headless(24, 80).layout_key();
        assert!(!needs_resize(false, vp(), vp(), &k, Some(&k)));
    }

    #[test]
    fn guard_viewport_change_forces_resize() {
        let k = WindowManager::new_headless(24, 80).layout_key();
        assert!(needs_resize(false, Rect::new(0, 0, 81, 24), vp(), &k, Some(&k)));
    }

    #[test]
    fn guard_layout_only_change_forces_resize() {
        let mut wm = WindowManager::new_headless(24, 80);
        let k0 = wm.layout_key();
        wm.config_mut().show_dos_fkey_bar = !wm.config().show_dos_fkey_bar;
        let k1 = wm.layout_key();
        assert!(needs_resize(false, vp(), vp(), &k1, Some(&k0)));
        // First frame (no stored key) always resizes.
        assert!(needs_resize(false, vp(), vp(), &k1, None));
    }

    /// Mimics a GUI frame: resize only when the guard says so.
    fn frame(wm: &mut WindowManager, last_key: &mut Option<LayoutKey>) {
        let key = wm.layout_key();
        if needs_resize(false, vp(), vp(), &key, last_key.as_ref()) {
            wm.resize_all(vp()).unwrap();
            *last_key = Some(key);
        }
    }

    fn pane_sizes(wm: &WindowManager) -> Vec<(u16, u16)> {
        wm.panes().values().map(|p| p.vt().size()).collect()
    }

    /// Ground truth: an unconditional `resize_all` (what the old per-frame call did).
    fn expected_sizes(wm: &mut WindowManager) -> Vec<(u16, u16)> {
        wm.resize_all(vp()).unwrap();
        pane_sizes(wm)
    }

    #[test]
    fn mode_zen_with_unchanged_viewport_reflows_panes() {
        let mut wm = WindowManager::new_headless(24, 80);
        // `:mode` only takes effect for non-Legacy appearance profiles.
        wm.config_mut().appearance.profile = Some(AppearanceProfile::Obsidian);
        wm.apply(crate::action::Action::SplitVertical).unwrap();
        let mut last_key = None;
        frame(&mut wm, &mut last_key);
        let before = pane_sizes(&wm);
        let act = parse_colon_command(":mode zen").unwrap().unwrap();
        wm.apply(act).unwrap();
        frame(&mut wm, &mut last_key);
        let after = pane_sizes(&wm);
        assert_ne!(before, after, "zen drops frames; guard must reflow");
        assert_eq!(after, expected_sizes(&mut wm));
    }

    #[test]
    fn config_bar_flag_with_unchanged_viewport_reflows_panes() {
        let mut wm = WindowManager::new_headless(24, 80);
        let mut last_key = None;
        wm.config_mut().show_dos_fkey_bar = false;
        frame(&mut wm, &mut last_key);
        let before = pane_sizes(&wm);
        let act = parse_colon_command(":set show_dos_fkey_bar true").unwrap().unwrap();
        wm.apply(act).unwrap();
        frame(&mut wm, &mut last_key);
        let after = pane_sizes(&wm);
        assert_ne!(before, after, "F-key bar steals a row");
        assert_eq!(after, expected_sizes(&mut wm));
    }
}
