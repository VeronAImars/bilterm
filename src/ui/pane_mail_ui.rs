//! Pane-mail envelope badge for a pane's title: closed (unread, blinking) or open (read).
//! Shared by the TUI and GUI pane-title paths (the GUI rasterises the same ratatui buffer).

use unicode_width::UnicodeWidthStr;

use crate::config::BiltermConfig;
use crate::ui::glyphs::GlyphPack;
use crate::wm::pane_mail::PaneMailView;

/// Unread envelope (U+2709, width 1).
pub const CLOSED: &str = "\u{2709}";
/// Read envelope (U+1F583).
pub const OPEN: &str = "\u{1F583}";
/// ASCII fallbacks used with the `ascii`/`cp437` glyph packs.
pub const CLOSED_ASCII: &str = "[M]";
pub const OPEN_ASCII: &str = "[m]";

/// Glyph for the state under `cfg`'s glyph pack.
pub fn glyph(cfg: &BiltermConfig, unread: bool) -> &'static str {
    let ascii = matches!(cfg.glyphs, GlyphPack::Ascii | GlyphPack::Cp437);
    match (ascii, unread) {
        (true, true) => CLOSED_ASCII,
        (true, false) => OPEN_ASCII,
        (false, true) => CLOSED,
        (false, false) => OPEN,
    }
}

/// Title badge text (no surrounding space), e.g. `✉3`, `🖃`; blanks of equal width during the
/// blink "off" phase so the title does not jitter.
pub fn badge(cfg: &BiltermConfig, view: &PaneMailView) -> String {
    let g = glyph(cfg, view.unread > 0);
    let text = if view.unread > 1 { format!("{g}{}", view.unread) } else { g.to_string() };
    if view.visible { text } else { " ".repeat(UnicodeWidthStr::width(text.as_str())) }
}

/// `title` with the badge appended (just the badge when the title is empty).
pub fn with_badge(title: &str, cfg: &BiltermConfig, view: &PaneMailView) -> String {
    let b = badge(cfg, view);
    if title.is_empty() { b } else { format!("{title} {b}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(unread: u32, visible: bool) -> PaneMailView {
        PaneMailView { unread, sender: "api".into(), visible }
    }

    #[test]
    fn closed_vs_open_and_counts() {
        let cfg = BiltermConfig::default();
        assert_eq!(badge(&cfg, &view(1, true)), "\u{2709}");
        assert_eq!(badge(&cfg, &view(3, true)), "\u{2709}3");
        assert_eq!(badge(&cfg, &view(0, true)), OPEN);
    }

    #[test]
    fn blink_off_keeps_width_and_ascii_fallback() {
        let mut cfg = BiltermConfig::default();
        let on = badge(&cfg, &view(12, true));
        let off = badge(&cfg, &view(12, false));
        assert!(off.trim().is_empty());
        assert_eq!(UnicodeWidthStr::width(on.as_str()), UnicodeWidthStr::width(off.as_str()));
        cfg.glyphs = GlyphPack::Ascii;
        assert_eq!(badge(&cfg, &view(1, true)), "[M]");
        assert_eq!(badge(&cfg, &view(0, true)), "[m]");
        assert!(badge(&cfg, &view(2, true)).is_ascii());
    }

    #[test]
    fn glyph_widths_are_known() {
        assert_eq!(UnicodeWidthStr::width(CLOSED), 1);
        assert_eq!(UnicodeWidthStr::width(OPEN), 1);
    }
}
