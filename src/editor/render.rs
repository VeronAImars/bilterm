//! Paints the editor into ANSI for the pane's terminal grid (`EDIT-03`): wrapping, selection,
//! markdown colours, status line and cursor placement.

use unicode_width::UnicodeWidthChar;

use super::highlight::{self, Kind};
use super::{EditorState, Mode};

const TAB: usize = 4;

fn cell_width(c: char, col: usize) -> usize {
    if c == '\t' {
        TAB - col % TAB
    } else if c.is_control() {
        1 // painted as a visible placeholder, never emitted raw
    } else {
        c.width().unwrap_or(0)
    }
}

/// Display width of the first `n` chars of `line`.
fn width_of(line: &str, n: usize) -> usize {
    line.chars().take(n).fold(0, |w, c| w + cell_width(c, w))
}

/// Char ranges of the visual rows of `line` at text width `w` (one range when not wrapping).
fn segments(line: &str, w: usize, wrap: bool) -> Vec<(usize, usize)> {
    let n = line.chars().count();
    if !wrap || w == 0 {
        return vec![(0, n)];
    }
    let (mut out, mut start, mut col) = (Vec::new(), 0, 0);
    for (i, c) in line.chars().enumerate() {
        let cw = cell_width(c, col);
        if col + cw > w && i > start {
            out.push((start, i));
            start = i;
            col = 0;
        }
        col += cell_width(c, col);
    }
    out.push((start, n));
    out
}

fn seg_index(segs: &[(usize, usize)], col: usize) -> usize {
    segs.iter().rposition(|(s, _)| *s <= col).unwrap_or(0)
}

impl EditorState {
    fn text_area(&self) -> (usize, usize) {
        (self.view.0.saturating_sub(1 + self.bar_rows() as u16).max(1) as usize, self.view.1.max(1) as usize)
    }

    fn rows_to_cursor(&self, w: usize) -> usize {
        let c = self.buf.cursor;
        let above: usize = (self.top..c.line).map(|l| segments(self.buf.line(l), w, self.wrap).len()).sum();
        above + seg_index(&segments(self.buf.line(c.line), w, self.wrap), c.col)
    }

    /// Scrolls so the cursor is visible in a `rows` x `cols` grid.
    pub(crate) fn ensure_visible(&mut self, rows: u16, cols: u16) {
        self.view = (rows, cols);
        let (h, w) = self.text_area();
        let c = self.buf.cursor;
        if c.line < self.top {
            self.top = c.line;
        }
        while self.top < c.line && self.rows_to_cursor(w) >= h {
            self.top += 1;
        }
        if self.wrap {
            self.left = 0;
        } else {
            let x = width_of(self.buf.line(c.line), c.col);
            if x < self.left {
                self.left = x;
            } else if x >= self.left + w {
                self.left = x + 1 - w;
            }
        }
    }

    /// Visible row count, used for page movement.
    pub(crate) fn page_rows(&self) -> usize {
        self.text_area().0
    }

    fn status_line(&self, cols: usize) -> String {
        let c = self.buf.cursor;
        let s = match &self.mode {
            _ if self.menu.is_active() => self.menu_hint().unwrap_or_default(),
            Mode::Find(q) => format!(" Find: {q}_   Enter next  Esc cancel"),
            Mode::SaveAs(p) => format!(" Save as: {p}_   Enter save  Esc cancel"),
            Mode::Goto(n) => format!(" Go to line: {n}_   Enter go  Esc cancel"),
            Mode::ConfirmOverwrite(p) => format!(" {} exists or changed on disk. Overwrite? (y/n)", p.display()),
            Mode::Edit => {
                let (done, total) = if self.markdown { self.progress_cache.map_or((0, 0), |c| c.1) } else { (0, 0) };
                let tasks = if total > 0 { format!("  [{done}/{total}]") } else { String::new() };
                let shared = match (self.is_artifact(), self.bus_conflict, self.bus_error.is_some()) {
                    (false, ..) => "",
                    (_, true, _) => "  CONFLICT",
                    (_, _, true) => "  NOT SHARED",
                    _ => "  SHARED",
                };
                let ext = if self.external_change { "  !EXT" } else { "" };
                let kind_tag = if self.render_mode {
                    "VIEW (e=edit, Space=check)"
                } else if self.markdown {
                    "MD"
                } else {
                    "TXT"
                };
                format!(
                    " {}{}{}  Ln {}/{} Col {}  {}  {}{}{}  F10=Menu{}",
                    self.display_name(),
                    if self.is_dirty() { " *" } else { "" },
                    ext,
                    c.line + 1,
                    self.buf.line_count(),
                    c.col + 1,
                    if self.wrap { "WRAP" } else { "NOWRAP" },
                    kind_tag,
                    shared,
                    tasks,
                    self.message.as_ref().map_or(String::new(), |m| format!("  | {m}")),
                )
            }
        };
        let mut out: String = s.chars().filter(|c| !c.is_control()).collect();
        let mut w = 0;
        out = out.chars().take_while(|c| { w += c.width().unwrap_or(0); w <= cols }).collect();
        let pad = cols.saturating_sub(w.min(cols));
        format!("{out}{}", " ".repeat(pad))
    }

