//! T-A4 host export tests, including an end-to-end tmux-backed pane (plan section 12 #11).

use crate::attach::msg::{HostMsg, Role};
use crate::attach::session::ClientSession;
use crate::attach::wire_grid::WireGrid;
use crate::tmux::TmuxControlSubprocess;
use crate::wm::{SplitDirection, WindowManager};
use std::time::Duration;

fn snapshot_text(msg: &HostMsg) -> String {
    let HostMsg::Snapshot { body, generation, .. } = msg else { panic!("not a snapshot: {msg:?}") };
    WireGrid::from_body(body).unwrap().to_snapshot(*generation).unwrap().contents()
}

#[test]
fn layout_and_pane_snapshot_follow_the_workspace() {
    let mut wm = WindowManager::new_headless(24, 80);
    let second = wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.pane_mut(second).unwrap().process_pty_output(b"hello-host-view");
    let view = wm.host_view();
    let layout = view.layout();
    assert_eq!(layout.panes.len(), 2);
    assert!(layout.panes.iter().any(|p| p.id == second));
    assert_eq!(layout.z_order.len(), 2);
    assert_eq!(layout.active, wm.active_pane_id());
    assert!(view.pane_snapshot(second).unwrap().contents().contains("hello-host-view"));
    assert!(view.pane_snapshot(9999).is_none());
}

#[test]
fn attach_session_streams_a_headless_pane_end_to_end() {
    let mut wm = WindowManager::new_headless(24, 80);
    let id = wm.active_pane_id();
    wm.pane_mut(id).unwrap().process_pty_output(b"first line\r\n");
    let mut s = ClientSession::new(Role::Rw, None);
    let out = s.pump(&wm.host_view().frames(), Duration::ZERO);
    assert!(matches!(out.msgs[0], HostMsg::Layout { .. }));
    assert!(snapshot_text(&out.msgs[1]).contains("first line"));
    s.on_ack(id, 1).unwrap();
    wm.pane_mut(id).unwrap().process_pty_output(b"second");
    let out = s.pump(&wm.host_view().frames(), Duration::ZERO);
    assert!(matches!(out.msgs.as_slice(), [HostMsg::Delta { .. }]));
}

#[test]
fn tmux_backed_pane_snapshot_end_to_end() {
    let mut wm = WindowManager::new_headless(24, 80);
    let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (line_tx, line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    wm.attach_tmux_control(TmuxControlSubprocess::from_channels("test", cmd_tx, line_rx));
    line_tx.send("%output %3 tmux-marker-xyz\r\n".to_string()).unwrap();
    wm.poll_all_ptys();
    let tmux_pane = *wm
        .panes()
        .iter()
        .find(|(_, p)| p.title().contains("%3"))
        .map(|(id, _)| id)
        .expect("tmux pane mapped");
    let mut s = ClientSession::new(Role::Ro, None);
    let out = s.pump(&wm.host_view().frames(), Duration::ZERO);
    let snap = out
        .msgs
        .iter()
        .find(|m| matches!(m, HostMsg::Snapshot { pane, .. } if *pane == tmux_pane))
        .expect("tmux pane is exported like any other pane");
    assert!(snapshot_text(snap).contains("tmux-marker-xyz"), "{}", snapshot_text(snap));
    let HostMsg::Layout { body } = &out.msgs[0] else { panic!() };
    let layout = crate::attach::layout::WireLayout::from_body(body).unwrap();
    assert!(layout.panes.iter().any(|p| p.id == tmux_pane && p.title.contains("%3")));
}
