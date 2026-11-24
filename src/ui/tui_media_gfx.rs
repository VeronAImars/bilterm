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
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use crate::ui::frame_sample;
use crate::ui::pixel_canvas::wrap_tmux_dcs_passthrough;
use crate::wm::WindowManager;

mod encode;
pub use encode::{MAX_FRAME_BYTES, frame_fits};
use encode::{encode_bmp_bgra, image_id, write_iterm2_image, write_kitty_chunked_rgba};

/// Minimum interval between pixel re-uploads of one placement whose geometry is unchanged.
pub const MIN_UPLOAD_INTERVAL: Duration = Duration::from_millis(50);

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
    /// Set once per frame in [`begin_frame`]: an overlay or the layout hides pixel placements.
    static FRAME_BLOCKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    #[cfg(test)]
    static PROTO_OVERRIDE: std::cell::Cell<Option<TuiPixelProto>> = const { std::cell::Cell::new(None) };
}

/// Active protocol; tests may override the process-wide detection per thread.
fn active_proto() -> TuiPixelProto {
    #[cfg(test)]
    if let Some(p) = PROTO_OVERRIDE.with(std::cell::Cell::get) {
        return p;
    }
    TuiPixelProto::detect()
}

/// RAII guard that ensures `TUI_FRAME_ACTIVE` is cleared even if `terminal.draw(...)` returns `Err`.
pub struct TuiFrameGuard;

impl Drop for TuiFrameGuard {
    fn drop(&mut self) {
        TUI_FRAME_ACTIVE.with(|f| f.set(false));
    }
}

/// Marks the start of a real TUI `terminal.draw(...)` pass and clears collected placements.
///
/// Decides once, for the whole frame, whether an overlay or the layout blocks pixel placements
/// (probing with the real `viewport`), so render and flush agree and blocked frames fall back to
/// half-block cells instead of blanking them.
pub fn begin_frame(wm: &WindowManager, viewport: Rect) -> TuiFrameGuard {
    TUI_FRAME_ACTIVE.with(|f| f.set(true));
    FRAME_BLOCKED.with(|f| f.set(has_blocking_overlay(wm, viewport)));
    FRAME_PLACEMENTS.with(|p| p.borrow_mut().clear());
    TuiFrameGuard
}

/// `true` while inside a live TUI draw pass with opt-in true-pixel rendering enabled and no
/// overlay or stacked layout hiding the placements.
pub fn use_true_pixel_for_frame() -> bool {
    TUI_FRAME_ACTIVE.with(std::cell::Cell::get)
        && !FRAME_BLOCKED.with(std::cell::Cell::get)
        && active_proto().is_true_pixel()
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
    let Some(image_id) = image_id(is_video, pane_id) else { return };
    FRAME_PLACEMENTS.with(|p| {
        p.borrow_mut().push(MediaPlacement { image_id, pane_id, is_video, rect, theme });
    });
}

