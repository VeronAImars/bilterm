//! `bilterm find [filters] [--act locate|focus|send|beep|flash|select]` (`UNI-R01`).
//!
//! Lowers to `find.<act>` with the same params as RPC/MCP; `--act` defaults to `locate`.

use super::{CliEnv, Parsed, RpcCall, base_params, str_val};
use crate::api::JsonValue;

const FILTERS: &[&str] = &["pid", "pgid", "sid", "pane", "limit", "pty", "cwd", "cmd", "name", "user", "env", "title", "text", "session"];
const ACTS: &[&str] = &["locate", "focus", "send", "beep", "flash", "select"];

pub(super) fn parse_find(p: &Parsed, env: &CliEnv) -> Result<RpcCall, String> {
    if let Some(stray) = p.pos.first() {
        return Err(format!("find takes --options only, got '{stray}' (bilterm find --pid N --act focus)"));
    }
    let mut act = p.flag("act").unwrap_or("locate").to_string();
    if p.flag("send").is_some() && p.flag("act").is_none() {
        act = "send".to_string();
    }
    if !ACTS.contains(&act.as_str()) {
        return Err(format!("unknown --act '{act}' (accepted: {})", ACTS.join(", ")));
    }
    let mut e: Vec<(&str, JsonValue)> = Vec::new();
    for key in FILTERS {
        if let Some(v) = p.flag(key) {
            e.push((key, str_val(v)));
        }
    }
    if act == "send" {
        let text = p.flag("send").ok_or("find --act send needs --send TEXT")?;
        e.push(("send", str_val(text)));
        if p.has("enter") {
            e.push(("enter", JsonValue::Bool(true)));
        }
    }
    if act == "select" && p.has("clear") {
        e.push(("clear", JsonValue::Bool(true)));
    }
    Ok(RpcCall { method: format!("find.{act}"), params: base_params(env, e), poll: None })
}
