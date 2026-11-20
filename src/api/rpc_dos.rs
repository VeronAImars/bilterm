//! JSON-RPC handlers for `dos.*` (D3a, UNI-R01): `dos.run|setup|doctor|remove|status|reset|
//! snapshot|restore`. Every method builds an `Action::Dos*` and runs it through
//! `WindowManager::apply`, the same path as `:dos ...`; the structured result is the action's
//! command data. Mutating verbs need `[api] allow_config_write` and an explicit `yes: true`
//! (there is no terminal to confirm on). Long verbs return `{"status":"started"}` and finish on a
//! worker; poll `dos.status` (`job.pending` / `job.last`).

use super::json::JsonValue;
use super::rpc::{AutomationControlPlane, SnapshotPolicy};
use crate::action::Action;

fn opt_str(p: &JsonValue, k: &str) -> Result<Option<String>, String> {
    match p.get(k) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("'{k}' must be a string")),
    }
}

fn flag(p: &JsonValue, k: &str) -> bool {
    p.get(k).and_then(JsonValue::as_bool).unwrap_or(false)
}

/// CLI-flag vector for the image verbs from RPC params (shared grammar with `bilterm dos ...`).
fn image_args(p: &JsonValue, flags: &[&str], valued: &[&str]) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for k in valued {
        if let Some(s) = opt_str(p, k)? {
            v.push(format!("--{k}"));
            v.push(s);
        }
    }
    for k in flags {
        if flag(p, k) {
            v.push(format!("--{k}"));
        }
    }
    Ok(v)
}

/// Builds the action for `dos.<verb>`; `None` for an unknown verb.
pub(super) fn dos_action(verb: &str, p: &JsonValue) -> Result<Option<Action>, String> {
    Ok(Some(match verb {
        "run" => Action::DosRun {
            name: opt_str(p, "name")?,
            image: opt_str(p, "image")?,
            drive_c: opt_str(p, "drive_c")?,
            backend: opt_str(p, "backend")?,
        },
        "setup" => {
            let entries = match p.get("entries") {
                None | Some(JsonValue::Null) => Vec::new(),
                Some(JsonValue::Array(a)) => a.iter().map(|e| e.as_str().map(str::to_string).ok_or("'entries' must be strings")).collect::<Result<_, _>>()?,
                Some(_) => return Err("'entries' must be an array of strings".into()),
            };
            Action::DosSetup { yes: flag(p, "yes"), full: flag(p, "full"), entries }
        }
        "doctor" => Action::DosDoctor,
        "status" => Action::DosStatus,
        "remove" => Action::DosRemove { all: flag(p, "all"), yes: flag(p, "yes") },
        "reset" => Action::DosReset { args: image_args(p, &["full", "yes"], &["name"])? },
        "restore" => Action::DosRestore { args: image_args(p, &["yes"], &["name", "tag"])? },
        "snapshot" => {
            let mut args = Vec::new();
            match opt_str(p, "op")?.as_deref() {
                None | Some("create") => {}
                Some(o @ ("list" | "rm")) => args.push(o.to_string()),
                Some(o) => return Err(format!("unknown snapshot op {o:?} (create|list|rm)")),
            }
            args.extend(image_args(p, &[], &["name", "tag"])?);
            Action::DosSnapshot { args }
        }
        _ => return Ok(None),
    }))
}

impl AutomationControlPlane {
    pub(super) fn rpc_dos(&mut self, method: &str, params: &JsonValue) -> Result<JsonValue, String> {
        let verb = method.strip_prefix("dos.").unwrap_or(method);
        let action = dos_action(verb, params)?.ok_or_else(|| format!("unknown method {method}"))?;
        let is_run = matches!(action, Action::DosRun { .. });
        if let Some(err) = self.wm.config_write_denied(&action) {
            return Err(err);
        }
        // Host paths from the automation plane would let a client point the emulator at arbitrary files.
        if let Action::DosRun { image, drive_c, .. } = &action
            && (image.is_some() || drive_c.is_some())
        {
            return Err("dos.run: image/drive_c are not allowed over the API".to_string());
        }
        self.wm.take_command_data();
        self.apply_with_policy(action, SnapshotPolicy::Trusted)?;
        if is_run {
            let msg = self.wm.active_pane().status_message().unwrap_or("").to_string();
            return if msg.starts_with("dos run:") || msg.starts_with("built without") {
                Err(msg)
            } else {
                Ok(JsonValue::obj([("status", JsonValue::String("ok".into())), ("message", JsonValue::String(msg))]))
            };
        }
        let data = self.wm.take_command_data().ok_or_else(|| format!("no {method} data (built without the `dos` feature?)"))?;
        let v = JsonValue::parse(&data).ok_or_else(|| format!("invalid {method} data"))?;
        // A failing doctor is a report (exit_code + output), not a failed call.
        let is_doctor = v.get("op").and_then(JsonValue::as_str) == Some("doctor");
        if !is_doctor && v.get("status").and_then(JsonValue::as_str) == Some("error") {
            let e = v.get("error").or_else(|| v.get("output")).and_then(JsonValue::as_str).unwrap_or("failed");
            return Err(format!("{method}: {}", e.trim_end()));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_lower_to_the_same_actions_as_the_colon_forms() {
        let p = |s: &str| JsonValue::parse(s).unwrap();
        let a = dos_action("setup", &p(r#"{"yes":true,"full":true,"entries":["x"]}"#)).unwrap().unwrap();
        assert_eq!(a.colon_form_or_summary(), ":dos setup --yes --full --entry x");
        let a = dos_action("restore", &p(r#"{"tag":"t","name":"a","yes":true}"#)).unwrap().unwrap();
        assert_eq!(a.colon_form_or_summary(), ":dos restore --name a --tag t --yes");
        let a = dos_action("snapshot", &p(r#"{"op":"list"}"#)).unwrap().unwrap();
        assert_eq!(a.colon_form_or_summary(), ":dos snapshot list");
        assert!(dos_action("snapshot", &p(r#"{"op":"zap"}"#)).is_err());
        assert!(dos_action("bogus", &p("{}")).unwrap().is_none());
    }
}
