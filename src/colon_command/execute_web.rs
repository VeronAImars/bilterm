//! Executor for `Action::Web` / `:web` (`WEB-FE-01..04`, `UNI-R01`, Phase 60).
//!
//! Every surface (keys, colon, palette, menu, `web.*` RPC, `bilterm_web_surface` MCP) lowers to
//! this one function, so they all report the same `WebSurfaceStatus` JSON via `command_data`.

use crate::action::WebOp;
use crate::wm::WindowManager;

/// Applies `op` to the workspace's shared web surface; never quits.
pub fn execute_web_action(wm: &mut WindowManager, op: &WebOp) -> bool {
    let surface = wm.web_surface().clone();
    let msg = match op {
        WebOp::Status => surface.status().summary(),
        WebOp::OpenBrowser => {
            let status = surface.status();
            if !status.enabled {
                status.summary()
            } else if wm.is_headless() || wm.is_api_origin() {
                format!("Web UI: {}", status.url)
            } else {
                match open_in_browser(&status.url) {
                    Ok(()) => format!("Web UI: opened {}", status.url),
                    Err(e) => format!("Web UI: {} (could not launch browser: {e})", status.url),
                }
            }
        }
        WebOp::SetPort(port) => match surface.set_port(*port) {
            Ok(()) => format!("Web UI: port set to {port} (used on next start)"),
            Err(e) => {
                wm.record_automation_error(e.clone());
                format!("Web UI: {e}")
            }
        },
        WebOp::Stop => {
            if surface.is_enabled() {
                surface.request_stop();
                "Web UI: stopping (no new connections accepted)".to_string()
            } else {
                "Web UI: not running".to_string()
            }
        }
    };
    wm.set_command_data(surface.status().to_json().to_json_string());
    wm.active_pane_mut().set_status_message(Some(msg));
    false
}

