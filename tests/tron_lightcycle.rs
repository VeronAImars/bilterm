//! TRON light cycles: the animated border of the active window (`:cycles`, `[appearance] lightcycle_speed`).
//! The phase is fixed with `WindowManager::set_lightcycle_phase`, so every assertion is deterministic.

use std::sync::Once;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::Widget;

use bilterm::colon_command::{execute_wm_colon_command, parse_colon_command};
use bilterm::config::BiltermConfig;
use bilterm::ui::chrome::{OutlineStyle, render_borland_frame_ext};
use bilterm::ui::design::tron::tron_palette;
use bilterm::ui::glyphs::GlyphPack;
use bilterm::ui::lightcycle::{head_cell, head_cell_ccw, head_color, perimeter_len, render_lightcycles};
use bilterm::ui::shell::ShellLayout;
use bilterm::ui::{buffer_to_snapshot_string, WorkspaceWidget};
use bilterm::wm::{SplitDirection, WindowManager};

static ENV: Once = Once::new();

fn wm() -> WindowManager {
    ENV.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bilterm-tron-lightcycle-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: executed exactly once, before any test in this binary reads the environment.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            std::env::set_var("XDG_STATE_HOME", dir.join("state"));
            std::env::set_var("XDG_DATA_HOME", dir.join("data"));
        }
    });
    WindowManager::new_headless(24, 100)
}

fn run(wm: &mut WindowManager, cmd: &str) {
    let action = parse_colon_command(cmd).unwrap_or_else(|| panic!("{cmd} not parsed")).unwrap();
    execute_wm_colon_command(wm, action);
}

fn render(wm: &WindowManager, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    WorkspaceWidget::new(wm).render(area, &mut buf);
    buf
}

/// Two TRON windows side by side; returns the manager, the viewport and the active window's frame.
fn two_windows() -> (WindowManager, Rect, Rect, Rect) {
    let mut w = wm();
    let area = Rect::new(0, 0, 100, 24);
    w.split_focused(SplitDirection::Vertical).unwrap();
    run(&mut w, ":tron on");
    w.resize_all(area).unwrap();
    let ws = ShellLayout::compute(area, w.config(), w.pane_count()).workspace;
    let rects = w.compute_workspace_rects(ws);
    let active = w.active_pane_id();
    let on = rects.iter().find(|(id, _)| *id == active).unwrap().1;
    let off = rects.iter().find(|(id, _)| *id != active).unwrap().1;
    (w, area, on, off)
}

fn tron_pal_head() -> Color {
    head_color(tron_palette().fkey_num_fg)
}

#[test]
fn head_cell_of_the_active_window_is_bright_and_advances_with_the_phase() {
    let (mut w, area, on, _) = two_windows();
    let head_fg = tron_pal_head();
    // start on the bottom edge, which carries no title text
    let base = u64::from(on.width + on.height) + 3;
    for phase in [base, base + 1, base + 7, base + 30] {
        w.set_lightcycle_phase(Some(phase));
        let buf = render(&w, area);
        let (dx, dy) = head_cell(phase, on.width, on.height);
        let cell = &buf[(on.x + dx, on.y + dy)];
        assert_eq!(cell.fg, head_fg, "head fg at phase {phase} ({dx},{dy})");
        assert!(cell.modifier.contains(Modifier::BOLD));
        // exactly one cell on the whole screen carries the cyan head color
        let heads = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|p| buf[*p].fg == head_fg)
            .count();
        assert_eq!(heads, 1, "single cyan head at phase {phase}");
    }
    // the orange cycle runs on the same border, in the other direction
    w.set_lightcycle_phase(Some(5));
    let buf = render(&w, area);
    let (ox, oy) = head_cell_ccw(5, on.width, on.height).expect("big window has a second cycle");
    let orange = head_color(tron_palette().prompt_prefix_fg);
    let found = (0..area.height).flat_map(|y| (0..area.width).map(move |x| (x, y))).any(|p| buf[p].fg == orange);
    assert!(found || buf[(on.x + ox, on.y + oy)].symbol().trim().is_empty(), "orange head at ({ox},{oy})");
}

