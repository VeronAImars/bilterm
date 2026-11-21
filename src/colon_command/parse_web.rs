//! Parser for `:web` colon commands (`WEB-FE-01..04`, `UNI-R01`, Phase 60).
//!
//! Syntax: `:web` | `:web status` | `:web open` | `:web port <N>` | `:web stop`

use super::ColonCommand;
use crate::action::WebOp;

pub const WEB_USAGE: &str = "Usage: :web [status | open [--external [url]] | external [url] | port <1-65535> | stop]";

/// Parses `:web ...` into `Action::Web`; `None` when `head` is not `web`.
pub fn parse_web_command(head: &str, parts: &[&str]) -> Option<Result<ColonCommand, String>> {
    if head != "web" {
        return None;
    }
    let sub = parts.get(1).copied().unwrap_or("status");
    // `:web external [url]` and `:web open --external [url]` hand a URL to the system browser.
    let external_args = match (sub, parts.get(2).copied()) {
        ("external", _) => Some(&parts[2..]),
        ("open", Some("--external")) => Some(&parts[3..]),
        _ => None,
    };
    if let Some(args) = external_args {
        return Some(match args {
            [] => Ok(ColonCommand::WebOpenExternal { url: None }),
            [url] => Ok(ColonCommand::WebOpenExternal { url: Some((*url).to_string()) }),
            _ => Err(WEB_USAGE.to_string()),
        });
    }
    let op = match (sub, parts.len()) {
        ("status", 1 | 2) => Ok(WebOp::Status),
        ("open", 2) => Ok(WebOp::OpenBrowser),
        ("stop", 2) => Ok(WebOp::Stop),
        ("port", 3) => match parts[2].parse::<u16>() {
            Ok(p) if p > 0 => Ok(WebOp::SetPort(p)),
            _ => Err(WEB_USAGE.to_string()),
        },
        _ => Err(WEB_USAGE.to_string()),
    };
    Some(op.map(ColonCommand::Web))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<Result<ColonCommand, String>> {
        let parts: Vec<&str> = s.split_whitespace().collect();
        parse_web_command(parts[0], &parts)
    }

    #[test]
    fn parses_all_forms() {
        assert_eq!(p("web"), Some(Ok(ColonCommand::Web(WebOp::Status))));
        assert_eq!(p("web status"), Some(Ok(ColonCommand::Web(WebOp::Status))));
        assert_eq!(p("web open"), Some(Ok(ColonCommand::Web(WebOp::OpenBrowser))));
        assert_eq!(p("web stop"), Some(Ok(ColonCommand::Web(WebOp::Stop))));
        assert_eq!(p("web port 9000"), Some(Ok(ColonCommand::Web(WebOp::SetPort(9000)))));
        assert!(matches!(p("web port 0"), Some(Err(_))));
        assert!(matches!(p("web port 70000"), Some(Err(_))));
        assert!(matches!(p("web port"), Some(Err(_))));
        assert!(matches!(p("web bogus"), Some(Err(_))));
        assert!(matches!(p("web status extra"), Some(Err(_))));
        assert_eq!(p("webx"), None);
        assert_eq!(p("web external"), Some(Ok(ColonCommand::WebOpenExternal { url: None })));
        assert_eq!(p("web open --external"), Some(Ok(ColonCommand::WebOpenExternal { url: None })));
        assert_eq!(
            p("web external https://a.b/c"),
            Some(Ok(ColonCommand::WebOpenExternal { url: Some("https://a.b/c".into()) }))
        );
        assert_eq!(
            p("web open --external https://a.b/c"),
            Some(Ok(ColonCommand::WebOpenExternal { url: Some("https://a.b/c".into()) }))
        );
        assert!(matches!(p("web external a b"), Some(Err(_))));
    }
}
