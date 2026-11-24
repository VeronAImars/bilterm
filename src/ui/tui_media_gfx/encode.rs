//! Streaming encoders, image ids and size caps for the TUI true-pixel layer.

use std::io::{self, Write};

use crate::ui::pixel_canvas::{base64_encode, wrap_tmux_dcs_passthrough};

/// Largest RGBA frame (bytes) uploaded as true pixels; larger frames fall back to half-block.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Raw bytes per base64 chunk: 3072 raw bytes encode to exactly 4096 base64 characters.
const RAW_PER_CHUNK: usize = 3072;

const BROWSER_ID_BASE: u32 = 0x1000_0000;
const VIDEO_ID_BASE: u32 = 0x2000_0000;

/// `true` when a `width_px x height_px` RGBA frame is within [`MAX_FRAME_BYTES`].
pub fn frame_fits(width_px: u16, height_px: u16) -> bool {
    let bytes = usize::from(width_px) * usize::from(height_px) * 4;
    bytes > 0 && bytes <= MAX_FRAME_BYTES
}

/// Collision-free Kitty image id: browser and video panes use disjoint id spaces and a pane id
/// that does not fit its space yields `None` (the pane then renders as half-block).
pub(crate) fn image_id(is_video: bool, pane_id: usize) -> Option<u32> {
    let pane = u32::try_from(pane_id).ok().filter(|p| *p < BROWSER_ID_BASE)?;
    let base = if is_video { VIDEO_ID_BASE } else { BROWSER_ID_BASE };
    base.checked_add(pane)
}

fn write_wrapped<W: Write>(out: &mut W, seq: &str, in_tmux: bool) -> io::Result<()> {
    if in_tmux {
        out.write_all(wrap_tmux_dcs_passthrough(seq).as_bytes())
    } else {
        out.write_all(seq.as_bytes())
    }
}

/// Streams a Kitty Graphics Protocol (`APC G`) upload of an RGBA frame, one 4096-char chunk at a
/// time, without ever materialising the whole base64 payload.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_kitty_chunked_rgba<W: Write>(
    out: &mut W,
    image_id: u32,
    width_px: u16,
    height_px: u16,
    cols: u16,
    rows: u16,
    rgba: &[u8],
    in_tmux: bool,
) -> io::Result<()> {
    let mut chunks = rgba.chunks(RAW_PER_CHUNK).peekable();
    let mut first = true;
    while let Some(raw) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        let b64 = base64_encode(raw);
        let chunk = if first {
            first = false;
            format!(
                "\x1b_Ga=T,f=32,s={width_px},v={height_px},c={},r={},i={image_id},p=1,C=1,q=2,m={more};{b64}\x1b\\",
                cols.max(1),
                rows.max(1),
            )
        } else {
            format!("\x1b_Gm={more},q=2;{b64}\x1b\\")
        };
        write_wrapped(out, &chunk, in_tmux)?;
    }
    Ok(())
}

/// Streams an iTerm2 `OSC 1337` inline image carrying `bmp`, base64-encoded chunk by chunk.
pub(crate) fn write_iterm2_image<W: Write>(
    out: &mut W,
    bmp: &[u8],
    cols: u16,
    rows: u16,
    in_tmux: bool,
) -> io::Result<()> {
    let head = format!(
        "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=0:",
        bmp.len(),
        cols.max(1),
        rows.max(1),
    );
    if in_tmux {
        out.write_all(b"\x1bPtmux;")?;
        out.write_all(head.replace('\x1b', "\x1b\x1b").as_bytes())?;
    } else {
        out.write_all(head.as_bytes())?;
    }
    for raw in bmp.chunks(RAW_PER_CHUNK) {
        out.write_all(base64_encode(raw).as_bytes())?;
    }
    if in_tmux {
        out.write_all(b"\x07\x1b\\")
    } else {
        out.write_all(b"\x07")
    }
}

