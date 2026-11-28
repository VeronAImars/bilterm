//! `Action::Daemon(New)`: one operation reached from the colon command, the Ctrl+X D C chord, the
//! palette/menu, RPC, MCP, the daemon window (key, button, mouse) and the CLI.
//!
//! Parity (parse/chord/palette/menu/window) runs everywhere and never spawns; the real-process
//! test drives `bilterm daemon new` end to end (Unix only).

mod common;

use std::sync::{Mutex, MutexGuard};

use bilterm::action::{Action, DaemonSupervisorOp};
use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::colon_command::parse_colon_command;
use bilterm::input::keymap::resolve_daemon_subchord;
use bilterm::ui::modals::{menubar_item_command, PALETTE_CATALOG};
use bilterm::wm::daemon_window::{DaemonWindowButton, DaemonWindowHit, DaemonWindowLayout};
use bilterm::wm::WindowManager;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn sandbox() -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = common::short_tempdir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("BILTERM_RUNTIME_DIR", dir.path()) };
    (g, dir)
}

fn new_op(name: Option<&str>, host_ptys: bool) -> Action {
    Action::DaemonSupervisor(DaemonSupervisorOp::New { name: name.map(String::from), restart: None, limits: None, host_ptys })
}

fn parse(s: &str) -> Result<Action, String> {
    parse_colon_command(s).unwrap_or_else(|| panic!("unrecognized {s}"))
}

#[test]
fn colon_forms_lower_to_one_action() {
    assert_eq!(parse(":daemon new"), Ok(new_op(None, false)));
    assert_eq!(parse(":daemon new w1"), Ok(new_op(Some("w1"), false)));
    assert_eq!(parse(":daemon new w1 --host-ptys"), Ok(new_op(Some("w1"), true)));
    assert_eq!(parse(":daemon new --host-ptys"), Ok(new_op(None, true)));
    assert!(parse(":daemon new a b").is_err());
    let Ok(Action::DaemonSupervisor(DaemonSupervisorOp::New { restart, .. })) = parse(":daemon new --restart on-crash") else { panic!() };
    assert!(restart.is_some());
    // start also accepts --host-ptys.
    assert!(matches!(
        parse(":daemon start w --host-ptys"),
        Ok(Action::DaemonSupervisor(DaemonSupervisorOp::Start { host_ptys: true, .. }))
    ));
}

#[test]
fn chord_palette_menu_and_completion_reach_new() {
    assert_eq!(resolve_daemon_subchord(KeyCode::Char('c')), Some(new_op(None, false)));
    assert!(PALETTE_CATALOG.iter().any(|(c, _, d)| *c == ":daemon new" && !d.is_empty()));
    let in_menu = (0..12).flat_map(|c| (0..12).map(move |i| (c, i))).any(|(c, i)| menubar_item_command(c, i) == Some(":daemon new"));
    assert!(in_menu, "menubar entry missing");
    let subs = bilterm::pane::completion::colon_command_table().iter().find(|(c, _)| *c == ":daemon").map(|(_, s)| *s).unwrap();
    assert!(subs.contains(&"new"), "Tab completion lacks `new`");
}

#[test]
fn rpc_and_mcp_refuse_existing_and_invalid_names_without_spawning() {
    let (_g, dir) = sandbox();
    // A crashed-looking lock record occupies the name "taken".
    let lock = bilterm::daemon::supervisor::daemon_lock_path_in(dir.path(), "taken").unwrap();
    let rec = bilterm::daemon::supervisor::DaemonLockRecord {
        name: "taken".into(),
        pid: 2_000_000_000,
        version: "0".into(),
        started_at_ms: 1,
        socket_path: dir.path().join("x.sock"),
    };
    bilterm::secure_fs::write_private_file(&lock, rec.to_json().as_bytes()).unwrap();

    let mut cp = AutomationControlPlane::new_headless(24, 80);
    let p = |n: &str| JsonValue::obj([("name", JsonValue::String(n.into()))]);
    let err = cp.dispatch_method("daemon.new", &p("taken")).unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    let err = cp.dispatch_method("daemon.new", &p("../evil")).unwrap_err();
    assert!(err.contains("invalid daemon name"), "{err}");

    let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_daemon","arguments":{"action":"new","name":"taken","host_ptys":true}}}"#;
    let res = JsonValue::parse(&cp.handle_mcp_line(call).unwrap()).unwrap();
    assert_eq!(res.get("result").and_then(|r| r.get("isError")).and_then(JsonValue::as_bool), Some(true), "{res:?}");
    assert!(bilterm::api::RPC_METHODS.contains(&"daemon.new"));
}

