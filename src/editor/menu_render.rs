//! Paints the editor menu bar, drop-down and Help > Keys dialog (ANSI, absolute positioning).
//! Colours come from the pane's own terminal colours only: reverse video for chrome, bold
//! normal video for the selection, dim plus `n/a` for disabled items, underline for mnemonics.
//! Nothing relies on hue, so it reads on every theme and for colour-blind users.

use unicode_width::UnicodeWidthStr;

use super::menu::{MENUS, MenuCmd, title_spans};
use super::{EditorState, menu};

const CHROME: &str = "\x1b[0;7m";
const SELECTED: &str = "\x1b[0;1m";
const DISABLED: &str = "\x1b[0;7;2m";

fn fit(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push_str(&" ".repeat(w - used));
    out
}

impl EditorState {
    /// Rows taken by the menu bar (0 in a window too small to show it).
    pub(crate) fn bar_rows(&self) -> usize {
        usize::from(self.view.0 >= 4)
    }

    fn item_label(&self, it: &menu::Item) -> String {
        if it.cmd == Some(MenuCmd::Wrap) { format!("{} {}", if self.wrap { "\u{2713}" } else { " " }, it.label) } else { format!("  {}", it.label) }
    }

    /// The bar on screen row 1.
    pub(super) fn paint_menu_bar(&self, cols: usize) -> String {
        let mut out = format!("\x1b[1;1H{CHROME}");
        let mut used = 0;
        for (i, m) in MENUS.iter().enumerate() {
            let open = self.menu.open == Some(i);
            let style = if open { SELECTED } else { CHROME };
            let mut chars = m.title.chars();
            let first = chars.next().unwrap_or(' ');
            out.push_str(&format!("{style} {style}\x1b[4m{first}{style}{} \x1b[0;7m", chars.as_str()));
            used += m.title.width() + 2;
        }
        out.push_str(&" ".repeat(cols.saturating_sub(used)));
        out.push_str("\x1b[0m");
        out
    }

    /// Geometry of the open drop-down: (x, width) in columns.
    fn dropdown_box(&self, menu: usize, cols: usize) -> (usize, usize) {
        let items = MENUS[menu].items;
        let inner = items
            .iter()
            .map(|it| self.item_label(it).width() + 2 + if it.cmd.is_some() { it.hotkey.width() } else { 3 })
            .max()
            .unwrap_or(8);
        let width = inner + 2;
        let x = title_spans()[menu].0.min(cols.saturating_sub(width));
        (x, width)
    }

    pub(super) fn paint_dropdown(&self, cols: usize, rows: usize) -> String {
        let Some(menu) = self.menu.open else { return String::new() };
        let (x, width) = self.dropdown_box(menu, cols);
        let inner = width - 2;
        let mut out = String::new();
        let at = |row: usize| format!("\x1b[{};{}H", row + 1, x + 1);
        let mut row = 1; // screen row 2 (zero based 1), under the bar
        out.push_str(&format!("{}{CHROME}\u{2554}{}\u{2557}", at(row), "\u{2550}".repeat(inner)));
        for (i, it) in MENUS[menu].items.iter().enumerate() {
            row += 1;
            if row + 1 >= rows {
                break;
            }
            if it.label == "-" {
                out.push_str(&format!("{}{CHROME}\u{255f}{}\u{2562}", at(row), "\u{2500}".repeat(inner)));
                continue;
            }
            let label = self.item_label(it);
            let key = if it.cmd.is_some() { it.hotkey } else { "n/a" };
            let gap = inner.saturating_sub(label.width() + key.width() + 2);
            let body = format!(" {label}{}{key} ", " ".repeat(gap));
            let style = if self.menu.item == i {
                SELECTED
            } else if it.cmd.is_none() {
                DISABLED
            } else {
                CHROME
            };
            out.push_str(&format!("{}{CHROME}\u{2551}{style}{}{CHROME}\u{2551}", at(row), fit(&body, inner)));
        }
        row += 1;
        if row + 1 < rows {
            out.push_str(&format!("{}{CHROME}\u{255a}{}\u{255d}", at(row), "\u{2550}".repeat(inner)));
        }
        out.push_str("\x1b[0m");
        out
    }

