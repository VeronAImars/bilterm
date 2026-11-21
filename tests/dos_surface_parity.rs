//! D3a: surface parity for the DOS verbs (`UNI-R01`): one verb, one `Action`, reachable as colon
//! command, palette entry, `dos.*` RPC and `bilterm_dos` MCP tool. Runs against an empty
//! throwaway data dir; nothing is downloaded or built.
#![cfg(feature = "dos")]

use bilterm::action::Action;
use bilterm::api::{AutomationControlPlane, JsonValue};
use bilterm::colon_command::parse_colon_command;
use bilterm::ui::modals::PALETTE_CATALOG;

fn sandbox() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    // SAFETY: serialized by ENV_LOCK; every test here calls sandbox() first.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", d.path());
        std::env::set_var("HOME", d.path());
    }
    (g, d)
}

const VERBS: &[(&str, &str, &str)] = &[
    // (verb, colon form, palette head)
    ("run", ":dos run", ":dos run"),
    ("setup", ":dos setup --yes", ":dos setup"),
    ("doctor", ":dos doctor", ":dos doctor"),
    ("remove", ":dos remove --yes", ":dos remove"),
    ("status", ":dos status", ":dos status"),
    ("reset", ":dos reset --yes", ":dos reset"),
    ("snapshot", ":dos snapshot", ":dos snapshot"),
    ("restore", ":dos restore --tag t --yes", ":dos restore --tag <T>"),
];

#[test]
fn every_dos_verb_has_colon_palette_rpc_and_mcp() {
    let mcp_src = include_str!("../src/api/mcp.rs");
    for (verb, colon, palette) in VERBS {
        let a: Action = parse_colon_command(colon).unwrap().unwrap_or_else(|e| panic!("{colon}: {e}"));
        assert!(a.colon_form_or_summary().starts_with(":dos "), "{colon}");
        assert!(PALETTE_CATALOG.iter().any(|e| e.0 == *palette), "palette entry for {verb}");
        assert!(bilterm::api::rpc::RPC_METHODS.contains(&format!("dos.{verb}").as_str()), "RPC dos.{verb}");
        assert!(mcp_src.contains(&format!("\"{verb}\" => Some(\"dos.{verb}\")")), "MCP bilterm_dos action {verb}");
    }
}

#[test]
fn rpc_and_mcp_status_doctor_and_gate_agree() {
    let (_g, _d) = sandbox();
    let mut cp = AutomationControlPlane::new_headless(30, 100);
    let none = JsonValue::parse("{}").unwrap();
    let st = cp.dispatch_method("dos.status", &none).unwrap();
    assert_eq!(st.get("images").and_then(|v| if let JsonValue::Array(a) = v { Some(a.len()) } else { None }), Some(0));
    assert!(st.get("job").is_some());
    let doc = cp.dispatch_method("dos.doctor", &none).unwrap();
    assert_eq!(doc.get("op").and_then(JsonValue::as_str), Some("doctor"));
    // Mutating verbs are refused over the API unless `[api] allow_config_write` is set.
    let yes = JsonValue::parse(r#"{"yes":true}"#).unwrap();
    let err = cp.dispatch_method("dos.setup", &yes).unwrap_err();
    assert!(err.contains("allow_config_write"), "{err}");
    // Read-only snapshot list is allowed; bad params surface as errors.
    let list = JsonValue::parse(r#"{"op":"list"}"#).unwrap();
    assert!(cp.dispatch_method("dos.snapshot", &list).is_ok());
    assert!(cp.dispatch_method("dos.restore", &yes).is_err());
    // MCP routes to the same handler.
    let out = cp
        .handle_mcp_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bilterm_dos","arguments":{"action":"status"}}}"#)
        .unwrap();
    assert!(out.contains("images") && !out.contains("\"isError\":true"), "{out}");
}
