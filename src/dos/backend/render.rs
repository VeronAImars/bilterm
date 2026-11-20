//! [`TextScreen`] -> pane grid. The renderer emits a bounded ANSI byte stream (24-bit colour
//! SGR, absolute cursor positioning) that the existing pane VT engine paints, so a DOS pane
//! reuses the normal grid, selection, recording and theming path. Only rows that changed
//! since the previous frame are re-emitted.

use super::screen::{TextCell, TextScreen};

/// CGA / VGA text palette (index 0..=15) as RGB.
pub const CGA_PALETTE: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0x00, 0x00, 0xAA),
    (0x00, 0xAA, 0x00),
    (0x00, 0xAA, 0xAA),
    (0xAA, 0x00, 0x00),
    (0xAA, 0x00, 0xAA),
    (0xAA, 0x55, 0x00),
    (0xAA, 0xAA, 0xAA),
    (0x55, 0x55, 0x55),
    (0x55, 0x55, 0xFF),
    (0x55, 0xFF, 0x55),
    (0x55, 0xFF, 0xFF),
    (0xFF, 0x55, 0x55),
    (0xFF, 0x55, 0xFF),
    (0xFF, 0xFF, 0x55),
    (0xFF, 0xFF, 0xFF),
];

/// Upper bound on one frame (25 rows x 80 cells x worst-case SGR + UTF-8, with headroom).
pub const MAX_FRAME_BYTES: usize = 128 * 1024;

pub fn fg_rgb(c: &TextCell) -> (u8, u8, u8) {
    CGA_PALETTE[(c.fg & 0x0f) as usize]
}

pub fn bg_rgb(c: &TextCell) -> (u8, u8, u8) {
    CGA_PALETTE[(c.bg & 0x07) as usize]
}

fn same_style(a: &TextCell, b: &TextCell) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.blink == b.blink
}

fn push_sgr(out: &mut Vec<u8>, c: &TextCell) {
    let (fr, fgc, fb) = fg_rgb(c);
    let (br, bgc, bb) = bg_rgb(c);
    let blink = if c.blink { "5" } else { "25" };
    out.extend_from_slice(
        format!("\x1b[0;{blink};38;2;{fr};{fgc};{fb};48;2;{br};{bgc};{bb}m").as_bytes(),
    );
}

fn row(s: &TextScreen, r: usize) -> &[TextCell] {
    let w = s.cols as usize;
    &s.cells[r * w..(r + 1) * w]
}

