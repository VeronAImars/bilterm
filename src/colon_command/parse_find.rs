//! Parser for `:find agent|session|history [text]` and the `:finder` modal verbs (Phase 50.1-01).
//!
//! D-U1: only `agent|agents|session|sessions|history` are reserved first words of `:find`; every
//! other `:find ...` returns `None` so the legacy scrollback-search arm in `parse.rs` keeps
//! handling it byte-identically.

use super::ColonCommand;
use crate::action::Action;
use crate::wm::find_cmd::{FindKind, FindOp, cap_query};

const FINDER_USAGE: &str = "Usage: :finder kind <agent|session|history|next|prev> | query [text] | move <+n|-n|top|bottom> | select <n> | enter | prefill | close";

fn find(op: FindOp) -> Option<Result<ColonCommand, String>> {
    Some(Ok(Action::Find(op)))
}

fn usage() -> Option<Result<ColonCommand, String>> {
    Some(Err(FINDER_USAGE.to_string()))
}

/// Returns `Some` for `:find <reserved word> ...` and every `:finder ...`; `None` otherwise.
pub(super) fn parse_find_colon_command(
    head: &str,
    parts: &[&str],
) -> Option<Result<ColonCommand, String>> {
    match head {
        "find" if parts.get(1).is_some_and(|p| p.starts_with("--")) => parse_session_find(None, &parts[1..]),
        "find-focus" | "find-send" | "find-beep" | "find-flash" | "find-select" | "find-prefill" => {
            parse_session_find(Some(head), &parts[1..])
        }
        "find" => {
            let kind = parts.get(1).copied().and_then(FindKind::parse)?;
            find(FindOp::Open { kind, query: cap_query(&parts[2..].join(" ")) })
        }
        "finder" => parse_finder(&parts[1..]),
        _ => None,
    }
}

fn parse_finder(args: &[&str]) -> Option<Result<ColonCommand, String>> {
    match (args.first().copied(), args.len()) {
        (Some("kind"), 2) => match args[1] {
            "next" => find(FindOp::CycleKind(true)),
            "prev" => find(FindOp::CycleKind(false)),
            raw => match FindKind::parse(raw) {
                Some(kind) => find(FindOp::SetKind(kind)),
                None => usage(),
            },
        },
        (Some("query"), 1) => find(FindOp::ClearQuery),
        (Some("query"), _) => find(FindOp::SetQuery(cap_query(&args[1..].join(" ")))),
        (Some("move"), 2) => match args[1] {
            "top" => find(FindOp::Move(i16::MIN)),
            "bottom" => find(FindOp::Move(i16::MAX)),
            raw => match raw.parse::<i16>() {
                Ok(n) => find(FindOp::Move(n)),
                Err(_) => usage(),
            },
        },
        (Some("select"), 2) => match args[1].parse::<usize>() {
            Ok(n) => find(FindOp::Select(n)),
            Err(_) => usage(),
        },
        (Some("enter"), 1) => find(FindOp::Activate),
        (Some("prefill"), 1) => find(FindOp::Prefill),
        (Some("close"), 1) => find(FindOp::Close),
        _ => usage(),
    }
}

/// `:find --pid N ...` (`act == None`) and `:find-<act> [filters] [--send TEXT] [--enter]`.
fn parse_session_find(act: Option<&str>, words: &[&str]) -> Option<Result<ColonCommand, String>> {
    use crate::wm::session_find::{FindAct, SessionFindOp, Tokens};
    let t = match Tokens::parse(words) {
        Ok(t) => t,
        Err(e) => return Some(Err(e)),
    };
    let op = match act {
        None => SessionFindOp::Locate(Box::new(t.query)),
        Some("find-prefill") => SessionFindOp::Prefill,
        Some("find-select") if t.clear => SessionFindOp::ClearSelection,
        Some(head) => {
            let act = match head {
                "find-focus" => FindAct::Focus,
                "find-beep" => FindAct::Beep,
                "find-flash" => FindAct::Flash,
                "find-select" => FindAct::Select,
                _ => match t.send {
                    Some(text) => FindAct::Send { text, enter: t.enter },
                    None => return Some(Err("Usage: :find-send [filters] --send TEXT [--enter]".to_string())),
                },
            };
            SessionFindOp::Act { act, query: Box::new(t.query) }
        }
    };
    Some(Ok(Action::SessionFind(op)))
}
