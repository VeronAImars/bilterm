//! Parser for `:mail` (pane-mail envelope): `:mail status [pane]`, `:mail ack [pane|all]`.

use super::ColonCommand;
use crate::action::Action;
use crate::wm::pane_mail::PaneMailOp;

const USAGE: &str = "Usage: :mail status [pane] | :mail ack [pane|all]";

pub(super) fn parse_pane_mail_colon_command(head: &str, parts: &[&str]) -> Option<Result<ColonCommand, String>> {
    if head != "mail" {
        return None;
    }
    let pane = |s: &&str| s.trim_start_matches('#').parse::<usize>().map_err(|_| format!("invalid pane '{s}'; {USAGE}"));
    let op = match (parts.get(1).copied(), parts.get(2), parts.len()) {
        (None | Some("status"), None, _) => Ok(PaneMailOp::Status { pane: None }),
        (Some("status"), Some(p), 3) => pane(p).map(|p| PaneMailOp::Status { pane: Some(p) }),
        (Some("ack" | "read"), None, _) => Ok(PaneMailOp::Ack { pane: None, all: false }),
        (Some("ack" | "read"), Some(&"all"), 3) => Ok(PaneMailOp::Ack { pane: None, all: true }),
        (Some("ack" | "read"), Some(p), 3) => pane(p).map(|p| PaneMailOp::Ack { pane: Some(p), all: false }),
        _ => Err(USAGE.to_string()),
    };
    Some(op.map(Action::PaneMail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colon_command::parse_colon_command as p;

    #[test]
    fn grammar() {
        assert_eq!(p(":mail"), Some(Ok(Action::PaneMail(PaneMailOp::Status { pane: None }))));
        assert_eq!(p(":mail status 3"), Some(Ok(Action::PaneMail(PaneMailOp::Status { pane: Some(3) }))));
        assert_eq!(p(":mail ack"), Some(Ok(Action::PaneMail(PaneMailOp::Ack { pane: None, all: false }))));
        assert_eq!(p(":mail read #2"), Some(Ok(Action::PaneMail(PaneMailOp::Ack { pane: Some(2), all: false }))));
        assert_eq!(p(":mail ack all"), Some(Ok(Action::PaneMail(PaneMailOp::Ack { pane: None, all: true }))));
        assert!(matches!(p(":mail ack x"), Some(Err(_))));
        assert!(matches!(p(":mail bogus"), Some(Err(_))));
    }
}
