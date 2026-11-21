//! Session-find `flash`: a reversed bright border on a matched pane for a moment.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

/// Repaints the border cells of `outer` in reverse video (bright yellow), keeping the glyphs.
pub fn highlight_border(buf: &mut Buffer, outer: Rect) {
    let area = buf.area;
    let clip = outer.intersection(area);
    if clip.width < 2 || clip.height < 2 {
        return;
    }
    let (x1, y1) = (clip.x + clip.width - 1, clip.y + clip.height - 1);
    for x in clip.x..=x1 {
        for y in [clip.y, y1] {
            mark(buf, x, y);
        }
    }
    for y in clip.y..=y1 {
        for x in [clip.x, x1] {
            mark(buf, x, y);
        }
    }
}

fn mark(buf: &mut Buffer, x: u16, y: u16) {
    let cell = &mut buf[(x, y)];
    cell.set_fg(Color::Black);
    cell.set_bg(Color::Yellow);
    cell.modifier.insert(Modifier::BOLD);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_is_highlighted_and_interior_is_not() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        highlight_border(&mut buf, Rect::new(1, 1, 6, 4));
        assert_eq!(buf[(1, 1)].bg, Color::Yellow);
        assert_eq!(buf[(6, 4)].bg, Color::Yellow);
        assert_ne!(buf[(3, 2)].bg, Color::Yellow);
    }
}
