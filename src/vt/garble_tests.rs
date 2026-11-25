//! Garble investigation 2026-11-24 (agy / Gemini CLI pane): width agreement for ○ ● ⚠ ┃ ⎿ ─,
//! partial line rewrites, pending-wrap + CUU/CR/EL in-place repaint, DEC 2026, DECSC/DECRC,
//! DECSTBM/RI spinners, and chunk-split invariance. Each test feeds bytes to `VtParser` and
//! compares the grid with an expected one.

use super::*;
use unicode_width::UnicodeWidthStr;

fn row_text(vt: &VtParser, r: u16) -> String {
    let (_, cols) = vt.size();
    let mut s = String::new();
    for c in 0..cols {
        if let Some(cell) = vt.cell_view(r, c) {
            if cell.width == 0 {
                continue;
            }
            if cell.grapheme.is_empty() { s.push(' ') } else { s.push_str(cell.grapheme) }
        }
    }
    s
}

fn grid(vt: &VtParser) -> Vec<String> {
    (0..vt.size().0).map(|r| row_text(vt, r)).collect()
}

fn trimmed(vt: &VtParser) -> Vec<String> {
    grid(vt).into_iter().map(|s| s.trim_end().to_string()).collect()
}

#[test]
fn h1_engine_width_agrees_with_unicode_width_for_agent_glyphs() {
    for g in ["○", "●", "⚠", "┃", "⎿", "─"] {
        let mut vt = VtParser::new(4, 40, 100);
        vt.process(format!("{g}X").as_bytes());
        let uw = UnicodeWidthStr::width(g);
        let (_, col) = vt.cursor_position();
        eprintln!("{g:?} unicode_width={uw} engine cursor col after glyph+X = {col}");
        assert_eq!(col as usize, uw + 1, "glyph {g:?}: engine advance disagrees with unicode_width (renderer's width)");
    }
}

/// KNOWN FAILING (confirmed): VS16 emoji presentation. The engine advances 1 column for
/// U+26A0 U+FE0F while `unicode_width` (used by the ratatui diff renderer) says 2.
#[test]
#[ignore = "KNOWN FAILING: engine ignores VS16 width, unicode_width/ratatui says 2 (garble 2026-11-24)"]
fn h1_vs16_emoji_presentation_width_disagreement() {
    for g in ["⚠\u{FE0F}", "❤\u{FE0F}", "✔\u{FE0F}"] {
        let mut vt = VtParser::new(4, 40, 100);
        vt.process(format!("{g}X").as_bytes());
        let cell0 = vt.cell_view(0, 0).unwrap();
        let (_, col) = vt.cursor_position();
        eprintln!("{g:?} engine cell0 width={} grapheme={:?} cursor={col} unicode_width={}", cell0.width, cell0.grapheme, UnicodeWidthStr::width(g));
        assert_eq!(col as usize, UnicodeWidthStr::width(g) + 1, "{g:?}");
    }
}

#[test]
fn h1_rewrite_after_warning_glyph_lands_at_expected_columns() {
    // "⚠ Statusline" then CHA/CUP rewrite of the tail.
    let mut vt = VtParser::new(4, 60, 100);
    vt.process("\u{26A0} Statusline Error\x1b[5GXY\x1b[K".as_bytes());
    assert_eq!(trimmed(&vt)[0], "\u{26A0} StXY");
}

