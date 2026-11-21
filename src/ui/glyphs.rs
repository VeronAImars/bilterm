use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

/// Configurable glyph pack preset (`UI-04`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum GlyphPack {
    /// Standard Unicode box-drawing + geometric dingbats (`╔═╗`, `[▲][▼]`, `[_][□][X]`, `◢`).
    #[default]
    Unicode,
    /// Classic IBM PC Code Page 437 symbols (`♦`, `▲`, `▼`, `■`, `↕`, `╬`).
    Cp437,
    /// Powerline + Nerd Font glyphs (``, ``, `│`, `[▲][▼]`).
    NerdFont,
    /// Pure 7-bit ASCII fallback (`[^][v]`, `[x]`, `[+]`, `/`).
    Ascii,
}

impl GlyphPack {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "unicode" | "default" => Some(Self::Unicode),
            "cp437" | "dos" | "ibm" => Some(Self::Cp437),
            "nerd" | "nerd-font" | "nerdfont" | "powerline" => Some(Self::NerdFont),
            "ascii" => Some(Self::Ascii),
            _ => None,
        }
    }

    pub fn glyphs(self) -> GlyphSet {
        match self {
            Self::Unicode => GlyphSet {
                pin_top_bottom: "[▲][▼]",
                minimize_badge: "[_]",
                close_badge: "[X]",
                zoom_badge: "[□]",
                resize_corner: "◢",
                oia_separator: "│",
                prompt_prefix: "==> ",
                prompt_continuation: "... ",
                status_ready_icon: "●",
                status_run_icon: "⟳",
                status_lock_icon: "♦",
                status_alt_icon: "▣",
                status_scroll_icon: "⇳",
                output_len_icon: "≣",
                cursor_pos_icon: "⌖",
                drag_grip: "⠿",
                drag_grip_hot: "⣿",
                zone_idle: ["┄", "┆", "┌", "┐", "└", "┘"],
                zone_hot: ["━", "┃", "┏", "┓", "┗", "┛"],
                zone_arrows: ["◀", "▶", "▲", "▼"],
                zone_ok: "✔",
                zone_no: "✘",
            },
            Self::Cp437 => GlyphSet {
                pin_top_bottom: "[▲][▼]",
                minimize_badge: "[_]",
                close_badge: "[X]",
                zoom_badge: "[↑]",
                resize_corner: "╬",
                oia_separator: "║",
                prompt_prefix: "♦=> ",
                prompt_continuation: "··· ",
                status_ready_icon: "♦",
                status_run_icon: "►",
                status_lock_icon: "‼",
                status_alt_icon: "■",
                status_scroll_icon: "↕",
                output_len_icon: "≡",
                cursor_pos_icon: "┼",
                drag_grip: "≡",
                drag_grip_hot: "☰",
                zone_idle: ["─", "│", "┌", "┐", "└", "┘"],
                zone_hot: ["▓", "▓", "▓", "▓", "▓", "▓"],
                zone_arrows: ["◄", "►", "▲", "▼"],
                zone_ok: "√",
                zone_no: "×",
            },
            Self::NerdFont => GlyphSet {
                pin_top_bottom: "[▲][▼]",
                minimize_badge: "[_]",
                close_badge: "[X]",
                zoom_badge: "[□]",
                resize_corner: "◢",
                oia_separator: "",
                prompt_prefix: "❯=> ",
                prompt_continuation: "... ",
                status_ready_icon: "✔",
                status_run_icon: "⚙",
                status_lock_icon: "",
                status_alt_icon: "",
                status_scroll_icon: "⇳",
                output_len_icon: "☰",
                cursor_pos_icon: "⌖",
                drag_grip: "⠿",
                drag_grip_hot: "⣿",
                zone_idle: ["┄", "┆", "┌", "┐", "└", "┘"],
                zone_hot: ["━", "┃", "┏", "┓", "┗", "┛"],
                zone_arrows: ["◀", "▶", "▲", "▼"],
                zone_ok: "✔",
                zone_no: "✘",
            },
            Self::Ascii => GlyphSet {
                pin_top_bottom: "[^][v]",
                minimize_badge: "[_]",
                close_badge: "[X]",
                zoom_badge: "[+]",
                resize_corner: "/",
                oia_separator: "|",
                prompt_prefix: "==> ",
                prompt_continuation: "... ",
                status_ready_icon: "*",
                status_run_icon: "~",
                status_lock_icon: "!",
                status_alt_icon: "#",
                status_scroll_icon: "^",
                output_len_icon: "=",
                cursor_pos_icon: "@",
                drag_grip: "=",
                drag_grip_hot: "#",
                zone_idle: [".", ":", ".", ".", ".", "."],
                zone_hot: ["#", "#", "#", "#", "#", "#"],
                zone_arrows: ["<", ">", "^", "v"],
                zone_ok: "+",
                zone_no: "x",
            },
        }
    }
}