/// Encodes an RGBA frame as a 32-bit top-down BMP (zero dependencies, natively decoded by iTerm2/NSImage).
pub(crate) fn encode_bmp_bgra(width: u16, height: u16, rgba: &[u8]) -> Option<Vec<u8>> {
    let w = u32::from(width);
    let h = u32::from(height);
    let pixels = w.checked_mul(h)?;
    let img_bytes = pixels.checked_mul(4)?;
    let file_size = 54u32.checked_add(img_bytes)?;
    if rgba.len() < (img_bytes as usize) {
        return None;
    }
    let mut buf = Vec::with_capacity(file_size as usize);
    // BITMAPFILEHEADER (14 bytes)
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER (40 bytes) - negative height = top-down row order
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&(w as i32).to_le_bytes());
    buf.extend_from_slice(&(-(h as i32)).to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&32u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&img_bytes.to_le_bytes());
    buf.extend_from_slice(&2835u32.to_le_bytes());
    buf.extend_from_slice(&2835u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    for px in rgba.chunks_exact(4).take(pixels as usize) {
        buf.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_stream_splits_at_4096_with_correct_m_flags_and_wraps_tmux() {
        let rgba = vec![0x80u8; 64 * 64 * 4];
        let mut out = Vec::new();
        write_kitty_chunked_rgba(&mut out, 7, 64, 64, 40, 20, &rgba, false).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,f=32,s=64,v=64,c=40,r=20,i=7,p=1,C=1,q=2,m=1;"));
        let chunks: Vec<&str> = s.split("\x1b\\").filter(|c| !c.is_empty()).collect();
        assert!(chunks.len() > 2);
        for (i, c) in chunks.iter().enumerate() {
            let last = i == chunks.len() - 1;
            assert!(c.contains(if last { "m=0" } else { "m=1" }), "chunk {i}: {c}");
            let payload = c.split_once(';').unwrap().1;
            assert!(payload.len() <= 4096 && (last || payload.len() == 4096));
        }
        let mut tmux = Vec::new();
        write_kitty_chunked_rgba(&mut tmux, 7, 2, 2, 10, 5, &[0u8; 16], true).unwrap();
        let t = String::from_utf8(tmux).unwrap();
        assert!(t.starts_with("\x1bPtmux;\x1b\x1b_Ga=T"), "ESC must be doubled: {t}");
        assert!(t.ends_with("\x1b\x1b\\\x1b\\"), "{t}");
    }

    #[test]
    fn iterm2_stream_is_wellformed_plain_and_in_tmux() {
        let bmp = encode_bmp_bgra(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        let mut plain = Vec::new();
        write_iterm2_image(&mut plain, &bmp, 10, 4, false).unwrap();
        let p = String::from_utf8(plain).unwrap();
        assert!(p.starts_with("\x1b]1337;File=inline=1;size=62;width=10;height=4;"));
        assert!(p.ends_with('\x07'));
        let mut tmux = Vec::new();
        write_iterm2_image(&mut tmux, &bmp, 10, 4, true).unwrap();
        let t = String::from_utf8(tmux).unwrap();
        assert!(t.starts_with("\x1bPtmux;\x1b\x1b]1337;"), "{t}");
        assert!(t.ends_with("\x07\x1b\\"), "{t}");
    }

    #[test]
    fn bmp_encoder_writes_valid_header_and_rejects_short_rgba() {
        let bmp = encode_bmp_bgra(2, 1, &[10, 20, 30, 255, 40, 50, 60, 255]).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 8);
        assert_eq!(&bmp[54..58], &[30, 20, 10, 255]);
        assert!(encode_bmp_bgra(4, 4, &[0u8; 8]).is_none());
    }

    #[test]
    fn image_ids_are_disjoint_and_never_panic() {
        assert_ne!(image_id(false, 10_000), image_id(true, 0));
        assert_ne!(image_id(false, 5), image_id(true, 5));
        assert_eq!(image_id(false, usize::MAX), None);
        assert_eq!(image_id(true, u32::MAX as usize), None);
        assert!(image_id(true, 0x0fff_ffff).is_some());
    }

    #[test]
    fn frame_cap_rejects_oversized_frames() {
        assert!(frame_fits(1280, 720));
        assert!(!frame_fits(u16::MAX, u16::MAX));
        assert!(!frame_fits(0, 10));
    }
}