    /// Lines of the Help > Keys dialog, generated from the menu table plus the fixed keys.
    pub(crate) fn keys_help_lines() -> Vec<String> {
        let mut lines = vec!["Menu: F10 or Alt+F/E/S/V/H, arrows move, Enter runs, Esc closes".to_string(), String::new()];
        for m in MENUS {
            for it in m.items.iter().filter(|it| it.cmd.is_some() && !it.hotkey.is_empty()) {
                lines.push(format!("{:<14}{}", it.hotkey, it.label.trim_end_matches("...")));
            }
        }
        lines.extend([
            format!("{:<14}{}", "Shift+arrows", "Select text"),
            format!("{:<14}{}", "Ctrl+arrows", "Move by word"),
            format!("{:<14}{}", "Ctrl+]", "Switch between buffer and prompt"),
        ]);
        lines
    }

    pub(super) fn paint_keys_dialog(&self, cols: usize, rows: usize) -> String {
        let lines = Self::keys_help_lines();
        let width = (lines.iter().map(|l| l.width()).max().unwrap_or(20) + 6).min(cols);
        let inner = width.saturating_sub(2);
        let avail = rows.saturating_sub(4).max(1);
        let shown = lines.len().min(avail);
        let x = (cols - width) / 2;
        let y = (rows.saturating_sub(shown + 4)) / 2 + 1;
        let at = |r: usize| format!("\x1b[{};{}H", r + 1, x + 1);
        let mut out = format!("{}{CHROME}\u{2554}{}\u{2557}", at(y), fit("\u{2550} Keys \u{2550}", inner));
        for (i, l) in lines.iter().take(shown).enumerate() {
            out.push_str(&format!("{}{CHROME}\u{2551}{}\u{2551}", at(y + 1 + i), fit(&format!("  {l}"), inner)));
        }
        out.push_str(&format!("{}{CHROME}\u{2551}{}\u{2551}", at(y + 1 + shown), fit("  Esc or Enter closes", inner)));
        out.push_str(&format!("{}{CHROME}\u{255a}{}\u{255d}\x1b[0m", at(y + 2 + shown), "\u{2550}".repeat(inner)));
        out
    }

    /// Hint shown on the bottom line while the menu is up.
    pub(super) fn menu_hint(&self) -> Option<String> {
        if self.menu.dialog {
            return Some(" Keys   Esc/Enter close".into());
        }
        let m = &MENUS[self.menu.open?];
        let it = &m.items[self.menu.item.min(m.items.len() - 1)];
        Some(format!(" {} > {}: {}", m.title, it.label.trim_end_matches("..."), it.hint))
    }

    /// Mouse press at (`row`, `col`) of the window grid. Returns `true` when the menu took it.
    /// A press on an enabled drop-down item records it in `menu.pending` for the window manager.
    pub fn menu_click(&mut self, row: u16, col: u16) -> bool {
        let (row, col) = (row as usize, col as usize);
        let cols = self.view.1 as usize;
        if self.bar_rows() == 1 && row == 0 {
            let hit = title_spans().iter().position(|(a, b)| col >= *a && col < *b);
            match hit {
                Some(i) if self.menu.open != Some(i) => self.menu.open_menu(i),
                _ => self.menu.close(),
            }
            return true;
        }
        let Some(menu) = self.menu.open else {
            if self.menu.dialog {
                self.menu.close();
                return true;
            }
            return false;
        };
        let (x, width) = self.dropdown_box(menu, cols);
        let items = MENUS[menu].items;
        // First item is on zero-based row 2 (bar = 0, top border = 1).
        let inside = col > x && col + 1 < x + width && row >= 2 && row - 2 < items.len();
        if inside {
            let i = row - 2;
            if items[i].label != "-" {
                self.menu.item = i;
                if let Some(c) = items[i].cmd {
                    self.menu.close();
                    self.menu.pending = Some(c);
                }
            }
        } else {
            self.menu.close();
        }
        true
    }
}
