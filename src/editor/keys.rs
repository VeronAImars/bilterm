//! Key handling for the editor (`EDIT-01`). Alt chords, `Ctrl+Alt` chords and function keys are
//! not consumed, so the window manager's shortcuts keep working over an editor window.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::buffer::Pos;
use super::{EditorState, Mode, SaveError, io};

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl EditorState {
    /// Records the outcome of a save; a conflict opens the overwrite confirmation for `path`.
    pub fn report_save(&mut self, r: Result<String, SaveError>, path: Option<&std::path::Path>) {
        match r {
            Ok(m) => self.message = Some(m),
            Err(SaveError::NeedsConfirm(m)) => {
                self.message = Some(m);
                if let Some(p) = path {
                    self.mode = Mode::ConfirmOverwrite(p.to_path_buf());
                }
            }
            Err(SaveError::Failed(m)) => self.message = Some(m),
        }
    }

    /// Saves to the bound path (prompting for a name when there is none).
    pub fn save_or_prompt(&mut self, force: bool) {
        match self.path.clone() {
            Some(p) => {
                let r = self.save(force);
                self.report_save(r, Some(&p));
            }
            None => self.mode = Mode::SaveAs(String::new()),
        }
    }

    /// Save-as with relative-path resolution against [`Self::cwd`].
    pub fn save_as_input(&mut self, raw: &str, force: bool) {
        match io::resolve_path(raw, &self.cwd) {
            Ok(p) => {
                let r = self.save_as(&p, force);
                self.report_save(r, Some(&p));
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn go(&mut self, to: Pos, extend: bool, keep_goal: bool) {
        if extend {
            self.buf.anchor.get_or_insert(self.buf.cursor);
        } else {
            self.buf.anchor = None;
        }
        self.buf.cursor = self.buf.clamp(to);
        if !keep_goal {
            self.buf.goal_col = None;
        }
    }

    fn word_left(&self) -> Pos {
        let mut c = self.buf.cursor;
        if c.col == 0 {
            return if c.line > 0 { Pos { line: c.line - 1, col: self.buf.line_len(c.line - 1) } } else { c };
        }
        let chars: Vec<char> = self.buf.line(c.line).chars().collect();
        while c.col > 0 && !is_word(chars[c.col - 1]) {
            c.col -= 1;
        }
        while c.col > 0 && is_word(chars[c.col - 1]) {
            c.col -= 1;
        }
        c
    }

    fn word_right(&self) -> Pos {
        let mut c = self.buf.cursor;
        let chars: Vec<char> = self.buf.line(c.line).chars().collect();
        if c.col >= chars.len() {
            return if c.line + 1 < self.buf.line_count() { Pos { line: c.line + 1, col: 0 } } else { c };
        }
        while c.col < chars.len() && !is_word(chars[c.col]) {
            c.col += 1;
        }
        while c.col < chars.len() && is_word(chars[c.col]) {
            c.col += 1;
        }
        c
    }

    fn vertical(&mut self, delta: i64, extend: bool) {
        let c = self.buf.cursor;
        let dc = self.display_col();
        let goal = *self.buf.goal_col.get_or_insert(dc);
        let line = (c.line as i64 + delta).clamp(0, self.buf.line_count() as i64 - 1) as usize;
        let col = self.col_for_display(line, goal);
        self.go(Pos { line, col }, extend, true);
    }

    fn horizontal(&mut self, dir: i64, extend: bool) {
        let c = self.buf.cursor;
        if !extend && let Some((s, e)) = self.buf.selection() {
            self.go(if dir < 0 { s } else { e }, false, false);
            return;
        }
        let to = if dir < 0 {
            if c.col > 0 { Pos { line: c.line, col: c.col - 1 } } else if c.line > 0 { Pos { line: c.line - 1, col: self.buf.line_len(c.line - 1) } } else { c }
        } else if c.col < self.buf.line_len(c.line) {
            Pos { line: c.line, col: c.col + 1 }
        } else if c.line + 1 < self.buf.line_count() {
            Pos { line: c.line + 1, col: 0 }
        } else {
            c
        };
        self.go(to, extend, false);
    }

    fn copy(&mut self) {
        if let Some(t) = self.buf.selected_text() {
            crate::clipboard::set_system_clipboard(&t);
            self.message = Some(format!("copied {} chars", t.chars().count()));
        }
    }

    /// Pastes `text` (bracketed paste or `Ctrl+V`); CRLF is normalised.
    pub fn paste(&mut self, text: &str) {
        self.buf.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut mode = std::mem::replace(&mut self.mode, Mode::Edit);
        if let Mode::ConfirmOverwrite(p) = mode {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                let r = if self.path.as_deref() == Some(p.as_path()) { self.save(true) } else { self.save_as(&p, true) };
                self.report_save(r, None);
            } else {
                self.message = Some("save cancelled".into());
            }
            return;
        }
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                match mode {
                    Mode::Find(q) => {
                        if !self.find(&q, !key.modifiers.contains(KeyModifiers::SHIFT)) {
                            self.message = Some(format!("not found: {q}"));
                        }
                        self.mode = Mode::Find(q);
                    }
                    Mode::SaveAs(p) => self.save_as_input(&p, false),
                    Mode::Goto(n) => {
                        if let Ok(n) = n.trim().parse() {
                            self.goto_line(n);
                        }
                    }
                    _ => {}
                }
                return;
            }
            KeyCode::Backspace => {
                if let Some(s) = mode.input_mut() {
                    s.pop();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                let digits_only = matches!(mode, Mode::Goto(_));
                if let Some(s) = mode.input_mut()
                    && (!digits_only || c.is_ascii_digit())
                {
                    s.push(c);
                }
            }
            _ => {}
        }
        self.mode = mode;
    }

    /// Handles a key. Returns `false` when the key is not the editor's (WM shortcuts).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let m = key.modifiers;
        if m.contains(KeyModifiers::ALT) || matches!(key.code, KeyCode::F(_)) {
            return false;
        }
        self.close_armed = false;
        if !matches!(self.mode, Mode::Edit) {
            self.handle_prompt_key(key);
            return true;
        }
        self.message = None;
        let (ctrl, shift) = (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::SHIFT));
        let c = self.buf.cursor;
        match key.code {
            KeyCode::Char(ch) if ctrl => match ch.to_ascii_lowercase() {
                's' => self.save_or_prompt(false),
                'o' => self.mode = Mode::SaveAs(String::new()),
                'f' => self.mode = Mode::Find(self.search.clone()),
                'n' => self.find_report(true),
                'p' => self.find_report(false),
                'g' => self.mode = Mode::Goto(String::new()),
                'l' => {
                    let w = self.set_wrap(None);
                    self.message = Some(format!("soft wrap {}", if w { "on" } else { "off" }));
                }
                'z' => {
                    if !self.buf.undo() {
                        self.message = Some("nothing to undo".into());
                    }
                }
                'y' => {
                    if !self.buf.redo() {
                        self.message = Some("nothing to redo".into());
                    }
                }
                'c' => self.copy(),
                'k' => {
                    let t = self.buf.cut();
                    crate::clipboard::set_system_clipboard(&t);
                }
                'v' => match crate::clipboard::get_system_clipboard() {
                    Some(t) => self.paste(&t),
                    None => self.message = Some("clipboard is empty".into()),
                },
                'a' => self.buf.select_all(),
                _ => {}
            },
            KeyCode::Left if ctrl => self.go(self.word_left(), shift, false),
            KeyCode::Right if ctrl => self.go(self.word_right(), shift, false),
            KeyCode::Home if ctrl => self.go(Pos::default(), shift, false),
            KeyCode::End if ctrl => self.go(self.buf.end_pos(), shift, false),
            KeyCode::Left => self.horizontal(-1, shift),
            KeyCode::Right => self.horizontal(1, shift),
            KeyCode::Up => self.vertical(-1, shift),
            KeyCode::Down => self.vertical(1, shift),
            KeyCode::PageUp => self.vertical(-(self.page_rows() as i64), shift),
            KeyCode::PageDown => self.vertical(self.page_rows() as i64, shift),
            KeyCode::Home => self.go(Pos { line: c.line, col: 0 }, shift, false),
            KeyCode::End => self.go(Pos { line: c.line, col: self.buf.line_len(c.line) }, shift, false),
            KeyCode::Backspace => self.buf.backspace(),
            KeyCode::Delete => self.buf.delete_forward(),
            KeyCode::Enter => {
                let indent: String = self.buf.line(c.line).chars().take(c.col).take_while(|ch| ch.is_whitespace()).collect();
                self.buf.insert(&format!("\n{indent}"));
            }
            KeyCode::Tab => self.buf.insert("    "),
            KeyCode::Esc => self.buf.anchor = None,
            KeyCode::Char(ch) => self.buf.insert(&ch.to_string()),
            _ => return false,
        }
        true
    }

    fn find_report(&mut self, forward: bool) {
        if self.search.is_empty() {
            self.message = Some("no search text (Ctrl+F)".into());
        } else if !self.find(&self.search.clone(), forward) {
            self.message = Some(format!("not found: {}", self.search));
        }
    }
}
