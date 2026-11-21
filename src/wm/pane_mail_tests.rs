use std::time::{Duration, Instant};

use super::pane_mail::{FLASH_HALF_MS, FOCUS_DWELL, OPEN_HIDE, PaneMailOp};
use crate::action::Action;
use crate::wm::WindowManager;

fn wm2() -> WindowManager {
    let mut wm = WindowManager::new_headless(80, 24);
    wm.apply(Action::SplitVertical).unwrap();
    wm
}

/// A pane that is not the active one.
fn other(wm: &WindowManager) -> usize {
    (1..=2).find(|i| *i != wm.active_pane_id()).unwrap()
}

#[test]
fn note_counts_and_ack_opens() {
    let mut wm = wm2();
    let id = other(&wm);
    let t0 = Instant::now();
    wm.note_pane_message(id, "api", t0);
    wm.note_pane_message(id, "bob", t0);
    let v = wm.pane_mail_view(id, t0).unwrap();
    assert_eq!((v.unread, v.sender.as_str()), (2, "bob"));
    assert_eq!(wm.pane_mail_ack(Some(id), t0), 1);
    let v = wm.pane_mail_view(id, t0).unwrap();
    assert_eq!(v.unread, 0, "read => open envelope");
    assert!(v.visible);
    assert_eq!(wm.pane_mail_ack(Some(id), t0), 0, "idempotent");
}

#[test]
fn unknown_pane_is_ignored_and_view_none_without_mail() {
    let mut wm = wm2();
    wm.note_pane_message(99, "api", Instant::now());
    assert!(wm.pane_mail_view(99, Instant::now()).is_none());
    assert!(wm.pane_mail_view(1, Instant::now()).is_none());
}

#[test]
fn flash_phase_is_one_hertz_and_toggle_disables() {
    let mut wm = wm2();
    let id = other(&wm);
    let t0 = Instant::now();
    wm.note_pane_message(id, "api", t0);
    let half = Duration::from_millis(FLASH_HALF_MS);
    assert!(wm.pane_mail_view(id, t0).unwrap().visible);
    assert!(!wm.pane_mail_view(id, t0 + half).unwrap().visible);
    assert!(wm.pane_mail_view(id, t0 + half * 2).unwrap().visible);
    wm.config_mut().ui.pane_mail_flash = false;
    assert!(wm.pane_mail_view(id, t0 + half).unwrap().visible, "no blink when flash is off");
    wm.config_mut().ui.pane_mail_indicator = false;
    assert!(wm.pane_mail_view(id, t0).is_none(), "indicator off hides it (state is kept)");
    assert_eq!(wm.pane_mail_json(id, t0).get("unread").and_then(|v| v.as_i64()), Some(1));
}

#[test]
fn tick_reports_blink_changes_dwell_ack_and_expiry() {
    let mut wm = wm2();
    let id = other(&wm);
    let t0 = Instant::now();
    wm.note_pane_message(id, "api", t0);
    wm.pane_mail_tick(t0);
    let half = Duration::from_millis(FLASH_HALF_MS);
    assert!(wm.pane_mail_tick(t0 + half), "blink phase flipped => redraw");
    assert!(!wm.pane_mail_tick(t0 + half + Duration::from_millis(10)), "same phase => no redraw");
    // Not focused: dwell never acks.
    assert_eq!(wm.pane_mail_view(id, t0).unwrap().unread, 1);
    // Focus it; after the dwell it counts as read.
    wm.apply(Action::FocusPane(id)).unwrap();
    let t1 = t0 + Duration::from_secs(5);
    wm.pane_mail_tick(t1);
    assert_eq!(wm.pane_mail_view(id, t1).unwrap().unread, 1, "dwell starts at focus");
    assert!(wm.pane_mail_tick(t1 + FOCUS_DWELL));
    assert_eq!(wm.pane_mail_view(id, t1).unwrap().unread, 0);
    // Open envelope disappears after OPEN_HIDE.
    assert!(wm.pane_mail_tick(t1 + FOCUS_DWELL + OPEN_HIDE));
    assert!(wm.pane_mail_view(id, t1).is_none());
}

#[test]
fn local_key_acks_active_pane_but_not_api_origin() {
    let mut wm = wm2();
    let id = wm.active_pane_id();
    wm.note_pane_message(id, "api", Instant::now());
    wm.set_api_origin(true);
    wm.pane_mail_on_local_key();
    wm.set_api_origin(false);
    assert_eq!(wm.pane_mail_view(id, Instant::now()).unwrap().unread, 1);
    wm.pane_mail_on_local_key();
    assert_eq!(wm.pane_mail_view(id, Instant::now()).unwrap().unread, 0);
}

#[test]
fn traffic_choke_point_marks_only_non_local_sources() {
    use crate::wm::traffic::{TrafficEndpoint as E, TrafficKind as K};
    let mut wm = wm2();
    let id = other(&wm);
    let now = Instant::now();
    wm.record_traffic_with(E::User, E::Pane(id), K::Keys, "x", false, now);
    wm.record_traffic_with(E::Pane(id), E::Pane(id), K::Keys, "x", false, now);
    wm.record_traffic_with(E::Api, E::Pane(id), K::Input, "x", false, now);
    assert!(wm.pane_mail_view(id, now).is_none(), "local typing/self/input never mark");
    wm.record_traffic_with(E::Api, E::Pane(id), K::Keys, "x", false, now);
    let v = wm.pane_mail_view(id, now).unwrap();
    assert_eq!((v.unread, v.sender.as_str()), (1, "api"));
}

#[test]
fn attach_input_marks_pane() {
    let mut wm = wm2();
    let id = other(&wm);
    wm.apply(Action::PaneInputBytes { pane: id, bytes: b"ls\r".to_vec() }).unwrap();
    assert_eq!(wm.pane_mail_view(id, Instant::now()).unwrap().sender, "attach");
}

#[test]
fn apply_status_and_ack_ops() {
    let mut wm = wm2();
    let id = other(&wm);
    wm.note_pane_message(id, "api", Instant::now());
    wm.apply(Action::PaneMail(PaneMailOp::Status { pane: None })).unwrap();
    let data = wm.take_command_data().unwrap();
    assert!(data.contains("\"unread\":1"), "{data}");
    assert!(wm.apply(Action::PaneMail(PaneMailOp::Ack { pane: Some(99), all: false })).is_ok());
    assert_eq!(wm.pane_mail_view(id, Instant::now()).unwrap().unread, 1, "bad pane acked nothing");
    wm.apply(Action::PaneMail(PaneMailOp::Ack { pane: Some(id), all: false })).unwrap();
    assert_eq!(wm.pane_mail_view(id, Instant::now()).unwrap().unread, 0);
}

#[test]
fn removed_pane_drops_state() {
    let mut wm = wm2();
    let id = other(&wm);
    wm.note_pane_message(id, "api", Instant::now());
    wm.remove_pane(id);
    assert!(wm.pane_mail_view(id, Instant::now()).is_none());
}
