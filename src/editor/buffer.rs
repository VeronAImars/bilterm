//! Text buffer: lines of chars, cursor, selection, undo/redo and search (`EDIT-01`).
//!
//! Every mutation goes through [`TextBuffer::replace_range`], which records an [`Edit`]; undo and
//! redo replay edits in reverse. Positions are `(line, char column)`.

/// A position in the buffer: line index and character column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Debug)]
struct Edit {
    start: Pos,
    removed: String,
    inserted: String,
    cursor_before: Pos,
    cursor_after: Pos,
}

const UNDO_CAP: usize = 2000;

#[derive(Clone, Debug)]
pub struct TextBuffer {
    lines: Vec<String>,
    pub cursor: Pos,
    /// Selection anchor; the selection spans `anchor..cursor`.
    pub anchor: Option<Pos>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Bumped on every content change (drives autosave and dirty tracking).
    revision: u64,
    saved_revision: u64,
    /// Preferred display column for vertical movement.
    pub goal_col: Option<usize>,
}

fn byte_of(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl TextBuffer {
    /// Builds a buffer from `text` (`\n` separated; `\r\n` must be normalised by the caller).
    pub fn from_text(text: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursor: Pos::default(),
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            saved_revision: 0,
            goal_col: None,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", String::as_str)
    }
    pub fn line_len(&self, i: usize) -> usize {
        self.line(i).chars().count()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos { line, col: p.col.min(self.line_len(line)) }
    }

    pub fn end_pos(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos { line, col: self.line_len(line) }
    }

    /// Text in `a..b` (either order).
    pub fn text_between(&self, a: Pos, b: Pos) -> String {
        let (s, e) = (self.clamp(a.min(b)), self.clamp(a.max(b)));
        if s.line == e.line {
            return self.line(s.line).chars().skip(s.col).take(e.col - s.col).collect();
        }
        let mut out: String = self.line(s.line).chars().skip(s.col).collect();
        for l in s.line + 1..e.line {
            out.push('\n');
            out.push_str(self.line(l));
        }
        out.push('\n');
        out.extend(self.line(e.line).chars().take(e.col));
        out
    }

    /// Where the cursor lands after inserting `text` at `start`.
    fn end_after(start: Pos, text: &str) -> Pos {
        match text.rsplit_once('\n') {
            None => Pos { line: start.line, col: start.col + text.chars().count() },
            Some((head, tail)) => Pos { line: start.line + head.matches('\n').count() + 1, col: tail.chars().count() },
        }
    }

    fn apply(&mut self, a: Pos, b: Pos, text: &str) -> String {
        let (s, e) = (self.clamp(a.min(b)), self.clamp(a.max(b)));
        let removed = self.text_between(s, e);
        let head: String = self.line(s.line).chars().take(s.col).collect();
        let tail = self.line(e.line)[byte_of(self.line(e.line), e.col)..].to_string();
        let joined = format!("{head}{text}{tail}");
        let new_lines: Vec<String> = joined.split('\n').map(str::to_string).collect();
        self.lines.splice(s.line..=e.line, new_lines);
        self.revision += 1;
        removed
    }

    /// Replaces `a..b` with `text`, recording undo; the cursor ends after the inserted text.
    pub fn replace_range(&mut self, a: Pos, b: Pos, text: &str) {
        let start = self.clamp(a.min(b));
        let cursor_before = self.cursor;
        let removed = self.apply(a, b, text);
        let after = Self::end_after(start, text);
        self.cursor = after;
        self.anchor = None;
        self.goal_col = None;
        self.redo.clear();
        let merge = removed.is_empty()
            && text.chars().count() == 1
            && !text.contains(['\n', ' '])
            && self.undo.last().is_some_and(|l| l.removed.is_empty() && l.cursor_after == start && !l.inserted.contains(['\n', ' ']));
        if merge {
            let l = self.undo.last_mut().expect("checked");
            l.inserted.push_str(text);
            l.cursor_after = after;
        } else {
            self.undo.push(Edit { start, removed, inserted: text.to_string(), cursor_before, cursor_after: after });
            if self.undo.len() > UNDO_CAP {
                self.undo.remove(0);
            }
        }
    }

    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| self.text_between(s, e))
    }

    /// Inserts `text`, replacing the selection if there is one.
    pub fn insert(&mut self, text: &str) {
        let (s, e) = self.selection().unwrap_or((self.cursor, self.cursor));
        self.replace_range(s, e, text);
    }

    /// Deletes the selection; returns whether anything was removed.
    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some((s, e)) => {
                self.replace_range(s, e, "");
                true
            }
            None => false,
        }
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() || self.cursor == Pos::default() {
            return;
        }
        let c = self.cursor;
        let from = if c.col > 0 { Pos { line: c.line, col: c.col - 1 } } else { Pos { line: c.line - 1, col: self.line_len(c.line - 1) } };
        self.replace_range(from, c, "");
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let c = self.cursor;
        let to = if c.col < self.line_len(c.line) {
            Pos { line: c.line, col: c.col + 1 }
        } else if c.line + 1 < self.lines.len() {
            Pos { line: c.line + 1, col: 0 }
        } else {
            return;
        };
        self.replace_range(c, to, "");
    }

    /// Removes the selection, or the whole current line when nothing is selected; returns the text.
    pub fn cut(&mut self) -> String {
        if let Some(t) = self.selected_text() {
            self.delete_selection();
            return t;
        }
        let l = self.cursor.line;
        let (s, e) = if l + 1 < self.lines.len() {
            (Pos { line: l, col: 0 }, Pos { line: l + 1, col: 0 })
        } else if l > 0 {
            (Pos { line: l - 1, col: self.line_len(l - 1) }, self.end_pos())
        } else {
            (Pos::default(), self.end_pos())
        };
        let t = self.text_between(s, e);
        self.replace_range(s, e, "");
        t
    }

    pub fn undo(&mut self) -> bool {
        let Some(e) = self.undo.pop() else { return false };
        let end = Self::end_after(e.start, &e.inserted);
        self.apply(e.start, end, &e.removed);
        self.cursor = e.cursor_before;
        self.anchor = None;
        self.redo.push(e);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(e) = self.redo.pop() else { return false };
        let end = Self::end_after(e.start, &e.removed);
        self.apply(e.start, end, &e.inserted);
        self.cursor = e.cursor_after;
        self.anchor = None;
        self.undo.push(e);
        true
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::default());
        self.cursor = self.end_pos();
    }

    /// Case-insensitive search for `query`; forward finds the first match at or after `from`,
    /// backward the last match strictly before it; both wrap once.
    pub fn find(&self, query: &str, from: Pos, forward: bool) -> Option<(Pos, Pos)> {
        if query.is_empty() {
            return None;
        }
        let q: Vec<char> = query.to_lowercase().chars().collect();
        let mut all = Vec::new();
        for line in 0..self.lines.len() {
            let chars: Vec<char> = self.line(line).to_lowercase().chars().collect();
            if chars.len() >= q.len() {
                all.extend((0..=chars.len() - q.len()).filter(|&c| chars[c..c + q.len()] == q[..]).map(|col| Pos { line, col }));
            }
        }
        let pick = if forward {
            all.iter().find(|p| **p >= from).or(all.first())
        } else {
            all.iter().rev().find(|p| **p < from).or(all.last())
        }?;
        Some((*pick, Pos { line: pick.line, col: pick.col + q.len() }))
    }
}