/// `true` when `pane_id` can get a collision-free image id (otherwise render half-block).
pub fn pane_id_supported(pane_id: usize) -> bool {
    image_id(false, pane_id).is_some() && image_id(true, pane_id).is_some()
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

impl FrameKey {
    /// Same placement geometry and theme (only the pixel content changed).
    fn same_geometry(&self, other: &Self) -> bool {
        self.rect == other.rect
            && self.width_px == other.width_px
            && self.height_px == other.height_px
            && self.theme == other.theme
    }
}

#[derive(Clone, Copy, Debug)]
struct ActiveImage {
    key: FrameKey,
    uploaded_at: Instant,
}

/// Persistent placement cache across TUI frames to avoid redundant pixel uploads and clear stale images.
#[derive(Default)]
pub struct TuiMediaGfxState {
    active: BTreeMap<u32, ActiveImage>,
    repaint: bool,
}

impl TuiMediaGfxState {
    /// Returns (and clears) whether the host screen must be cleared and repainted because cells
    /// under a removed iTerm2 image were blanked behind Ratatui's back.
    pub fn take_repaint_request(&mut self) -> bool {
        std::mem::take(&mut self.repaint)
    }

    /// Removes every placement this state uploaded (Kitty `d=i` per tracked id, iTerm2 blank
    /// rects) so the host terminal is left clean without touching other programs' images.
    pub fn clear_all<W: Write>(&mut self, out: &mut W) -> std::io::Result<()> {
        let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
        self.clear_all_with_proto(active_proto(), in_tmux, out)
    }

    pub(crate) fn clear_all_with_proto<W: Write>(
        &mut self,
        proto: TuiPixelProto,
        in_tmux: bool,
        out: &mut W,
    ) -> std::io::Result<()> {
        let old = std::mem::take(&mut self.active);
        for (id, img) in &old {
            write_removal(out, proto, in_tmux, *id, img.key.rect)?;
        }
        out.flush()
    }
}

/// Emits the protocol-specific removal of one placement.
fn write_removal<W: Write>(
    out: &mut W,
    proto: TuiPixelProto,
    in_tmux: bool,
    id: u32,
    rect: Rect,
) -> std::io::Result<()> {
    match proto {
        TuiPixelProto::Kitty => {
            let del = format!("\x1b_Ga=d,d=i,i={id},q=2\x1b\\");
            out.write_all(if in_tmux { wrap_tmux_dcs_passthrough(&del) } else { del }.as_bytes())
        }
        TuiPixelProto::ITerm2 => {
            // Overwrite every cell of the old rect with blanks; iTerm2 drops the image per cell.
            out.write_all(b"\x1b7")?;
            let blank = " ".repeat(usize::from(rect.width));
            for y in rect.y..rect.y.saturating_add(rect.height) {
                write!(out, "\x1b[{};{}H{blank}", u32::from(y) + 1, u32::from(rect.x) + 1)?;
            }
            out.write_all(b"\x1b8")
        }
        TuiPixelProto::HalfBlock => Ok(()),
    }
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
/// occluded by host-terminal graphics placements. `probe` is the real viewport.
pub(crate) fn has_blocking_overlay(wm: &WindowManager, probe: Rect) -> bool {
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
/// immediately after Ratatui finishes updating text cells. Chunks are streamed to `out`.
pub fn flush_tui_media_gfx<W: Write>(
    wm: &WindowManager,
    out: &mut W,
    state: &mut TuiMediaGfxState,
) -> std::io::Result<()> {
    TUI_FRAME_ACTIVE.with(|f| f.set(false));
    let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
    flush_with_proto(wm, active_proto(), in_tmux, Instant::now(), out, state)
}

pub(crate) fn flush_with_proto<W: Write>(
    wm: &WindowManager,
    proto: TuiPixelProto,
    in_tmux: bool,
    now: Instant,
    out: &mut W,
    state: &mut TuiMediaGfxState,
) -> std::io::Result<()> {
    if !proto.is_true_pixel() {
        return Ok(());
    }
    // `begin_frame` decided once whether placements are hidden; render recorded none in that case.
    let placements: Vec<MediaPlacement> = if FRAME_BLOCKED.with(std::cell::Cell::get) {
        Vec::new()
    } else {
        FRAME_PLACEMENTS.with(|p| p.borrow().clone())
    };

    // Remove images whose panes closed, hid behind an overlay, or moved.
    let mut wrote = false;
    for (&old_id, old) in &state.active {
        let still_same = placements.iter().any(|p| p.image_id == old_id && p.rect == old.key.rect);
        if still_same {
            continue;
        }
        write_removal(out, proto, in_tmux, old_id, old.key.rect)?;
        wrote = true;
        if proto == TuiPixelProto::ITerm2 {
            state.repaint = true;
        }
    }
    if state.repaint {
        // Cells were blanked behind Ratatui's diff: the caller clears the screen and redraws, and
        // every image is re-uploaded on that next frame.
        state.active.clear();
        return out.flush();
    }

    let mut next_active = BTreeMap::new();
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
        if expected == 0 || expected > MAX_FRAME_BYTES || f.rgba.len() < expected {
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
        if let Some(prev) = state.active.get(&p.image_id) {
            let unchanged = prev.key == key;
            let throttled = prev.key.same_geometry(&key)
                && now.saturating_duration_since(prev.uploaded_at) < MIN_UPLOAD_INTERVAL;
            if unchanged || throttled {
                next_active.insert(p.image_id, *prev);
                continue;
            }
        }
        let pixels = apply_theme_rgba(f.width_px, f.height_px, raw_pixels, p.theme);
        let bmp = if proto == TuiPixelProto::ITerm2 {
            let Some(bmp) = encode_bmp_bgra(f.width_px, f.height_px, &pixels) else { continue };
            Some(bmp)
        } else {
            None
        };
        if !moved_cursor {
            // Save cursor (DECSC) so Ratatui's hardware cursor state is restored after emission.
            out.write_all(b"\x1b7")?;
            moved_cursor = true;
        }
        write!(out, "\x1b[{};{}H", p.rect.y.saturating_add(1), p.rect.x.saturating_add(1))?;
        match (proto, &bmp) {
            (TuiPixelProto::Kitty, _) => write_kitty_chunked_rgba(
                out,
                p.image_id,
                f.width_px,
                f.height_px,
                p.rect.width,
                p.rect.height,
                &pixels,
                in_tmux,
            )?,
            (TuiPixelProto::ITerm2, Some(bmp)) => {
                write_iterm2_image(out, bmp, p.rect.width, p.rect.height, in_tmux)?;
            }
            _ => {}
        }
        wrote = true;
        next_active.insert(p.image_id, ActiveImage { key, uploaded_at: now });
    }
    if moved_cursor {
        out.write_all(b"\x1b8")?;
    }

    state.active = next_active;
    if wrote {
        out.flush()?;
    }
    Ok(())
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


    fn kitty_gfx_wm() -> (WindowManager, usize) {
        let mut wm = WindowManager::new_headless(30, 100);
        let id = wm.active_pane().id();
        wm.active_pane_mut().open_browser("https://example.test");
        assert!(wm.pane(id).unwrap().browser_canvas().unwrap().cached_frame().is_some());
        (wm, id)
    }

    const VIEWPORT: Rect = Rect::new(0, 0, 100, 30);
    const THEME: Option<PageThemeRgb> = Some(((18, 24, 36), (230, 235, 240)));

    fn flush(
        wm: &WindowManager,
        proto: TuiPixelProto,
        now: Instant,
        state: &mut TuiMediaGfxState,
    ) -> String {
        let mut out = Vec::new();
        flush_with_proto(wm, proto, false, now, &mut out, state).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn flush_emits_cursor_save_restore_dedups_by_seq_and_deletes_on_overlay_or_clear() {
        let (mut wm, id) = kitty_gfx_wm();
        let mut state = TuiMediaGfxState::default();
        let rect = Rect::new(2, 3, 40, 15);
        let t0 = Instant::now();
        {
            let _guard = begin_frame(&wm, VIEWPORT);
            record_placement(id, false, rect, THEME);
        }
        assert!(!TUI_FRAME_ACTIVE.with(std::cell::Cell::get), "guard must clear TUI_FRAME_ACTIVE");

        let s = flush(&wm, TuiPixelProto::Kitty, t0, &mut state);
        assert!(s.starts_with("\x1b7\x1b[4;3H\x1b_Ga=T"), "{s}");
        assert!(s.ends_with("\x1b8"), "{s}");

        let s2 = flush(&wm, TuiPixelProto::Kitty, t0, &mut state);
        assert!(s2.is_empty(), "unchanged seq must not re-upload");

        // Theme change invalidates cache and re-uploads (even inside the throttle interval).
        {
            let _guard = begin_frame(&wm, VIEWPORT);
            record_placement(id, false, rect, Some(((0, 0, 0), (0, 255, 0))));
        }
        assert!(!flush(&wm, TuiPixelProto::Kitty, t0, &mut state).is_empty());

        // Opening a blocking overlay hides placements and deletes the Kitty image
        wm.toggle_drawer();
        {
            let _guard = begin_frame(&wm, VIEWPORT);
            record_placement(id, false, rect, THEME);
        }
        let s3 = flush(&wm, TuiPixelProto::Kitty, t0, &mut state);
        assert!(s3.contains("\x1b_Ga=d,d=i,i="), "overlay must delete active Kitty image: {s3}");
    }

    #[test]
    fn exit_clear_deletes_only_tracked_ids_never_delete_all() {
        let (wm, id) = kitty_gfx_wm();
        let mut state = TuiMediaGfxState::default();
        {
            let _guard = begin_frame(&wm, VIEWPORT);
            record_placement(id, false, Rect::new(0, 0, 10, 5), THEME);
        }
        flush(&wm, TuiPixelProto::Kitty, Instant::now(), &mut state);
        let mut out = Vec::new();
        state.clear_all_with_proto(TuiPixelProto::Kitty, true, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;") && s.contains("a=d,d=i,i="), "{s}");
        assert!(!s.contains("d=a"), "{s}");
        let mut again = Vec::new();
        state.clear_all_with_proto(TuiPixelProto::Kitty, false, &mut again).unwrap();
        assert!(again.is_empty(), "nothing tracked, nothing emitted");
    }

    #[test]
    fn second_seq_inside_interval_is_throttled_then_uploaded_later() {
        let (mut wm, id) = kitty_gfx_wm();
        let mut state = TuiMediaGfxState::default();
        let rect = Rect::new(0, 0, 20, 10);
        let t0 = Instant::now();
        let place = |wm: &WindowManager| {
            let _guard = begin_frame(wm, VIEWPORT);
            record_placement(id, false, rect, THEME);
        };
        let seq_of = |wm: &WindowManager| {
            wm.pane(id).unwrap().browser_canvas().unwrap().cached_frame().unwrap().seq
        };
        place(&wm);
        assert!(!flush(&wm, TuiPixelProto::Kitty, t0, &mut state).is_empty());
        let before = seq_of(&wm);
        wm.active_pane_mut().browser_canvas_mut().unwrap().bump_cached_frame_seq_for_test();
        assert_ne!(seq_of(&wm), before, "navigation must bump the frame seq");
        place(&wm);
        let soon = t0 + Duration::from_millis(10);
        assert!(flush(&wm, TuiPixelProto::Kitty, soon, &mut state).is_empty(), "inside interval");
        let later = t0 + MIN_UPLOAD_INTERVAL + Duration::from_millis(1);
        assert!(flush(&wm, TuiPixelProto::Kitty, later, &mut state).contains("a=T"));
    }

    #[test]
    fn geometry_change_bypasses_throttle() {
        let (wm, id) = kitty_gfx_wm();
        let mut state = TuiMediaGfxState::default();
        let t0 = Instant::now();
        for rect in [Rect::new(0, 0, 20, 10), Rect::new(0, 0, 30, 12)] {
            let _guard = begin_frame(&wm, VIEWPORT);
            record_placement(id, false, rect, THEME);
            drop(_guard);
            assert!(flush(&wm, TuiPixelProto::Kitty, t0, &mut state).contains("a=T"));
        }
    }

    #[test]
    fn iterm2_overlay_clears_old_rect_and_requests_repaint() {
        let (mut wm, id) = kitty_gfx_wm();
        let mut state = TuiMediaGfxState::default();
        let rect = Rect::new(2, 3, 6, 2);
        {
            let _guard = begin_frame(&wm, VIEWPORT);
            record_placement(id, false, rect, THEME);
        }
        let up = flush(&wm, TuiPixelProto::ITerm2, Instant::now(), &mut state);
        assert!(up.contains("\x1b]1337;File=inline=1;"), "{up}");
        assert!(!state.take_repaint_request());

        wm.toggle_drawer();
        {
            let _guard = begin_frame(&wm, VIEWPORT);
        }
        let s = flush(&wm, TuiPixelProto::ITerm2, Instant::now(), &mut state);
        assert!(s.contains("\x1b[4;3H      \x1b[5;3H      "), "old rect must be blanked: {s:?}");
        assert!(state.take_repaint_request(), "cells changed behind ratatui: repaint required");
        assert!(state.active.is_empty());

        // Exit also blanks tracked iTerm2 rects.
        let mut st2 = TuiMediaGfxState::default();
        st2.active.insert(
            image_id(false, 1).unwrap(),
            ActiveImage { key: FrameKey { rect, seq: 1, width_px: 1, height_px: 1, theme: None, fallback_hash: 0 }, uploaded_at: Instant::now() },
        );
        let mut out = Vec::new();
        st2.clear_all_with_proto(TuiPixelProto::ITerm2, false, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("\x1b[4;3H      "));
    }

    #[test]
    fn stacked_layout_and_modal_fall_back_to_halfblock_in_pixel_mode() {
        PROTO_OVERRIDE.with(|p| p.set(Some(TuiPixelProto::Kitty)));
        let (mut wm, _id) = kitty_gfx_wm();
        wm.active_pane_mut().prompt_mut().manual_raw_override = true;
        let render = |wm: &WindowManager| {
            let mut buf = ratatui::buffer::Buffer::empty(VIEWPORT);
            let _guard = begin_frame(wm, VIEWPORT);
            ratatui::widgets::Widget::render(crate::ui::WorkspaceWidget::new(wm), VIEWPORT, &mut buf);
            let pixel = use_true_pixel_for_frame();
            let halves = buf.content().iter().filter(|c| c.symbol() == "\u{2580}").count();
            (pixel, halves, FRAME_PLACEMENTS.with(|p| p.borrow().len()))
        };
        let (pixel, halves, placed) = render(&wm);
        assert!(pixel && placed == 1 && halves == 0, "unblocked frame uses pixels: {pixel} {placed} {halves}");

        wm.toggle_drawer();
        let (pixel, halves, placed) = render(&wm);
        assert!(!pixel && placed == 0 && halves > 0, "drawer open must render half-block");
        wm.toggle_drawer();

        wm.apply(crate::action::Action::SplitVertical).unwrap();
        wm.set_layout_mode(crate::wm::WorkspaceLayoutMode::Stacked);
        let blocked = has_blocking_overlay(&wm, VIEWPORT);
        assert!(blocked, "stacked with 2 panes blocks pixels");
        PROTO_OVERRIDE.with(|p| p.set(None));
    }

    #[test]
    fn oversized_frames_do_not_fit() {
        assert!(!frame_fits(4096, 4096));
        assert!(pane_id_supported(3));
        assert!(!pane_id_supported(usize::MAX));
    }
}
