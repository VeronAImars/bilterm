//! Area-averaged sampling of browser/video frames for the cell grid and GUI textures.
//!
//! A page is rendered at ~10x20 px per cell, so a cell covers a whole block of source pixels.
//! Picking one pixel per cell (nearest neighbour) drops thin strokes such as text entirely and
//! shows noise; averaging the block keeps them as a darker smudge.

/// Averages the source block `[x0, x1) x [y0, y1)` (at least 1x1, clamped to the frame) into RGB.
/// Large blocks are sampled on a stride so a frame costs a bounded number of reads.
pub fn average_block(rgba: &[u8], fw: usize, fh: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> [u8; 3] {
    let x0 = x0.min(fw.saturating_sub(1));
    let y0 = y0.min(fh.saturating_sub(1));
    let x1 = x1.clamp(x0 + 1, fw);
    let y1 = y1.clamp(y0 + 1, fh);
    let step_x = ((x1 - x0) / 6).max(1);
    let step_y = ((y1 - y0) / 6).max(1);
    let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let i = (y * fw + x) * 4;
            r += u32::from(rgba[i]);
            g += u32::from(rgba[i + 1]);
            b += u32::from(rgba[i + 2]);
            n += 1;
            x += step_x;
        }
        y += step_y;
    }
    [(r / n) as u8, (g / n) as u8, (b / n) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20x20 white frame with a black 1-px vertical line at x = 3.
    fn line_frame() -> Vec<u8> {
        let mut f = vec![255u8; 20 * 20 * 4];
        for y in 0..20 {
            let i = (y * 20 + 3) * 4;
            f[i..i + 3].copy_from_slice(&[0, 0, 0]);
        }
        f
    }

    #[test]
    fn thin_strokes_darken_the_block_instead_of_vanishing() {
        let f = line_frame();
        let hit = average_block(&f, 20, 20, 0, 0, 10, 10);
        assert!(hit[0] < 250, "a stroke inside the block must show, got {hit:?}");
        let miss = average_block(&f, 20, 20, 10, 0, 20, 10);
        assert_eq!(miss, [255, 255, 255]);
    }

    #[test]
    fn degenerate_blocks_still_read_one_pixel() {
        let f = line_frame();
        assert_eq!(average_block(&f, 20, 20, 3, 0, 3, 0), [0, 0, 0]);
        assert_eq!(average_block(&f, 20, 20, 99, 99, 120, 120), [255, 255, 255]);
    }
}