#[test]
fn trail_fades_from_the_cycle_color_towards_the_border_color() {
    let (mut w, area, on, _) = two_windows();
    let at = u64::from(on.width + on.height) + 20;
    w.set_lightcycle_phase(Some(at));
    let buf = render(&w, area);
    let pal = tron_palette();
    let fg_behind = |k: u64| {
        let (dx, dy) = head_cell(at - k, on.width, on.height);
        match buf[(on.x + dx, on.y + dy)].fg {
            Color::Rgb(r, g, b) => (u32::from(r) + u32::from(g) + u32::from(b)) as i64,
            other => panic!("non-rgb {other:?}"),
        }
    };
    let base = match pal.active_border_fg {
        Color::Rgb(r, g, b) => (u32::from(r) + u32::from(g) + u32::from(b)) as i64,
        _ => unreachable!(),
    };
    // Trail cells are cyan (0,246,255) fading to the border color: distinct from it near the head.
    assert_ne!(buf[(on.x + head_cell(at - 1, on.width, on.height).0, on.y + head_cell(at - 1, on.width, on.height).1)].fg, pal.active_border_fg);
    assert!((fg_behind(1) - base).abs() >= (fg_behind(8) - base).abs(), "trail dims with distance");
}

#[test]
fn inactive_window_and_static_phase_keep_the_plain_border() {
    let (mut w, area, on, off) = two_windows();
    let plain = render(&w, area);
    assert_eq!(w.lightcycle_phase(), None, "headless windows do not animate until told to");
    w.set_lightcycle_phase(Some(12));
    let moving = render(&w, area);
    for y in off.y..off.y + off.height {
        for x in off.x..off.x + off.width {
            assert_eq!(plain[(x, y)], moving[(x, y)], "inactive window cell {x},{y} changed");
        }
    }
    let changed = (on.y..on.y + on.height)
        .flat_map(|y| (on.x..on.x + on.width).map(move |x| (x, y)))
        .filter(|p| plain[*p] != moving[*p])
        .count();
    assert!(changed >= 3, "active border shows the cycles: {changed} cells differ");
    // Symbols never change, so text snapshots stay byte-identical whatever the phase is.
    assert_eq!(buffer_to_snapshot_string(&plain), buffer_to_snapshot_string(&moving));
}

#[test]
fn animation_is_tron_only() {
    let (mut w, area, on, _) = two_windows();
    w.set_lightcycle_phase(Some(3));
    run(&mut w, ":tron off");
    let buf = render(&w, area);
    let (dx, dy) = head_cell(3, on.width, on.height);
    assert_ne!(buf[(on.x + dx, on.y + dy)].fg, tron_pal_head());
}

#[test]
fn heads_wrap_through_all_four_corners_for_several_window_sizes() {
    for (w, h) in [(4u16, 3u16), (5, 3), (4, 6), (12, 5), (40, 12), (80, 24), (200, 50)] {
        let p = u64::from(perimeter_len(w, h));
        let corners = [(0, 0), (w - 1, 0), (w - 1, h - 1), (0, h - 1)];
        let mut hit = Vec::new();
        for t in 0..p * 2 {
            let c = head_cell(t, w, h);
            assert_eq!(c, head_cell(t + p, w, h), "{w}x{h}: period is the perimeter");
            if corners.contains(&c) && !hit.contains(&c) {
                hit.push(c);
            }
            let next = head_cell(t + 1, w, h);
            let step = (i32::from(c.0) - i32::from(next.0)).abs() + (i32::from(c.1) - i32::from(next.1)).abs();
            assert_eq!(step, 1, "{w}x{h}: head moves one cell per tick at t={t}");
        }
        assert_eq!(hit, corners.to_vec(), "{w}x{h}: corners in clockwise order");
    }
}

