//! Row highlight styles shared by the Classic and Navigator drawer renderers.
//!
//! Two distinct-but-consistent marks, both derived from [`ThemePalette`] roles (no literal colours):
//! * **Selected** (keyboard cursor / clicked row): the strong mark. Modern look: `title_fg` on a
//!   `window_frame_bg` -> `active_border_fg` blend, bold. Classic look (or a palette whose colours cannot
//!   be blended): reverse video.
//! * **Hover** (mouse pointer): the light mark. A faint `active_border_fg` tint plus an underline, never
//!   reverse video, so it can not be mistaken for the selection.
//!
//! Policy: last input wins. Any selection change clears the hover row (`WorkspaceDrawer::hover_row`), and a
//! pointer move sets it again. A hovered row that is also the selected row shows the selected mark.

use ratatui::style::{Modifier, Style};

use crate::ui::design::tokens::blend;
use crate::ui::theme::ThemePalette;

/// How a drawer row is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowMark {
    Plain,
    Hover,
    Selected,
}

impl RowMark {
    /// Selection beats hover; `None` row indices never match.
    pub fn of(row: usize, selected: usize, hover: Option<usize>) -> Self {
        if row == selected {
            Self::Selected
        } else if hover == Some(row) {
            Self::Hover
        } else {
            Self::Plain
        }
    }
}

/// `true` when selection of this palette is drawn as a coloured bar rather than reverse video.
pub fn selection_is_bar(palette: &ThemePalette, is_modern: bool) -> bool {
    is_modern && selection_bg(palette) != palette.window_frame_bg
}

fn selection_bg(palette: &ThemePalette) -> ratatui::style::Color {
    blend(palette.window_frame_bg, palette.active_border_fg, 30)
}

/// The style for a row carrying `mark`, starting from the row's ordinary `style`.
pub fn mark_style(style: Style, palette: &ThemePalette, is_modern: bool, mark: RowMark) -> Style {
    match mark {
        RowMark::Plain => style,
        RowMark::Selected if selection_is_bar(palette, is_modern) => Style::default()
            .fg(palette.title_fg)
            .bg(selection_bg(palette))
            .add_modifier(Modifier::BOLD),
        RowMark::Selected => style.add_modifier(Modifier::REVERSED),
        RowMark::Hover => {
            let tint = blend(palette.window_frame_bg, palette.active_border_fg, 14);
            let style = if tint != palette.window_frame_bg { style.bg(tint) } else { style };
            style.add_modifier(Modifier::UNDERLINED)
        }
    }
}

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod tests;
