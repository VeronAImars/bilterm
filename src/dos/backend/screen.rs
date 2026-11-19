//! VGA text-buffer (0xB8000) decoding: 80x25 (char, attribute) cells, CP437.

use super::{BackendError, GUEST_COLS, GUEST_ROWS, Result};

pub const TEXT_BUF_ADDR: u64 = 0xB8000;
pub const TEXT_BUF_LEN: usize = GUEST_COLS as usize * GUEST_ROWS as usize * 2;

/// BIOS Data Area page (0x400..0x500): video mode at 0x449, cursor at 0x450 (col, row).
pub const BDA_ADDR: u64 = 0x400;
pub const BDA_LEN: usize = 0x100;

const CP437_LOW: &str = "\u{20}☺☻♥♦♣♠•◘○◙♂♀♪♫☼►◄↕‼¶§▬↨↑↓→←∟↔▲▼";
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

/// CP437 byte to Unicode. Byte 0 (and NUL cells) render as a space.
pub fn cp437(b: u8) -> char {
    match b {
        0 => ' ',
        1..=0x1f => CP437_LOW.chars().nth(b as usize).unwrap_or(' '),
        0x20..=0x7e => b as char,
        0x7f => '⌂',
        _ => CP437_HIGH.chars().nth(b as usize - 0x80).unwrap_or(' '),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextCell {
    pub ch: char,
    /// Foreground 0..=15 (CGA palette index).
    pub fg: u8,
    /// Background 0..=7; the blink/bright-bg bit is `blink`.
    pub bg: u8,
    pub blink: bool,
}

impl TextCell {
    pub fn from_raw(ch: u8, attr: u8) -> Self {
        Self {
            ch: cp437(ch),
            fg: attr & 0x0f,
            bg: (attr >> 4) & 0x07,
            blink: attr & 0x80 != 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextScreen {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<TextCell>,
    /// (col, row) when known from the BIOS data area.
    pub cursor: Option<(u16, u16)>,
    /// BIOS video mode (BDA 0x449) when known.
    pub video_mode: Option<u8>,
}

impl TextScreen {
    pub fn blank() -> Self {
        Self {
            cols: GUEST_COLS,
            rows: GUEST_ROWS,
            cells: vec![TextCell::from_raw(b' ', 0x07); GUEST_COLS as usize * GUEST_ROWS as usize],
            cursor: Some((0, 0)),
            video_mode: Some(3),
        }
    }

    /// Decodes a 4000-byte text buffer. `bda` (the 256-byte BIOS data area) adds cursor and mode.
    pub fn from_vga(raw: &[u8], bda: Option<&[u8]>) -> Result<Self> {
        if raw.len() != TEXT_BUF_LEN {
            return Err(BackendError::Qmp(format!(
                "text buffer is {} bytes, expected {TEXT_BUF_LEN}",
                raw.len()
            )));
        }
        let cells = raw
            .chunks_exact(2)
            .map(|p| TextCell::from_raw(p[0], p[1]))
            .collect();
        let mut s = Self {
            cols: GUEST_COLS,
            rows: GUEST_ROWS,
            cells,
            cursor: None,
            video_mode: None,
        };
        if let Some(b) = bda.filter(|b| b.len() >= 0x52) {
            s.video_mode = Some(b[0x49]);
            let (c, r) = (b[0x50] as u16, b[0x51] as u16);
            if c < s.cols && r < s.rows {
                s.cursor = Some((c, r));
            }
        }
        Ok(s)
    }

    /// True for the text modes this buffer is meaningful in (0x2, 0x3, 0x7). Unknown counts as text.
    pub fn is_text_mode(&self) -> bool {
        self.video_mode.is_none_or(|m| matches!(m, 2 | 3 | 7))
    }

    pub fn cell(&self, col: u16, row: u16) -> Option<&TextCell> {
        (col < self.cols && row < self.rows)
            .then(|| &self.cells[row as usize * self.cols as usize + col as usize])
    }

    /// Rows as strings with trailing spaces trimmed.
    pub fn lines(&self) -> Vec<String> {
        self.cells
            .chunks(self.cols as usize)
            .map(|r| {
                r.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.lines().iter().any(|l| l.contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_tables_are_complete() {
        assert_eq!(CP437_LOW.chars().count(), 32);
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(cp437(0xC9), '╔');
        assert_eq!(cp437(0xCD), '═');
        assert_eq!(cp437(0xBB), '╗');
        assert_eq!(cp437(0xDB), '█');
        assert_eq!(cp437(0x80), 'Ç');
        assert_eq!(cp437(0x41), 'A');
        assert_eq!(cp437(0x03), '♥');
        assert_eq!(cp437(0), ' ');
    }

    #[test]
    fn decodes_golden_buffer() {
        let mut raw = vec![0u8; TEXT_BUF_LEN];
        for (i, b) in b"FreeCom".iter().enumerate() {
            raw[i * 2] = *b;
            raw[i * 2 + 1] = 0x1f;
        }
        raw[7 * 2] = 0xC9;
        raw[7 * 2 + 1] = 0x8e;
        let mut bda = vec![0u8; BDA_LEN];
        bda[0x49] = 3;
        bda[0x50] = 7;
        bda[0x51] = 0;
        let s = TextScreen::from_vga(&raw, Some(&bda)).unwrap();
        assert!(s.contains("FreeCom"));
        assert_eq!(s.cell(0, 0).unwrap().fg, 0x0f);
        assert_eq!(s.cell(0, 0).unwrap().bg, 1);
        let box_cell = s.cell(7, 0).unwrap();
        assert_eq!(box_cell.ch, '╔');
        assert!(box_cell.blink);
        assert_eq!(box_cell.fg, 0x0e);
        assert_eq!(s.cursor, Some((7, 0)));
        assert!(s.is_text_mode());
        assert_eq!(s.lines().len(), 25);
    }

    #[test]
    fn rejects_wrong_length_and_detects_graphics() {
        assert!(TextScreen::from_vga(&[0; 10], None).is_err());
        let mut bda = vec![0u8; BDA_LEN];
        bda[0x49] = 0x13;
        bda[0x50] = 200; // out of range cursor ignored
        let s = TextScreen::from_vga(&vec![0; TEXT_BUF_LEN], Some(&bda)).unwrap();
        assert!(!s.is_text_mode());
        assert_eq!(s.cursor, None);
        assert!(
            TextScreen::from_vga(&vec![0; TEXT_BUF_LEN], None)
                .unwrap()
                .is_text_mode()
        );
    }
}