#[test]
fn h2_chunked_line_with_el_ech_cha_rep_leaves_no_gap() {
    // "Waiting fo" ... later "r sound-studio" rewritten via CHA, with stale tail cleared.
    let mut vt = VtParser::new(4, 120, 100);
    vt.process("  android-tests passed (34/34 tests). Waiting fo\x1b[K".as_bytes());
    vt.process("\x1b[1G\x1b[2K  android-tests passed (34/34 tests). Waiting for sound-studio and android-ui".as_bytes());
    vt.process(b"\x1b[3Ptail\x1b[5X\x1b[2b\x1b[K");
    let t = trimmed(&vt);
    assert!(t[0].starts_with("  android-tests passed (34/34 tests). Waiting for sound-studio and android-ui"), "{:?}", t[0]);
    assert!(!t[0].contains("  fo  "), "{:?}", t[0]);
    // Bare chunks: "Waiting fo" then (next write) "r s" must be adjacent without any cursor move.
    let mut vt = VtParser::new(4, 120, 100);
    vt.process(b"Waiting fo");
    vt.process(b"r s");
    vt.process(b"ound-stu");
    vt.process(b"dio");
    assert_eq!(trimmed(&vt)[0], "Waiting for sound-studio");
}

fn spinner_frames(cols: usize, rows_above: usize, frames: usize, extra_pre: &str, extra_post: &str) -> (VtParser, Vec<Vec<String>>) {
    let mut vt = VtParser::new(24, cols as u16, 1000);
    let mut s = String::new();
    for i in 0..rows_above {
        s.push_str(&format!("row {i}\r\n"));
    }
    // Rule exactly pane width (pending wrap at last column), then a spinner line, then prompt.
    s.push_str(&"─".repeat(cols));
    s.push_str("\r\n");
    s.push_str("spinner 0\r\n> ");
    vt.process(s.as_bytes());
    let base_scrolled = vt.total_lines_scrolled();
    let mut snaps = Vec::new();
    for f in 0..frames {
        // Spinner line is 2 rows up from the prompt row; rule exactly width is redrawn too.
        let mut b = String::new();
        b.push_str(extra_pre);
        b.push_str("\x1b[1A\r\x1b[2K");
        b.push_str(&format!("spinner {}", f + 1));
        b.push_str("\x1b[1A\r\x1b[2K");
        b.push_str(&"─".repeat(cols)); // exactly pane width, wrap pending
        b.push_str("\x1b[2B\r\x1b[2K> ");
        b.push_str(extra_post);
        vt.process(b.as_bytes());
        snaps.push(trimmed(&vt));
        assert_eq!(vt.total_lines_scrolled(), base_scrolled, "frame {f}: grid scrolled during in-place repaint");
    }
    (vt, snaps)
}

#[test]
fn h5_exact_width_rule_then_cuu_cr_el_never_scrolls() {
    let (vt, snaps) = spinner_frames(80, 18, 20, "", "");
    for (i, s) in snaps.iter().enumerate() {
        assert_eq!(s[0], "row 0", "frame {i}: top row scrolled off");
        // Only the spinner text differs between frames.
        assert_eq!(s.iter().filter(|l| l.starts_with("spinner")).count(), 1, "frame {i}: {s:?}");
    }
    assert_eq!(vt.cursor_position().0, snaps[0].iter().position(|l| l == ">").unwrap() as u16);
}

#[test]
fn h5_pending_wrap_is_deferred_not_early() {
    let mut vt = VtParser::new(5, 10, 100);
    vt.process("0123456789".as_bytes());
    assert_eq!(vt.cursor_position().0, 0, "wrap must stay pending after writing the last column");
    vt.process(b"\r\n");
    assert_eq!(vt.cursor_position().0, 1, "CRLF after full row: exactly one row advance");
    vt.process("0123456789\x1b[1A\r\x1b[2K".as_bytes());
    assert_eq!(vt.cursor_position().0, 0);
    vt.process("0123456789X".as_bytes());
    assert_eq!(trimmed(&vt)[0], "0123456789");
    assert_eq!(trimmed(&vt)[1], "X123456789", "11th char wraps to next row col 0");
}