    /// Full-screen ANSI paint (absolute positioning, no scrolling) for a `rows` x `cols` grid.
    pub fn render(&mut self, rows: u16, cols: u16) -> String {
        self.ensure_visible(rows, cols);
        let (h, w) = self.text_area();
        let off = self.bar_rows();
        let mut out = String::from("\x1b[?25l");
        let mut cursor_xy = (0usize, 0usize);
        let version = self.buf.version();
        if self.markdown && self.progress_cache.is_none_or(|c| c.0 != version) {
            self.progress_cache = Some((version, super::artifact::checklist_progress(&self.buf)));
        }
        if self.render_mode {
            let rw = w.saturating_sub(2);
            let (_, _, rendered) = match self.rows_cache.take() {
                Some(c) if c.0 == version && c.1 == rw => c,
                _ => (version, rw, super::artifact::render_markdown_rows(&self.buf, rw)),
            };
            let cur_line = self.buf.cursor.line;
            let mut start_row = rendered.iter().position(|r| r.src_line >= self.top).unwrap_or(0);
            if let Some(cur_row) = rendered.iter().position(|r| r.src_line == cur_line) {
                if cur_row < start_row {
                    start_row = cur_row;
                } else if cur_row >= start_row + h {
                    start_row = cur_row + 1 - h;
                }
                if let Some(rr) = rendered.get(start_row) {
                    self.top = rr.src_line;
                }
            }
            self.rendered_rows.clear();
            for row in 0..h {
                out.push_str(&format!("\x1b[{};1H\x1b[0m", row + 1 + off));
                if let Some(rr) = rendered.get(start_row + row) {
                    self.rendered_rows.push((rr.src_line, rr.is_checkbox));
                    let active = rr.src_line == cur_line;
                    if active {
                        out.push_str("\x1b[1;36m▸ \x1b[0m");
                    } else {
                        out.push_str("  ");
                    }
                    out.push_str(&rr.ansi);
                } else {
                    out.push_str("\x1b[0;2m~\x1b[0m");
                }
                out.push_str("\x1b[0m\x1b[K");
            }
            self.rows_cache = Some((version, rw, rendered));
        } else {
            self.rendered_rows.clear();
            let fences = if self.markdown { highlight::fence_states((0..self.buf.line_count()).map(|l| self.buf.line(l))) } else { Vec::new() };
            let sel = self.buf.selection();
            let mut row = 0;
            let mut line = self.top;
            while row < h {
                if line >= self.buf.line_count() {
                    out.push_str(&format!("\x1b[{};1H\x1b[0;2m~\x1b[0m\x1b[K", row + 1 + off));
                    row += 1;
                    continue;
                }
                let text = self.buf.line(line);
                let kinds = if self.markdown { highlight::classify(text, fences.get(line).copied().unwrap_or(false)) } else { Vec::new() };
                let chars: Vec<char> = text.chars().collect();
                let segs = segments(text, w, self.wrap);
                for (si, &(s, e)) in segs.iter().enumerate() {
                    if row >= h {
                        break;
                    }
                    out.push_str(&format!("\x1b[{};1H\x1b[0m", row + 1 + off));
                    let mut cur_style = (Kind::Plain, false);
                    let base = if self.wrap { 0 } else { self.left };
                    let mut col_abs = 0usize; // display col from segment start
                    for (i, &c) in chars.iter().enumerate().take(e).skip(s) {
                        let cw = cell_width(c, col_abs);
                        let abs_start = col_abs;
                        col_abs += cw;
                        if line == self.buf.cursor.line && i == self.buf.cursor.col {
                            cursor_xy = (row, abs_start.saturating_sub(base));
                        }
                        if abs_start < base || abs_start + cw > base + w {
                            continue;
                        }
                        let in_sel = sel.is_some_and(|(a, b)| (a.line, a.col) <= (line, i) && (line, i) < (b.line, b.col));
                        let style = (kinds.get(i).copied().unwrap_or(Kind::Plain), in_sel);
                        if style != cur_style {
                            out.push_str(&format!("\x1b[0;{}{}m", style.0.sgr(), if style.1 { ";7" } else { "" }));
                            cur_style = style;
                        }
                        if c == '\t' {
                            out.push_str(&" ".repeat(cw));
                        } else if c.is_control() {
                            out.push('\u{FFFD}');
                        } else {
                            out.push(c);
                        }
                    }
                    if line == self.buf.cursor.line && self.buf.cursor.col >= e && si + 1 == segs.len() {
                        cursor_xy = (row, col_abs.saturating_sub(base));
                    }
                    out.push_str("\x1b[0m\x1b[K");
                    row += 1;
                }
                line += 1;
            }
        }
        let status = self.status_line(w);
        out.push_str(&format!("\x1b[{};1H\x1b[0;7m{status}\x1b[0m", h + 1 + off));
        if off == 1 {
            out.push_str(&self.paint_menu_bar(w));
            out.push_str(&self.paint_dropdown(w, h + 2));
            if self.menu.dialog {
                out.push_str(&self.paint_keys_dialog(w, h + 2));
            }
        }
        if matches!(self.mode, Mode::Edit) && !self.menu.is_active() && !self.render_mode {
            out.push_str(&format!("\x1b[{};{}H\x1b[?25h", cursor_xy.0 + 1 + off, cursor_xy.1.min(w.saturating_sub(1)) + 1));
        }
        out
    }

