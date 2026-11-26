//! API callers may only read/write editor files under the editor's working directory
//! (`editor_path_denied`, `io::confine_to_root`): `..`, absolute paths, symlinks and Windows
//! device names are refused; local (non-API) callers are unaffected.

use bilterm::api::json::JsonValue;
use bilterm::api::rpc::AutomationControlPlane;

fn sandbox() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir, std::path::PathBuf) {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let ws = d.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    unsafe {
        std::env::set_var("XDG_STATE_HOME", d.path().join("state"));
        std::env::set_var("XDG_DATA_HOME", d.path().join("data"));
        std::env::set_var("XDG_CONFIG_HOME", d.path().join("config"));
        std::env::set_var("HOME", d.path());
    }
    std::env::set_current_dir(&ws).unwrap();
    (g, d, ws)
}

fn p(path: &str) -> JsonValue {
    JsonValue::parse(&format!(r#"{{"path":{},"force":true}}"#, JsonValue::String(path.to_string()).to_json_string())).unwrap()
}

fn cp() -> AutomationControlPlane {
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    cp.wm.config_mut().api.allow_config_write = true;
    cp.dispatch_method("editor.scratch", &JsonValue::parse("{}").unwrap()).unwrap();
    cp.dispatch_method("editor.insert", &JsonValue::parse(r#"{"text":"payload"}"#).unwrap()).unwrap();
    cp
}

#[test]
fn save_as_inside_workspace_works() {
    let (_g, _d, ws) = sandbox();
    let mut cp = cp();
    cp.dispatch_method("editor.save_as", &p("inside.md")).unwrap();
    assert_eq!(std::fs::read_to_string(ws.join("inside.md")).unwrap(), "payload\n");
}

#[test]
fn save_as_refuses_dotdot_and_absolute_escapes() {
    let (_g, d, ws) = sandbox();
    let mut cp = cp();
    for bad in ["../escape.md", "sub/../../escape.md", "../../../../../../../../tmp/bilterm-escape.md"] {
        let e = cp.dispatch_method("editor.save_as", &p(bad)).unwrap_err();
        assert!(e.contains("API callers may only use files under"), "{bad}: {e}");
    }
    let abs = d.path().join("abs.md");
    let e = cp.dispatch_method("editor.save_as", &p(abs.to_str().unwrap())).unwrap_err();
    assert!(e.contains("outside the workspace root"), "{e}");
    assert!(!abs.exists() && !d.path().join("escape.md").exists() && !ws.join("escape.md").exists());
}

#[cfg(unix)]
#[test]
fn save_as_refuses_symlink_escapes() {
    let (_g, d, ws) = sandbox();
    let outside = d.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let victim = outside.join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&outside, ws.join("dirlink")).unwrap();
    std::os::unix::fs::symlink(&victim, ws.join("filelink")).unwrap();
    std::os::unix::fs::symlink(outside.join("new.txt"), ws.join("dangling")).unwrap();
    let mut cp = cp();
    for bad in ["dirlink/new.txt", "dirlink/victim.txt", "filelink", "dangling"] {
        let e = cp.dispatch_method("editor.save_as", &p(bad)).unwrap_err();
        assert!(e.contains("API callers may only use files under"), "{bad}: {e}");
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    assert!(!outside.join("new.txt").exists());
    // A symlink that stays inside the workspace is fine.
    std::fs::write(ws.join("real.md"), "old").unwrap();
    std::os::unix::fs::symlink(ws.join("real.md"), ws.join("inlink")).unwrap();
    cp.dispatch_method("editor.save_as", &p("inlink")).unwrap();
    assert_eq!(std::fs::read_to_string(ws.join("real.md")).unwrap(), "payload\n");
}

#[test]
fn save_as_refuses_windows_device_and_unc_names() {
    let (_g, _d, _ws) = sandbox();
    let mut cp = cp();
    for bad in ["NUL", "con.txt", "sub/COM1", "lpt9.md", "aux."] {
        let e = cp.dispatch_method("editor.save_as", &p(bad)).unwrap_err();
        assert!(e.contains("reserved device name"), "{bad}: {e}");
    }
}

#[test]
fn existing_target_still_needs_force_and_open_is_confined() {
    let (_g, d, ws) = sandbox();
    std::fs::write(ws.join("have.md"), "x").unwrap();
    let mut cp = cp();
    let noforce = JsonValue::parse(r#"{"path":"have.md"}"#).unwrap();
    assert!(cp.dispatch_method("editor.save_as", &noforce).unwrap_err().contains("exists"));
    assert_eq!(std::fs::read_to_string(ws.join("have.md")).unwrap(), "x");
    // editor.open outside the workspace is refused.
    let secret = d.path().join("secret.txt");
    std::fs::write(&secret, "s").unwrap();
    let e = cp.dispatch_method("editor.open", &JsonValue::parse(&format!(r#"{{"path":"{}"}}"#, secret.display())).unwrap()).unwrap_err();
    assert!(e.contains("outside the workspace root"), "{e}");
    let e = cp.dispatch_method("editor.open", &JsonValue::parse(r#"{"path":"../secret.txt"}"#).unwrap()).unwrap_err();
    assert!(e.contains("API callers may only use files under"), "{e}");
}

#[test]
fn local_callers_are_not_confined() {
    use bilterm::action::Action;
    use bilterm::editor::EditorOp;
    let (_g, d, _ws) = sandbox();
    let mut wm = bilterm::wm::WindowManager::new_headless(30, 100);
    wm.apply(Action::Editor(EditorOp::Scratch { name: None })).unwrap();
    wm.apply(Action::Editor(EditorOp::Insert("local".into()))).unwrap();
    let target = d.path().join("local.md");
    wm.apply(Action::Editor(EditorOp::SaveAs { path: target.display().to_string(), force: false })).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "local\n");
}
