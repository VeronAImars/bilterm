//! Terminal event loop of `bilterm attach`: keys/paste/resize in, host messages in, ratatui out.
//!
//! Reconnect policy (minimal, bounded): if the connection drops without a `Bye`/`Error` from the
//! host, retry up to `RECONNECT_ATTEMPTS` times with doubling backoff, then exit with `EXIT_IO`.
//! A reconnect is a brand-new client (plan Q5): state is rebuilt from the fresh snapshots.

#![cfg(unix)]

use std::io::{self, IsTerminal};
use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::net::UnixStream;

use super::keys::{KeyAction, KeyMap};
use super::link::{connect_unix, Link, LinkError};
use super::outbox::Outbox;
use super::state::{Applied, ClientState};
use super::ui::{announce_size, draw, StatusInfo};
use super::{EXIT_DETACHED, EXIT_HOST_SHUTDOWN, EXIT_IO, EXIT_KICKED, EXIT_PROTOCOL, EXIT_REFUSED};
use crate::attach::msg::{ByeReason, ClientMsg, ErrorCode, Role};

pub const RECONNECT_ATTEMPTS: u32 = 3;
const RECONNECT_BASE: Duration = Duration::from_millis(250);
const TICK: Duration = Duration::from_millis(25);
const PING_EVERY: Duration = Duration::from_secs(10);
const MESSAGE_TTL: Duration = Duration::from_secs(4);
/// An `Error` this recent before EOF means the host closed us on purpose: do not reconnect.
const ERROR_THEN_EOF: Duration = Duration::from_secs(2);

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Raw mode + alternate screen + bracketed paste, restored on drop (also on panic unwinding).
struct TermGuard;

impl TermGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let g = Self;
        execute!(io::stdout(), EnterAlternateScreen, crossterm::event::EnableBracketedPaste)?;
        Ok(g)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), crossterm::event::DisableBracketedPaste, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// How a session ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub message: String,
}

fn out(code: i32, message: impl Into<String>) -> Outcome {
    Outcome { code, message: message.into() }
}

/// Exit code for a failed connect/handshake.
pub fn code_for(e: &LinkError) -> i32 {
    match e {
        LinkError::Refused(_) | LinkError::Host(..) | LinkError::Handshake(_) => EXIT_REFUSED,
        LinkError::Frame(_) | LinkError::Msg(_) => EXIT_PROTOCOL,
        LinkError::Io(_) | LinkError::Eof => EXIT_IO,
    }
}

/// Exit code and text for a `Bye`.
pub fn bye_outcome(r: ByeReason) -> Outcome {
    match r {
        ByeReason::HostShutdown => out(EXIT_HOST_SHUTDOWN, "host shut down"),
        ByeReason::Kicked => out(EXIT_KICKED, "kicked by the host"),
        ByeReason::LaggedResyncFailed => out(EXIT_KICKED, "disconnected: this client fell too far behind"),
        ByeReason::IdleTimeout => out(EXIT_KICKED, "disconnected: idle timeout"),
        ByeReason::Unknown => out(EXIT_KICKED, "disconnected by the host"),
    }
}

/// Connects, runs the UI until it ends and returns the process exit code.
pub async fn run(path: &Path, role: Role) -> i32 {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("bilterm: attach needs an interactive terminal");
        return EXIT_IO;
    }
    let (tc, tr) = crossterm::terminal::size().unwrap_or((80, 24));
    let (cols, rows) = announce_size(tc, tr);
    let link = match connect_unix(path, role, cols, rows).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bilterm: cannot attach to {}: {e}", path.display());
            return code_for(&e);
        }
    };
    let guard = match TermGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bilterm: cannot enter raw mode: {e}");
            return EXIT_IO;
        }
    };
    let mut term = match Terminal::new(CrosstermBackend::new(io::stdout())) {
        Ok(t) => t,
        Err(e) => {
            drop(guard);
            eprintln!("bilterm: cannot start terminal: {e}");
            return EXIT_IO;
        }
    };
    let outcome = event_loop(&mut term, link, path, role, (cols, rows)).await;
    drop(term);
    drop(guard);
    if !outcome.message.is_empty() {
        eprintln!("bilterm: {}", outcome.message);
    }
    outcome.code
}

fn status<'a>(link: &'a Link<UnixStream>, role: Role, msg: &'a Option<(String, Instant)>, keys: &KeyMap, reconnect: Option<u32>) -> StatusInfo<'a> {
    StatusInfo {
        host: &link.info.host_name,
        role,
        message: msg.as_ref().map(|(m, _)| m.as_str()),
        prefix: keys.prefix_pending(),
        reconnect,
    }
}

