//! GUI path: the browser nav bar glyphs and the page texture must land where the cells are.
use std::sync::Once;

use ratatui::layout::Rect;

use bilterm::ui::renderer::{GuiFrameSurface, GuiWorkspaceRenderer, WorkspaceFrameRenderer};
use bilterm::wm::WindowManager;

fn enable_synthetic_browser() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: synchronized via Once before any browser canvas reads this env var.
        unsafe { std::env::set_var("BILTERM_BROWSER_SYNTHETIC", "1") };
    });
}

#[test]
fn gui_surface_has_nav_bar_glyphs_and_page_texture_below_it() {
    enable_synthetic_browser();
    let viewport = Rect::new(0, 0, 100, 30);
    let mut wm = WindowManager::new_headless(30, 100);
    let id = wm.active_pane_id();
    wm.pane_mut(id).unwrap().open_browser("https://example.com");
    let mut renderer = GuiWorkspaceRenderer::new(8, 16);
    let mut surface = GuiFrameSurface::new(8, 16);
    renderer.render_workspace(&wm, viewport, &mut surface);

    let canvas = wm.pane(id).unwrap().canvas_rect(viewport);
    let on_nav_row: String = surface
        .glyphs
        .iter()
        .filter(|g| g.row == canvas.y)
        .map(|g| g.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(on_nav_row.contains("◄") && on_nav_row.contains("example.com") && on_nav_row.contains("✕"), "nav row glyphs: {on_nav_row:?}");

    let tex = surface.images.iter().find(|i| i.id == 910_000 + id as u32).expect("page texture");
    assert_eq!(tex.y_px, u32::from(canvas.y + 1) * 16, "texture starts under the nav row");
    assert_eq!(tex.x_px, u32::from(canvas.x) * 8);
    assert_eq!(tex.width_px, u32::from(canvas.width) * 8);
    assert_eq!(tex.height_px, u32::from(canvas.height - 1) * 16);
}

#[test]
fn gui_multi_pane_browser_texture_aligns_with_framed_inner_canvas() {
    enable_synthetic_browser();
    let viewport = Rect::new(0, 0, 120, 34);
    let mut wm = WindowManager::new_headless(34, 120);
    wm.set_gui_surface(true);
    let _ = wm.apply(bilterm::action::Action::SplitVertical).unwrap();
    assert!(wm.is_framed(), "multi-pane workspace must be framed for this regression test");
    let id = wm.active_pane_id();
    wm.pane_mut(id).unwrap().open_browser("https://example.com");

    let mut renderer = GuiWorkspaceRenderer::new(8, 16);
    let mut surface = GuiFrameSurface::new(8, 16);
    renderer.render_workspace(&wm, viewport, &mut surface);

    let shell = bilterm::ui::shell::ShellLayout::compute(viewport, wm.config(), wm.pane_count());
    let outer = wm.compute_workspace_rects(shell.workspace).into_iter().find(|(pid, _)| *pid == id).unwrap().1;
    let inner = bilterm::wm::inset_window_frame(outer);
    let canvas = wm.pane(id).unwrap().canvas_rect(inner);

    let tex = surface.images.iter().find(|i| i.id == 910_000 + id as u32).expect("framed page texture");
    assert_eq!(tex.x_px, u32::from(canvas.x) * 8, "framed browser texture x must match inner canvas");
    assert_eq!(tex.y_px, u32::from(canvas.y + 1) * 16, "framed browser texture y must start right under framed nav bar");
    assert_eq!(tex.width_px, u32::from(canvas.width) * 8);
    assert_eq!(tex.height_px, u32::from(canvas.height - 1) * 16);
}
