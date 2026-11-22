//! Executors for `Action::Dos*`. `DosRun` (D2a) opens a DOS pane through
//! [`WindowManager::spawn_dos_pane`]; the lifecycle verbs (D3a) run the shared `bilterm dos` core
//! ([`crate::dos::ops`]), long ones on a worker so the UI loop never blocks. Outcomes go to the
//! status bar and to the structured command data (`wm.take_command_data()`, read by RPC/MCP).

use crate::wm::WindowManager;

#[cfg(feature = "dos")]
pub(super) fn execute_dos_run(
    wm: &mut WindowManager,
    name: Option<String>,
    image: Option<String>,
    drive_c: Option<String>,
    backend: Option<String>,
) {
    use crate::dos::launch::{RunRequest, with_host_env};
    // Host paths from the automation plane are refused until the D3 surface defines a policy
    // (an RPC client must not be able to point the emulator at arbitrary host files).
    if wm.is_api_origin() && (image.is_some() || drive_c.is_some()) {
        wm.active_pane_mut()
            .set_status_message(Some("dos run: --image/--drive-c are not allowed from the API".into()));
        return;
    }
    let req = RunRequest {
        name,
        image: image.map(Into::into),
        drive_c: drive_c.map(Into::into),
        backend,
    };
    let exe = match crate::daemon::process::running_exe() {
        Ok(e) => e,
        Err(e) => {
            wm.active_pane_mut().set_status_message(Some(format!("dos run: cannot locate bilterm: {e}")));
            return;
        }
    };
    // The pane size is checked by `spawn_dos_pane` after the split; no terminal size here.
    let res = with_host_env(None, |env| wm.spawn_dos_pane(&req, env, &exe));
    let msg = match res {
        Ok(Ok(id)) => format!("DOS pane #{id} opened"),
        Ok(Err(e)) => format!("dos run: {e}"),
        Err(e) => format!("dos run: {e}"),
    };
    wm.active_pane_mut().set_status_message(Some(msg));
}

#[cfg(not(feature = "dos"))]
pub(super) fn execute_dos_run(
    wm: &mut WindowManager,
    _name: Option<String>,
    _image: Option<String>,
    _drive_c: Option<String>,
    _backend: Option<String>,
) {
    wm.active_pane_mut().set_status_message(Some(
        "built without the `dos` feature (rebuild with `cargo build --features dos`)".into(),
    ));
}

