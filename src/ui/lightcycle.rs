//! TRON light cycles: bright heads with fading trails racing around the active window's border.
//!
//! Everything here is a pure function of the animation `phase` (cells travelled so far) and the window
//! size, so tests can assert the head cell at tick N. The clock that advances the phase lives in
//! [`crate::wm::lightcycle`]; the speed is `[appearance] lightcycle_speed` (cells per second, `0` = off).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use serde::{Deserialize, Serialize};

use crate::ui::theme::ThemePalette;

/// Default speed in cells per second (a lap of an 80x24 window takes about 17 s).
pub const DEFAULT_SPEED: u8 = 12;
/// Fastest accepted speed in cells per second.
pub const MAX_SPEED: u8 = 60;
/// Windows with a shorter perimeter than this get a single cycle (two would fill the border).
const TWO_CYCLE_MIN_PERIMETER: u32 = 20;
/// Longest trail behind a head, in cells.
const MAX_TRAIL: u32 = 10;

/// `[appearance] lightcycle_speed`: cells per second, `0` turns the animation off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CycleSpeed(pub u8);

impl Default for CycleSpeed {
    fn default() -> Self {
        Self(DEFAULT_SPEED)
    }
}

impl CycleSpeed {
    /// The effective speed, clamped to `0..=MAX_SPEED`.
    pub fn get(self) -> u8 {
        self.0.min(MAX_SPEED)
    }
}

/// `:cycles` operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightCycleOp {
    /// Report the current setting.
    Show,
    /// Turn the animation on (restores the default speed when it was off).
    On,
    /// Turn the animation off (the border stays static).
    Off,
    /// Set the speed in cells per second (`1..=MAX_SPEED`).
    Speed(u8),
}

/// Parses the argument of `:cycles [on | off | <1..60>]`.
pub fn parse_cycles_arg(arg: Option<&str>) -> Result<LightCycleOp, String> {
    let usage = || format!("Usage: :cycles [on | off | <1..{MAX_SPEED}>]  (cells per second; default {DEFAULT_SPEED})");
    match arg.map(str::to_ascii_lowercase).as_deref() {
        None | Some("show" | "status") => Ok(LightCycleOp::Show),
        Some("on" | "true" | "yes" | "enable") => Ok(LightCycleOp::On),
        Some("off" | "false" | "no" | "disable" | "0") => Ok(LightCycleOp::Off),
        Some(n) => match n.parse::<u8>() {
            Ok(v) if (1..=MAX_SPEED).contains(&v) => Ok(LightCycleOp::Speed(v)),
            _ => Err(usage()),
        },
    }
}

/// Number of border cells of a `w` x `h` window (`0` when it has no border).
pub fn perimeter_len(w: u16, h: u16) -> u32 {
    if w < 2 || h < 2 {
        return 0;
    }
    2 * (u32::from(w) + u32::from(h)) - 4
}

/// Offset `(dx, dy)` from the window's top-left of border cell `idx`, walking clockwise from the
/// top-left corner: along the top, down the right side, back along the bottom, up the left side.
pub fn perimeter_cell(idx: u32, w: u16, h: u16) -> (u16, u16) {
    let p = perimeter_len(w, h);
    if p == 0 {
        return (0, 0);
    }
    let i = idx % p;
    let (w32, h32) = (u32::from(w), u32::from(h));
    let (x, y) = if i < w32 {
        (i, 0)
    } else if i < w32 + h32 - 1 {
        (w32 - 1, i - w32 + 1)
    } else if i < 2 * w32 + h32 - 2 {
        (w32 - 2 - (i - (w32 + h32 - 1)), h32 - 1)
    } else {
        (0, h32 - 1 - (i - (2 * w32 + h32 - 2)) - 1)
    };
    (x as u16, y as u16)
}

/// Border index of the clockwise (cyan) head at `phase`, and of the counter-clockwise (orange) head,
/// which starts on the opposite side and only exists when the window is big enough for two cycles.
pub fn head_indices(phase: u64, w: u16, h: u16) -> (u32, Option<u32>) {
    let p = perimeter_len(w, h);
    if p == 0 {
        return (0, None);
    }
    let step = (phase % u64::from(p)) as u32;
    let cw = step;
    let ccw = (p / 2 + p - step) % p;
    (cw, (p >= TWO_CYCLE_MIN_PERIMETER).then_some(ccw))
}

/// Offset of the clockwise (cyan) head cell inside the window at `phase`.
pub fn head_cell(phase: u64, w: u16, h: u16) -> (u16, u16) {
    perimeter_cell(head_indices(phase, w, h).0, w, h)
}

/// Offset of the counter-clockwise (orange) head cell, `None` for windows too small for two cycles.
pub fn head_cell_ccw(phase: u64, w: u16, h: u16) -> Option<(u16, u16)> {
    head_indices(phase, w, h).1.map(|i| perimeter_cell(i, w, h))
}

/// Trail length in cells behind a head for a border of `perimeter` cells.
pub fn trail_len(perimeter: u32) -> u32 {
    (perimeter / 8).clamp(2, MAX_TRAIL)
}

fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Blends `from` towards `to` by `num / den`; non-RGB colors stay as `from`.
fn mix(from: Color, to: Color, num: u32, den: u32) -> Color {
    let (Some(a), Some(b)) = (rgb(from), rgb(to)) else { return from };
    let f = |x: u8, y: u8| ((u32::from(x) * (den - num) + u32::from(y) * num) / den.max(1)) as u8;
    Color::Rgb(f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// Head color of a cycle: its color pushed towards white.
pub fn head_color(cycle: Color) -> Color {
    mix(cycle, Color::Rgb(255, 255, 255), 6, 10)
}

/// Draws the light cycles over the border of the (already drawn) frame `outer`. Only the foreground
/// color of the border cells changes (title text and window controls on the top edge are tinted while
/// a cycle passes under them); blank title padding and the resize corner are left alone.
pub fn render_lightcycles(buf: &mut Buffer, outer: Rect, phase: u64, palette: &ThemePalette) {
    if outer.width < 4 || outer.height < 3 {
        return;
    }
    let (w, h) = (outer.width, outer.height);
    let p = perimeter_len(w, h);
    let trail = trail_len(p);
    let (cw, ccw) = head_indices(phase, w, h);
    let cycles = [(Some(cw), true, palette.fkey_num_fg), (ccw, false, palette.prompt_prefix_fg)];
    for (head, clockwise, color) in cycles {
        let Some(head) = head else { continue };
        for k in 0..=trail {
            let idx = if clockwise { (head + p - k) % p } else { (head + k) % p };
            let (dx, dy) = perimeter_cell(idx, w, h);
            let Some(cell) = buf.cell_mut((outer.x + dx, outer.y + dy)) else { continue };
            // Blank cells (title padding) stay dark; the resize corner keeps its own color.
            if cell.symbol().trim().is_empty() || (dx == w - 1 && dy == h - 1) {
                continue;
            }
            if k == 0 {
                cell.set_fg(head_color(color));
                cell.modifier.insert(Modifier::BOLD);
            } else {
                cell.set_fg(mix(color, palette.active_border_fg, k, trail + 1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perimeter_walk_is_clockwise_closed_and_visits_every_border_cell_once() {
        for (w, h) in [(4u16, 3u16), (5, 3), (4, 4), (10, 6), (80, 24)] {
            let p = perimeter_len(w, h);
            assert_eq!(p, 2 * (u32::from(w) + u32::from(h)) - 4);
            let mut seen = std::collections::BTreeSet::new();
            let mut prev = perimeter_cell(p - 1, w, h);
            for i in 0..p {
                let c = perimeter_cell(i, w, h);
                assert!(c.0 == 0 || c.1 == 0 || c.0 == w - 1 || c.1 == h - 1, "{c:?} is on the border");
                assert!(seen.insert(c), "{c:?} visited twice in {w}x{h}");
                let d = (i32::from(c.0) - i32::from(prev.0)).abs() + (i32::from(c.1) - i32::from(prev.1)).abs();
                assert_eq!(d, 1, "step {i} in {w}x{h} is adjacent");
                prev = c;
            }
            assert_eq!(seen.len() as u32, p);
            assert_eq!(perimeter_cell(0, w, h), (0, 0));
            assert_eq!(perimeter_cell(p, w, h), (0, 0), "wraps");
        }
    }

    #[test]
    fn corners_are_hit_in_clockwise_order() {
        let (w, h) = (7u16, 5u16);
        let corners: Vec<(u16, u16)> = (0..perimeter_len(w, h) as u64)
            .map(|t| head_cell(t, w, h))
            .filter(|&(x, y)| (x == 0 || x == w - 1) && (y == 0 || y == h - 1))
            .collect();
        assert_eq!(corners, vec![(0, 0), (w - 1, 0), (w - 1, h - 1), (0, h - 1)]);
    }

    #[test]
    fn second_cycle_runs_the_other_way_from_the_opposite_side() {
        assert_eq!(head_cell_ccw(0, 4, 3), None, "smallest window has one cycle");
        let (w, h) = (20u16, 8u16);
        let p = u64::from(perimeter_len(w, h));
        assert!(head_cell_ccw(0, w, h).is_some());
        assert_ne!(head_cell(0, w, h), head_cell_ccw(0, w, h).unwrap());
        // One step later the ccw head sits on the cell the cw direction reaches one step before its own start.
        let idx = |t: u64| head_indices(t, w, h).1.unwrap();
        assert_eq!((idx(1) + 1) % p as u32, idx(0));
    }

    #[test]
    fn parse_cycles_arg_accepts_words_and_bounded_numbers() {
        assert_eq!(parse_cycles_arg(None), Ok(LightCycleOp::Show));
        assert_eq!(parse_cycles_arg(Some("ON")), Ok(LightCycleOp::On));
        assert_eq!(parse_cycles_arg(Some("off")), Ok(LightCycleOp::Off));
        assert_eq!(parse_cycles_arg(Some("0")), Ok(LightCycleOp::Off));
        assert_eq!(parse_cycles_arg(Some("25")), Ok(LightCycleOp::Speed(25)));
        assert!(parse_cycles_arg(Some("61")).is_err());
        assert!(parse_cycles_arg(Some("fast")).is_err());
    }
}