#[test]
fn window_key_button_and_mouse_open_the_prompt_and_validate() {
    let (_g, dir) = sandbox();
    let mut wm = WindowManager::new_headless(100, 40);
    wm.open_daemon_window_from(dir.path());
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let ws = Rect::new(0, 0, 100, 40);

    // Key path: `n` opens the prompt with the next free auto name.
    wm.daemon_window_key(key(KeyCode::Char('n')));
    assert_eq!(wm.daemon_window().new_prompt.as_ref().unwrap().name, "daemon-1");
    wm.daemon_window_key(key(KeyCode::Esc));
    assert!(wm.daemon_window().new_prompt.is_none() && wm.daemon_window().active, "Esc cancels the prompt only");

    // Mouse path: the [n] New button, then the host-PTYs checkbox.
    let lay = DaemonWindowLayout::new(wm.daemon_window_rect(ws).unwrap());
    let (_, x, _) = lay.button_spans().into_iter().find(|(b, _, _)| *b == DaemonWindowButton::New).unwrap();
    assert_eq!(wm.daemon_window_click(ws, x, lay.footer_y), DaemonWindowHit::Button(DaemonWindowButton::New));
    assert!(wm.daemon_window().new_prompt.is_some());
    let (tx, _) = lay.prompt_toggle_span();
    assert_eq!(wm.daemon_window_click(ws, tx, lay.status_y), DaemonWindowHit::PromptHostPtys);
    assert!(wm.daemon_window().new_prompt.as_ref().unwrap().host_ptys);

    // Invalid names stay in the prompt with an error and spawn nothing.
    for _ in 0..8 {
        wm.daemon_window_key(key(KeyCode::Backspace));
    }
    wm.daemon_window_key(key(KeyCode::Char('.')));
    wm.daemon_window_key(key(KeyCode::Char('a')));
    for c in "bc".chars() {
        wm.daemon_window_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wm.daemon_window().new_prompt.as_ref().unwrap().name, "abc", "illegal chars are filtered");
}

#[cfg(unix)]
mod real {
    use std::process::{Command, Output, Stdio};

    struct Sb {
        root: tempfile::TempDir,
    }

    impl Sb {
        fn run(&self, args: &[&str]) -> Output {
            let p = self.root.path();
            let mut c = Command::new(env!("CARGO_BIN_EXE_bilterm"));
            c.args(args)
                .env("HOME", p.join("home"))
                .env("XDG_RUNTIME_DIR", p.join("run"))
                .env("XDG_STATE_HOME", p.join("state"))
                .env("XDG_CONFIG_HOME", p.join("config"))
                .stdin(Stdio::null());
            for k in ["BILTERM_DAEMON", "BILTERM_SOCKET", "BILTERM_RUNTIME_DIR", "BILTERM_TOKEN_FILE", "BILTERM_PANE_ID"] {
                c.env_remove(k);
            }
            c.output().unwrap()
        }
        fn out(o: &Output) -> String {
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
        }
    }

    impl Drop for Sb {
        fn drop(&mut self) {
            for n in ["daemon-1", "daemon-2", "named"] {
                let _ = self.run(&["daemon", "stop", n, "--force"]);
            }
        }
    }

    #[test]
    fn cli_new_auto_names_refuses_duplicates_and_host_ptys_reaches_the_host() {
        let root = super::common::short_tempdir();
        for d in ["home", "run", "state", "config"] {
            std::fs::create_dir(root.path().join(d)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path().join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sb = Sb { root };

        let a = sb.run(&["daemon", "new", "--json"]);
        assert!(a.status.success(), "{}", Sb::out(&a));
        assert!(Sb::out(&a).contains("\"name\":\"daemon-1\""), "{}", Sb::out(&a));
        let b = sb.run(&["daemon", "new", "--host-ptys"]);
        assert!(b.status.success() && Sb::out(&b).contains("'daemon-2'"), "{}", Sb::out(&b));

        // Never overwrite a running daemon.
        let dup = sb.run(&["daemon", "new", "daemon-1"]);
        assert_eq!(dup.status.code(), Some(1));
        assert!(Sb::out(&dup).contains("already exists"), "{}", Sb::out(&dup));
        let bad = sb.run(&["daemon", "new", "a/b"]);
        assert!(!bad.status.success());

        // host_ptys is real: the host reports it, the plain one does not.
        let st = |n: &str| Sb::out(&sb.run(&["ctl", "--daemon-name", n, "workspace.status"]));
        assert!(st("daemon-2").contains("\"host_ptys\":true"), "{}", st("daemon-2"));
        assert!(st("daemon-1").contains("\"host_ptys\":false"), "{}", st("daemon-1"));

        let list = Sb::out(&sb.run(&["daemon", "list"]));
        assert!(list.contains("daemon-1") && list.contains("daemon-2"), "{list}");
    }
}