    /// Buffer position under grid cell (`row`, `col`); `None` below the text area.
    fn pos_at(&self, row: u16, col: u16) -> Option<super::buffer::Pos> {
        let (h, w) = self.text_area();
        let row = (row as usize).checked_sub(self.bar_rows())?;
        if row >= h {
            return None;
        }
        let mut remaining = row;
        let mut line = self.top;
        while line < self.buf.line_count() {
            let segs = segments(self.buf.line(line), w, self.wrap);
            if remaining < segs.len() {
                let (s, e) = segs[remaining];
                let target = col as usize + if self.wrap { 0 } else { self.left };
                let (mut x, mut i) = (0, s);
                for (k, c) in self.buf.line(line).chars().enumerate().skip(s).take(e - s) {
                    let cw = cell_width(c, x);
                    if x + cw > target {
                        break;
                    }
                    x += cw;
                    i = k + 1;
                }
                return Some(self.buf.clamp(super::buffer::Pos { line, col: i.min(e) }));
            }
            remaining -= segs.len();
            line += 1;
        }
        Some(self.buf.end_pos())
    }

    /// Mouse press: moves the cursor to the clicked cell (or toggles a Markdown checkbox).
    pub fn click(&mut self, row: u16, col: u16) {
        if self.menu_click(row, col) {
            return;
        }
        if self.render_mode {
            if let Some(vis_row) = (row as usize).checked_sub(self.bar_rows())
                && let Some(&(src_line, is_cb)) = self.rendered_rows.get(vis_row)
            {
                self.buf.anchor = None;
                self.buf.cursor = self.buf.clamp(super::buffer::Pos { line: src_line, col: 0 });
                self.buf.goal_col = None;
                if is_cb {
                    let _ = self.toggle_check();
                }
            }
            return;
        }
        if let Some(p) = self.pos_at(row, col) {
            self.buf.anchor = None;
            self.buf.cursor = p;
            self.buf.goal_col = None;
            if self.markdown
                && let Some(cb) = super::artifact::find_checkbox(self.buf.line(p.line))
                && p.col >= cb.box_col
                && p.col < cb.end_col
            {
                let _ = self.toggle_check();
            }
        }
    }

    /// Mouse drag: extends the selection from where the press landed.
    pub fn drag(&mut self, row: u16, col: u16) {
        if self.render_mode {
            return;
        }
        if let Some(p) = self.pos_at(row, col) {
            self.buf.anchor.get_or_insert(self.buf.cursor);
            self.buf.cursor = p;
        }
    }

    /// Scrolls the view by `delta` lines (mouse wheel); the cursor follows into view.
    pub fn scroll(&mut self, delta: i32) {
        let max = self.buf.line_count().saturating_sub(1);
        self.top = (self.top as i64 + delta as i64).clamp(0, max as i64) as usize;
        let h = self.text_area().0;
        let c = self.buf.cursor;
        let line = c.line.clamp(self.top, (self.top + h - 1).min(max));
        if line != c.line {
            self.buf.cursor = self.buf.clamp(super::buffer::Pos { line, col: c.col });
        }
    }

    pub(crate) fn display_col(&self) -> usize {
        width_of(self.buf.line(self.buf.cursor.line), self.buf.cursor.col)
    }

    pub(crate) fn col_for_display(&self, line: usize, target: usize) -> usize {
        let (mut x, mut i) = (0, 0);
        for c in self.buf.line(line).chars() {
            let cw = cell_width(c, x);
            if x + cw > target {
                break;
            }
            x += cw;
            i += 1;
        }
        i
    }
}
