//! Drag handle and drop-zone overlay for window drag-and-drop (TUI and GUI share this buffer render).
//!
//! Never colour-only: zones differ by border line weight (`GlyphSet::zone_idle` vs `zone_hot`), arrows,
//! the text labels inside them, a REVERSED hot fill, and a status banner that spells out what a release
//! does (or "NO DROP HERE"). Theme colours are only an extra cue.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::widgets::{Block, Borders, Widget};

use crate::ui::glyphs::GlyphSet;
use crate::ui::theme::ThemePalette;
use crate::wm::WindowManager;
use crate::wm::window_drag::{DropZone, DropZoneKind, WindowDragView, grip_glyph_x, grip_hit_span};
use crate::wm::window_drag_place::PlaceHow;

fn set_of(c: &[&'static str; 6]) -> border::Set<'static> {
    border::Set {
        top_left: c[2],
        top_right: c[3],
        bottom_left: c[4],
        bottom_right: c[5],
        vertical_left: c[1],
        vertical_right: c[1],
        horizontal_top: c[0],
        horizontal_bottom: c[0],
    }
}

/// Draws the title-bar drag handle of a window: one cell at the left end of the title bar. Idle shows the plain
/// grip; hovered or being dragged it switches glyph and goes bold + reversed.
pub fn render_grip(wm: &WindowManager, id: crate::pane::PaneId, outer: Rect, palette: &ThemePalette, glyphs: &GlyphSet, buf: &mut Buffer) {
    let (Some((a, b)), Some(gx)) = (grip_hit_span(outer), grip_glyph_x(outer)) else { return };
    let dragging = wm.window_drag_source() == Some(id);
    let hot = dragging || wm.grip_hover() == Some(id);
    let ctrl = palette.controls_fg.unwrap_or(palette.title_fg);
    for x in a..=b {
        let Some(cell) = buf.cell_mut((x, outer.y)) else { continue };
        if x == gx {
            cell.set_symbol(if hot { glyphs.drag_grip_hot } else { glyphs.drag_grip });
            let mut st = Style::default().fg(ctrl).bg(palette.window_frame_bg).add_modifier(Modifier::BOLD);
            if hot {
                st = st.add_modifier(Modifier::REVERSED);
            }
            cell.set_style(st);
        } else {
            cell.set_symbol(" ");
            let mut st = Style::default().bg(palette.window_frame_bg);
            if hot {
                st = st.add_modifier(Modifier::REVERSED);
            }
            cell.set_style(st);
        }
    }
}

fn arrow(glyphs: &GlyphSet, how: PlaceHow) -> &'static str {
    match how {
        PlaceHow::Left => glyphs.zone_arrows[0],
        PlaceHow::Right => glyphs.zone_arrows[1],
        PlaceHow::Up => glyphs.zone_arrows[2],
        PlaceHow::Down => glyphs.zone_arrows[3],
        _ => "",
    }
}

/// Short label inside a zone, long label for the banner.
fn label(kind: DropZoneKind, long: bool) -> String {
    match (kind, long) {
        (DropZoneKind::Place { target, how: PlaceHow::Swap }, false) => format!("SWAP #{target}"),
        (DropZoneKind::Place { target, how: PlaceHow::Swap }, true) => format!("swap places with window #{target}"),
        (DropZoneKind::Place { how, .. }, false) => format!("SPLIT {}", match how {
            PlaceHow::Left => "LEFT",
            PlaceHow::Right => "RIGHT",
            PlaceHow::Up => "TOP",
            _ => "BOTTOM",
        }),
        (DropZoneKind::Place { target, how }, true) => format!(
            "split window #{target}, new tile on its {}",
            match how {
                PlaceHow::Left => "left",
                PlaceHow::Right => "right",
                PlaceHow::Up => "top",
                _ => "bottom",
            }
        ),
        (DropZoneKind::Float, false) => "FLOAT".to_string(),
        (DropZoneKind::Float, true) => "float: Stacked layout, window on top".to_string(),
        (DropZoneKind::Drawer(_), false) => "MOVE TO SESSION".to_string(),
        (DropZoneKind::Drawer(_), true) => "move to the session under the pointer".to_string(),
    }
}

fn put_text(buf: &mut Buffer, x: u16, y: u16, max: u16, text: &str, style: Style) {
    for (i, ch) in text.chars().take(usize::from(max)).enumerate() {
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_symbol(ch.encode_utf8(&mut [0; 4]));
            cell.set_style(style);
        }
    }
}

fn draw_outline(buf: &mut Buffer, r: Rect, set: border::Set<'static>, style: Style) {
    Block::default().borders(Borders::ALL).border_set(set).border_style(style).render(r, buf);
}