async fn event_loop(term: &mut Term, mut link: Link<UnixStream>, path: &Path, role: Role, mut size: (u16, u16)) -> Outcome {
    let mut state = ClientState::new(link.info.client_id);
    let mut keys = KeyMap::default();
    let mut outbox = Outbox::new();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut message: Option<(String, Instant)> = None;
    let mut last_error: Option<Instant> = None;
    let mut last_ping = Instant::now();
    let mut dirty = true;
    let mut ping_n = 0u64;
    macro_rules! say {
        ($($a:tt)*) => {{ message = Some((format!($($a)*), Instant::now())); }};
    }
    loop {
        if dirty {
            dirty = false;
            let info = status(&link, role, &message, &keys, None);
            if let Err(e) = term.draw(|f| draw(f, &mut state, &info)) {
                return out(EXIT_IO, format!("terminal error: {e}"));
            }
        }
        let lost: LinkError = tokio::select! {
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) => {
                        match keys.handle(k) {
                            KeyAction::Send(_) if role == Role::Ro => say!("read-only: input ignored"),
                            KeyAction::Send(b) => {
                                if !outbox.push(&b) { say!("input queue full: input dropped"); }
                            }
                            KeyAction::Detach => {
                                let _ = link.send(&ClientMsg::Detach {}).await;
                                return out(EXIT_DETACHED, "");
                            }
                            KeyAction::NextPane => { state.cycle(1); }
                            KeyAction::PrevPane => { state.cycle(-1); }
                            KeyAction::Nothing => {}
                        }
                        dirty = true;
                    }
                    Some(Ok(Event::Paste(text))) => {
                        if role == Role::Ro { say!("read-only: paste ignored"); }
                        else if !outbox.push(text.as_bytes()) { say!("paste too large: dropped"); }
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(c, r))) => {
                        size = announce_size(c, r);
                        if role == Role::Rw {
                            // A failed send surfaces as an error on the next recv.
                            let _ = link.send(&ClientMsg::Resize { cols: size.0, rows: size.1, pixel_w: None, pixel_h: None }).await;
                        }
                        let _ = term.autoresize();
                        dirty = true;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return out(EXIT_IO, format!("terminal input error: {e}")),
                    None => return out(EXIT_IO, "terminal input closed"),
                }
                continue;
            }
            msg = link.recv() => match msg {
                Ok(m) => {
                    match state.apply(m) {
                        Ok(Applied::Nothing) => {}
                        Ok(Applied::Redraw) => dirty = true,
                        Ok(Applied::Ack { pane, seq }) => {
                            dirty = true;
                            let _ = link.send(&ClientMsg::Ack { pane, seq }).await;
                        }
                        Ok(Applied::Bye(r)) => return bye_outcome(r),
                        Ok(Applied::Error(code, text)) => {
                            last_error = Some(Instant::now());
                            match code {
                                ErrorCode::RateLimited => { outbox.on_refused(Instant::now()); say!("host rate-limited input; backing off"); }
                                ErrorCode::ReadOnly => say!("host says this client is read-only"),
                                _ => say!("host: {text}"),
                            }
                            dirty = true;
                        }
                        Err(e) => return out(EXIT_PROTOCOL, e.to_string()),
                    }
                    continue;
                }
                Err(e) => e,
            },
            _ = tick.tick() => {
                let now = Instant::now();
                if message.as_ref().is_some_and(|(_, t)| now.duration_since(*t) > MESSAGE_TTL) {
                    message = None;
                    dirty = true;
                }
                let mut failed = None;
                if let Some(pane) = state.current_pane() {
                    while let Some(chunk) = outbox.next(now) {
                        match link.send_input(pane, &chunk).await {
                            Ok(()) => outbox.on_sent(),
                            Err(e) => { failed = Some(e); break; }
                        }
                    }
                }
                if failed.is_none() && now.duration_since(last_ping) >= PING_EVERY {
                    last_ping = now;
                    ping_n += 1;
                    failed = link.send(&ClientMsg::Ping { n: ping_n }).await.err();
                }
                match failed { Some(e) => e, None => continue }
            }
        };
        // The connection broke (or a send on it did).
        let recent_error = last_error.is_some_and(|t| t.elapsed() < ERROR_THEN_EOF);
        if recent_error || !matches!(lost, LinkError::Eof | LinkError::Io(_)) {
            return match lost {
                LinkError::Msg(_) | LinkError::Frame(_) => out(EXIT_PROTOCOL, lost.to_string()),
                _ => out(EXIT_PROTOCOL, format!("host closed the connection after an error ({lost})")),
            };
        }
        match reconnect(term, &mut link, path, role, size, &mut state, &keys).await {
            Ok(()) => {
                outbox = Outbox::new();
                state = ClientState::new(link.info.client_id);
                say!("reconnected (new client {})", link.info.client_id);
                dirty = true;
            }
            Err(e) => return out(code_for(&e), format!("lost connection to the host ({lost}); reconnect failed: {e}")),
        }
    }
}

async fn reconnect(
    term: &mut Term,
    link: &mut Link<UnixStream>,
    path: &Path,
    role: Role,
    size: (u16, u16),
    state: &mut ClientState,
    keys: &KeyMap,
) -> Result<(), LinkError> {
    let mut last = LinkError::Eof;
    for attempt in 1..=RECONNECT_ATTEMPTS {
        let info = status(link, role, &None, keys, Some(attempt));
        let _ = term.draw(|f| draw(f, state, &info));
        tokio::time::sleep(RECONNECT_BASE * 2u32.pow(attempt - 1)).await;
        match connect_unix(path, role, size.0, size.1).await {
            Ok(l) => {
                *link = l;
                return Ok(());
            }
            // The host answered but does not want us: retrying will not help.
            Err(e @ (LinkError::Refused(_) | LinkError::Host(..) | LinkError::Handshake(_))) => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct_and_documented() {
        assert_eq!(bye_outcome(ByeReason::HostShutdown).code, EXIT_HOST_SHUTDOWN);
        for r in [ByeReason::Kicked, ByeReason::LaggedResyncFailed, ByeReason::IdleTimeout, ByeReason::Unknown] {
            assert_eq!(bye_outcome(r).code, EXIT_KICKED);
        }
        assert_eq!(code_for(&LinkError::Refused("x".into())), EXIT_REFUSED);
        assert_eq!(code_for(&LinkError::Eof), EXIT_IO);
        assert_eq!(code_for(&LinkError::Io(io::Error::other("x"))), EXIT_IO);
    }
}