fn open_in_browser(url: &str) -> std::io::Result<()> {
    if std::env::var_os("BILTERM_NO_BROWSER").is_some() {
        return Err(std::io::Error::other("BILTERM_NO_BROWSER is set"));
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Hint appended to the status line while an external-browser offer is pending.
pub const EXTERNAL_OFFER_PROMPT: &str = "Open in system browser? [y/N] Ctrl+Alt+U or :web external";

/// Records a non-blocking offer to open `url` in the system browser and shows it on pane `pane_id`'s
/// status line. No-op when `[browser] offer_external = false`, for API-originated actions, or when
/// `url` could never be opened externally. Nothing is launched here.
pub fn offer_external_browser(wm: &mut WindowManager, url: &str, pane_id: crate::pane::PaneId) {
    if !wm.config().browser.offer_external || wm.is_api_origin() {
        return;
    }
    let Ok(valid) = crate::ext_browser::validate_external_url(url) else {
        return;
    };
    wm.pending_external_url = Some(valid);
    if let Some(pane) = wm.pane_mut(pane_id) {
        let base = pane.status_message().map(str::to_string);
        let msg = match base {
            Some(b) if !b.is_empty() => format!("{b} | {EXTERNAL_OFFER_PROMPT}"),
            _ => EXTERNAL_OFFER_PROMPT.to_string(),
        };
        pane.set_status_message(Some(msg));
    }
}

/// Executor for `Action::WebOpenExternal`: opens `url`, else the pending offer, else the active
/// browser canvas URL, in the system default browser. Headless and `BILTERM_NO_BROWSER` runs
/// report what would be opened and launch nothing. Never quits.
pub fn execute_web_open_external(wm: &mut WindowManager, url: Option<&str>) -> bool {
    let candidate = url
        .map(str::to_string)
        .or_else(|| wm.pending_external_url.clone())
        .or_else(|| wm.active_pane().browser_canvas().map(|b| b.current_url().to_string()));
    let (ok, msg) = match candidate {
        None => (false, "Web: no URL to open (give one: :web external <url>)".to_string()),
        Some(raw) => match crate::ext_browser::validate_external_url(&raw) {
            Err(e) => (false, format!("Web: cannot open externally: {e}")),
            Ok(valid) => {
                wm.pending_external_url = None;
                if wm.is_headless() {
                    (true, format!("Web: would open {valid} in the system browser (headless)"))
                } else {
                    match crate::ext_browser::launch_detached(&valid) {
                        Ok(()) => (true, format!("Web: opened {valid} in the system browser")),
                        Err(e) => (false, format!("Web: could not launch system browser for {valid}: {e}")),
                    }
                }
            }
        },
    };
    if !ok {
        wm.record_automation_error(msg.clone());
    }
    let data = format!("{{\"ok\":{ok},\"message\":{}}}", crate::api::JsonValue::String(msg.clone()).to_json_string());
    wm.set_command_data(data);
    wm.active_pane_mut().set_status_message(Some(msg));
    false
}

#[cfg(test)]
mod external_tests {
    use super::*;
    use crate::action::{Action, BrowserOp};
    use crate::api::{AutomationControlPlane, JsonValue};

    fn msg(wm: &WindowManager) -> String {
        wm.active_pane().status_message().unwrap_or("").to_string()
    }

    #[test]
    fn headless_open_external_reports_without_launching() {
        let mut wm = WindowManager::new_headless(100, 30);
        wm.apply(Action::WebOpenExternal { url: Some("https://example.com/x".into()) }).unwrap();
        assert!(msg(&wm).contains("would open https://example.com/x"));
        let v = JsonValue::parse(&wm.take_command_data().unwrap()).unwrap();
        assert_eq!(v.get("ok"), Some(&JsonValue::Bool(true)));
    }

    #[test]
    fn bad_scheme_or_missing_url_is_refused() {
        let mut wm = WindowManager::new_headless(100, 30);
        let e = wm.apply_automation(Action::WebOpenExternal { url: Some("javascript:alert(1)".into()) }, None);
        assert!(e.is_err());
        let e = wm.apply_automation(Action::WebOpenExternal { url: None }, None);
        assert!(e.unwrap_err().contains("no URL"));
    }

    #[test]
    fn browser_open_raises_offer_and_accept_consumes_it() {
        let mut wm = WindowManager::new_headless(100, 30);
        execute_browser_open(&mut wm, "https://example.com/offer");
        assert_eq!(wm.pending_external_url.as_deref(), Some("https://example.com/offer"));
        assert!(msg(&wm).contains("Open in system browser? [y/N]"));
        wm.apply(Action::WebOpenExternal { url: None }).unwrap();
        assert!(msg(&wm).contains("would open https://example.com/offer"));
        assert!(wm.pending_external_url.is_none());
    }

    #[test]
    fn offer_respects_config_and_api_origin_and_bad_urls() {
        let mut wm = WindowManager::new_headless(100, 30);
        wm.config_mut().browser.offer_external = false;
        execute_browser_open(&mut wm, "https://example.com");
        assert!(wm.pending_external_url.is_none());
        wm.config_mut().browser.offer_external = true;
        execute_browser_open(&mut wm, "about:blank");
        assert!(wm.pending_external_url.is_none());
    }

    #[test]
    fn rpc_is_gated_like_config_writes_and_validates() {
        let mut plane = AutomationControlPlane::new_headless(30, 100);
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"web.open_external","params":{"url":"https://example.com"}}"#;
        let out = plane.handle_jsonrpc_line(line);
        assert!(out.contains("allow_config_write"), "{out}");
        plane.wm.config_mut().api.allow_config_write = true;
        let out = plane.handle_jsonrpc_line(line);
        assert!(out.contains("would open https://example.com"), "{out}");
        let bad = r#"{"jsonrpc":"2.0","id":2,"method":"web.open_external","params":{"url":"file"}}"#;
        assert!(plane.handle_jsonrpc_line(bad).contains("error"));
    }

    fn execute_browser_open(wm: &mut WindowManager, url: &str) {
        let _ = wm.apply(Action::Browser(BrowserOp::Open { url: url.into(), pane_id: None }));
    }
}