fn draw_zone(buf: &mut Buffer, z: &DropZone, hot: bool, glyphs: &GlyphSet, palette: &ThemePalette) {
    let r = z.rect;
    if r.width == 0 || r.height == 0 {
        return;
    }
    let base = if hot { palette.active_border_fg } else { palette.inactive_border_fg };
    let mut style = Style::default().fg(base).bg(palette.window_frame_bg);
    if hot {
        style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
    }
    let set = set_of(if hot { &glyphs.zone_hot } else { &glyphs.zone_idle });
    let text = match z.kind {
        DropZoneKind::Place { how, .. } if how != PlaceHow::Swap => {
            let a = arrow(glyphs, how);
            if r.width >= 14 && r.height >= 3 { format!("{a} {} {a}", label(z.kind, false)) } else { a.to_string() }
        }
        _ => label(z.kind, false),
    };
    if r.width >= 3 && r.height >= 3 {
        if hot {
            // Hot fill: a REVERSED wash over the interior so the target is unmistakable.
            for y in r.y + 1..r.y + r.height - 1 {
                for x in r.x + 1..r.x + r.width - 1 {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        c.set_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM));
                    }
                }
            }
        }
        draw_outline(buf, r, set, style);
        let inner_w = r.width - 2;
        let shown: String = text.chars().take(usize::from(inner_w)).collect();
        let x = r.x + 1 + (inner_w.saturating_sub(shown.chars().count() as u16)) / 2;
        put_text(buf, x, r.y + r.height / 2, inner_w, &shown, style.add_modifier(Modifier::BOLD));
    } else {
        // Too thin for a box: one labelled strip.
        let shown: String = text.chars().take(usize::from(r.width)).collect();
        let x = r.x + r.width.saturating_sub(shown.chars().count() as u16) / 2;
        put_text(buf, x, r.y, r.width, &shown, style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
    }
}

/// Renders the overlay for an in-progress window drag. No-op otherwise.
pub fn render_window_drag(wm: &WindowManager, workspace_area: Rect, palette: &ThemePalette, glyphs: &GlyphSet, buf: &mut Buffer) {
    let Some(view) = wm.window_drag_view(workspace_area) else { return };
    paint(&view, workspace_area, palette, glyphs, buf);
}

fn paint(view: &WindowDragView, area: Rect, palette: &ThemePalette, glyphs: &GlyphSet, buf: &mut Buffer) {
    let hot_rect = |k: DropZoneKind| view.zones.iter().find(|z| z.kind == k).map(|z| z.rect);
    // Idle zones first, the hot one last so its heavier outline wins on shared edges.
    for z in view.zones.iter().filter(|z| Some(z.kind) != view.hover) {
        draw_zone(buf, z, false, glyphs, palette);
    }
    if let Some(dr) = view.drawer_rect {
        let hot = matches!(view.hover, Some(DropZoneKind::Drawer(_)));
        let z = DropZone { rect: dr, kind: DropZoneKind::Drawer(0) };
        if hot {
            let outline = Style::default().fg(palette.active_border_fg).add_modifier(Modifier::BOLD | Modifier::REVERSED);
            draw_outline(buf, dr, set_of(&glyphs.zone_hot), outline);
        } else {
            let idle = Style::default().fg(palette.inactive_border_fg);
            draw_outline(buf, dr, set_of(&glyphs.zone_idle), idle);
            put_text(buf, dr.x + 1, dr.y, dr.width.saturating_sub(2), &format!(" {} ", label(z.kind, false)), idle.add_modifier(Modifier::BOLD));
        }
    }
    if let Some(src) = view.source_rect {
        let st = Style::default().fg(palette.title_fg).add_modifier(Modifier::BOLD | Modifier::ITALIC);
        let mut dotted = glyphs.zone_idle;
        dotted[0] = glyphs.zone_idle[1];
        draw_outline(buf, src, set_of(&dotted), st);
        put_text(buf, src.x + 1, src.y, src.width.saturating_sub(2), &format!(" {} MOVING #{} ", glyphs.drag_grip_hot, view.source), st.add_modifier(Modifier::REVERSED));
    }
    if let Some(k) = view.hover
        && let Some(r) = hot_rect(k)
    {
        draw_zone(buf, &DropZone { rect: r, kind: k }, true, glyphs, palette);
    }

    // Banner on the bottom row: always spells out what a release does, plus how to cancel.
    if area.width >= 20 && area.height >= 2 {
        let (mark, msg) = match view.hover {
            Some(k) => (glyphs.zone_ok, format!("DROP: {}", label(k, true))),
            None => (glyphs.zone_no, "NO DROP HERE: release to leave the window where it was".to_string()),
        };
        let text = format!(" {mark} #{} {msg} | Esc cancels ", view.source);
        let y = area.y + area.height - 1;
        let w = area.width.min(text.chars().count() as u16);
        let mut st = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
        st = st.fg(if view.hover.is_some() { palette.active_border_fg } else { Color::Rgb(255, 85, 85) });
        for x in area.x..area.x + area.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_symbol(" ");
                c.set_style(st);
            }
        }
        put_text(buf, area.x, y, w, &text, st);
    }
}
