//! Ratatui rendering for the attach client: the selected pane's grid plus one status line.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use super::keys::HELP;
use super::state::ClientState;
use crate::attach::limits::{MAX_ATTACH_COLS, MAX_ATTACH_ROWS};
use crate::attach::msg::Role;
use crate::vt::snapshot::{GridSnapshot, UnderlineKind};

/// Non-grid facts for the status line.
pub struct StatusInfo<'a> {
    pub host: &'a str,
    pub role: Role,
    pub message: Option<&'a str>,
    pub prefix: bool,
    /// `Some(n)` while reconnecting (attempt number).
    pub reconnect: Option<u32>,
}

/// Terminal size -> size to announce to the host (one row is the status line).
pub fn announce_size(term_cols: u16, term_rows: u16) -> (u16, u16) {
    (term_cols.clamp(1, MAX_ATTACH_COLS), term_rows.saturating_sub(1).clamp(1, MAX_ATTACH_ROWS))
}

/// "who sets the size" text for the status line.
pub fn owner_text(st: &ClientState) -> String {
    let size = st.host_size.map(|(c, r)| format!("{c}x{r}")).unwrap_or_else(|| "?".into());
    match st.owner {
        Some(o) if o == st.my_id => format!("size {size} (you)"),
        Some(o) => format!("size {size} (client {o})"),
        None => format!("size {size}"),
    }
}

pub fn draw(f: &mut Frame, st: &mut ClientState, info: &StatusInfo) {
    let area = f.area();
    if area.height == 0 || area.width == 0 {
        return;
    }
    let grid_area = Rect { height: area.height - 1, ..area };
    let status_area = Rect { y: area.y + area.height - 1, height: 1, ..area };
    let cur = st.current_pane();
    let mut cursor = None;
    if let Some(snap) = cur.and_then(|id| st.snapshot(id)) {
        paint(f.buffer_mut(), grid_area, snap);
        let c = snap.cursor;
        if c.visible && c.col < grid_area.width && c.row < grid_area.height {
            cursor = Some((grid_area.x + c.col, grid_area.y + c.row));
        }
    }
    if let Some(pos) = cursor {
        f.set_cursor_position(pos);
    }
    let line = status_line(st, cur, info);
    let style = Style::default().add_modifier(Modifier::REVERSED);
    let buf = f.buffer_mut();
    for x in 0..status_area.width {
        let cell = &mut buf[(status_area.x + x, status_area.y)];
        cell.reset();
        cell.set_style(style);
    }
    buf.set_stringn(status_area.x, status_area.y, line, usize::from(status_area.width), style);
}

fn status_line(st: &ClientState, cur: Option<usize>, info: &StatusInfo) -> String {
    let role = if info.role == Role::Ro { "ro" } else { "rw" };
    let pane = match cur {
        Some(id) => {
            let (i, n) = st.pane_index(id).unwrap_or((0, 0));
            let exited = match st.pane_exited(id) {
                Some(Some(s)) => format!(" [exited {s}]"),
                Some(None) => " [exited]".to_string(),
                None => String::new(),
            };
            format!("pane {i}/{n} {}{exited}", st.pane_title(id).unwrap_or(""))
        }
        None => "waiting for host".to_string(),
    };
    let tail = if let Some(n) = info.reconnect {
        format!("reconnecting ({n})...")
    } else if info.prefix {
        "Ctrl-]: d detach, n/p pane".to_string()
    } else if let Some(m) = info.message {
        m.to_string()
    } else {
        HELP.to_string()
    };
    format!(" {} | {role} | {pane} | {} | {tail}", info.host, owner_text(st))
}

fn style_of(c: &crate::vt::snapshot::CellSnapshot) -> Style {
    let mut s = Style::default().fg(c.fg.to_ratatui_color()).bg(c.bg.to_ratatui_color());
    let mut m = Modifier::empty();
    for (on, flag) in [
        (c.bold, Modifier::BOLD),
        (c.dim, Modifier::DIM),
        (c.italic, Modifier::ITALIC),
        (c.inverse, Modifier::REVERSED),
        (c.strikethrough, Modifier::CROSSED_OUT),
        (c.underline != UnderlineKind::None, Modifier::UNDERLINED),
    ] {
        if on {
            m |= flag;
        }
    }
    if let Some(u) = c.underline_color {
        s = s.underline_color(Color::Rgb(u.r, u.g, u.b));
    }
    s.add_modifier(m)
}

/// Draws `snap` at the top-left of `area`, cropping what does not fit.
pub fn paint(buf: &mut Buffer, area: Rect, snap: &GridSnapshot) {
    for (y, row) in snap.grid_rows.iter().enumerate().take(usize::from(area.height)) {
        for (x, cell) in row.cells.iter().enumerate().take(usize::from(area.width)) {
            if cell.width == 0 {
                continue;
            }
            let (px, py) = (area.x + x as u16, area.y + y as u16);
            let out = &mut buf[(px, py)];
            out.set_symbol(if cell.grapheme.is_empty() { " " } else { &cell.grapheme });
            out.set_style(style_of(cell));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::layout::{WireLayout, WirePaneInfo};
    use crate::attach::msg::HostMsg;
    use crate::attach::wire_grid::WireGrid;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn state_with_text(text: &str) -> ClientState {
        let mut st = ClientState::new(2);
        let l = WireLayout { panes: vec![WirePaneInfo { id: 1, title: "sh".into(), rows: 3, cols: 10, read_only: false }], z_order: vec![1], active: 1 };
        st.apply(HostMsg::Layout { body: l.to_body() }).unwrap();
        let mut g = GridSnapshot::empty(3, 10);
        for (i, ch) in text.chars().enumerate() {
            g.grid_rows[0].cells[i].grapheme = ch.to_string();
            g.grid_rows[0].cells[i].bold = true;
        }
        st.apply(HostMsg::Snapshot { pane: 1, seq: 1, generation: 1, body: WireGrid::from(&g).to_body() }).unwrap();
        st
    }

    #[test]
    fn renders_grid_and_status_with_owner_indicator() {
        let mut st = state_with_text("hello");
        st.apply(HostMsg::Resized { cols: 10, rows: 3, owner_client: Some(2) }).unwrap();
        let mut term = Terminal::new(TestBackend::new(70, 4)).unwrap();
        let info = StatusInfo { host: "dev", role: Role::Rw, message: None, prefix: false, reconnect: None };
        term.draw(|f| draw(f, &mut st, &info)).unwrap();
        let buf = term.backend().buffer();
        let row = |y| (0..70).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        assert!(row(0).starts_with("hello"));
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
        let status = row(3);
        assert!(status.contains("dev") && status.contains("rw") && status.contains("pane 1/1") && status.contains("size 10x3 (you)"), "{status}");
    }

    #[test]
    fn owner_text_variants_and_small_terminals_do_not_panic() {
        let mut st = state_with_text("x");
        assert_eq!(owner_text(&st), "size ?");
        st.apply(HostMsg::Resized { cols: 80, rows: 24, owner_client: Some(9) }).unwrap();
        assert_eq!(owner_text(&st), "size 80x24 (client 9)");
        for (w, h) in [(1, 1), (3, 2), (200, 1)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let info = StatusInfo { host: "d", role: Role::Ro, message: Some("m"), prefix: true, reconnect: Some(1) };
            term.draw(|f| draw(f, &mut st, &info)).unwrap();
        }
        assert_eq!(announce_size(0, 0), (1, 1));
        assert_eq!(announce_size(80, 24), (80, 23));
        assert_eq!(announce_size(9999, 9999), (MAX_ATTACH_COLS, MAX_ATTACH_ROWS));
    }
}