#[test]
fn h6_sync_output_and_decsc_decrc_spinner_loop_is_stable() {
    let (_, a) = spinner_frames(80, 18, 10, "\x1b[?2026h\x1b7", "\x1b8\x1b[?2026l");
    let (_, b) = spinner_frames(80, 18, 10, "\x1b[?2026h\x1b[s", "\x1b[u\x1b[?2026l");
    assert_eq!(a[0][0], "row 0");
    assert_eq!(b[0][0], "row 0");
    // Save/restore with the spinner rewrite inside must come back to the prompt row.
    let mut vt = VtParser::new(6, 20, 100);
    vt.process(b"a\r\nb\r\n> \x1b7\x1b[2A\r\x1b[2Kspin\x1b8x");
    assert_eq!(trimmed(&vt), ["spin", "b", "> x", "", "", ""].map(String::from));
    vt.process(b"\x1b[s\x1b[2A\r\x1b[2Kz\x1b[u!");
    assert_eq!(trimmed(&vt), ["z", "b", "> x!", "", "", ""].map(String::from), "CSI s/u without 2026");
    vt.process(b"\x1b[?2026h\x1b[s\x1b[2A\r\x1b[2Ky\x1b[u?\x1b[?2026l");
    assert!(!vt.is_synchronized_output());
    assert_eq!(trimmed(&vt), ["y", "b", "> x!?", "", "", ""].map(String::from), "CSI s/u inside 2026");
}

#[test]
fn h8_decstbm_and_reverse_index_spinner_do_not_scroll_outside_region() {
    let mut vt = VtParser::new(10, 30, 100);
    for i in 0..10 {
        vt.process(format!("\x1b[{};1Hrow{i}", i + 1).as_bytes());
    }
    let before = vt.total_lines_scrolled();
    // Region rows 4..6; RI at region top scrolls region down only.
    vt.process(b"\x1b[4;6r\x1b[4;1H\x1bMNEW\x1b[r");
    let t = trimmed(&vt);
    assert_eq!(t[0], "row0");
    assert_eq!(t[2], "row2");
    assert_eq!(t[3], "NEW");
    assert_eq!(t[4], "row3");
    assert_eq!(t[5], "row4");
    assert_eq!(t[6], "row6", "row below region untouched");
    assert_eq!(t[9], "row9");
    assert_eq!(vt.total_lines_scrolled(), before, "region scroll must not push into scrollback");
}

#[test]
fn h7_agent_drawing_wider_than_pane_wraps_and_resize_back_is_consistent() {
    // Documents expected terminal behaviour: a 120-col rule in an 80-col pane wraps to 2 rows.
    let mut vt = VtParser::new(6, 80, 100);
    vt.process(format!("{}\r\nnext", "─".repeat(120)).as_bytes());
    let t = trimmed(&vt);
    assert_eq!(t[0].chars().count(), 80);
    assert_eq!(t[1].chars().count(), 40);
    // Agent redraws after the pane is resized to 120: grid must not keep stale wrapped tail.
    vt.set_size(6, 120);
    vt.process(b"\x1b[H\x1b[J");
    vt.process(format!("{}\r\nnext", "─".repeat(120)).as_bytes());
    let t = trimmed(&vt);
    assert_eq!(t[0].chars().count(), 120);
    assert_eq!(t[1], "next");
}

#[test]
fn h4_agent_glyph_stream_is_split_invariant() {
    let s = "\u{25CB} \u{25CF} Messaged 1 subagent\r\n\u{26A0} Statusline Error\r\n  \u{23BF}  \u{26A0} command failed\r\n \u{2503} self needs approval\r\n\x1b[2A\r\x1b[2K\u{25CF} x\x1b[3G\u{26A0}\x1b[K\x1b[2B\r\u{2500}\u{2500}".as_bytes();
    let whole = {
        let mut vt = VtParser::new(8, 40, 100);
        vt.process(s);
        (grid(&vt), vt.cursor_position())
    };
    for n in 1..=7 {
        let mut vt = VtParser::new(8, 40, 100);
        for c in s.chunks(n) {
            vt.process(c);
        }
        assert_eq!(whole, (grid(&vt), vt.cursor_position()), "chunk size {n}");
    }
}
