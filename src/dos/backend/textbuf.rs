//! Text-buffer (`qemu-textbuf`) plumbing: guest memory dumps over QMP `pmemsave`, the adaptive
//! poll loop, and key forwarding. Everything decoded from guest memory is length checked and
//! rendered through fixed-size tables, so a hostile guest cannot grow host memory.
//!
//! Dump files live in the per-session 0700 directory the backend owns (named
//! `vga.bin`/`bda.bin`), are unlinked before each `pmemsave` (a stale file is never mistaken
//! for a fresh dump) and are read back through a `take(len + 1)` bound.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use super::qmp::Qmp;
use super::render::render_ansi;
use super::screen::{BDA_ADDR, BDA_LEN, TEXT_BUF_ADDR, TEXT_BUF_LEN, TextScreen};
use super::{BackendError, DosBackend, DosKey, Result};

/// Fastest poll rate (20 Hz) while the screen is changing or input just arrived.
pub const MIN_INTERVAL: Duration = Duration::from_millis(50);
/// Slowest rate (4 Hz) once the screen has been idle for a while.
pub const MAX_INTERVAL: Duration = Duration::from_millis(250);
/// Idle polls before the interval starts growing.
const IDLE_POLLS_BEFORE_BACKOFF: u32 = 10;
/// Consecutive read failures after which the loop gives up.
pub const MAX_CONSECUTIVE_ERRORS: u32 = 5;
/// Keys forwarded to the guest per input batch (its keyboard buffer is tiny anyway).
pub const MAX_KEYS_PER_BATCH: usize = 256;

/// Dumps guest memory through `pmemsave` into files in `dir`.
#[derive(Clone, Debug)]
pub struct VgaReader {
    dir: PathBuf,
}

impl VgaReader {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn dump<R: Read, W: Write>(
        &self,
        q: &mut Qmp<R, W>,
        addr: u64,
        len: usize,
        name: &str,
    ) -> Result<Vec<u8>> {
        let path: PathBuf = self.dir.join(name);
        remove_stale(&path)?;
        q.pmemsave(addr, len, &path)?;
        let mut buf = Vec::with_capacity(len);
        File::open(&path)?
            .take(len as u64 + 1)
            .read_to_end(&mut buf)?;
        if buf.len() != len {
            return Err(BackendError::Qmp(format!(
                "dump {name} was {} bytes, expected {len}",
                buf.len()
            )));
        }
        Ok(buf)
    }

    /// Reads the BIOS data area, then (in a text mode) the 80x25 buffer. In a graphics mode
    /// the buffer is not fetched and the returned screen is the "graphics mode" notice.
    pub fn read<R: Read, W: Write>(&self, q: &mut Qmp<R, W>) -> Result<TextScreen> {
        let bda = self.dump(q, BDA_ADDR, BDA_LEN, "bda.bin")?;
        let mode = bda[0x49];
        if !matches!(mode, 2 | 3 | 7) {
            let mut s = TextScreen::graphics_notice(Some(mode));
            s.video_mode = Some(mode);
            return Ok(s);
        }
        let raw = self.dump(q, TEXT_BUF_ADDR, TEXT_BUF_LEN, "vga.bin")?;
        TextScreen::from_vga(&raw, Some(&bda))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn remove_stale(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Adaptive poll interval: 20 Hz while active, easing to 4 Hz when the screen sits idle.
#[derive(Clone, Debug)]
pub struct Pacer {
    idle: u32,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    pub fn new() -> Self {
        Self { idle: 0 }
    }
    pub fn on_input(&mut self) {
        self.idle = 0;
    }
    pub fn on_result(&mut self, changed: bool) {
        self.idle = if changed { 0 } else { self.idle.saturating_add(1) };
    }
    pub fn interval(&self) -> Duration {
        let over = self.idle.saturating_sub(IDLE_POLLS_BEFORE_BACKOFF);
        (MIN_INTERVAL + Duration::from_millis(20) * over.min(10)).min(MAX_INTERVAL)
    }
}

/// One poll outcome handed to the pane.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    /// Nothing visible changed.
    Unchanged,
    /// Bytes for the pane VT engine (see [`render_ansi`]) plus the screen they depict.
    Frame { ansi: Vec<u8>, screen: Box<TextScreen> },
}

/// Turns repeated `read_text` calls into minimal frames.
#[derive(Default)]
pub struct TextPoller {
    prev: Option<TextScreen>,
    pacer: Pacer,
    errors: u32,
}

impl TextPoller {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn interval(&self) -> Duration {
        self.pacer.interval()
    }
    pub fn note_input(&mut self) {
        self.pacer.on_input();
    }
    /// Forces the next frame to be a full repaint (pane resized or re-attached).
    pub fn invalidate(&mut self) {
        self.prev = None;
    }

    pub fn poll(&mut self, b: &mut dyn DosBackend) -> Result<Poll> {
        let raw = match b.read_text() {
            Ok(s) => {
                self.errors = 0;
                s
            }
            Err(BackendError::NotRunning) => return Err(BackendError::NotRunning),
            Err(e) => {
                self.errors += 1;
                return if self.errors >= MAX_CONSECUTIVE_ERRORS {
                    Err(e)
                } else {
                    Ok(Poll::Unchanged)
                };
            }
        };
        let shown = raw.for_display();
        let ansi = render_ansi(&shown, self.prev.as_ref());
        self.pacer.on_result(!ansi.is_empty());
        if ansi.is_empty() {
            return Ok(Poll::Unchanged);
        }
        self.prev = Some(shown.clone());
        Ok(Poll::Frame {
            ansi,
            screen: Box::new(shown),
        })
    }
}

/// The poll loop: forwards key batches from `input`, polls at the adaptive rate, and hands
/// every changed frame to `sink`. Returns `Ok` when `stop` is set or `input` disconnects, and
/// an error when the guest is gone or reads keep failing.
pub fn run_loop(
    b: &mut dyn DosBackend,
    input: &Receiver<Vec<DosKey>>,
    stop: &AtomicBool,
    mut sink: impl FnMut(&[u8], &TextScreen),
) -> Result<()> {
    let mut poller = TextPoller::new();
    while !stop.load(Ordering::Relaxed) {
        if let Poll::Frame { ansi, screen } = poller.poll(b)? {
            sink(&ansi, &screen);
        }
        match input.recv_timeout(poller.interval()) {
            Ok(mut keys) => {
                keys.truncate(MAX_KEYS_PER_BATCH);
                poller.note_input();
                b.send_keys(&keys)?;
                // Give the guest a moment to react, then loop straight into a poll.
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_ramps_and_resets() {
        let mut p = Pacer::new();
        assert_eq!(p.interval(), MIN_INTERVAL);
        for _ in 0..IDLE_POLLS_BEFORE_BACKOFF {
            p.on_result(false);
        }
        assert_eq!(p.interval(), MIN_INTERVAL);
        for _ in 0..100 {
            p.on_result(false);
        }
        assert_eq!(p.interval(), MAX_INTERVAL);
        p.on_input();
        assert_eq!(p.interval(), MIN_INTERVAL);
        p.on_result(false);
        p.on_result(true);
        assert_eq!(p.interval(), MIN_INTERVAL);
    }
}
