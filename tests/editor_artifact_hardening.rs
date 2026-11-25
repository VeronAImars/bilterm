//! `EDIT-07` hardening: shared artifacts use compare-and-set on `ContextBus`, surface bus failures,
//! sanitise bus-originated text, gate `artifact`/`broadcast` for API callers and canonicalise names.
#![cfg(unix)]

use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::editor::EditorState;
use bilterm::wm::SavedEditor;
use bilterm::wm::WindowManager;
use bilterm::wm::context_bus::{ContextBus, ContextBusError};

fn sandbox() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    // SAFETY: serialized by ENV_LOCK; every test calls sandbox() first.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", d.path().join("state"));
        std::env::set_var("XDG_DATA_HOME", d.path().join("data"));
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("config"));
        std::env::set_var("HOME", d.path());
    }
    (g, d)
}

fn json(s: &str) -> JsonValue {
    JsonValue::parse(s).unwrap()
}

fn cp_open() -> AutomationControlPlane {
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    cp.wm.config_mut().api.allow_config_write = true;
    cp
}

fn msg(v: &JsonValue) -> String {
    v.get("message").and_then(JsonValue::as_str).unwrap_or_default().to_string()
}

#[test]
fn set_if_revision_is_compare_and_set() {
    let bus = ContextBus::new();
    let r1 = bus.set_if_revision("k", "a", None, 0).unwrap();
    assert_eq!(bus.set_if_revision("k", "b", None, 0), Err(ContextBusError::Conflict { expected: 0, actual: r1 }));
    let r2 = bus.set_if_revision("k", "b", None, r1).unwrap();
    assert!(r2 > r1);
    assert_eq!(bus.set_if_revision("k", "c", None, r1), Err(ContextBusError::Conflict { expected: r1, actual: r2 }));
    assert_eq!(bus.get("k").unwrap().value, "b");
    assert_eq!(bus.set_if_revision("gone", "x", None, 5), Err(ContextBusError::Conflict { expected: 5, actual: 0 }));
}

