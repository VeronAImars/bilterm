//! Session find (`:find --pid ... `, `:find-<act>`): OS-attribute location of panes and actions on
//! the matches, through every surface (colon, RPC, MCP, CLI, key) lowering to one `Action`.

use bilterm::action::Action;
use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::colon_command::parse_colon_command;
use bilterm::pane::CompositePane;
use bilterm::pty::PtyConfig;
use bilterm::wm::session_find::{FindAct, SessionFindOp};
use bilterm::wm::{SplitDirection, WindowManager};

fn colon(wm: &mut WindowManager, cmd: &str) -> JsonValue {
    let action = parse_colon_command(cmd).unwrap_or_else(|| panic!("{cmd}: not a command")).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    wm.apply(action).unwrap();
    JsonValue::parse(&wm.take_command_data().unwrap_or_default()).unwrap_or(JsonValue::Null)
}

fn params(json: &str) -> JsonValue {
    JsonValue::parse(json).unwrap()
}

fn rows(v: &JsonValue) -> Vec<JsonValue> {
    v.get("rows").and_then(JsonValue::as_array).expect("rows").to_vec()
}

fn pane_ids(v: &JsonValue) -> Vec<i64> {
    rows(v).iter().filter_map(|r| r.get("pane").and_then(JsonValue::as_i64)).collect()
}

fn two_panes() -> (WindowManager, usize, usize) {
    let mut wm = WindowManager::new_headless(24, 80);
    let a = wm.active_pane_id();
    let b = wm.split_focused(SplitDirection::Vertical).unwrap();
    wm.pane_mut(a).unwrap().set_title("alpha build");
    wm.pane_mut(b).unwrap().set_title("beta shell");
    (wm, a, b)
}

#[test]
fn title_filter_locates_and_ands_filters() {
    let (mut wm, a, _b) = two_panes();
    let v = colon(&mut wm, ":find --title alpha");
    assert_eq!(pane_ids(&v), vec![a as i64]);
    assert!(v.get("platform").and_then(JsonValue::as_str).is_some());
    let v = colon(&mut wm, ":find --title alpha --pid 999999");
    assert!(rows(&v).is_empty());
    let v = colon(&mut wm, ":find --all");
    assert_eq!(rows(&v).len(), 2);
    assert_eq!(v.get("total").and_then(JsonValue::as_i64), Some(2));
}

#[test]
fn text_filter_searches_screen_and_counts_only() {
    let (mut wm, _a, b) = two_panes();
    wm.pane_mut(b).unwrap().vt_mut().process(b"needle in the haystack\r\nNEEDLE again\r\n");
    let v = colon(&mut wm, ":find --text needle");
    assert_eq!(pane_ids(&v), vec![b as i64]);
    assert_eq!(rows(&v)[0].get("text_hits").and_then(JsonValue::as_i64), Some(2));
    assert!(!v.to_json_string().contains("haystack"), "screen text is never returned");
}

#[test]
fn actions_flash_select_beep_focus_and_reuse_last_matches() {
    let (mut wm, a, b) = two_panes();
    colon(&mut wm, ":find --title beta");
    colon(&mut wm, ":find-flash");
    assert!(wm.find_is_flashing(b) && !wm.find_is_flashing(a));
    colon(&mut wm, ":find-select --title alpha");
    assert!(wm.find_is_selected(a) && !wm.find_is_selected(b));
    colon(&mut wm, ":find-select clear");
    assert!(wm.find_selection().is_empty());
    colon(&mut wm, ":find-beep --title beta");
    assert!(wm.pane(b).unwrap().vt().unread_bell());
    assert!(wm.take_host_bell() && !wm.take_host_bell());
    colon(&mut wm, ":find-focus --title alpha");
    assert_eq!(wm.active_pane_id(), a);
    let v = colon(&mut wm, ":find-focus --title nomatchatall");
    assert!(v.get("error").is_some());
}

#[test]
fn send_skips_read_only_and_caps_targets() {
    let (mut wm, a, b) = two_panes();
    wm.pane_mut(a).unwrap().set_read_only(true);
    let v = colon(&mut wm, ":find-send --send hi --enter");
    assert!(v.get("error").is_some(), "no last matches and no filters: error, not a broadcast: {v:?}");
    colon(&mut wm, ":find --all");
    let v = colon(&mut wm, ":find-send --send hi");
    let skipped: Vec<i64> = v.get("skipped").and_then(JsonValue::as_array).unwrap().iter().filter_map(|s| s.get("pane").and_then(JsonValue::as_i64)).collect();
    assert_eq!(skipped, vec![a as i64], "read-only pane skipped: {v:?}");
    let acted = v.get("acted").and_then(JsonValue::as_array).unwrap();
    assert!(acted.len() <= 1 && acted.iter().all(|x| x.as_i64() != Some(a as i64)));
    let _ = b;
}

