//! Pure window-size limits for the native GUI windows (Linux X11 today; usable by any backend).
//!
//! Without a minimum-size hint a window manager is free to shrink the window to a few pixels
//! (for example when a maximized window is un-maximized by double-clicking the title bar), leaving
//! it invisible. These helpers define the floor and the "sane size" used to recover from it.

/// Smallest usable client width in pixels.
pub const MIN_WINDOW_W_PX: u32 = 320;
/// Smallest usable client height in pixels.
pub const MIN_WINDOW_H_PX: u32 = 200;
/// Smallest usable grid, in cells (matches the viewport clamp used by the GUI loops).
pub const MIN_COLS: u32 = 20;
pub const MIN_ROWS: u32 = 6;
/// Initial / fallback window size in pixels.
pub const DEFAULT_WINDOW_PX: (u32, u32) = (1120, 720);

/// Minimum client size in pixels for the current cell size: at least `MIN_COLS x MIN_ROWS` cells
/// and never below the absolute pixel floor.
pub fn min_window_px(cell_w: u16, cell_h: u16) -> (u32, u32) {
    (
        MIN_WINDOW_W_PX.max(MIN_COLS * u32::from(cell_w.max(1))),
        MIN_WINDOW_H_PX.max(MIN_ROWS * u32::from(cell_h.max(1))),
    )
}

/// Clamps a requested size (zero or garbage included) up to the minimum.
pub fn clamp_window_px(w: u32, h: u32, cell_w: u16, cell_h: u16) -> (u32, u32) {
    let (mw, mh) = min_window_px(cell_w, cell_h);
    (w.max(mw), h.max(mh))
}

/// `true` when `(w, h)` is below the minimum on either axis and must be corrected.
pub fn is_below_min(w: u32, h: u32, cell_w: u16, cell_h: u16) -> bool {
    let (mw, mh) = min_window_px(cell_w, cell_h);
    w < mw || h < mh
}

/// Size to force the window back to after a too-small resize: the last size that was fine,
/// itself clamped (falls back to [`DEFAULT_WINDOW_PX`] when there is none).
pub fn recovery_size(last_good: Option<(u32, u32)>, cell_w: u16, cell_h: u16) -> (u32, u32) {
    let (w, h) = last_good.unwrap_or(DEFAULT_WINDOW_PX);
    clamp_window_px(w, h, cell_w, cell_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_is_pixel_minimum_for_small_cells() {
        assert_eq!(min_window_px(8, 16), (320, 200));
    }

    #[test]
    fn floor_grows_with_large_cells() {
        assert_eq!(min_window_px(24, 48), (480, 288));
    }

    #[test]
    fn zero_cell_size_does_not_panic_or_collapse() {
        assert_eq!(min_window_px(0, 0), (320, 200));
    }

    #[test]
    fn clamp_raises_tiny_and_zero_sizes() {
        assert_eq!(clamp_window_px(1, 1, 8, 16), (320, 200));
        assert_eq!(clamp_window_px(0, 0, 8, 16), (320, 200));
        assert_eq!(clamp_window_px(64, 900, 8, 16), (320, 900));
    }

    #[test]
    fn clamp_keeps_sane_sizes() {
        assert_eq!(clamp_window_px(1120, 720, 8, 16), (1120, 720));
    }

    #[test]
    fn below_min_detects_either_axis() {
        assert!(is_below_min(10, 10, 8, 16));
        assert!(is_below_min(1000, 100, 8, 16));
        assert!(is_below_min(100, 1000, 8, 16));
        assert!(!is_below_min(320, 200, 8, 16));
    }

    #[test]
    fn recovery_uses_last_good_clamped_or_default() {
        assert_eq!(recovery_size(Some((900, 600)), 8, 16), (900, 600));
        assert_eq!(recovery_size(Some((5, 5)), 8, 16), (320, 200));
        assert_eq!(recovery_size(None, 8, 16), DEFAULT_WINDOW_PX);
    }
}
