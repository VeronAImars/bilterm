//! Opt-in true-pixel TUI rendering for `:browser` and `:video` canvases (`Kitty APC G` and `iTerm2 OSC 1337`).
//!
//! Half-block (`▀`) rendering is the default because terminal environment variables are unreliable
//! over SSH, tmux, and nested multiplexers. Opt in explicitly via
//! `BILTERM_BROWSER_GFX=kitty|iterm2|auto`.
//!
//! Light `:browser` frames (`mean_luma > 0.5`) are remapped through
//! [`crate::ui::frame_sample::themed_pixel`] before pixel upload so users who rely on Bilterm's
//! theme-following colour contrast keep identical visual contrast in pixel mode.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::OnceLock;

use ratatui::layout::Rect;

use crate::ui::frame_sample;
use crate::ui::pixel_canvas::{base64_encode, wrap_tmux_dcs_passthrough};
use crate::wm::WindowManager;

/// Theme `(background, foreground)` RGB pair applied to light browser pages.
pub type PageThemeRgb = ((u8, u8, u8), (u8, u8, u8));

/// Host terminal pixel protocol used for TUI media canvases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiPixelProto {
    Kitty,
    ITerm2,
    HalfBlock,
}

impl TuiPixelProto {
    /// Resolves the active TUI media pixel protocol from `BILTERM_BROWSER_GFX` (cached once).
    ///
    /// Defaults to [`TuiPixelProto::HalfBlock`] unless the user opts in via
    /// `BILTERM_BROWSER_GFX=kitty|iterm2|auto`.
    pub fn detect() -> Self {
        static DETECTED: OnceLock<TuiPixelProto> = OnceLock::new();
        *DETECTED.get_or_init(|| Self::detect_from_env(|k| std::env::var(k).ok()))
    }

    pub(crate) fn detect_from_env<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let Some(ov) = get("BILTERM_BROWSER_GFX") else {
            return Self::HalfBlock;
        };
        match ov.trim().to_ascii_lowercase().as_str() {
            "kitty" | "apc" => Self::Kitty,
            "iterm" | "iterm2" | "osc1337" => Self::ITerm2,
            "auto" => Self::detect_terminal_env(&get),
            _ => Self::HalfBlock,
        }
    }

    fn detect_terminal_env<F>(get: &F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let term_prog = get("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
        let term = get("TERM").unwrap_or_default().to_ascii_lowercase();
        if get("KITTY_WINDOW_ID").is_some_and(|v| !v.is_empty())
            || get("GHOSTTY_RESOURCES_DIR").is_some_and(|v| !v.is_empty())
            || term_prog.contains("ghostty")
            || term_prog.contains("kitty")
            || term_prog.contains("wezterm")
            || term.contains("xterm-kitty")
            || term.contains("xterm-ghostty")
        {
            return Self::Kitty;
        }
        if term_prog.contains("iterm") || get("ITERM_SESSION_ID").is_some_and(|v| !v.is_empty()) {
            return Self::ITerm2;
        }
        Self::HalfBlock
    }

    pub fn is_true_pixel(self) -> bool {
        !matches!(self, Self::HalfBlock)
    }
}

/// Recorded visible media canvas placement collected during Ratatui's `draw()` pass.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MediaPlacement {
    image_id: u32,
    pane_id: usize,
    is_video: bool,
    rect: Rect,
    theme: Option<PageThemeRgb>,
}

