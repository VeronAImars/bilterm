use super::*;
use crate::wm::SplitDirection;
use ratatui::buffer::Buffer;
use ratatui::widgets::Widget;

const AREA: Rect = Rect { x: 0, y: 0, width: 80, height: 24 };

fn two() -> (WindowManager, PaneId, PaneId) {
    let mut wm = WindowManager::new_headless(24, 80);
    wm.config_mut().session_dir = Some(std::env::temp_dir().join(format!("bilterm_wdrag_{}", std::process::id())).to_string_lossy().into_owned());
    wm.config_mut().show_top_menubar = false;
    wm.config_mut().show_dos_fkey_bar = false;
    let a = wm.active_pane_id();
    let b = wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.resize_all(AREA).unwrap();
    (wm, a, b)
}

fn rect_of(wm: &WindowManager, id: PaneId) -> Rect {
    wm.split_tree().compute_rects(AREA).into_iter().find(|(p, _)| *p == id).unwrap().1
}

fn center(r: Rect) -> (u16, u16) {
    (r.x + r.width / 2, r.y + r.height / 2)
}

#[test]
fn zones_cover_edges_swap_and_float_and_skip_the_source() {
    let (mut wm, a, b) = two();
    wm.window_drag_update(a, 1, 1, AREA, None);
    let v = wm.window_drag_view(AREA).unwrap();
    assert!(v.zones.iter().all(|z| !matches!(z.kind, DropZoneKind::Place { target, .. } if target == a)));
    let hows: Vec<_> = v.zones.iter().filter_map(|z| if let DropZoneKind::Place { how, .. } = z.kind { Some(how) } else { None }).collect();
    for h in [PlaceHow::Left, PlaceHow::Right, PlaceHow::Up, PlaceHow::Down, PlaceHow::Swap] {
        assert!(hows.contains(&h), "missing {h:?}");
    }
    assert!(v.zones.iter().any(|z| z.kind == DropZoneKind::Float));
    assert_eq!(v.hover, None, "pointer over the source window is 'no drop here'");
    let (cx, cy) = center(rect_of(&wm, b));
    wm.window_drag_update(a, cx, cy, AREA, None);
    assert_eq!(wm.window_drag_view(AREA).unwrap().hover, Some(DropZoneKind::Place { target: b, how: PlaceHow::Swap }));
}

#[test]
fn hover_picks_the_edge_band_under_the_pointer() {
    let (mut wm, a, b) = two();
    let r = rect_of(&wm, b);
    wm.window_drag_update(a, r.x + r.width - 1, r.y + r.height / 2, AREA, None);
    assert_eq!(wm.window_drag_view(AREA).unwrap().hover, Some(DropZoneKind::Place { target: b, how: PlaceHow::Right }));
    wm.window_drag_update(a, r.x + r.width / 2, r.y, AREA, None);
    assert_eq!(wm.window_drag_view(AREA).unwrap().hover, Some(DropZoneKind::Place { target: b, how: PlaceHow::Up }));
}

#[test]
fn drop_swaps_and_dragging_alone_changes_nothing() {
    let (mut wm, a, b) = two();
    let before = wm.split_tree().leaves();
    let (cx, cy) = center(rect_of(&wm, b));
    wm.window_drag_update(a, cx, cy, AREA, None);
    assert_eq!(wm.split_tree().leaves(), before, "nothing moves while dragging");
    assert!(wm.window_drag_drop().is_some());
    assert_eq!(wm.split_tree().leaves(), vec![b, a]);
    assert!(!wm.window_drag_active());
}

#[test]
fn drop_on_an_edge_splits_and_no_drop_zone_leaves_layout_alone() {
    let (mut wm, a, b) = two();
    let c = wm.split_focused(SplitDirection::Horizontal).unwrap();
    wm.resize_all(AREA).unwrap();
    let r = rect_of(&wm, b);
    wm.window_drag_update(c, r.x, r.y + r.height / 2, AREA, None);
    assert_eq!(wm.window_drag_view(AREA).unwrap().hover, Some(DropZoneKind::Place { target: b, how: PlaceHow::Left }));
    wm.window_drag_drop();
    let rb = rect_of(&wm, b);
    let rc = rect_of(&wm, c);
    assert!(rc.x < rb.x, "c is left of b: {rc:?} {rb:?}");
    assert_eq!(wm.split_tree().leaves().len(), 3);
    assert!(wm.split_tree().leaves().contains(&a));

    let before = wm.split_tree().leaves();
    wm.window_drag_update(a, 0, 0, AREA, None);
    let own = rect_of(&wm, a);
    wm.window_drag_update(a, own.x + own.width / 2, own.y + own.height / 2, AREA, None);
    assert_eq!(wm.window_drag_view(AREA).unwrap().hover, None);
    assert!(wm.window_drag_drop().unwrap().starts_with("No drop here"));
    assert_eq!(wm.split_tree().leaves(), before);
}

