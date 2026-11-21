//! JSON-RPC handlers for session find (`find.locate|focus|send|beep|flash|select`, `UNI-R01`).
//!
//! Each method builds the same [`SessionFindOp`] the `:find ...` colon form, the CLI, the MCP tool,
//! the palette and `Ctrl+Alt+D` use and runs it through `WindowManager::apply`, returning the JSON
//! the executor stored in `command_data`. Params are the query filters (`pid`, `pgid`, `sid`,
//! `pty`, `cwd`, `cmd`, `name`, `user`, `env`, `title`, `text`, `session`, `pane`, `limit`);
//! `find.send` adds `text_to_send` (`send`) and `enter`; `find.select` accepts `clear: true`.
//! `find.send` is refused on a daemon without `--host-ptys` (like `pane.send`) and skips
//! read-only and `!ECHO` password panes.

use super::json::JsonValue;
use super::rpc::{AutomationControlPlane, SnapshotPolicy};
use crate::action::Action;
use crate::wm::session_find::{FindAct, FindQuery, SessionFindOp};

const FILTERS: &[&str] = &["pid", "pgid", "sid", "pane", "limit", "pty", "cwd", "cmd", "name", "user", "env", "title", "text", "session"];

/// Builds a query from RPC params (numbers or strings accepted for numeric filters).
pub fn query_from_params(params: &JsonValue) -> Result<FindQuery, String> {
    let mut q = FindQuery::default();
    for key in FILTERS {
        let raw = match params.get(key) {
            None | Some(JsonValue::Null) => continue,
            Some(JsonValue::String(s)) => s.clone(),
            Some(JsonValue::Number(n)) => n.to_string(),
            Some(_) => return Err(format!("invalid '{key}' parameter")),
        };
        q.set(key, &raw)?;
    }
    Ok(q)
}

/// Op for `method` (`find.<verb>`) and `params`.
pub fn op_from_params(method: &str, params: &JsonValue) -> Result<SessionFindOp, String> {
    let query = query_from_params(params)?;
    let act = match method {
        "find.locate" => return Ok(SessionFindOp::Locate(Box::new(query))),
        "find.focus" => FindAct::Focus,
        "find.beep" => FindAct::Beep,
        "find.flash" => FindAct::Flash,
        "find.select" => {
            if params.get("clear").and_then(JsonValue::as_bool).unwrap_or(false) {
                return Ok(SessionFindOp::ClearSelection);
            }
            FindAct::Select
        }
        "find.send" => {
            let text = params
                .get("send")
                .and_then(JsonValue::as_str)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| "missing 'send' parameter (the text to type)".to_string())?;
            FindAct::Send { text: text.to_string(), enter: params.get("enter").and_then(JsonValue::as_bool).unwrap_or(false) }
        }
        other => return Err(format!("Method not found: {other}")),
    };
    Ok(SessionFindOp::Act { act, query: Box::new(query) })
}

impl AutomationControlPlane {
    pub(super) fn dispatch_session_find(&mut self, method: &str, params: &JsonValue) -> Result<JsonValue, String> {
        let op = op_from_params(method, params)?;
        // The RPC is not the local user: attribute any injected text to `api` (traffic graph, pane-mail envelope).
        let prev = self.wm.is_api_origin();
        self.wm.set_api_origin(true);
        let applied = self.apply_with_policy(Action::SessionFind(op), SnapshotPolicy::Trusted);
        self.wm.set_api_origin(prev);
        applied?;
        let data = self.wm.take_command_data().unwrap_or_default();
        let value = JsonValue::parse(&data).ok_or_else(|| "find produced no result".to_string())?;
        match value.get("error").and_then(JsonValue::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(value),
        }
    }
}
