//! Closing a browser must never take down a neighbouring window (opt-in: real shells + real Chrome).
#![cfg(unix)]
use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use bilterm::colon_command::parse_colon_command;
use bilterm::input::mouse::handle_wm_mouse_event;
use bilterm::wm::WindowManager;
use bilterm::wm::split_tree::SplitDirection;

fn click(wm: &mut WindowManager, screen: Rect, column: u16, row: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        handle_wm_mouse_event(wm, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }, screen).unwrap();
    }
}

fn settle(wm: &mut WindowManager, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        wm.poll_all_ptys();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn dump(wm: &WindowManager, screen: Rect) -> Vec<String> {
    let mut buf = ratatui::buffer::Buffer::empty(screen);
    ratatui::widgets::Widget::render(bilterm::ui::WorkspaceWidget::new(wm), screen, &mut buf);
    (0..screen.height).map(|y| (0..screen.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn find(rows: &[String], needle: &str, nth_from_right: bool) -> (u16, u16) {
    let mut hits = vec![];
    for (y, r) in rows.iter().enumerate() {
        for (bx, _) in r.match_indices(needle) {
            hits.push((r[..bx].chars().count() as u16, y as u16));
        }
    }
    *(if nth_from_right { hits.last() } else { hits.first() }).unwrap_or_else(|| panic!("{needle} not drawn"))
}

fn scenario(how: &str) {
    let screen = Rect::new(0, 0, 120, 30);
    let mut wm = WindowManager::spawn_default(30, 120).unwrap();
    let a = wm.active_pane_id();
    let b = wm.split_focused(SplitDirection::Horizontal).unwrap();
    let c = wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.resize_all(screen).unwrap();
    settle(&mut wm, 500);
    assert_eq!(wm.pane_count(), 3);
    let shells: Vec<(usize, u32)> = [a, b, c].iter().map(|&i| (i, wm.pane(i).unwrap().child_pid().expect("pty pid"))).collect();
    let _ = wm.apply(parse_colon_command(":browser open https://example.com").unwrap().unwrap());
    let target = wm.active_pane_id();
    assert!(wm.pane(target).unwrap().browser_canvas().is_some());
    settle(&mut wm, 1500);
    let (pgid, _) = wm.pane(target).unwrap().browser_canvas().unwrap().process_info().expect("chrome");
    eprintln!("[{how}] ids {a} {b} {c} target {target} chrome pgid {pgid} shells {shells:?}");
    let rows = dump(&wm, screen);
    match how {
        "nav" => {
            let (x, y) = find(&rows, "[✕]", false);
            click(&mut wm, screen, x + 1, y);
        }
        "title" => {
            // the browser pane's title row is the row above its nav row
            let (_, ny) = find(&rows, "[◄]", false);
            let (x, _) = rows[(ny - 1) as usize].match_indices("[X]").last().map(|(bx, _)| (rows[(ny - 1) as usize][..bx].chars().count() as u16, 0)).unwrap();
            click(&mut wm, screen, x + 1, ny - 1);
        }
        "key" => {
            let _ = wm.close_focused_pane();
        }
        _ => unreachable!(),
    }
    settle(&mut wm, 1500);
    eprintln!("[{how}] after: ids {:?}", wm.panes().keys().collect::<Vec<_>>());
    for (id, pid) in shells {
        if id == target && how != "nav" { continue; }
        assert!(wm.pane(id).is_some(), "[{how}] pane {id} vanished");
        assert!(!wm.pane(id).unwrap().has_exited(), "[{how}] pane {id} exited");
        assert!(pid_alive(pid), "[{how}] shell pid {pid} of pane {id} was killed");
    }
}

#[test]
fn closing_browser_keeps_every_other_pane() {
    if std::env::var("BILTERM_BROWSER_CDP").as_deref() != Ok("1") {
        return;
    }
    for how in ["nav", "title", "key"] {
        scenario(how);
    }
}

fn headless_close(layout: bilterm::wm::WorkspaceLayoutMode, how: &str, n_panes: usize) {
    // SAFETY: process-local test env var
    unsafe { std::env::set_var("BILTERM_BROWSER_SYNTHETIC", "1") };
    let screen = Rect::new(0, 0, 120, 30);
    let mut wm = WindowManager::new_headless(30, 120);
    for _ in 1..n_panes {
        wm.split_focused(SplitDirection::Horizontal).unwrap();
    }
    wm.set_layout_mode(layout);
    wm.resize_all(screen).unwrap();
    let target = wm.active_pane_id();
    wm.pane_mut(target).unwrap().open_browser("https://example.com");
    let before = wm.panes().keys().copied().collect::<Vec<_>>();
    let rows = dump(&wm, screen);
    let (x, y) = find(&rows, "[✕]", false);
    if how == "nav" {
        click(&mut wm, screen, x + 1, y);
        assert_eq!(wm.panes().keys().copied().collect::<Vec<_>>(), before, "{layout:?}/{how}: nav close must keep all panes");
        assert!(wm.pane(target).unwrap().browser_canvas().is_none());
    } else {
        // title bar [X] of the browser window (row above the nav row, last [X] left of the nav close)
        let title = &rows[(y - 1) as usize];
        let bx = title[..title.char_indices().nth(x as usize + 3).map_or(title.len(), |p| p.0)].rfind("[X]").expect("title X");
        click(&mut wm, screen, title[..bx].chars().count() as u16 + 1, y - 1);
        let mut expect = before.clone();
        expect.retain(|&i| i != target);
        assert_eq!(wm.panes().keys().copied().collect::<Vec<_>>(), expect, "{layout:?}/{how}: only the browser window closes");
    }
}

#[test]
fn headless_close_matrix() {
    use bilterm::wm::WorkspaceLayoutMode::{Stacked, Tiled};
    for layout in [Tiled, Stacked] {
        for n in [2, 3] {
            for how in ["nav", "title"] {
                headless_close(layout, how, n);
            }
        }
    }
}

#[test]
fn nav_bar_works_when_the_prompt_is_hidden_in_raw_mode() {
    // SAFETY: process-local test env var
    unsafe { std::env::set_var("BILTERM_BROWSER_SYNTHETIC", "1") };
    let screen = Rect::new(0, 0, 120, 30);
    let mut wm = WindowManager::new_headless(30, 120);
    let id = wm.active_pane_id();
    wm.pane_mut(id).unwrap().open_browser("https://example.com");
    wm.active_pane_mut().toggle_manual_raw_override();
    assert!(!wm.pane(id).unwrap().is_prompt_visible(), "raw mode hides the prompt");
    let rows = dump(&wm, screen);
    let (x, y) = find(&rows, "[↻]", false);
    assert!(rows[y as usize].contains("[◄]"), "nav bar still drawn without the prompt");
    let (cx, cy) = find(&rows, "[✕]", false);
    click(&mut wm, screen, cx + 1, cy);
    assert!(wm.pane(id).unwrap().browser_canvas().is_none(), "nav [✕] must close the browser in raw mode too (reload at {x},{y})");
}