#[test]
fn float_zone_switches_to_stacked_and_esc_cancel_clears_the_drag() {
    let (mut wm, a, _b) = two();
    let f = float_zone_rect(AREA).unwrap();
    wm.window_drag_update(a, f.x + 1, f.y + 1, AREA, None);
    assert_eq!(wm.window_drag_view(AREA).unwrap().hover, Some(DropZoneKind::Float));
    wm.window_drag_drop();
    assert_eq!(wm.layout_mode(), WorkspaceLayoutMode::Stacked);

    let (mut wm, a, _b) = two();
    wm.window_drag_update(a, 5, 5, AREA, None);
    assert!(wm.apply(Action::DrawerNav(crate::wm::drawer::DrawerNavOp::DragCancel)).is_ok());
    assert!(!wm.window_drag_active());
    assert!(wm.window_drag_view(AREA).is_none());
}

#[test]
fn place_action_surfaces_agree_and_validate() {
    let (mut wm, a, b) = two();
    let before = wm.split_tree().clone();
    for (target, how) in [(Some(a), PlaceHow::Swap), (None, PlaceHow::Left)] {
        let _ = wm.apply(Action::WindowManage(WindowManageOp::Place { window_id: Some(a), target, how }));
        assert_eq!(*wm.split_tree(), before, "invalid {how:?} must not change the layout");
    }
    assert!(wm.place_window(Some(a), Some(a), PlaceHow::Swap).is_err());
    assert!(wm.apply(Action::WindowManage(WindowManageOp::Place { window_id: Some(a), target: Some(b), how: PlaceHow::Down })).is_ok());
    let (ra, rb) = (rect_of(&wm, a), rect_of(&wm, b));
    assert!(ra.y > rb.y);
    let parsed = crate::colon_command::parse_colon_command(":window place 2 left 1").unwrap().unwrap();
    assert_eq!(parsed.colon_form_or_summary(), ":window place 2 left 1");
    assert!(crate::colon_command::parse_colon_command(":window place 1 swap").unwrap().is_err());
    assert_eq!(crate::colon_command::parse_colon_command(":drag zones off").unwrap().unwrap(), Action::DragZones(false));
}

#[test]
fn zones_can_be_turned_off_and_float_or_drawer_never_hide_the_handle() {
    let (mut wm, a, _b) = two();
    wm.config_mut().appearance.drag_zones = Some(false);
    wm.window_drag_update(a, 5, 5, AREA, None);
    assert!(wm.window_drag_view(AREA).is_none());
    assert!(!wm.drag_zones_enabled());
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect()
}

#[test]
fn overlay_renders_labels_and_banner_in_every_glyph_pack() {
    use crate::ui::glyphs::GlyphPack;
    for pack in [GlyphPack::Unicode, GlyphPack::Cp437, GlyphPack::Ascii] {
        let (mut wm, a, b) = two();
        wm.config_mut().glyphs = pack;
        let (cx, cy) = center(rect_of(&wm, b));
        wm.window_drag_update(a, cx, cy, AREA, None);
        let mut buf = Buffer::empty(AREA);
        crate::ui::WorkspaceWidget::new(&wm).render(AREA, &mut buf);
        let all: String = (0..AREA.height).map(|y| row_text(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(all.contains(&format!("SWAP #{b}")), "{pack:?} missing swap label");
        assert!(all.contains("FLOAT"), "{pack:?} missing float label");
        assert!(all.contains("Esc cancels"), "{pack:?} missing banner");
        assert!(all.contains(&format!("MOVING #{a}")), "{pack:?} missing source label");
        let g = pack.glyphs();
        assert!(all.contains(g.zone_hot[2]), "{pack:?} missing hot border");
        // No-drop state is spelled out in text.
        wm.window_drag_update(a, 1, 1, AREA, None);
        let mut buf = Buffer::empty(AREA);
        crate::ui::WorkspaceWidget::new(&wm).render(AREA, &mut buf);
        assert!(row_text(&buf, AREA.height - 1).contains("NO DROP HERE"));
    }
}

#[test]
fn grip_is_drawn_in_each_pack_and_hover_marks_it() {
    use crate::ui::glyphs::GlyphPack;
    for pack in [GlyphPack::Unicode, GlyphPack::Cp437, GlyphPack::Ascii] {
        let (mut wm, a, _b) = two();
        wm.config_mut().glyphs = pack;
        let outer = rect_of(&wm, a);
        let gx = grip_glyph_x(outer).unwrap();
        let g = pack.glyphs();
        let mut buf = Buffer::empty(AREA);
        crate::ui::WorkspaceWidget::new(&wm).render(AREA, &mut buf);
        assert_eq!(buf.cell((gx, outer.y)).unwrap().symbol(), g.drag_grip, "{pack:?} idle grip");
        wm.update_grip_hover(gx, outer.y, AREA);
        assert_eq!(wm.grip_hover(), Some(a));
        let mut buf = Buffer::empty(AREA);
        crate::ui::WorkspaceWidget::new(&wm).render(AREA, &mut buf);
        assert_eq!(buf.cell((gx, outer.y)).unwrap().symbol(), g.drag_grip_hot, "{pack:?} hot grip");
    }
}
