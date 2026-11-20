//! Bridge pane source (D3b): drives a [`LaunchKind::Bridge`] backend (`qemu-textbuf`) from inside
//! the `bilterm dos run` wrapper. The wrapper owns the pane's PTY, so the "pane source" is just
//! this process: it launches the emulator headless, polls the VGA text buffer over QMP, writes
//! the rendered ANSI frames to its stdout (the pane VT engine or the user's terminal), and
//! forwards host key events to the guest. Lock, `.dirty` marker and cleanup stay with the
//! wrapper exactly as for curses runs.
//!
//! Closing the pane delivers SIGHUP (unix). FreeDOS ignores the ACPI power button, so at an idle
//! prompt the wrapper types `fdapm poweroff` (waits up to [`POWEROFF_WAIT`]), then asks the guest
//! to power down over QMP (waits up to [`STOP_GRACE`]), and only then quits/kills the emulator. The `.dirty` marker
//! is cleared only when the guest itself shut the emulator down.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use super::PreparedRun;
use crate::dos::backend::keys::keys_for_text;
use crate::dos::backend::hostkeys::from_host_batch;
use crate::dos::backend::textbuf::{MAX_KEYS_PER_BATCH, run_loop};
use crate::dos::backend::{DosBackend, DosKey, SessionSpec, StopOutcome, TextScreen};

/// How long a graceful stop waits for the guest before quitting the emulator. With [`POWEROFF_WAIT`]
/// it stays below the pane reaper's SIGHUP-to-SIGKILL grace (`pty::CLOSE_HUP_GRACE`).
pub const STOP_GRACE: Duration = Duration::from_secs(2);
/// How long a typed `fdapm poweroff` gets (measured about 10 s on FreeDOS 1.4 under QEMU) to end the emulator before the QMP request.
pub const POWEROFF_WAIT: Duration = Duration::from_secs(20);

/// Host key that ends the session when running in a plain terminal (Ctrl-]); everything else
/// goes to the guest.
fn is_host_exit(k: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    // Terminals send 0x1d for Ctrl-]; crossterm decodes that as Ctrl-5.
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char(']' | '5'))
}

/// Set by SIGHUP/SIGTERM (pane closed, wrapper terminated); polled by the bridge loop.
static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        for sig in [libc::SIGHUP, libc::SIGTERM] {
            libc::signal(sig, on_signal as *const () as libc::sighandler_t);
        }
    }
}

/// Result of one bridged session.
#[derive(Debug, PartialEq, Eq)]
pub struct DriveReport {
    /// Why the session ended abnormally (launch failure or lost guest), if it did.
    pub error: Option<String>,
    pub outcome: StopOutcome,
}