#[test]
fn smallest_bordered_window_renders_one_cycle_without_touching_anything_else() {
    let pal = tron_palette();
    let glyphs = GlyphPack::Unicode.glyphs();
    let outer = Rect::new(2, 1, 4, 3);
    for phase in 0..u64::from(perimeter_len(4, 3)) {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 6));
        render_borland_frame_ext(outer, "", true, OutlineStyle::Rounded, false, &pal, &glyphs, &mut buf);
        let before = buf.clone();
        render_lightcycles(&mut buf, outer, phase, &pal);
        let (dx, dy) = head_cell(phase, 4, 3);
        let head = &buf[(outer.x + dx, outer.y + dy)];
        if !before[(outer.x + dx, outer.y + dy)].symbol().trim().is_empty() && (dx, dy) != (3, 2) {
            assert_eq!(head.fg, tron_pal_head(), "phase {phase}");
        }
        assert!(head_cell_ccw(phase, 4, 3).is_none());
        for y in 0..6 {
            for x in 0..10 {
                let inside = x > outer.x && x < outer.x + 3 && y > outer.y && y < outer.y + 2;
                let on_border = !inside && x >= outer.x && x < outer.x + 4 && y >= outer.y && y < outer.y + 3;
                if !on_border {
                    assert_eq!(buf[(x, y)], before[(x, y)], "non-border cell {x},{y} changed");
                }
            }
        }
    }
    // Below the frame minimum nothing is drawn and nothing panics.
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 6));
    let before = buf.clone();
    render_lightcycles(&mut buf, Rect::new(0, 0, 3, 3), 1, &pal);
    assert_eq!(buf, before);
}

#[test]
fn cycles_command_sets_speed_off_on_and_reports() {
    let mut w = wm();
    assert_eq!(w.lightcycle_speed(), 12, "moderate default");
    run(&mut w, ":cycles 30");
    assert_eq!(w.lightcycle_speed(), 30);
    run(&mut w, ":cycles off");
    assert_eq!(w.lightcycle_speed(), 0);
    run(&mut w, ":cycles on");
    assert_eq!(w.lightcycle_speed(), 12, "on restores the default speed");
    run(&mut w, ":cycles");
    assert!(w.active_pane().status_message().is_some_and(|m| m.contains("12 cells/s")));
    assert!(parse_colon_command(":cycles 61").unwrap().is_err());
    assert!(parse_colon_command(":cycles fast").unwrap().is_err());
}

#[test]
fn speed_is_a_saved_config_key() {
    let cfg = BiltermConfig::from_toml_str("[appearance]\nlightcycle_speed = 25\n").unwrap();
    assert_eq!(cfg.appearance.lightcycle_speed.get(), 25);
    let off = BiltermConfig::from_toml_str("[appearance]\nlightcycle_speed = 0\n").unwrap();
    assert_eq!(off.appearance.lightcycle_speed.get(), 0);
    assert_eq!(BiltermConfig::default().appearance.lightcycle_speed.get(), 12);
    assert_eq!(BiltermConfig::from_toml_str("[appearance]\nlightcycle_speed = 250\n").unwrap().appearance.lightcycle_speed.get(), 60);

    let mut w = wm();
    run(&mut w, ":set appearance.lightcycle_speed 33");
    assert_eq!(w.lightcycle_speed(), 33);
    let text = toml::to_string(w.config()).unwrap();
    assert!(text.contains("lightcycle_speed = 33"), "{text}");
}

#[test]
fn the_tick_clock_redraws_only_at_the_animation_rate_and_never_when_off() {
    let t0 = Instant::now();
    let mut w = wm();
    // TRON off: never asks for a redraw, however often it is polled.
    for ms in 0..200 {
        assert!(!w.tick_lightcycle(t0 + Duration::from_millis(ms * 16)));
    }
    run(&mut w, ":tron on");
    let mut redraws = 0;
    for ms in 0..=62 {
        // 16 ms loop tick over ~1 s at the default speed
        if w.tick_lightcycle(t0 + Duration::from_millis(ms * 16)) {
            redraws += 1;
        }
    }
    assert!((8..=13).contains(&redraws), "about one redraw per cell at 12 cells/s: {redraws}");
    run(&mut w, ":cycles off");
    assert!(w.tick_lightcycle(t0 + Duration::from_secs(2)), "one last redraw back to the static border");
    for ms in 0..100 {
        assert!(!w.tick_lightcycle(t0 + Duration::from_secs(2) + Duration::from_millis(ms * 16)));
    }
    assert_eq!(w.lightcycle_phase(), None);
}
