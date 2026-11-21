use super::pane_origin::{OriginKind, with_badge};
use crate::action::Action;
use crate::ui::glyphs::GlyphPack;
use crate::wm::WindowManager;

fn wm2() -> WindowManager {
    let mut wm = WindowManager::new_headless(80, 24);
    wm.apply(Action::SplitVertical).unwrap();
    wm
}

#[test]
fn local_by_default_and_unknown_pane() {
    let wm = wm2();
    assert!(!wm.pane_origin(1).is_remote());
    assert_eq!(wm.pane_origin(99).kind, OriginKind::Local);
    assert_eq!(wm.pane_origin(1).tag(), None);
}

#[test]
fn ssh_and_tmux_link_targets_classified() {
    let mut wm = wm2();
    wm.pane_mut(1).unwrap().set_remote_target(Some("root@web1".into()));
    wm.pane_mut(2).unwrap().set_remote_target(Some("bob@db2/main".into()));
    let a = wm.pane_origin(1);
    assert_eq!((a.kind, a.host.as_deref(), a.tag().as_deref()), (OriginKind::Ssh, Some("web1"), Some("[R:web1]")));
    let b = wm.pane_origin(2);
    assert_eq!((b.kind, b.host.as_deref()), (OriginKind::TmuxLink, Some("db2")));
}

#[test]
fn badge_is_text_and_glyph_pack_aware() {
    let mut wm = wm2();
    wm.pane_mut(1).unwrap().set_remote_target(Some("web1".into()));
    let o = wm.pane_origin(1);
    let mut cfg = wm.config().clone();
    cfg.glyphs = GlyphPack::Unicode;
    assert_eq!(o.badge(&cfg).unwrap(), "\u{21C4}[R:web1]");
    cfg.glyphs = GlyphPack::Ascii;
    assert!(o.badge(&cfg).unwrap().is_ascii());
    cfg.glyphs = GlyphPack::Cp437;
    assert_eq!(o.oia_text(&cfg).unwrap(), "[R:web1] ssh");
    assert_eq!(with_badge("t", "[R:x]"), "[R:x] t");
}

#[test]
fn sync_pushes_badge_to_pane_once() {
    let mut wm = wm2();
    assert!(!wm.sync_origin_badges());
    wm.pane_mut(2).unwrap().set_remote_target(Some("h".into()));
    assert!(wm.sync_origin_badges());
    assert!(wm.pane(2).unwrap().origin_badge().unwrap().contains("[R:h]"));
    assert!(wm.pane(1).unwrap().origin_badge().is_none());
    assert!(!wm.sync_origin_badges());
}

#[test]
fn remote_status_colon_and_json() {
    let mut wm = wm2();
    wm.pane_mut(1).unwrap().set_remote_target(Some("u@web1".into()));
    wm.apply(Action::RemoteStatus { pane: None }).unwrap();
    let j = crate::api::JsonValue::parse(&wm.take_command_data().unwrap()).unwrap();
    assert_eq!(j.get("remote_panes").and_then(|v| v.as_i64()), Some(1));
    wm.apply(Action::RemoteStatus { pane: Some(2) }).unwrap();
    let j = crate::api::JsonValue::parse(&wm.take_command_data().unwrap()).unwrap();
    assert_eq!(j.get("remote_panes").and_then(|v| v.as_i64()), Some(0));
    assert!(wm.remote_status_apply(Some(77)).is_err());
    assert_eq!(crate::colon_command::parse_colon_command(":remote status 2"), Some(Ok(Action::RemoteStatus { pane: Some(2) })));
    assert!(matches!(crate::colon_command::parse_colon_command(":remote status x"), Some(Err(_))));
}
