//! Headless tests for `attach.kick` (colon parse, RPC/MCP lowering, errors, trust boundary).

use crate::action::{Action, DaemonSupervisorOp};
use crate::api::{AutomationControlPlane, JsonValue};
use crate::attach::msg::Role;
use crate::attach::registry::SharedRegistry;
use crate::colon_command::parse_colon_command;

fn plane_with(reg: &SharedRegistry) -> AutomationControlPlane {
    let mut p = AutomationControlPlane::new_headless(24, 80);
    p.wm.set_attach_registry(reg.clone());
    p
}

fn rpc(p: &mut AutomationControlPlane, method: &str, params: &str) -> JsonValue {
    let line = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    JsonValue::parse(&p.handle_jsonrpc_line(&line)).expect("json")
}

fn err_text(v: &JsonValue) -> String {
    v.get("error")
        .and_then(|e| e.get("message"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string()
}

#[test]
fn colon_forms_parse() {
    let k = |c: &str| parse_colon_command(c).unwrap().unwrap();
    assert_eq!(
        k(":daemon attach kick 7"),
        Action::DaemonSupervisor(DaemonSupervisorOp::AttachKick {
            client_id: Some(7),
            all_ro: false
        })
    );
    assert_eq!(
        k(":daemon attach kick --ro"),
        Action::DaemonSupervisor(DaemonSupervisorOp::AttachKick {
            client_id: None,
            all_ro: true
        })
    );
    assert_eq!(
        k(":daemon attach"),
        Action::DaemonSupervisor(DaemonSupervisorOp::AttachClients)
    );
    for bad in [
        ":daemon attach kick",
        ":daemon attach kick x",
        ":daemon attach kick 1 2",
        ":daemon attach kick -1",
    ] {
        assert!(parse_colon_command(bad).unwrap().is_err(), "{bad}");
    }
}

#[test]
fn rpc_kicks_one_client_and_reports_unknown() {
    let reg = SharedRegistry::new(4);
    let a = reg.reserve().unwrap();
    let b = reg.reserve().unwrap();
    let mut p = plane_with(&reg);
    let r = rpc(
        &mut p,
        "attach.kick",
        &format!(r#"{{"client_id":{}}}"#, a.id()),
    );
    let res = r.get("result").expect("result");
    assert_eq!(res.get("count").and_then(JsonValue::as_i64), Some(1));
    let r = rpc(&mut p, "attach.kick", r#"{"client_id":999}"#);
    assert!(err_text(&r).contains("unknown attach client 999"), "{r:?}");
    drop((a, b));
}

#[test]
fn rpc_validates_params() {
    let reg = SharedRegistry::new(2);
    let mut p = plane_with(&reg);
    for params in [
        "{}",
        r#"{"client_id":1,"ro":true}"#,
        r#"{"client_id":"x"}"#,
        r#"{"client_id":-3}"#,
    ] {
        assert!(
            rpc(&mut p, "attach.kick", params).get("error").is_some(),
            "{params}"
        );
    }
}

#[test]
fn kick_ro_only_hits_read_only_clients() {
    let reg = SharedRegistry::new(4);
    let rw = reg.reserve().unwrap();
    let ro = reg.reserve().unwrap();
    reg.lock()
        .set_attached(rw.id(), "m".into(), Role::Rw, 80, 24);
    reg.lock()
        .set_attached(ro.id(), "m".into(), Role::Ro, 80, 24);
    let mut p = plane_with(&reg);
    let r = rpc(&mut p, "attach.kick", r#"{"ro":true}"#);
    let kicked = r
        .get("result")
        .and_then(|x| x.get("kicked"))
        .and_then(JsonValue::as_array)
        .unwrap()
        .to_vec();
    assert_eq!(kicked.len(), 1);
    assert_eq!(kicked[0].as_i64(), Some(ro.id() as i64));
}

#[test]
fn disabled_listener_is_a_clear_error() {
    let mut p = AutomationControlPlane::new_headless(24, 80);
    let r = rpc(&mut p, "attach.kick", r#"{"client_id":1}"#);
    assert!(err_text(&r).contains("not enabled"), "{r:?}");
}

#[test]
fn mcp_attach_kick_maps_to_rpc() {
    let reg = SharedRegistry::new(2);
    let a = reg.reserve().unwrap();
    let mut p = plane_with(&reg);
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"bilterm_daemon","arguments":{{"action":"attach_kick","client_id":{}}}}}}}"#,
        a.id()
    );
    let out = p.handle_mcp_line(&call).expect("resp");
    assert!(!out.contains("\"isError\":true"), "{out}");
    let bad = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"bilterm_daemon","arguments":{"action":"attach_kick","client_id":4242}}}"#;
    assert!(p
        .handle_mcp_line(bad)
        .expect("resp")
        .contains("unknown attach client"));
}

#[test]
fn attach_origin_can_never_kick() {
    let reg = SharedRegistry::new(2);
    let a = reg.reserve().unwrap();
    let mut p = plane_with(&reg);
    p.wm.set_attach_origin(true);
    let act = Action::Daemon(DaemonSupervisorOp::AttachKick {
        client_id: Some(a.id()),
        all_ro: false,
    });
    let e =
        p.wm.apply_automation(act, None)
            .expect_err("must be refused");
    assert!(e.contains("cannot kick"), "{e}");
    p.wm.set_attach_origin(false);
}

#[test]
fn rpc_method_is_listed() {
    assert!(crate::api::RPC_METHODS.contains(&"attach.kick"));
}
