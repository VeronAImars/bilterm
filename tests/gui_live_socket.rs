//! The native GUI windows run a synchronous loop and used to announce `BILTERM_SOCKET` to their
//! panes without ever binding `instances/<pid>.sock`. `GuiLive` is what they now start and pump.
#![cfg(unix)]

use bilterm::api::live::GuiLive;
use bilterm::api::instance_registry::{instance_socket_path, list_live_instances};
use bilterm::wm::WindowManager;
use std::time::{Duration, Instant};

#[test]
fn gui_instance_binds_serves_and_cleans_up_its_socket_despite_inherited_env() {
    let root = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // SAFETY: single test in this binary; set before any runtime/thread reads them.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", root.path());
        std::env::set_var("BILTERM_RUNTIME_DIR", root.path());
        // A pane's inherited, stale BILTERM_* must not redirect or suppress this instance's socket.
        std::env::set_var("BILTERM_SOCKET", root.path().join("instances/859194.sock"));
        std::env::set_var("BILTERM_PANE_ID", "3");
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let _enter = rt.enter();
    let runtime = bilterm::secure_fs::private_runtime_dir().unwrap();
    // A leftover socket file of a dead pid is swept by discovery.
    let stale = instance_socket_path(&runtime, 2_000_000_000);
    bilterm::secure_fs::ensure_private_dir(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, b"").unwrap();

    let mut wm = WindowManager::new_headless(24, 80);
    let mut live = GuiLive::start(wm.traffic_meter()).expect("GUI must bind its instance socket");
    let expected = instance_socket_path(&runtime, std::process::id());
    assert_eq!(live.socket_path(), expected);
    assert!(expected.exists());

    let ep = format!("unix:{}", expected.display());
    let client = rt.spawn(async move { bilterm::api::send_rpc_request_with_token(&ep, "workspace.status", bilterm::api::JsonValue::obj([] as [(&str, bilterm::api::JsonValue); 0]), None).await });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.is_finished() {
        assert!(Instant::now() < deadline, "request never served by the GUI pump");
        live.pump(&mut wm);
        std::thread::sleep(Duration::from_millis(10));
    }
    let resp = rt.block_on(client).unwrap().unwrap();
    assert!(resp.contains("pane_count"), "{resp}");

    let live_pids: Vec<u32> = list_live_instances(&runtime).iter().map(|i| i.pid).collect();
    assert!(live_pids.contains(&std::process::id()), "{live_pids:?}");
    assert!(!stale.exists(), "dead-pid socket must be swept");

    drop(live);
    assert!(!expected.exists(), "socket removed when the GUI exits");
}