#[test]
fn rpc_matches_colon_and_validates() {
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    let a = plane.wm.active_pane_id();
    plane.wm.pane_mut(a).unwrap().set_title("rpc target");
    let v = plane.dispatch_method("find.locate", &params(r#"{"title":"rpc"}"#)).unwrap();
    assert_eq!(pane_ids(&v), vec![a as i64]);
    let v = plane.dispatch_method("find.flash", &params(r#"{"title":"rpc"}"#)).unwrap();
    assert_eq!(v.get("action").and_then(JsonValue::as_str), Some("flash"));
    assert!(plane.wm.find_is_flashing(a));
    assert!(plane.dispatch_method("find.locate", &params(r#"{"pid":"abc"}"#)).is_err());
    assert!(plane.dispatch_method("find.locate", &params(r#"{"bogus":1}"#)).is_ok(), "unknown keys are ignored like other methods");
    assert!(plane.dispatch_method("find.send", &params(r#"{"title":"rpc"}"#)).is_err(), "send needs text");
    assert!(plane.dispatch_method("find.focus", &params(r#"{"title":"zzz"}"#)).is_err());
    let v = plane.dispatch_method("find.select", &params(r#"{"clear":true}"#)).unwrap();
    assert!(v.get("selected").is_some());
}

#[test]
fn mcp_tool_maps_to_find_methods() {
    let mut plane = AutomationControlPlane::new_headless(24, 80);
    let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_session_find","arguments":{"action":"locate"}}}"#;
    let resp = plane.handle_mcp_line(line).expect("response");
    assert!(resp.contains("rows"), "{resp}");
}

#[test]
fn cli_lowers_to_the_same_rpc() {
    use bilterm::cli::{CliEnv, parse};
    let args: Vec<String> = ["find", "--pid", "42", "--act", "send", "--send", "ls", "--enter"].iter().map(|s| s.to_string()).collect();
    let call = parse(&args, &CliEnv::default()).unwrap();
    assert_eq!(call.method, "find.send");
    assert_eq!(call.params.get("pid").and_then(JsonValue::as_str), Some("42"));
    let op = bilterm::api::rpc_find::op_from_params(&call.method, &call.params).unwrap();
    let colon_op = match parse_colon_command(":find-send --pid 42 --send ls --enter").unwrap().unwrap() {
        Action::SessionFind(op) => op,
        other => panic!("{other:?}"),
    };
    assert_eq!(op, colon_op);
    let bad: Vec<String> = ["find", "--act", "send"].iter().map(|s| s.to_string()).collect();
    assert!(parse(&bad, &CliEnv::default()).is_err());
    let loc: Vec<String> = ["find", "--text", "x"].iter().map(|s| s.to_string()).collect();
    assert_eq!(parse(&loc, &CliEnv::default()).unwrap().method, "find.locate");
}

#[test]
fn colon_forms_round_trip_and_key_prefills() {
    for cmd in [":find --pid 7 --cwd /tmp", ":find-flash --text a b", ":find-send --pty pts/3 --send ls -l --enter", ":find-select clear", ":find --all"] {
        let a = parse_colon_command(cmd).unwrap().unwrap();
        let again = parse_colon_command(&a.colon_form_or_summary()).unwrap().unwrap();
        assert_eq!(a, again, "{cmd} -> {}", a.colon_form_or_summary());
    }
    assert!(parse_colon_command(":find --pid").unwrap().is_err());
    assert!(parse_colon_command(":find-send --cwd /x").unwrap().is_err());
    // legacy scrollback search is untouched
    assert!(matches!(parse_colon_command(":find hello").unwrap().unwrap(), Action::SearchQuery(_)));
    let mut wm = WindowManager::new_headless(24, 80);
    for _ in 0..2 {
        wm.split_focused(SplitDirection::Vertical).unwrap();
    }
    wm.resize_all(ratatui::layout::Rect::new(0, 0, 80, 24)).unwrap();
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let chord = KeyModifiers::CONTROL | KeyModifiers::ALT;
    bilterm::input::handle_wm_key_event(&mut wm, KeyEvent::new(KeyCode::Char('d'), chord)).unwrap();
    assert_eq!(wm.active_pane().prompt().text(), ":find --text ");
    let _ = (FindAct::Focus, SessionFindOp::Prefill);
}

#[cfg(target_os = "linux")]
#[test]
fn real_pty_pane_is_found_by_pid_pty_cwd_and_name() {
    let cfg = PtyConfig {
        rows: 24,
        cols: 80,
        program: "/bin/sh".to_string(),
        args: vec!["-c".into(), "exec sleep 30".into()],
        cwd: Some(std::path::PathBuf::from("/")),
        ..Default::default()
    };
    let pane = CompositePane::spawn(1, cfg).expect("spawn sh");
    let pid = pane.child_pid().expect("child pid");
    let mut wm = WindowManager::from_initial_pane(pane, false);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let v = colon(&mut wm, &format!(":find --pid {pid}"));
    assert_eq!(rows(&v).len(), 1, "{v:?}");
    let row = &rows(&v)[0];
    let tty = row.get("tty").and_then(JsonValue::as_str).expect("pty path").to_string();
    assert!(tty.starts_with("/dev/pts/"), "{tty}");
    assert_eq!(rows(&colon(&mut wm, &format!(":find --pty {tty}"))).len(), 1);
    assert_eq!(rows(&colon(&mut wm, ":find --name sleep")).len(), 1, "exec'd sleep is the shell pid and foreground");
    assert_eq!(rows(&colon(&mut wm, ":find --cwd / --name sleep")).len(), 1);
    assert!(rows(&colon(&mut wm, ":find --pid 1")).is_empty());
    let sid = row.get("shell").and_then(|s| s.get("sid")).and_then(JsonValue::as_i64).expect("sid");
    assert_eq!(rows(&colon(&mut wm, &format!(":find --sid {sid}"))).len(), 1);
    let v = colon(&mut wm, ":find-send --pid 0 --send x");
    assert!(v.get("error").is_some());
    let v = colon(&mut wm, &format!(":find-send --pid {pid} --send echo hi --enter"));
    assert_eq!(v.get("acted").and_then(JsonValue::as_array).map(|a| a.len()), Some(1), "{v:?}");
}