/// Resolved glyph tokens guaranteed to maintain deterministic terminal cell widths (`UI-04`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphSet {
    pub pin_top_bottom: &'static str,
    pub minimize_badge: &'static str,
    pub close_badge: &'static str,
    pub zoom_badge: &'static str,
    pub resize_corner: &'static str,
    pub oia_separator: &'static str,
    pub prompt_prefix: &'static str,
    pub prompt_continuation: &'static str,
    pub status_ready_icon: &'static str,
    pub status_run_icon: &'static str,
    pub status_lock_icon: &'static str,
    pub status_alt_icon: &'static str,
    pub status_scroll_icon: &'static str,
    pub output_len_icon: &'static str,
    pub cursor_pos_icon: &'static str,
    /// Title-bar drag handle (1 cell) and its hover/dragging form.
    pub drag_grip: &'static str,
    pub drag_grip_hot: &'static str,
    /// Drop-zone border cells `[horizontal, vertical, top-left, top-right, bottom-left, bottom-right]`:
    /// idle zones, and the zone under the pointer (a different line weight, never colour alone).
    pub zone_idle: [&'static str; 6],
    pub zone_hot: [&'static str; 6],
    /// Edge arrows `[left, right, up, down]`, drop accepted / refused marks.
    pub zone_arrows: [&'static str; 4],
    pub zone_ok: &'static str,
    pub zone_no: &'static str,
}

impl Default for GlyphSet {
    fn default() -> Self {
        GlyphPack::default().glyphs()
    }
}

impl GlyphSet {
    /// Verifies that every badge in the `GlyphSet` has the exact expected column width.
    pub fn validate_cell_widths(&self) -> bool {
        UnicodeWidthStr::width(self.pin_top_bottom) == 6
            && UnicodeWidthStr::width(self.minimize_badge) == 3
            && UnicodeWidthStr::width(self.close_badge) == 3
            && UnicodeWidthStr::width(self.zoom_badge) == 3
            && UnicodeWidthStr::width(self.resize_corner) == 1
            && UnicodeWidthStr::width(self.oia_separator) == 1
            && UnicodeWidthStr::width(self.prompt_prefix) == 4
            && UnicodeWidthStr::width(self.prompt_continuation) == 4
            && UnicodeWidthStr::width(self.drag_grip) == 1
            && UnicodeWidthStr::width(self.drag_grip_hot) == 1
            && self.zone_idle.iter().chain(&self.zone_hot).chain(&self.zone_arrows).all(|g| UnicodeWidthStr::width(*g) == 1)
            && UnicodeWidthStr::width(self.zone_ok) == 1
            && UnicodeWidthStr::width(self.zone_no) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_glyph_packs_preserve_strict_cell_width_alignment() {
        for pack in [
            GlyphPack::Unicode,
            GlyphPack::Cp437,
            GlyphPack::NerdFont,
            GlyphPack::Ascii,
        ] {
            assert!(
                pack.glyphs().validate_cell_widths(),
                "GlyphPack {pack:?} failed cell-width validation"
            );
        }
    }
}
