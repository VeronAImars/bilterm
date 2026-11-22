//! Incremental host-terminal output must equal a full repaint: frames are fed through ratatui's
//! `Buffer::diff` into a model terminal (wide glyphs cover two columns, a write into either half
//! of a wide glyph destroys it) and the result is compared with the final frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use bilterm::ui::WorkspaceWidget;
use bilterm::wm::WindowManager;

struct Rng(u64);
impl Rng {
    fn n(&mut self, m: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % m
    }
}

fn stream(r: &mut Rng) -> Vec<u8> {
    let words = ["hello", "日本語", "한국어", "中文字符", "😀", "👍🏽", "e\u{301}", "❯", "⏺", "⏺\u{FE0F}", "⎿", "✻", "·", "…", "│", "世界abc", "ｱｲｳ", "Ａ", "🇯🇵", "👨\u{200d}👩\u{200d}👧", "❤\u{fe0f}"];
    let csis = ["A", "B", "C", "D", "G", "K", "2K", "1G", "3A", "2B", "5C", "4D", "3P", "2@", "4X", "m", "1m", "0m", "7m", "?25l", "?25h", "38;2;215;24;58m"];
    let mut out = Vec::new();
    for _ in 0..(5 + r.n(20)) {
        match r.n(10) {
            0..=3 => out.extend_from_slice(words[r.n(words.len())].as_bytes()),
            4..=6 => {
                out.extend_from_slice(b"\x1b[");
                out.extend_from_slice(csis[r.n(csis.len())].as_bytes());
            }
            7 => out.extend_from_slice([&b"\r"[..], b"\n", b"\r\n", b"\x08"][r.n(4)]),
            _ => out.extend_from_slice(b"abc def "),
        }
    }
    out
}

/// Model screen: one entry per column; `None` = right half of a wide glyph.
fn blank(w: u16, h: u16) -> Vec<Vec<Option<String>>> {
    vec![vec![Some(" ".to_string()); w as usize]; h as usize]
}

fn put(screen: &mut [Vec<Option<String>>], x: u16, y: u16, sym: &str) {
    let row = &mut screen[y as usize];
    let x = x as usize;
    let w = UnicodeWidthStr::width(sym).clamp(1, 2);
    // Overwriting the right half of a wide glyph erases the glyph.
    if row[x].is_none() && x > 0 {
        row[x - 1] = Some(" ".to_string());
    }
    // Overwriting the left half of a wide glyph blanks its right half.
    if let Some(old) = &row[x]
        && UnicodeWidthStr::width(old.as_str()) == 2
        && x + 1 < row.len()
        && row[x + 1].is_none()
    {
        row[x + 1] = Some(" ".to_string());
    }
    row[x] = Some(sym.to_string());
    if w == 2 && x + 1 < row.len() {
        // The right half covers whatever was there; if that was a wide glyph's left half its
        // right half becomes a blank.
        if let Some(Some(s)) = row.get(x + 1)
            && UnicodeWidthStr::width(s.as_str()) == 2
            && x + 2 < row.len()
        {
            row[x + 2] = Some(" ".to_string());
        }
        row[x + 1] = None;
    }
}

fn expected(buf: &Buffer) -> Vec<Vec<Option<String>>> {
    let a = buf.area;
    let mut s = blank(a.width, a.height);
    for y in 0..a.height {
        let mut skip = 0;
        for x in 0..a.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            put(&mut s, x, y, sym);
            skip = UnicodeWidthStr::width(sym).saturating_sub(1);
        }
    }
    s
}

#[test]
fn incremental_updates_equal_a_full_repaint_with_wide_glyphs() {
    let area = Rect::new(0, 0, 100, 30);
    for seed in 0..150u64 {
        let mut wm = WindowManager::new_headless(100, 30);
        let id = wm.compute_workspace_rects(area)[0].0;
        let mut r = Rng(seed + 1);
        let mut prev = Buffer::empty(area);
        let mut screen = expected(&prev);
        for frame in 0..8 {
            let b = stream(&mut r);
            wm.pane_mut(id).unwrap().process_pty_output(&b);
            let mut cur = Buffer::empty(area);
            WorkspaceWidget::new(&wm).render(area, &mut cur);
            for (x, y, cell) in prev.diff(&cur) {
                put(&mut screen, x, y, cell.symbol());
            }
            let want = expected(&cur);
            if screen != want {
                let mut msgs = Vec::new();
                for (y, (a, b)) in screen.iter().zip(&want).enumerate() {
                    for (x, (c, d)) in a.iter().zip(b).enumerate() {
                        if c != d {
                            msgs.push(format!("({x},{y}) incremental {c:?} vs full {d:?}; prev {:?} cur {:?}", prev[(x as u16, y as u16)].symbol(), cur[(x as u16, y as u16)].symbol()));
                        }
                    }
                }
                msgs.truncate(8);
                panic!("seed {seed} frame {frame}: incremental != full repaint: {msgs:#?}");
            }
            prev = cur;
        }
    }
}
