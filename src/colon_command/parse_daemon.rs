//! Parser for `:daemon` colon commands (`DAEM-05..08`, Phase 55).
//!
//! Syntax:
//! - `:daemon` | `:daemon list`
//! - `:daemon status [name]`
//! - `:daemon start [name] [--restart never|on-crash] [--memory-max SIZE] [--memory-high SIZE] [--host-ptys]`
//! - `:daemon new [name] [same options]` (creates a NEW daemon; no name = first free `daemon-N`)
//! - `:daemon stop [name] [--force]`
//! - `:daemon switch <name>`
//! - `:daemon prune`
//! - `:daemon host [name]`
//! - `:daemon transfer [<session>...|--all] --to <daemon> [--takeover] [--retire]`
//! - `:daemon restart --live [name]`

use super::ColonCommand;
use crate::action::DaemonSupervisorOp;
use crate::daemon::limits::MemoryLimits;
use crate::daemon::restart_policy::RestartMode;

const USAGE: &str = "Usage: :daemon [list | status [name] | start [name] [--restart never|on-crash] [--memory-max SIZE] [--memory-high SIZE] [--host-ptys] | new [name] [same options] | stop [name] [--force] | switch <name> | prune | host [name] | attach [kick <client-id>|--ro] | transfer [<session>...|--all] --to <daemon> | restart --live [name]]";

