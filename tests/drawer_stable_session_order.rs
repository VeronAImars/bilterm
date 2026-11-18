//! Navigator drawer: switching / selecting a session must never reorder the session rows.

use bilterm::colon_command::parse_colon_command;
use bilterm::wm::{DrawerRowKind, WindowManager};

fn isolate_xdg() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bilterm-drawer-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            std::env::set_var("XDG_STATE_HOME", dir.join("state"));
            std::env::set_var("XDG_DATA_HOME", dir.join("data"));
        }
    });
}

fn session_names(wm: &WindowManager) -> Vec<String> {
    wm.drawer_rows()
        .into_iter()
        .filter_map(|r| match r.kind {
            DrawerRowKind::SessionHeader(n) => Some(n),
            _ => None,
        })
        .collect()
}

fn saved_only(wm: &WindowManager) -> Vec<String> {
    session_names(wm)
        .into_iter()
        .filter(|n| ["alpha", "beta", "gamma"].contains(&n.as_str()))
        .collect()
}

#[test]
fn activating_a_saved_session_does_not_hoist_it() {
    isolate_xdg();
    let dir = tempfile::tempdir().unwrap();
    let mut wm = WindowManager::new_headless(30, 100);
    let cmd = parse_colon_command(":set drawer.style navigator")
        .unwrap()
        .unwrap();
    let _ = wm.apply(cmd);
    wm.config_mut().session_dir = Some(dir.path().to_str().unwrap().to_string());
    for n in ["alpha", "beta", "gamma"] {
        std::fs::write(dir.path().join(format!("{n}.toml")), "").unwrap();
    }
    wm.config_mut().history_session = "alpha".to_string();
    let _ = wm.apply(parse_colon_command(":drawer").unwrap().unwrap());
    let baseline = saved_only(&wm);
    assert_eq!(baseline, ["alpha", "beta", "gamma"]);
    for active in ["beta", "gamma", "alpha", "gamma"] {
        wm.config_mut().history_session = active.to_string();
        let _ = wm.apply(parse_colon_command(":drawer").unwrap().unwrap());
        assert_eq!(
            saved_only(&wm),
            baseline,
            "activating {active} reordered the drawer"
        );
    }
}
