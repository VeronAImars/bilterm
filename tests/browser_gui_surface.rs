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

    // GUI surface must keep the full-resolution page texture + nav bar while NOT drawing `▀` smear glyphs.
    wm.set_gui_surface(true);
    renderer.render_workspace(&wm, viewport, &mut surface);
    assert!(
        surface.images.iter().any(|i| i.id == 910_000 + id as u32),
        "GUI mode must still emit the full-resolution page texture"
    );
    let gui_nav: String = surface
        .glyphs
        .iter()
        .filter(|g| g.row == canvas.y)
        .map(|g| g.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(gui_nav.contains("example.com"), "GUI mode must preserve nav bar glyphs: {gui_nav:?}");
    let smeared = surface
        .glyphs
        .iter()
        .any(|g| g.row > canvas.y && g.row < canvas.bottom() && g.col >= canvas.x && g.col < canvas.right() && g.text == "▀");
    assert!(!smeared, "GUI browser body must not paint half-block glyphs over the GPU texture");
}
