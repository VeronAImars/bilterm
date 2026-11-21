//! Deterministic in-memory backend: no emulator, no I/O, no clock. Prints a canned 80x25 screen,
//! echoes typed keys at a `C:\>` prompt and supports fake snapshots, so hosts and conformance
//! tests run offline.

use std::collections::HashMap;

use super::screen::TextCell;
use super::{
    BackendCaps, BackendError, ControlEndpoint, DosBackend, DosKey, GUEST_COLS, GUEST_ROWS,
    LaunchKind, LaunchPlan, Result, SessionSpec, TextScreen,
};

pub const PROMPT: &str = "C:\\>";
pub const BANNER: [&str; 2] = ["MOCK DOS 1.0 (bilterm test backend)", ""];

#[derive(Clone, Default)]
struct State {
    lines: Vec<String>,
    input: String,
}

#[derive(Default)]
pub struct MockBackend {
    running: bool,
    st: State,
    snaps: HashMap<String, State>,
    size: Option<(u16, u16)>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Last size passed to `resize` (tests).
    pub fn last_size(&self) -> Option<(u16, u16)> {
        self.size
    }

    fn require_running(&self) -> Result<()> {
        if self.running {
            Ok(())
        } else {
            Err(BackendError::NotRunning)
        }
    }

    fn enter(&mut self) {
        let cmd = std::mem::take(&mut self.st.input);
        self.st.lines.push(format!("{PROMPT}{cmd}"));
        match cmd.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "ver" => self.st.lines.push("FreeDOS mock 1.4".into()),
            _ => self.st.lines.push("Bad command or file name".into()),
        }
    }

    fn apply(&mut self, k: DosKey) {
        match k {
            DosKey::Char(c) if self.st.input.len() < 78 - PROMPT.len() => self.st.input.push(c),
            DosKey::Backspace => {
                self.st.input.pop();
            }
            DosKey::Enter => self.enter(),
            _ => {}
        }
    }
}

impl DosBackend for MockBackend {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn caps(&self) -> BackendCaps {
        BackendCaps {
            text_pane_native: false,
            textbuf: true,
            graphics_dump: false,
            snapshots: true,
            host_folder: false,
        }
    }

    fn plan(&mut self, _spec: &SessionSpec) -> Result<LaunchPlan> {
        Ok(LaunchPlan {
            kind: LaunchKind::Bridge,
            program: "mock".into(),
            argv: vec!["--mock".into()],
            env: Vec::new(),
            control: ControlEndpoint::None,
        })
    }

    fn launch(&mut self, spec: &SessionSpec) -> Result<()> {
        if self.running {
            return Err(BackendError::Process("already launched".into()));
        }
        self.size = Some((spec.size_cols, spec.size_rows));
        self.st = State {
            lines: BANNER.iter().map(|s| s.to_string()).collect(),
            input: String::new(),
        };
        self.running = true;
        Ok(())
    }

    fn attach(&mut self) -> Result<()> {
        Err(BackendError::Unsupported(
            "mock backend is launched, not attached",
        ))
    }

    fn send_keys(&mut self, keys: &[DosKey]) -> Result<()> {
        self.require_running()?;
        for k in keys {
            k.chord()?; // same validation as real backends
            self.apply(*k);
        }
        Ok(())
    }

    fn read_text(&mut self) -> Result<TextScreen> {
        self.require_running()?;
        let mut rows: Vec<String> = self.st.lines.clone();
        rows.push(format!("{PROMPT}{}", self.st.input));
        let keep = GUEST_ROWS as usize;
        let rows = &rows[rows.len().saturating_sub(keep)..];
        let mut s = TextScreen::blank();
        for (r, line) in rows.iter().enumerate() {
            for (c, ch) in line.chars().take(GUEST_COLS as usize).enumerate() {
                s.cells[r * GUEST_COLS as usize + c] = TextCell {
                    ch,
                    fg: 7,
                    bg: 0,
                    blink: false,
                };
            }
        }
        let last = rows.len() - 1;
        let col = rows[last].chars().count().min(GUEST_COLS as usize - 1);
        s.cursor = Some((col as u16, last as u16));
        Ok(s)
    }

    fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if cols == 0 || rows == 0 {
            return Err(BackendError::Invalid("zero-sized resize".into()));
        }
        self.size = Some((cols, rows));
        Ok(())
    }

    fn snapshot(&mut self, tag: &str) -> Result<()> {
        self.require_running()?;
        crate::secure_fs::validate_safe_name(tag).map_err(BackendError::Invalid)?;
        self.snaps.insert(tag.to_string(), self.st.clone());
        Ok(())
    }

    fn restore(&mut self, tag: &str) -> Result<()> {
        self.require_running()?;
        self.st = self
            .snaps
            .get(tag)
            .cloned()
            .ok_or_else(|| BackendError::Invalid(format!("no snapshot {tag:?}")))?;
        Ok(())
    }

    fn is_running(&mut self) -> bool {
        self.running
    }

    fn shutdown(&mut self) -> Result<()> {
        self.running = false;
        Ok(())
    }

    /// The mock "guest" always powers down when asked.
    fn stop(&mut self, _timeout: std::time::Duration) -> Result<super::StopOutcome> {
        self.running = false;
        Ok(super::StopOutcome::Clean)
    }
}
