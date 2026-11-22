//! Chunking invariance: the grid must depend on the byte stream only, never on how PTY reads
//! split it (UTF-8 multibyte, wide CJK, emoji, combining marks, Claude-Code-like redraws).

use super::*;

fn dump(vt: &VtParser) -> String {
    let (rows, cols) = vt.size();
    let mut s = String::new();
    for r in 0..rows {
        for c in 0..cols {
            if let Some(cell) = vt.cell_view(r, c) {
                s.push_str(&format!("{}|{}|{:?}|{:?};", cell.grapheme, cell.width, cell.fg, cell.bg));
            }
        }
        s.push('\n');
    }
    s.push_str(&format!("{:?}", vt.cursor_position()));
    s
}

fn run(chunks: &[&[u8]]) -> String {
    let mut vt = VtParser::new(8, 40, 100);
    for c in chunks {
        vt.process(c);
    }
    dump(&vt)
}

fn assert_split_invariant(stream: &[u8]) {
    let whole = run(&[stream]);
    for i in 1..stream.len() {
        assert_eq!(whole, run(&[&stream[..i], &stream[i..]]), "two-way split at {i}");
    }
    let bytewise: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(whole, run(&bytewise), "byte-at-a-time");
    for n in 2..=7 {
        let ch: Vec<&[u8]> = stream.chunks(n).collect();
        assert_eq!(whole, run(&ch), "chunks of {n}");
    }
}

#[test]
fn cjk_emoji_combining_are_split_invariant() {
    assert_split_invariant("日本語のテキスト 한국어 中文字符\r\nemoji 😀👍🏽 e\u{301}a\u{308} ⏺\u{FE0F} ❯ ⎿\r\n".as_bytes());
}

#[test]
fn claude_like_prompt_redraw_is_split_invariant() {
    let mut s = Vec::new();
    s.extend_from_slice("╭──────────────╮\r\n│ > 日本語を入力 │\r\n╰──────────────╯\r\n".as_bytes());
    for i in 0..3 {
        s.extend_from_slice(b"\x1b[2A\r\x1b[2K");
        s.extend_from_slice(format!("\x1b[38;2;215;24;58m│\x1b[0m \x1b[1m> 日本語{i}を入力\x1b[22m 中文").as_bytes());
        s.extend_from_slice(b"\x1b[K\x1b[2B\r\x1b[?25l\x1b[3A\x1b[5Gx\x1b[?25h\x1b[2B");
    }
    assert_split_invariant(&s);
}

#[test]
fn wide_chars_after_cursor_motion_are_split_invariant() {
    assert_split_invariant("abc\x1b[3D日本\x1b[2C語\x1b[1G\x1b[K中\x1b[2P文\x1b[3@字".as_bytes());
}

#[test]
fn flags_and_zwj_sequences_are_split_invariant() {
    assert_split_invariant("a🇯🇵b 👨\u{200d}👩\u{200d}👧 c❤\u{fe0f}d ✔\u{fe0f}e\r\n🇺🇸🇩🇪x".as_bytes());
}

#[test]
fn live_grid_equals_same_size_rebuild_for_flags_zwj_and_cjk() {
    let stream = "a🇯🇵b 👨\u{200d}👩\u{200d}👧 c❤\u{fe0f}d 中文 ｱｲ e\u{301}\r\n🇺🇸🇩🇪x\r\nyy".as_bytes();
    let mut vt = VtParser::new(8, 40, 100);
    vt.process(stream);
    let live = dump(&vt);
    vt.set_size(8, 41);
    vt.set_size(8, 40);
    let rebuilt = dump(&vt);
    assert_eq!(live, rebuilt);
}