#[cfg(feature = "dos")]
pub(super) fn execute_dos_op(wm: &mut WindowManager, action: crate::action::Action) {
    use crate::action::Action;
    use crate::api::json::JsonValue;
    use crate::dos::cli::Cmd;
    use crate::dos::ops;
    let cmd = match action {
        Action::DosSetup { yes, full, entries } => Ok(Cmd::Setup { yes, full, entries }),
        Action::DosRemove { all, yes } => Ok(Cmd::Remove { all, yes }),
        Action::DosDoctor => Ok(Cmd::Doctor),
        Action::DosStatus => Ok(Cmd::Status),
        Action::DosReset { args } => ops::parse_image("reset", &args),
        Action::DosSnapshot { args } => ops::parse_image("snapshot", &args),
        Action::DosRestore { args } => ops::parse_image("restore", &args),
        _ => return,
    };
    let cmd = match cmd {
        Ok(c) => c,
        Err(e) => {
            let msg = e.trim_end().to_string();
            wm.set_command_data(JsonValue::obj([("status", JsonValue::String("error".into())), ("error", JsonValue::String(msg.clone()))]).to_json_string());
            wm.active_pane_mut().set_status_message(Some(format!("dos: {msg}")));
            return;
        }
    };
    let name = ops::op_name(&cmd);
    let last_line = |t: &str| t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("done").to_string();
    if matches!(cmd, Cmd::Status) {
        match ops::status_json() {
            Ok(JsonValue::Object(mut m)) => {
                let pending = wm.dos_job_pending().map_or(JsonValue::Null, |l| JsonValue::String(l.to_string()));
                let last = wm.dos_job_last().map_or(JsonValue::Null, |d| {
                    JsonValue::obj([
                        ("op", JsonValue::String(d.label.clone())),
                        ("ok", JsonValue::Bool(d.ok)),
                        ("message", JsonValue::String(d.message.clone())),
                    ])
                });
                m.insert("job".into(), JsonValue::obj([("pending", pending), ("last", last)]));
                wm.set_command_data(JsonValue::Object(m).to_json_string());
                let text = ops::run_cmd(&cmd).output;
                wm.active_pane_mut().set_status_message(Some(format!("dos status: {}", text.lines().nth(1).unwrap_or("ok"))));
            }
            Ok(_) => {}
            Err(e) => wm.active_pane_mut().set_status_message(Some(format!("dos status: {e}"))),
        }
        return;
    }
    if ops::is_long_running(&cmd) && !wm.is_headless() {
        let job_cmd = cmd;
        let started = wm.spawn_dos_job(name, move || {
            let r = ops::run_cmd(&job_cmd);
            (r.ok(), r.output)
        });
        let (status, text) = match started {
            Ok(()) => ("started", format!("dos {name}: started in the background (see :dos status)")),
            Err(e) => ("error", format!("dos {name}: {e}")),
        };
        let mut o = vec![("op", JsonValue::String(name.into())), ("status", JsonValue::String(status.into()))];
        if status == "error" {
            o.push(("error", JsonValue::String(text.clone())));
        }
        wm.set_command_data(JsonValue::obj(o).to_json_string());
        wm.active_pane_mut().set_status_message(Some(text));
        return;
    }
    let r = ops::run_cmd(&cmd);
    wm.set_command_data(ops::result_json(name, &r).to_json_string());
    let msg = format!("dos {name}: {} {}", if r.ok() { "ok" } else { "FAILED" }, last_line(&r.output));
    wm.active_pane_mut().set_status_message(Some(msg));
}

#[cfg(not(feature = "dos"))]
pub(super) fn execute_dos_op(wm: &mut WindowManager, _action: crate::action::Action) {
    wm.active_pane_mut().set_status_message(Some(
        "built without the `dos` feature (rebuild with `cargo build --features dos`)".into(),
    ));
}

#[cfg(all(test, feature = "dos"))]
mod tests {
    use crate::action::Action;
    use crate::api::json::JsonValue;
    use crate::wm::WindowManager;

    #[test]
    fn doctor_and_bad_args_report_structured_data_headless() {
        let mut wm = WindowManager::new_headless(100, 30);
        wm.apply(Action::DosDoctor).unwrap();
        let v = JsonValue::parse(&wm.take_command_data().unwrap()).unwrap();
        assert_eq!(v.get("op").and_then(JsonValue::as_str), Some("doctor"));
        assert!(v.get("exit_code").is_some());
        wm.apply(Action::DosRestore { args: vec![] }).unwrap();
        let v = JsonValue::parse(&wm.take_command_data().unwrap()).unwrap();
        assert_eq!(v.get("status").and_then(JsonValue::as_str), Some("error"));
    }

    #[test]
    fn api_gate_covers_mutating_verbs_only() {
        let wm = WindowManager::new_headless(80, 25);
        let setup = Action::DosSetup { yes: true, full: false, entries: vec![] };
        assert!(wm.config_write_denied(&setup).is_some());
        assert!(wm.config_write_denied(&Action::DosRestore { args: vec![] }).is_some());
        assert!(wm.config_write_denied(&Action::DosSnapshot { args: vec!["create".into()] }).is_some());
        for ok in [Action::DosStatus, Action::DosDoctor, Action::DosSnapshot { args: vec!["list".into()] }] {
            assert!(wm.config_write_denied(&ok).is_none(), "{ok:?}");
        }
    }
}