/// Parses `:daemon` command invocations into a [`ColonCommand`].
pub(super) fn parse_daemon_colon_command(
    head: &str,
    parts: &[&str],
) -> Option<Result<ColonCommand, String>> {
    if head != "daemon" {
        return None;
    }

    let sub = parts.get(1).copied().unwrap_or("list");
    match sub {
        "list" | "ls" => {
            if parts.len() > 2 {
                return Some(Err(USAGE.to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::List)))
        }
        "status" => {
            let name = parts.get(2).map(|s| s.to_string());
            if parts.len() > 3 {
                return Some(Err(USAGE.to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(
                DaemonSupervisorOp::Status { name },
            )))
        }
        "start" | "new" => {
            let mut name = None;
            let mut restart = None;
            let mut host_ptys = false;
            let (mut mem_max, mut mem_high): (Option<&str>, Option<&str>) = (None, None);
            let mut it = parts[2..].iter();
            while let Some(&arg) = it.next() {
                if arg == "--host-ptys" {
                    host_ptys = true;
                } else if arg == "--restart" {
                    let Some(&v) = it.next() else {
                        return Some(Err(USAGE.to_string()));
                    };
                    match RestartMode::parse(v) {
                        Ok(m) => restart = Some(m),
                        Err(e) => return Some(Err(e)),
                    }
                } else if arg == "--memory-max" || arg == "--memory-high" {
                    let Some(&v) = it.next() else {
                        return Some(Err(USAGE.to_string()));
                    };
                    if arg == "--memory-max" { mem_max = Some(v) } else { mem_high = Some(v) }
                } else if let Some(v) = arg.strip_prefix("--memory-max=") {
                    mem_max = Some(v);
                } else if let Some(v) = arg.strip_prefix("--memory-high=") {
                    mem_high = Some(v);
                } else if let Some(v) = arg.strip_prefix("--restart=") {
                    match RestartMode::parse(v) {
                        Ok(m) => restart = Some(m),
                        Err(e) => return Some(Err(e)),
                    }
                } else if name.is_none() && !arg.starts_with('-') {
                    name = Some(arg.to_string());
                } else {
                    return Some(Err(USAGE.to_string()));
                }
            }
            let limits = match MemoryLimits::from_args(mem_max, mem_high) {
                Ok(l) if l.is_unlimited() => None,
                Ok(l) => Some(l),
                Err(e) => return Some(Err(e)),
            };
            let op = if sub == "new" {
                DaemonSupervisorOp::New { name, restart, limits, host_ptys }
            } else {
                DaemonSupervisorOp::Start { name, restart, limits, host_ptys }
            };
            Some(Ok(ColonCommand::DaemonSupervisor(op)))
        }
        "stop" => {
            let mut name = None;
            let mut force = false;
            for &arg in &parts[2..] {
                if arg == "--force" || arg == "-f" {
                    force = true;
                } else if name.is_none() {
                    name = Some(arg.to_string());
                } else {
                    return Some(Err(USAGE.to_string()));
                }
            }
            Some(Ok(ColonCommand::DaemonSupervisor(
                DaemonSupervisorOp::Stop { name, force },
            )))
        }
        "transfer" => {
            let (mut sessions, mut all, mut to, mut takeover, mut retire) = (Vec::new(), false, None, false, false);
            let mut it = parts[2..].iter();
            while let Some(&a) = it.next() {
                match a {
                    "--all" => all = true,
                    "--takeover" => takeover = true,
                    "--retire" => retire = true,
                    "--to" => match it.next() {
                        Some(&n) if to.is_none() => to = Some(n.to_string()),
                        _ => return Some(Err(USAGE.to_string())),
                    },
                    s if s.starts_with('-') => return Some(Err(USAGE.to_string())),
                    s => sessions.push(s.to_string()),
                }
            }
            let Some(to) = to else {
                return Some(Err(format!("missing --to <daemon>; {USAGE}")));
            };
            if all && !sessions.is_empty() {
                return Some(Err("use either session names or --all, not both".to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::SessionTransfer { sessions, all, to, takeover, retire })))
        }
        "restart" => {
            let mut live = false;
            let mut name = None;
            for &arg in &parts[2..] {
                if arg == "--live" {
                    live = true;
                } else if name.is_none() && !arg.starts_with('-') {
                    name = Some(arg.to_string());
                } else {
                    return Some(Err(USAGE.to_string()));
                }
            }
            if !live {
                return Some(Err("only a live restart exists: :daemon restart --live [name] (use stop + start to restart without keeping sessions)".to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::RestartLive { name })))
        }
        "switch" => {
            let Some(name) = parts.get(2) else {
                return Some(Err(
                    "Usage: :daemon switch <name> (missing daemon name)".to_string(),
                ));
            };
            if parts.len() > 3 {
                return Some(Err(USAGE.to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(
                DaemonSupervisorOp::Switch {
                    name: (*name).to_string(),
                },
            )))
        }
        "prune" => {
            if parts.len() > 2 {
                return Some(Err(USAGE.to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(
                DaemonSupervisorOp::Prune,
            )))
        }
        "host" => {
            let name = parts.get(2).map(|s| s.to_string());
            if parts.len() > 3 {
                return Some(Err(USAGE.to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::HostState { name })))
        }
        "attach" if parts.get(2) == Some(&"kick") => {
            let op = match (parts.get(3), parts.len()) {
                (Some(&"--ro"), 4) => DaemonSupervisorOp::AttachKick { client_id: None, all_ro: true },
                (Some(id), 4) => match id.parse::<u64>() {
                    Ok(id) => DaemonSupervisorOp::AttachKick { client_id: Some(id), all_ro: false },
                    Err(_) => return Some(Err(format!("invalid attach client id '{id}'; {USAGE}"))),
                },
                _ => return Some(Err(USAGE.to_string())),
            };
            Some(Ok(ColonCommand::DaemonSupervisor(op)))
        }
        "attach" => {
            if parts.len() > 2 {
                return Some(Err(USAGE.to_string()));
            }
            Some(Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::AttachClients)))
        }
        _ => Some(Err(USAGE.to_string())),
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    fn p(parts: &[&str]) -> Result<ColonCommand, String> {
        parse_daemon_colon_command("daemon", parts).expect("daemon head")
    }

    #[test]
    fn transfer_and_live_restart_parse() {
        assert_eq!(
            p(&["daemon", "transfer", "a", "b", "--to", "z"]),
            Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::SessionTransfer { sessions: vec!["a".into(), "b".into()], all: false, to: "z".into(), takeover: false, retire: false }))
        );
        assert_eq!(
            p(&["daemon", "transfer", "--all", "--to", "z", "--retire"]),
            Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::SessionTransfer { sessions: vec![], all: true, to: "z".into(), takeover: false, retire: true }))
        );
        assert!(p(&["daemon", "transfer", "a"]).unwrap_err().contains("--to"));
        assert!(p(&["daemon", "transfer", "a", "--all", "--to", "z"]).is_err());
        assert_eq!(p(&["daemon", "restart", "--live", "w"]), Ok(ColonCommand::DaemonSupervisor(DaemonSupervisorOp::RestartLive { name: Some("w".into()) })));
        assert!(p(&["daemon", "restart"]).unwrap_err().contains("--live"));
    }
}