#[test]
fn concurrent_agent_write_is_not_overwritten_and_user_is_told() {
    let (_g, _d) = sandbox();
    let mut cp = cp_open();
    cp.dispatch_method("editor.artifact", &json(r#"{"name":"plan"}"#)).unwrap();
    cp.wm.context_bus().set("artifact:plan", "AGENT VALUE", None).unwrap();
    let r = cp.dispatch_method("editor.insert", &json(r#"{"text":"local edit"}"#)).unwrap();
    assert!(msg(&r).contains("changed elsewhere"), "{}", msg(&r));
    assert_eq!(cp.wm.context_bus().get("artifact:plan").unwrap().value, "AGENT VALUE");
    let ed = cp.wm.active_pane().editor().unwrap();
    assert!(ed.status_json().contains("\"bus_conflict\":true") && ed.status_json().contains("\"shared\":false"));
    let attempts = ed.bus_attempts();
    // Ticks neither overwrite nor retry while the buffer is unchanged.
    for _ in 0..3 {
        cp.wm.editor_tick();
    }
    assert_eq!(cp.wm.context_bus().get("artifact:plan").unwrap().value, "AGENT VALUE");
    assert_eq!(cp.wm.active_pane().editor().unwrap().bus_attempts(), attempts);
    // `:editor revert` takes the shared version and clears the conflict.
    cp.dispatch_method("editor.revert", &json("{}")).unwrap();
    let ed = cp.wm.active_pane().editor().unwrap();
    assert_eq!(ed.buf.text(), "AGENT VALUE");
    assert!(ed.status_json().contains("\"bus_conflict\":false"));
}

#[test]
fn oversize_artifact_surfaces_error_and_is_not_retried() {
    let (_g, _d) = sandbox();
    let mut cp = cp_open();
    cp.dispatch_method("editor.artifact", &json(r#"{"name":"big"}"#)).unwrap();
    let before = cp.wm.context_bus().get("artifact:big").unwrap().value;
    let big = "a".repeat(65 * 1024);
    let r = cp.dispatch_method("editor.insert", &json(&format!(r#"{{"text":"{big}"}}"#))).unwrap();
    assert!(msg(&r).contains("not shared") && msg(&r).contains("too large"), "{}", msg(&r));
    let st = cp.dispatch_method("editor.status", &json("{}")).unwrap();
    let e = st.get("editor").unwrap();
    assert_eq!(e.get("shared").and_then(JsonValue::as_bool), Some(false));
    assert!(e.get("bus_error").and_then(JsonValue::as_str).is_some_and(|s| s.contains("too large")));
    let attempts = cp.wm.active_pane().editor().unwrap().bus_attempts();
    for _ in 0..5 {
        cp.wm.editor_tick();
    }
    assert_eq!(cp.wm.active_pane().editor().unwrap().bus_attempts(), attempts, "one attempt per buffer revision");
    assert_eq!(cp.wm.context_bus().get("artifact:big").unwrap().value, before);
}

#[test]
fn artifact_and_broadcast_are_gated_for_api_callers() {
    let (_g, d) = sandbox();
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    for (m, p) in [("editor.artifact", r#"{"name":"g"}"#), ("editor.broadcast", "{}")] {
        let e = cp.dispatch_method(m, &json(p)).unwrap_err();
        assert!(e.contains("allow_config_write"), "{m}: {e}");
    }
    cp.wm.config_mut().api.allow_config_write = true;
    cp.dispatch_method("editor.artifact", &json(r#"{"name":"g"}"#)).unwrap();
    cp.dispatch_method("editor.broadcast", &json("{}")).unwrap();
    // Broadcasting a host file buffer is refused even when allowed.
    let f = d.path().join("secret.md");
    std::fs::write(&f, "top secret").unwrap();
    cp.dispatch_method("editor.open", &json(&format!(r#"{{"path":"{}"}}"#, f.display()))).unwrap();
    let e = cp.dispatch_method("editor.broadcast", &json("{}")).unwrap_err();
    assert!(e.contains("shared artifact"), "{e}");
    assert!(cp.wm.context_bus().list(None).iter().all(|e| !e.value.contains("top secret")));
}

#[test]
fn bus_text_is_sanitised_in_the_edit_view() {
    let (_g, _d) = sandbox();
    let mut cp = cp_open();
    cp.wm.context_bus().set("artifact:esc", "a\x1b]52;c;evil\x07b\u{202e}\u{200b}c", None).unwrap();
    cp.dispatch_method("editor.artifact", &json(r#"{"name":"esc"}"#)).unwrap();
    let out = cp.wm.active_pane_mut().editor_mut().unwrap().render(10, 60);
    assert!(!out.contains("\x1b]52") && !out.contains('\x07') && !out.contains('\u{202e}') && !out.contains('\u{200b}'), "{out:?}");
    assert!(out.contains("a]52;c;evilbc"), "{out:?}");
}

#[test]
fn control_characters_in_file_buffers_are_painted_as_placeholders() {
    let (_g, d) = sandbox();
    let f = d.path().join("x.txt");
    std::fs::write(&f, "x\x1b[31my\x07z\n").unwrap();
    let mut ed = EditorState::open_file(&f).unwrap();
    let out = ed.render(10, 60);
    assert!(out.contains("x\u{FFFD}[31my\u{FFFD}z"), "{out:?}");
    assert!(!out.contains("x\x1b[31m") && !out.contains('\x07'));
}

#[test]
fn artifact_names_are_canonicalised_to_lowercase() {
    let (_g, _d) = sandbox();
    let mut cp = cp_open();
    let a = cp.dispatch_method("editor.artifact", &json(r#"{"name":"Plan"}"#)).unwrap();
    let b = cp.dispatch_method("editor.artifact", &json(r#"{"name":"plan"}"#)).unwrap();
    let c = cp.dispatch_method("editor.artifact", &json(r#"{"name":"PLAN"}"#)).unwrap();
    assert_eq!(a.get("bus_key").and_then(JsonValue::as_str), Some("artifact:plan"));
    assert_eq!(b.get("pane"), a.get("pane"));
    assert_eq!(c.get("pane"), a.get("pane"));
    assert!(cp.wm.context_bus().get("artifact:Plan").is_none());
}

#[test]
fn windows_device_names_are_not_valid_artifact_names() {
    let (_g, _d) = sandbox();
    let mut cp = cp_open();
    for n in ["con", "NUL", "com1"] {
        let e = cp.dispatch_method("editor.artifact", &json(&format!(r#"{{"name":"{n}"}}"#))).unwrap_err();
        assert!(e.contains("artifact name"), "{n}: {e}");
    }
}

#[test]
fn broadcast_note_is_sanitised_and_capped() {
    let (_g, _d) = sandbox();
    let mut cp = cp_open();
    cp.wm.set_agent_messaging_enabled(true);
    let worker = cp.wm.active_pane_id();
    cp.dispatch_method("agent.name", &json(&format!(r#"{{"pane_id":{worker},"name":"worker-1"}}"#))).unwrap();
    cp.dispatch_method("editor.artifact", &json(r#"{"name":"n"}"#)).unwrap();
    cp.dispatch_method("editor.broadcast", &json("{\"note\":\"ok\\u202eevil\"}")).unwrap();
    let inbox = cp.dispatch_method("agent.inbox", &json(r#"{"agent":"worker-1"}"#)).unwrap().to_json_string();
    assert!(inbox.contains("okevil") && !inbox.contains("202e") && !inbox.contains('\u{202e}'), "{inbox}");
    let long = "x".repeat(10 * 1024);
    let r = cp.dispatch_method("editor.broadcast", &json(&format!(r#"{{"note":"{long}"}}"#))).unwrap();
    assert_eq!(r.get("notified_agents").and_then(JsonValue::as_bool), Some(true));
    let inbox = cp.dispatch_method("agent.inbox", &json(r#"{"agent":"worker-1"}"#)).unwrap().to_json_string();
    assert!(inbox.contains(&"x".repeat(512)) && !inbox.contains(&"x".repeat(513)));
}

#[test]
fn mcp_missing_editor_action_names_the_new_verbs() {
    let (_g, _d) = sandbox();
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    let out = cp
        .handle_mcp_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_editor","arguments":{}}}"#)
        .unwrap();
    for v in ["artifact", "render", "check", "broadcast"] {
        assert!(out.contains(v), "{v}: {out}");
    }
}

#[test]
fn saved_editor_artifact_round_trips_and_old_sessions_still_load() {
    let (_g, _d) = sandbox();
    let mut wm = WindowManager::new_headless(30, 100);
    let a = SavedEditor::Artifact { name: "Plan".into(), render_mode: true };
    let s = serde_json::to_string(&a).unwrap();
    assert_eq!(serde_json::from_str::<SavedEditor>(&s).unwrap(), a);
    // Old session files: no render_mode on artifacts; plain scratch/file entries.
    assert_eq!(
        serde_json::from_str::<SavedEditor>(r#"{"kind":"artifact","name":"x"}"#).unwrap(),
        SavedEditor::Artifact { name: "x".into(), render_mode: false }
    );
    assert!(matches!(serde_json::from_str::<SavedEditor>(r#"{"kind":"scratch","name":"s"}"#).unwrap(), SavedEditor::Scratch { .. }));
    assert!(matches!(serde_json::from_str::<SavedEditor>(r#"{"kind":"file","path":"/x","wrap":true}"#).unwrap(), SavedEditor::File { .. }));
    // A live artifact pane snapshots and restores into a fresh window manager.
    wm.apply(bilterm::action::Action::Editor(bilterm::action::EditorOp::Artifact { name: Some("Spec".into()) })).unwrap();
    let id = wm.active_pane_id();
    let saved = wm.snapshot_session("art");
    let mut fresh = WindowManager::new_headless(30, 100);
    fresh.apply_saved_session_snapshot(&saved);
    let ed = fresh.pane(id).unwrap().editor().unwrap();
    assert_eq!(ed.artifact_bus_key().as_deref(), Some("artifact:spec"));
}
