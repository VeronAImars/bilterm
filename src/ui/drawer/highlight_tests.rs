//! Headless render tests for the drawer hover / keyboard-selection highlight (both styles, many palettes).

use super::*;
use crate::action::Action;
use crate::input::drawer_hover::drawer_row_at;
use crate::ui::glyphs::GlyphSet;
use crate::ui::render_workspace_drawer;
use crate::ui::theme::ThemePreset;
use crate::ui::visual_style::modern_obsidian_palette;
use crate::wm::drawer::{DrawerNavOp, DrawerStyle};
use crate::wm::WindowManager;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const AREA: Rect = Rect { x: 0, y: 0, width: 100, height: 30 };

fn fixture(style: DrawerStyle, modern: bool) -> WindowManager {
    let mut wm = WindowManager::new_headless(30, 100);
    wm.config_mut().drawer.style = style;
    wm.config_mut().modern_mode = modern;
    let _ = wm.split_focused(crate::wm::SplitDirection::Horizontal);
    let _ = wm.create_new_session(Some("two"), false);
    wm.toggle_drawer();
    assert!(wm.is_drawer_effectively_visible());
    wm
}

fn render(wm: &WindowManager, pal: &ThemePalette) -> Buffer {
    let mut buf = Buffer::empty(AREA);
    render_workspace_drawer(wm, AREA, wm.config(), pal, &GlyphSet::default(), &mut buf);
    buf
}

/// Screen cell `(x, y)` of the first line of drawer row `idx`.
fn row_cell(wm: &WindowManager, idx: usize) -> Option<(u16, u16)> {
    let dr = wm.drawer_rect(AREA).unwrap();
    let x = dr.x + 2;
    (dr.y..dr.y + dr.height).find(|&y| drawer_row_at(wm, AREA, x, y) == Some(idx)).map(|y| (x, y))
}

fn style_at(buf: &Buffer, (x, y): (u16, u16)) -> Style {
    buf[(x, y)].style()
}

fn palettes() -> Vec<(ThemePalette, bool)> {
    let mut v: Vec<_> = ThemePreset::all().iter().map(|p| (p.palette(), false)).collect();
    v.push((modern_obsidian_palette(false), true));
    v.push((modern_obsidian_palette(true), true));
    v
}

fn hover(wm: &mut WindowManager, row: Option<usize>) {
    let _ = wm.apply(Action::DrawerNav(DrawerNavOp::Hover(row)));
}

#[test]
fn hover_marks_every_row_of_all_three_lists_in_both_styles() {
    for style in [DrawerStyle::Navigator, DrawerStyle::Classic] {
        for (pal, modern) in palettes() {
            let mut wm = fixture(style, modern);
            let rows = wm.drawer_rows();
            assert!(rows.len() >= 3, "{style:?}: sessions, windows and panes");
            let selected = wm.drawer().selected_index;
            let mut tested = 0;
            for i in (0..rows.len()).filter(|&i| i != selected) {
                let Some(cell) = row_cell(&wm, i) else { continue };
                tested += 1;
                hover(&mut wm, Some(i));
                let buf = render(&wm, &pal);
                let st = style_at(&buf, cell);
                assert!(
                    st.add_modifier.contains(Modifier::UNDERLINED),
                    "{style:?} {} row {i} ({:?}) not marked on hover",
                    pal.name,
                    rows[i].kind
                );
                assert!(!st.add_modifier.contains(Modifier::REVERSED), "hover must differ from selection");
                for j in (0..rows.len()).filter(|&j| j != i && j != selected).take(2) {
                    let Some(c) = row_cell(&wm, j) else { continue };
                    let other = style_at(&buf, c);
                    assert!(!other.add_modifier.contains(Modifier::UNDERLINED), "only the hovered row is marked");
                }
            }
            assert!(tested >= 3, "{style:?} {}: only {tested} visible rows", pal.name);
        }
    }
}

#[test]
fn keyboard_selection_is_marked_and_distinct_from_hover() {
    for style in [DrawerStyle::Navigator, DrawerStyle::Classic] {
        for (pal, modern) in palettes() {
            let mut wm = fixture(style, modern);
            let n = wm.drawer_rows().len();
            for _ in 0..n {
                let _ = wm.apply(Action::DrawerNav(DrawerNavOp::Next));
                let sel = wm.drawer().selected_index;
                let buf = render(&wm, &pal);
                let st = style_at(&buf, row_cell(&wm, sel).expect("selected row is scrolled into view"));
                let bar = selection_is_bar(&pal, modern);
                if bar {
                    assert_eq!(st.bg, Some(blend(pal.window_frame_bg, pal.active_border_fg, 30)), "{style:?} {}", pal.name);
                    assert!(st.add_modifier.contains(Modifier::BOLD));
                } else {
                    assert!(st.add_modifier.contains(Modifier::REVERSED), "{style:?} {} row {sel}", pal.name);
                }
                assert!(!st.add_modifier.contains(Modifier::UNDERLINED));
            }
        }
    }
}

#[test]
fn keyboard_move_clears_hover_last_input_wins() {
    let mut wm = fixture(DrawerStyle::Navigator, false);
    let sel = wm.drawer().selected_index;
    let other = (sel + 1) % wm.drawer_rows().len();
    hover(&mut wm, Some(other));
    assert_eq!(wm.drawer().hover_row, Some(other));
    let _ = wm.apply(Action::DrawerNav(DrawerNavOp::Next));
    assert_eq!(wm.drawer().hover_row, None);
    let _ = wm.apply(Action::DrawerNav(DrawerNavOp::Prev));
    hover(&mut wm, Some(0));
    let _ = wm.apply(Action::DrawerNav(DrawerNavOp::Scroll(1)));
    assert_eq!(wm.drawer().hover_row, None);
}

#[test]
fn hover_on_selected_row_shows_selection_and_hover_changes_report_redraw() {
    let mut wm = fixture(DrawerStyle::Navigator, false);
    let sel = wm.drawer().selected_index;
    hover(&mut wm, Some(sel));
    let buf = render(&wm, &modern_obsidian_palette(false));
    let st = style_at(&buf, row_cell(&wm, sel).expect("selected row is scrolled into view"));
    assert!(!st.add_modifier.contains(Modifier::UNDERLINED));
    assert!(wm.apply(Action::DrawerNav(DrawerNavOp::Hover(Some(0)))).is_ok());
}

#[test]
fn selection_is_bar_needs_modern_and_blendable_palette() {
    let rgb = modern_obsidian_palette(false);
    assert!(selection_is_bar(&rgb, true));
    assert!(!selection_is_bar(&rgb, false));
    let mut indexed = rgb.clone();
    indexed.window_frame_bg = ratatui::style::Color::Blue;
    assert!(!selection_is_bar(&indexed, true), "unblendable colours fall back to reverse video");
    let sel = mark_style(Style::default(), &indexed, true, RowMark::Selected);
    assert!(sel.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn row_mark_selection_beats_hover() {
    assert_eq!(RowMark::of(2, 2, Some(2)), RowMark::Selected);
    assert_eq!(RowMark::of(1, 2, Some(1)), RowMark::Hover);
    assert_eq!(RowMark::of(0, 2, Some(1)), RowMark::Plain);
    assert_eq!(RowMark::of(0, 2, None), RowMark::Plain);
}
