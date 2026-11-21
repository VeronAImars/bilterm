use super::*;
use crate::wm::SplitDirection;
use crate::wm::drawer::DrawerNavOp;

fn session_dir(wm: &mut WindowManager, tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bilterm_drag_{tag}_{}", std::process::id()));
    wm.config_mut().session_dir = Some(dir.to_string_lossy().into_owned());
    dir
}

fn pane_row(wm: &WindowManager, id: PaneId) -> usize {
    wm.drawer_rows().iter().position(|r| r.kind == DrawerRowKind::Pane(id)).unwrap()
}

fn window_ids(wm: &WindowManager) -> Vec<PaneId> {
    wm.drawer_rows()
        .iter()
        .filter_map(|r| if let DrawerRowKind::Pane(p) = r.kind { Some(p) } else { None })
        .collect()
}

#[test]
fn drag_window_lowers_to_reorder_and_moves_it() {
    let mut wm = WindowManager::new_headless(24, 80);
    let dir = session_dir(&mut wm, "win");
    let a = wm.active_pane_id();
    let b = wm.split_focused(SplitDirection::Horizontal).unwrap();
    let c = wm.split_focused(SplitDirection::Horizontal).unwrap();
    let before = window_ids(&wm);
    assert_eq!(before.len(), 3);
    let (first, last) = (before[0], before[2]);

    let src = DrawerRowKind::Pane(first);
    let action = wm.drawer_drop_action(&src, pane_row(&wm, last)).unwrap();
    assert_eq!(
        action,
        Action::WindowManage(WindowManageOp::Reorder { window_id: Some(first), to: "3".into() })
    );

    // Dragging alone rearranges nothing.
    wm.drawer_drag_begin(src);
    assert!(wm.drawer_drag_update(Some(pane_row(&wm, last))));
    assert!(wm.drawer_drag_active());
    assert_eq!(window_ids(&wm), before);

    assert!(wm.drawer_drag_drop());
    let after = window_ids(&wm);
    assert_eq!(after, vec![before[1], before[2], first]);
    assert_eq!(wm.drawer_rows()[wm.drawer().selected_index].kind, DrawerRowKind::Pane(first));
    let _ = (a, b, c);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn drag_cancel_and_release_outside_change_nothing() {
    let mut wm = WindowManager::new_headless(24, 80);
    let dir = session_dir(&mut wm, "cancel");
    wm.split_focused(SplitDirection::Horizontal).unwrap();
    let before = window_ids(&wm);
    let src = DrawerRowKind::Pane(before[0]);

    wm.drawer_drag_begin(src.clone());
    wm.drawer_drag_update(Some(pane_row(&wm, before[1])));
    assert!(wm.drawer().drop_target_row().is_some());
    assert!(wm.apply(Action::DrawerNav(DrawerNavOp::DragCancel)).is_ok());
    assert!(!wm.drawer_drag_active());
    assert!(!wm.drawer_drag_drop());
    assert_eq!(window_ids(&wm), before);

    wm.drawer_drag_begin(src);
    wm.drawer_drag_update(Some(pane_row(&wm, before[1])));
    wm.drawer_drag_update(None); // pointer left the rows
    assert!(wm.drawer_drag_drop());
    assert_eq!(window_ids(&wm), before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn drag_session_and_cross_session_drop_lower_to_existing_actions() {
    let mut wm = WindowManager::new_headless(24, 80);
    let dir = session_dir(&mut wm, "sess");
    wm.create_new_session(Some("alpha"), false).unwrap();
    wm.create_new_session(Some("beta"), false).unwrap();
    let header = |wm: &WindowManager, n: &str| {
        wm.drawer_rows().iter().position(|r| r.kind == DrawerRowKind::SessionHeader(n.into())).unwrap()
    };
    let order = wm.ordered_sessions_with_active();
    let last = order.last().unwrap().clone();
    let first = order[0].clone();

    let act = wm.drawer_drop_action(&DrawerRowKind::SessionHeader(first.clone()), header(&wm, &last)).unwrap();
    assert_eq!(
        act,
        Action::SessionManage(SessionManageOp::MoveOrder {
            name: Some(first.clone()),
            target_pos: order.len().to_string()
        })
    );
    wm.drawer_drag_begin(DrawerRowKind::SessionHeader(first.clone()));
    wm.drawer_drag_update(Some(header(&wm, &last)));
    assert_eq!(wm.ordered_sessions_with_active(), order, "no rearranging while dragging");
    assert!(wm.drawer_drag_drop());
    assert_eq!(wm.ordered_sessions_with_active().last(), Some(&first));

    // A window dropped on another session's header is a window move there.
    let cur = wm.config().history_session.clone();
    let other = wm.ordered_sessions_with_active().into_iter().find(|s| *s != cur).unwrap();
    let win = wm.drawer_rows().iter().find_map(|r| match r.kind {
        DrawerRowKind::Pane(p) => Some(p),
        _ => None,
    });
    if let Some(win) = win {
        let act = wm.drawer_drop_action(&DrawerRowKind::Pane(win), header(&wm, &other));
        assert_eq!(
            act,
            Some(Action::WindowManage(WindowManageOp::Move { window_id: Some(win), target_session: other, switch: false }))
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