/// Launches `b`, runs the poll/forward loop until `stop`, input disconnect or guest exit, then
/// stops the guest gracefully. Pure with respect to the process (all I/O is injected).
pub fn drive(
    b: &mut dyn DosBackend,
    spec: &SessionSpec,
    input: &Receiver<Vec<DosKey>>,
    stop: &AtomicBool,
    sink: impl FnMut(&[u8], &TextScreen),
    grace: Duration,
    poweroff_wait: Duration,
) -> DriveReport {
    if let Err(e) = b.launch(spec) {
        let _ = b.shutdown();
        return DriveReport { error: Some(format!("cannot start the emulator: {e}")), outcome: StopOutcome::Forced };
    }
    let mut error = run_loop(b, input, stop, sink).err().map(|e| e.to_string());
    if !b.is_running() {
        // The guest powered itself off (or crashed): the exit status decides the outcome below,
        // the failed poll that noticed it is not an error.
        error = None;
    } else if at_clean_prompt(b) {
        // FreeDOS ignores the ACPI power button; typing FDAPM at an idle prompt is how it
        // shuts down cleanly (QEMU then exits 0). Never typed into a running program.
        if let Ok(keys) = keys_for_text(POWEROFF_CMD) {
            if b.send_keys(&keys).is_ok() {
                let start = std::time::Instant::now();
                while b.is_running() && start.elapsed() < poweroff_wait {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    let outcome = b.stop(grace).unwrap_or(StopOutcome::Forced);
    DriveReport { error, outcome }
}

/// Typed at an idle DOS prompt before the QMP power-down request.
pub const POWEROFF_CMD: &str = "fdapm poweroff\n";

/// True when the cursor sits right after a `X:\...>` prompt with nothing typed on the line.
pub fn at_clean_prompt(b: &mut dyn DosBackend) -> bool {
    let Ok(s) = b.read_text() else { return false };
    let Some((col, row)) = s.cursor else { return false };
    let Some(line) = s.lines().into_iter().nth(usize::from(row)) else { return false };
    let t = line.trim_end();
    let b0 = t.as_bytes();
    usize::from(col) == t.chars().count()
        && t.ends_with('>')
        && b0.len() >= 3
        && b0[0].is_ascii_alphabetic()
        && b0[1] == b':'
}

/// Reads host key events on a thread until `done`, the exit chord, or the terminal goes away.
fn spawn_input(done: &'static AtomicBool) -> Receiver<Vec<DosKey>> {
    use crossterm::event::{Event, KeyEventKind};
    let (tx, rx): (Sender<Vec<DosKey>>, _) = channel();
    let _ = std::thread::Builder::new().name("bilterm-dos-keys".into()).spawn(move || {
        while !done.load(Ordering::Relaxed) {
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let mut evs = Vec::new();
            loop {
                match crossterm::event::read() {
                    Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                        if is_host_exit(&k) {
                            done.store(true, Ordering::Relaxed);
                            return;
                        }
                        evs.push(k);
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
                if !matches!(crossterm::event::poll(Duration::ZERO), Ok(true)) || evs.len() >= MAX_KEYS_PER_BATCH {
                    break;
                }
            }
            let keys = from_host_batch(&evs, MAX_KEYS_PER_BATCH);
            if !keys.is_empty() && tx.send(keys).is_err() {
                return;
            }
        }
    });
    rx
}

struct RawGuard(bool);

impl RawGuard {
    fn enter() -> Self {
        Self(crossterm::terminal::enable_raw_mode().is_ok())
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// Foreground bridged run for `bilterm dos run`: returns the process exit code.
pub(super) fn run(mut prep: PreparedRun, out: &mut String) -> i32 {
    for n in &prep.notes {
        out.push_str(&format!("note: {n}\n"));
    }
    install_signal_handlers();
    let rx = spawn_input(&STOP);
    let raw = RawGuard::enter();
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(b"\x1b[0m\x1b[2J\x1b[H");
    let _ = stdout.flush();
    let spec = prep.spec.clone();
    let report = drive(
        prep.backend.as_mut(),
        &spec,
        &rx,
        &STOP,
        |ansi, _| {
            // Errors (terminal gone) are ignored: the SIGHUP path is already stopping the loop.
            let _ = stdout.write_all(ansi);
            let _ = stdout.flush();
        },
        STOP_GRACE,
        POWEROFF_WAIT,
    );
    STOP.store(true, Ordering::Relaxed); // lets the key thread exit
    drop(raw);
    let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\r\n");
    let _ = stdout.flush();
    let clean = report.outcome == StopOutcome::Clean;
    let mut code = 0;
    if let Some(e) = &report.error {
        out.push_str(&format!("{e}\n"));
        code = 1;
    }
    if !clean {
        out.push_str("the guest did not shut down by itself; the image keeps its dirty marker (checked on next start)\n");
    }
    if let Err(e) = prep.finish(clean) {
        out.push_str(&format!("cleanup failed: {e}\n"));
        code = 1;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dos::backend::mock::MockBackend;
    use std::sync::mpsc::channel;

    #[test]
    fn drive_renders_forwards_keys_and_stops_clean() {
        let mut b = MockBackend::new();
        let (tx, rx) = channel();
        tx.send(crate::dos::backend::keys::keys_for_text("ver\n").unwrap()).unwrap();
        drop(tx); // input disconnect ends the loop after the keys went in
        let stop = AtomicBool::new(false);
        let mut frames = Vec::new();
        let rep = drive(&mut b, &SessionSpec::default(), &rx, &stop, |a, s| {
            frames.push((a.to_vec(), s.contains("FreeDOS mock 1.4")));
        }, Duration::from_millis(10), Duration::from_millis(10));
        assert_eq!(rep, DriveReport { error: None, outcome: StopOutcome::Clean });
        assert!(!frames.is_empty());
        assert!(!b.is_running());
    }

    #[test]
    fn prompt_detection_requires_an_empty_prompt_line() {
        let mut b = MockBackend::new();
        b.launch(&SessionSpec::default()).unwrap();
        assert!(at_clean_prompt(&mut b));
        b.send_keys(&[DosKey::Char('d')]).unwrap();
        assert!(!at_clean_prompt(&mut b), "typed text on the line: not an idle prompt");
    }

    #[test]
    fn stop_flag_ends_the_loop() {
        let mut b = MockBackend::new();
        let (_tx, rx) = channel();
        let stop = AtomicBool::new(true);
        let rep = drive(&mut b, &SessionSpec::default(), &rx, &stop, |_, _| {}, Duration::ZERO, Duration::ZERO);
        assert_eq!(rep.outcome, StopOutcome::Clean);
        assert!(rep.error.is_none());
    }
}