/// Renders `cur`; when `prev` has identical geometry only differing rows are emitted.
/// The result is empty when nothing changed. Output is capped at [`MAX_FRAME_BYTES`].
pub fn render_ansi(cur: &TextScreen, prev: Option<&TextScreen>) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 * 1024);
    let comparable = prev.filter(|p| p.cols == cur.cols && p.rows == cur.rows);
    if comparable.is_none() {
        out.extend_from_slice(b"\x1b[0m\x1b[2J");
    }
    let mut last: Option<TextCell> = None;
    for r in 0..cur.rows as usize {
        if let Some(p) = comparable
            && row(p, r) == row(cur, r)
        {
            continue;
        }
        out.extend_from_slice(format!("\x1b[{};1H", r + 1).as_bytes());
        for cell in row(cur, r) {
            if last.as_ref().is_none_or(|l| !same_style(l, cell)) {
                push_sgr(&mut out, cell);
                last = Some(*cell);
            }
            let mut b = [0u8; 4];
            out.extend_from_slice(cell.ch.encode_utf8(&mut b).as_bytes());
        }
        if out.len() >= MAX_FRAME_BYTES {
            break;
        }
    }
    let cursor_changed = comparable.is_none_or(|p| {
        p.cursor != cur.cursor || p.cursor_visible != cur.cursor_visible || !out.is_empty()
    });
    if cursor_changed {
        match (cur.cursor, cur.cursor_visible) {
            (Some((c, r)), true) => {
                out.extend_from_slice(format!("\x1b[0m\x1b[{};{}H\x1b[?25h", r + 1, c + 1).as_bytes())
            }
            _ => out.extend_from_slice(b"\x1b[0m\x1b[?25l"),
        }
    }
    out.truncate(MAX_FRAME_BYTES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dos::backend::screen::{BDA_LEN, TEXT_BUF_LEN};

    fn paint(frames: &[Vec<u8>]) -> vt100_ctt::Parser {
        let mut p = vt100_ctt::Parser::new(25, 80, 0);
        for f in frames {
            p.process(f);
        }
        p
    }

    fn fixture(name: &str) -> TextScreen {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/dos/backend/fixtures/");
        let vga = std::fs::read(format!("{dir}{name}.vga")).unwrap();
        let bda = std::fs::read(format!("{dir}{name}.bda")).unwrap();
        assert_eq!((vga.len(), bda.len()), (TEXT_BUF_LEN, BDA_LEN));
        TextScreen::from_vga(&vga, Some(&bda)).unwrap()
    }

    #[test]
    fn palette_is_cga() {
        assert_eq!(CGA_PALETTE[6], (0xAA, 0x55, 0x00)); // brown, not dark yellow
        assert_eq!(CGA_PALETTE[7], (0xAA, 0xAA, 0xAA));
        assert_eq!(CGA_PALETTE[15], (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn grid_matches_screen_for_real_guest_fixtures() {
        for name in ["boot", "installer"] {
            let s = fixture(name);
            let p = paint(&[render_ansi(&s, None)]);
            let g = p.screen();
            for r in 0..25u16 {
                for c in 0..80u16 {
                    let want = s.cell(c, r).unwrap();
                    let got = g.cell(r, c).unwrap();
                    assert_eq!(got.contents().trim(), want.ch.to_string().trim(), "{name} {c},{r}");
                    let (fr, fg, fb) = fg_rgb(want);
                    if want.ch != ' ' {
                        assert_eq!(got.fgcolor(), vt100_ctt::Color::Rgb(fr, fg, fb), "{name} {c},{r}");
                    }
                    let (br, bg, bb) = bg_rgb(want);
                    assert_eq!(got.bgcolor(), vt100_ctt::Color::Rgb(br, bg, bb), "{name} {c},{r}");
                }
            }
        }
    }

    #[test]
    fn installer_fixture_text_and_hidden_cursor() {
        let s = fixture("installer");
        assert!(s.contains("What is your preferred language?"));
        assert!(!s.cursor_visible, "installer hides the cursor (BDA 0x461 bit 5)");
        assert!(!s.odd_geometry);
        let p = paint(&[render_ansi(&s, None)]);
        assert!(p.screen().hide_cursor());
        let b = fixture("boot");
        assert!(b.cursor_visible);
        assert_eq!(b.cursor, Some((0, 24)));
        let p = paint(&[render_ansi(&b, None)]);
        assert!(!p.screen().hide_cursor());
        assert_eq!(p.screen().cursor_position(), (24, 0));
    }

    #[test]
    fn golden_ansi_for_small_screen() {
        let mut s = TextScreen::blank();
        s.cells[0] = TextCell::from_raw(b'H', 0x1f);
        s.cells[1] = TextCell::from_raw(0xC9, 0x1f);
        s.cursor = Some((2, 0));
        let out = String::from_utf8(render_ansi(&s, None)).unwrap();
        assert!(out.starts_with("\x1b[0m\x1b[2J\x1b[1;1H"));
        assert!(out.contains("\x1b[0;25;38;2;255;255;255;48;2;0;0;170mH╔"));
        assert!(out.ends_with("\x1b[0m\x1b[1;3H\x1b[?25h"));
    }

    #[test]
    fn diff_emits_only_changed_rows_and_nothing_when_equal() {
        let a = fixture("boot");
        assert!(render_ansi(&a, Some(&a)).is_empty());
        let mut b = a.clone();
        b.cells[3 * 80 + 5] = TextCell::from_raw(b'Z', 0x07);
        let d = String::from_utf8(render_ansi(&b, Some(&a))).unwrap();
        assert_eq!(d.matches("H").count() >= 1, true);
        assert!(d.contains("\x1b[4;1H"));
        assert!(!d.contains("\x1b[5;1H"));
        assert!(!d.contains("\x1b[2J"));
        let p = paint(&[render_ansi(&a, None), render_ansi(&b, Some(&a))]);
        assert_eq!(p.screen().cell(3, 5).unwrap().contents(), "Z");
        // cursor-only change still emits a (tiny) frame
        let mut c = a.clone();
        c.cursor = Some((9, 9));
        assert!(!render_ansi(&c, Some(&a)).is_empty());
    }

    #[test]
    fn blink_attribute_sets_and_clears() {
        let mut s = TextScreen::blank();
        s.cells[0] = TextCell::from_raw(b'B', 0x87);
        let out = String::from_utf8(render_ansi(&s, None)).unwrap();
        assert!(out.contains("\x1b[0;5;38;2;170;170;170;48;2;0;0;0mB"));
    }

    #[test]
    fn frame_is_bounded_for_hostile_guest_memory() {
        // Alternate every attribute so every cell needs a fresh SGR.
        let mut raw = vec![0u8; TEXT_BUF_LEN];
        for (i, p) in raw.chunks_exact_mut(2).enumerate() {
            p[0] = 0xB0 + (i % 3) as u8;
            p[1] = (i % 256) as u8;
        }
        let s = TextScreen::from_vga(&raw, None).unwrap();
        let out = render_ansi(&s, None);
        assert!(out.len() <= MAX_FRAME_BYTES);
        assert!(out.len() > 1000);
    }

    #[test]
    fn graphics_and_odd_geometry_show_notice() {
        let mut bda = vec![0u8; BDA_LEN];
        bda[0x49] = 0x13;
        let g = TextScreen::from_vga(&vec![0xFF; TEXT_BUF_LEN], Some(&bda)).unwrap();
        let d = g.for_display();
        assert!(d.contains("graphics mode 0x13"));
        let mut bda = vec![0u8; BDA_LEN];
        bda[0x49] = 3;
        bda[0x4a] = 80;
        bda[0x84] = 49; // 50 rows
        let o = TextScreen::from_vga(&vec![0; TEXT_BUF_LEN], Some(&bda)).unwrap();
        assert!(o.odd_geometry);
        assert!(o.for_display().contains("not 80x25"));
        bda[0x84] = 24;
        assert!(!TextScreen::from_vga(&vec![0; TEXT_BUF_LEN], Some(&bda)).unwrap().odd_geometry);
    }
}