thread_local! {
    static FRAME_PLACEMENTS: RefCell<Vec<MediaPlacement>> = const { RefCell::new(Vec::new()) };
    static TUI_FRAME_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// RAII guard that ensures `TUI_FRAME_ACTIVE` is cleared even if `terminal.draw(...)` returns `Err`.
pub struct TuiFrameGuard;

impl Drop for TuiFrameGuard {
    fn drop(&mut self) {
        TUI_FRAME_ACTIVE.with(|f| f.set(false));
    }
}

/// Marks the start of a real TUI `terminal.draw(...)` pass and clears collected placements.
pub fn begin_frame() -> TuiFrameGuard {
    TUI_FRAME_ACTIVE.with(|f| f.set(true));
    FRAME_PLACEMENTS.with(|p| p.borrow_mut().clear());
    TuiFrameGuard
}

/// `true` while inside a live TUI `App::run` draw pass with opt-in true-pixel rendering enabled.
pub fn use_true_pixel_for_frame() -> bool {
    TUI_FRAME_ACTIVE.with(std::cell::Cell::get) && TuiPixelProto::detect().is_true_pixel()
}

/// Records a visible browser or video canvas rect during `CompositePaneWidget::render`.
pub fn record_placement(
    pane_id: usize,
    is_video: bool,
    rect: Rect,
    theme: Option<PageThemeRgb>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let image_id = (if is_video { 920_000 } else { 910_000 }) + (pane_id as u32);
    FRAME_PLACEMENTS.with(|p| {
        p.borrow_mut().push(MediaPlacement { image_id, pane_id, is_video, rect, theme });
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameKey {
    rect: Rect,
    seq: u64,
    width_px: u16,
    height_px: u16,
    theme: Option<PageThemeRgb>,
    fallback_hash: u64,
}

/// Persistent placement cache across TUI frames to avoid redundant pixel uploads and clear stale images.
#[derive(Default)]
pub struct TuiMediaGfxState {
    active: BTreeMap<u32, FrameKey>,
}

fn frame_fingerprint(w: u16, h: u16, rgba: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in [w.to_le_bytes(), h.to_le_bytes()].into_iter().flatten() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let step = (rgba.len() / 2048).max(4) & !3;
    let mut i = 0;
    while i + 4 <= rgba.len() {
        let word = u32::from_le_bytes([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]);
        hash ^= u64::from(word);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += step.max(4);
    }
    hash ^ (rgba.len() as u64)
}

/// Remaps light browser pages (`mean_luma > 0.5`) onto `theme` via [`frame_sample::themed_pixel`],
/// matching the half-block renderer's contrast and colourblind accessibility guarantees.
pub(crate) fn apply_theme_rgba<'a>(
    width_px: u16,
    height_px: u16,
    rgba: &'a [u8],
    theme: Option<PageThemeRgb>,
) -> Cow<'a, [u8]> {
    let Some((bg, fg)) = theme else {
        return Cow::Borrowed(rgba);
    };
    let (fw, fh) = (width_px as usize, height_px as usize);
    if frame_sample::mean_luma(rgba, fw, fh) <= 0.5 {
        return Cow::Borrowed(rgba);
    }
    let mut mapped = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let [r, g, b] = frame_sample::themed_pixel([px[0], px[1], px[2]], bg, fg);
        mapped.extend_from_slice(&[r, g, b, px[3]]);
    }
    Cow::Owned(mapped)
}

/// Returns `true` when any modal, floating panel, dropdown, or stacked-window overlap would be
/// occluded by host-terminal graphics placements.
pub(crate) fn has_blocking_overlay(wm: &WindowManager) -> bool {
    let probe = Rect::new(0, 0, 120, 40);
    !matches!(wm.modal(), crate::wm::Modal::None)
        || wm.exit_guard_state().is_some()
        || wm.menubar_open_category().is_some()
        || wm.rename_dialog_open()
        || wm.is_quick_terminal_visible()
        || wm.active_pane().prompt().history_popup_open
        || wm.active_pane().pending_unsafe_paste().is_some()
        || wm.drawer_rect(probe).is_some()
        || wm.drawer_preview_rect(probe).is_some()
        || wm.mcp_console().panel_rect(probe).is_some()
        || wm.agents_window_rect(probe).is_some()
        || wm.agents_list_rect(probe).is_some()
        || wm.clipboard_window_rect(probe).is_some()
        || wm.traffic_monitor_rect(probe).is_some()
        || wm.fonts_window_rect(probe).is_some()
        || wm.command_window_rect(probe).is_some()
        || wm.daemon_window_rect(probe).is_some()
        || wm.script_studio_rect(probe).is_some()
        || (wm.layout_mode() == crate::wm::WorkspaceLayoutMode::Stacked && wm.pane_count() > 1)
}

/// Flushes Kitty `APC G` or iTerm2 `OSC 1337` pixel placements for visible browser/video panes
/// immediately after Ratatui finishes updating text cells.
pub fn flush_tui_media_gfx<W: Write>(
    wm: &WindowManager,
    out: &mut W,
    state: &mut TuiMediaGfxState,
) -> std::io::Result<()> {
    TUI_FRAME_ACTIVE.with(|f| f.set(false));
    let proto = TuiPixelProto::detect();
    let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
    flush_with_proto(wm, proto, in_tmux, out, state)
}

pub(crate) fn flush_with_proto<W: Write>(
    wm: &WindowManager,
    proto: TuiPixelProto,
    in_tmux: bool,
    out: &mut W,
    state: &mut TuiMediaGfxState,
) -> std::io::Result<()> {
    if !proto.is_true_pixel() {
        return Ok(());
    }
    let placements: Vec<MediaPlacement> = if has_blocking_overlay(wm) {
        Vec::new()
    } else {
        FRAME_PLACEMENTS.with(|p| p.borrow().clone())
    };

    let mut next_active = BTreeMap::new();
    let mut seq = String::new();

    // Remove images whose panes closed, hid behind an overlay, or moved.
    for (&old_id, &old_key) in &state.active {
        let still_same = placements.iter().any(|p| p.image_id == old_id && p.rect == old_key.rect);
        if !still_same && proto == TuiPixelProto::Kitty {
            let del = format!("\x1b_Ga=d,d=i,i={old_id},q=2\x1b\\");
            seq.push_str(&if in_tmux { wrap_tmux_dcs_passthrough(&del) } else { del });
        }
    }

    let mut moved_cursor = false;
    for p in &placements {
        let Some(pane) = wm.pane(p.pane_id) else { continue };
        let frame = if p.is_video {
            pane.video_canvas().and_then(|v| v.cached_frame())
        } else {
            pane.browser_canvas().and_then(|b| b.cached_frame())
        };
        let Some(f) = frame else { continue };
        let Some(expected) = (f.width_px as usize)
            .checked_mul(f.height_px as usize)
            .and_then(|px| px.checked_mul(4))
        else {
            continue;
        };
        if expected == 0 || f.rgba.len() < expected {
            continue;
        }
        let raw_pixels = &f.rgba[..expected];
        let key = FrameKey {
            rect: p.rect,
            seq: f.seq,
            width_px: f.width_px,
            height_px: f.height_px,
            theme: p.theme,
            fallback_hash: if f.seq == 0 {
                frame_fingerprint(f.width_px, f.height_px, raw_pixels)
            } else {
                0
            },
        };
        next_active.insert(p.image_id, key);
        if state.active.get(&p.image_id) == Some(&key) {
            continue;
        }
        let pixels = apply_theme_rgba(f.width_px, f.height_px, raw_pixels, p.theme);
        if !moved_cursor {
            // Save cursor (DECSC) so Ratatui's hardware cursor state is restored after emission.
            seq.push_str("\x1b7");
            moved_cursor = true;
        }
        seq.push_str(&format!(
            "\x1b[{};{}H",
            p.rect.y.saturating_add(1),
            p.rect.x.saturating_add(1)
        ));
        match proto {
            TuiPixelProto::Kitty => {
                encode_kitty_chunked_rgba(
                    &mut seq,
                    p.image_id,
                    f.width_px,
                    f.height_px,
                    p.rect.width,
                    p.rect.height,
                    &pixels,
                    in_tmux,
                );
            }
            TuiPixelProto::ITerm2 => {
                let Some(bmp) = encode_bmp_bgra(f.width_px, f.height_px, &pixels) else {
                    continue;
                };
                let b64 = base64_encode(&bmp);
                let raw = format!(
                    "\x1b]1337;File=inline=1;size={};width={};height={};preserveAspectRatio=0:{}\x07",
                    bmp.len(),
                    p.rect.width.max(1),
                    p.rect.height.max(1),
                    b64
                );
                seq.push_str(&if in_tmux { wrap_tmux_dcs_passthrough(&raw) } else { raw });
            }
            TuiPixelProto::HalfBlock => {}
        }
    }
    if moved_cursor {
        seq.push_str("\x1b8");
    }

    state.active = next_active;
    if !seq.is_empty() {
        out.write_all(seq.as_bytes())?;
        out.flush()?;
    }
    Ok(())
}

/// Clears all Kitty graphics placements on TUI shutdown so the host terminal is left clean.
pub fn clear_all_tui_media_gfx<W: Write>(out: &mut W) -> std::io::Result<()> {
    let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
    clear_all_with_proto(TuiPixelProto::detect(), in_tmux, out)
}

pub(crate) fn clear_all_with_proto<W: Write>(
    proto: TuiPixelProto,
    in_tmux: bool,
    out: &mut W,
) -> std::io::Result<()> {
    if proto == TuiPixelProto::Kitty {
        let del = "\x1b_Ga=d,d=a,q=2\x1b\\";
        let payload = if in_tmux { wrap_tmux_dcs_passthrough(del) } else { del.to_string() };
        out.write_all(payload.as_bytes())?;
        out.flush()?;
    }
    Ok(())
}

/// Encodes an RGBA frame using Kitty Graphics Protocol (`APC G`) with mandatory 4096-byte chunking.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_kitty_chunked_rgba(
    out: &mut String,
    image_id: u32,
    width_px: u16,
    height_px: u16,
    cols: u16,
    rows: u16,
    rgba: &[u8],
    in_tmux: bool,
) {
    let b64 = base64_encode(rgba);
    const CHUNK: usize = 4096;
    let bytes = b64.as_bytes();
    let total = bytes.len();
    let mut offset = 0usize;
    let mut first = true;
    while offset < total {
        let end = (offset + CHUNK).min(total);
        let more = usize::from(end < total);
        let slice = std::str::from_utf8(&bytes[offset..end]).unwrap_or("");
        let chunk = if first {
            first = false;
            format!(
                "\x1b_Ga=T,f=32,s={width_px},v={height_px},c={},r={},i={image_id},p=1,C=1,q=2,m={more};{slice}\x1b\\",
                cols.max(1),
                rows.max(1),
            )
        } else {
            format!("\x1b_Gm={more},q=2;{slice}\x1b\\")
        };
        if in_tmux {
            out.push_str(&wrap_tmux_dcs_passthrough(&chunk));
        } else {
            out.push_str(&chunk);
        }
        offset = end;
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
    // BITMAPINFOHEADER (40 bytes) — negative height = top-down row order
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
    fn default_detection_is_halfblock_and_opt_in_enables_pixels() {
        // Without BILTERM_BROWSER_GFX, terminal env vars must NEVER turn on pixel protocols.
        assert_eq!(TuiPixelProto::detect_from_env(|_| None), TuiPixelProto::HalfBlock);
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| (k == "TERM_PROGRAM").then(|| "ghostty".into())),
            TuiPixelProto::HalfBlock
        );
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| (k == "KITTY_WINDOW_ID").then(|| "1".into())),
            TuiPixelProto::HalfBlock
        );
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| (k == "ITERM_SESSION_ID").then(|| "w0t0".into())),
            TuiPixelProto::HalfBlock
        );

        // Explicit opt-in via BILTERM_BROWSER_GFX
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| (k == "BILTERM_BROWSER_GFX").then(|| "kitty".into())),
            TuiPixelProto::Kitty
        );
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| (k == "BILTERM_BROWSER_GFX").then(|| "iterm2".into())),
            TuiPixelProto::ITerm2
        );
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| match k {
                "BILTERM_BROWSER_GFX" => Some("auto".into()),
                "TERM_PROGRAM" => Some("ghostty".into()),
                _ => None,
            }),
            TuiPixelProto::Kitty
        );
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| match k {
                "BILTERM_BROWSER_GFX" => Some("auto".into()),
                "TERM_PROGRAM" => Some("iTerm.app".into()),
                _ => None,
            }),
            TuiPixelProto::ITerm2
        );
        assert_eq!(
            TuiPixelProto::detect_from_env(|k| match k {
                "BILTERM_BROWSER_GFX" => Some("halfblock".into()),
                "TERM_PROGRAM" => Some("ghostty".into()),
                _ => None,
            }),
            TuiPixelProto::HalfBlock
        );
    }

    #[test]
    fn light_browser_frames_pass_through_themed_pixel_mapping() {
        let theme = ((10u8, 20u8, 30u8), (200u8, 210u8, 220u8));
        // 4x1 light page: 3 white background pixels + 1 black text pixel (mean luma = 0.75 > 0.5).
        let light = [
            255, 255, 255, 255,
            255, 255, 255, 255,
            255, 255, 255, 255,
            0, 0, 0, 255,
        ];
        let mapped = apply_theme_rgba(4, 1, &light, Some(theme));
        assert_eq!(&mapped[0..4], &[10, 20, 30, 255], "white page pixel must map to theme bg");
        assert_eq!(&mapped[12..16], &[200, 210, 220, 255], "black text pixel must map to theme fg");

        // Dark page (mean luma <= 0.5) stays unmapped (borrowed zero-copy).
        let dark = [10, 15, 20, 255, 10, 15, 20, 255, 10, 15, 20, 255, 10, 15, 20, 255];
        let kept = apply_theme_rgba(4, 1, &dark, Some(theme));
        assert!(matches!(kept, Cow::Borrowed(_)));
        assert_eq!(&kept[..], &dark[..]);
    }

    #[test]
    fn kitty_chunked_encoder_splits_large_payloads_at_4096_and_wraps_tmux() {
        let rgba = vec![0x80u8; 64 * 64 * 4];
        let mut s = String::new();
        encode_kitty_chunked_rgba(&mut s, 910_001, 64, 64, 40, 20, &rgba, false);
        assert!(s.starts_with("\x1b_Ga=T,f=32,s=64,v=64,c=40,r=20,i=910001,p=1,C=1,q=2,m=1;"));
        assert!(s.ends_with("\x1b\\"));
        assert!(s.contains("\x1b_Gm=0,q=2;"));

        let mut tmux = String::new();
        encode_kitty_chunked_rgba(&mut tmux, 910_001, 2, 2, 10, 5, &[0u8; 16], true);
        assert!(tmux.starts_with("\x1bPtmux;"), "{tmux}");
        assert!(tmux.ends_with("\x1b\\"), "{tmux}");
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
    fn flush_emits_cursor_save_restore_dedups_by_seq_and_deletes_on_overlay_or_clear() {
        let mut wm = WindowManager::new_headless(30, 100);
        let id = wm.active_pane().id();
        wm.active_pane_mut().open_browser("https://example.test");
        assert!(wm.pane(id).unwrap().browser_canvas().unwrap().cached_frame().is_some());

        let mut state = TuiMediaGfxState::default();
        let rect = Rect::new(2, 3, 40, 15);
        let theme = Some(((18, 24, 36), (230, 235, 240)));
        {
            let _guard = begin_frame();
            record_placement(id, false, rect, theme);
        }
        assert!(!TUI_FRAME_ACTIVE.with(std::cell::Cell::get), "guard must clear TUI_FRAME_ACTIVE");

        let mut out = Vec::new();
        flush_with_proto(&wm, TuiPixelProto::Kitty, false, &mut out, &mut state).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b7\x1b[4;3H\x1b_Ga=T"), "{s}");
        assert!(s.ends_with("\x1b8"), "{s}");

        // Same seq + same rect + same theme -> zero bytes emitted
        let mut out2 = Vec::new();
        flush_with_proto(&wm, TuiPixelProto::Kitty, false, &mut out2, &mut state).unwrap();
        assert!(out2.is_empty(), "unchanged seq must not re-upload");

        // Theme change invalidates cache and re-uploads
        {
            let _guard = begin_frame();
            record_placement(id, false, rect, Some(((0, 0, 0), (0, 255, 0))));
        }
        let mut out_theme = Vec::new();
        flush_with_proto(&wm, TuiPixelProto::Kitty, false, &mut out_theme, &mut state).unwrap();
        assert!(!out_theme.is_empty(), "theme change must re-upload remapped pixel frame");

        // Opening a blocking overlay hides placements and deletes the Kitty image
        wm.toggle_drawer();
        assert!(has_blocking_overlay(&wm));
        let mut out3 = Vec::new();
        flush_with_proto(&wm, TuiPixelProto::Kitty, false, &mut out3, &mut state).unwrap();
        let s3 = String::from_utf8(out3).unwrap();
        assert!(s3.contains("\x1b_Ga=d,d=i,i="), "overlay must delete active Kitty image: {s3}");

        // Clear all emits Kitty delete-all (with tmux DCS wrap when in_tmux=true)
        let mut out4 = Vec::new();
        clear_all_with_proto(TuiPixelProto::Kitty, true, &mut out4).unwrap();
        let s4 = String::from_utf8(out4).unwrap();
        assert!(s4.starts_with("\x1bPtmux;") && s4.contains("a=d,d=a"), "{s4}");
    }
}
