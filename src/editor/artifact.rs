//! Shared artifact helpers, interactive Markdown checklists, rendered Markdown view, and
//! HTML preview rendering for the built-in editor (`EDIT-07`).

use unicode_width::UnicodeWidthChar;

use super::buffer::{Pos, TextBuffer};
use super::highlight;

/// Default shared artifact name when `:artifact` is run without arguments.
pub const DEFAULT_ARTIFACT: &str = "shared";

/// Prefix used to store shared editor artifacts on [`crate::wm::context_bus::ContextBus`].
pub const ARTIFACT_KEY_PREFIX: &str = "artifact:";

/// Returns the canonical `ContextBus` key for a named shared artifact.
pub fn artifact_bus_key(name: &str) -> String {
    format!("{ARTIFACT_KEY_PREFIX}{name}")
}

/// Starter template used when opening a brand-new shared artifact.
pub fn artifact_template(name: &str) -> String {
    format!(
        "# Shared Artifact: {name}\n\n\
         ## Instructions\n\
         - Clarify goals, constraints, or handoff notes here while agents work.\n\n\
         ## Checklist\n\
         - [ ] Review agent progress\n\
         - [ ] Broadcast updated instructions (`:editor broadcast`)\n"
    )
}

/// Location of a Markdown task-list checkbox (`[ ]` or `[x]`) on a line (char indices).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckboxSpan {
    /// Char index of `[`.
    pub box_col: usize,
    /// Char index of the mark (`' '`, `'x'`, or `'X'`).
    pub mark_col: usize,
    /// Char index immediately after `]`.
    pub end_col: usize,
    /// `true` when checked (`[x]` or `[X]`).
    pub checked: bool,
}

/// Finds a Markdown task checkbox on `line` (`- [ ]`, `* [x]`, `+ [X]`, `1. [ ]`).
pub fn find_checkbox(line: &str) -> Option<CheckboxSpan> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n && matches!(chars[i], ' ' | '\t') {
        i += 1;
    }
    if i >= n {
        return None;
    }
    let after_marker = if matches!(chars[i], '-' | '*' | '+') && chars.get(i + 1) == Some(&' ') {
        i + 2
    } else {
        let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && chars.get(i + digits) == Some(&'.') && chars.get(i + digits + 1) == Some(&' ') {
            i + digits + 2
        } else {
            return None;
        }
    };
    let mut b = after_marker;
    while b < n && chars[b] == ' ' {
        b += 1;
    }
    if b + 2 < n
        && chars[b] == '['
        && matches!(chars[b + 1], ' ' | 'x' | 'X')
        && chars[b + 2] == ']'
        && chars.get(b + 3).is_none_or(|c| c.is_whitespace())
    {
        Some(CheckboxSpan {
            box_col: b,
            mark_col: b + 1,
            end_col: b + 3,
            checked: chars[b + 1] != ' ',
        })
    } else {
        None
    }
}

/// Counts `(done, total)` Markdown task checkboxes across `buf` (ignoring fenced code blocks).
pub fn checklist_progress(buf: &TextBuffer) -> (usize, usize) {
    let fences = highlight::fence_states((0..buf.line_count()).map(|l| buf.line(l)));
    let (mut done, mut total) = (0usize, 0usize);
    for l in 0..buf.line_count() {
        let line = buf.line(l);
        if fences.get(l).copied().unwrap_or(false) || highlight::is_fence(line) {
            continue;
        }
        if let Some(cb) = find_checkbox(line) {
            total += 1;
            if cb.checked {
                done += 1;
            }
        }
    }
    (done, total)
}

/// Toggles the Markdown checkbox on `line` through `TextBuffer::replace_range` (full undo/redo).
/// Returns `Some(new_checked_state)` if `line` had a toggleable checkbox outside a fenced code block.
pub fn toggle_checkbox_on_line(buf: &mut TextBuffer, line: usize) -> Option<bool> {
    if line >= buf.line_count() {
        return None;
    }
    let fences = highlight::fence_states((0..buf.line_count()).map(|l| buf.line(l)));
    if fences.get(line).copied().unwrap_or(false) {
        return None;
    }
    let cb = find_checkbox(buf.line(line))?;
    let next = !cb.checked;
    let mark = if next { "x" } else { " " };
    let saved_cursor = buf.cursor;
    buf.replace_range(
        Pos { line, col: cb.mark_col },
        Pos { line, col: cb.mark_col + 1 },
        mark,
    );
    buf.cursor = buf.clamp(saved_cursor);
    Some(next)
}

/// One visual row produced for Rendered Markdown / Checklist view.
#[derive(Clone, Debug)]
pub struct RenderedRow {
    /// 0-based source buffer line this visual row came from.
    pub src_line: usize,
    /// Pre-styled ANSI line content (without cursor positioning or `\x1b[K`).
    pub ansi: String,
    /// `true` when this row belongs to a toggleable checklist item.
    pub is_checkbox: bool,
}

/// Strips C0/C1 control characters (including ESC `\x1b`) from untrusted buffer text before
/// emitting it into a terminal VT stream; expands tabs to four spaces.
fn sanitize_terminal_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn truncate_width(s: &str, max_w: usize) -> String {
    let mut w = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max_w {
            break;
        }
        w += cw;
        out.push(c);
    }
    out
}

/// Wraps plain text to `max_w` display columns, breaking on spaces when a word fits and falling
/// back to character breaks for tokens wider than `max_w`.
fn wrap_plain(text: &str, max_w: usize) -> Vec<String> {
    let w = max_w.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for token in text.split_inclusive(' ') {
        let tok_w: usize = token.chars().map(|c| c.width().unwrap_or(0)).sum();
        if cur_w + tok_w <= w {
            cur.push_str(token);
            cur_w += tok_w;
            continue;
        }
        if !cur.is_empty() {
            rows.push(std::mem::take(&mut cur).trim_end_matches(' ').to_string());
            cur_w = 0;
        }
        if tok_w <= w {
            cur.push_str(token);
            cur_w = tok_w;
        } else {
            for ch in token.chars() {
                let cw = ch.width().unwrap_or(0);
                if cur_w + cw > w && !cur.is_empty() {
                    rows.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += cw;
            }
        }
    }
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur.trim_end_matches(' ').to_string());
    }
    rows
}

/// Renders minimal inline Markdown (`**bold**` and `` `code` ``) to ANSI.
fn style_inline_ansi(text: &str, base_sgr: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = format!("\x1b[{base_sgr}m");
    let mut i = 0;
    while i < n {
        if chars[i] == '`'
            && let Some(j) = chars[i + 1..].iter().position(|&c| c == '`')
        {
            out.push_str("\x1b[0;33m");
            for &c in &chars[i + 1..i + 1 + j] {
                out.push(c);
            }
            out.push_str(&format!("\x1b[{base_sgr}m"));
            i += j + 2;
            continue;
        }
        if i + 1 < n
            && chars[i] == '*'
            && chars[i + 1] == '*'
            && let Some(j) = chars[i + 2..].windows(2).position(|w| w == ['*', '*'])
        {
            out.push_str("\x1b[1m");
            for &c in &chars[i + 2..i + 2 + j] {
                out.push(c);
            }
            out.push_str(&format!("\x1b[{base_sgr}m"));
            i += j + 4;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out.push_str("\x1b[0m");
    out
}

/// Converts the buffer into styled terminal rows for Rendered Markdown / Checklist view.
pub fn render_markdown_rows(buf: &TextBuffer, width: usize) -> Vec<RenderedRow> {
    let w = width.max(1);
    let fences = highlight::fence_states((0..buf.line_count()).map(|l| buf.line(l)));
    let mut rows = Vec::new();
    for line_idx in 0..buf.line_count() {
        let raw_owned = sanitize_terminal_text(buf.line(line_idx));
        let raw = raw_owned.as_str();
        let in_fence = fences.get(line_idx).copied().unwrap_or(false);
        if highlight::is_fence(raw) {
            let lang = raw.trim().trim_start_matches(['`', '~']).trim();
            let label = if in_fence {
                "└────────────────────────".to_string()
            } else if lang.is_empty() {
                "┌── code ────────────────".to_string()
            } else {
                format!("┌── {lang} ──────────────")
            };
            rows.push(RenderedRow {
                src_line: line_idx,
                ansi: format!("\x1b[0;2;33m{}\x1b[0m", truncate_width(&label, w)),
                is_checkbox: false,
            });
            continue;
        }
        if in_fence {
            for seg in wrap_plain(raw, w.saturating_sub(2)) {
                rows.push(RenderedRow {
                    src_line: line_idx,
                    ansi: format!("\x1b[0;2;33m│ \x1b[0;33m{seg}\x1b[0m"),
                    is_checkbox: false,
                });
            }
            continue;
        }
        let trimmed = raw.trim_start();
        let indent_cols = raw.chars().take_while(|c| c.is_whitespace()).count().min(w.saturating_sub(3)).min(8);
        let indent = " ".repeat(indent_cols);

        // Headings
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed.chars().nth(hashes).is_none_or(|c| c == ' ') {
            let title = trimmed[hashes..].trim();
            let sgr = match hashes {
                1 => "1;4;36",
                2 => "1;36",
                _ => "1;34",
            };
            for seg in wrap_plain(title, w) {
                rows.push(RenderedRow {
                    src_line: line_idx,
                    ansi: format!("\x1b[0;{sgr}m{seg}\x1b[0m"),
                    is_checkbox: false,
                });
            }
            continue;
        }

        // Horizontal rule
        let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() >= 3
            && (compact.chars().all(|c| c == '-')
                || compact.chars().all(|c| c == '*')
                || compact.chars().all(|c| c == '_'))
        {
            rows.push(RenderedRow {
                src_line: line_idx,
                ansi: format!("\x1b[0;2;36m{}\x1b[0m", "─".repeat(w.min(60))),
                is_checkbox: false,
            });
            continue;
        }

        // Interactive checklist row
        if let Some(cb) = find_checkbox(raw) {
            let body: String = raw.chars().skip(cb.end_col).collect();
            let body = body.trim_start();
            let (glyph, glyph_sgr, body_sgr) = if cb.checked {
                ("☑", "1;32", "0;2;9")
            } else {
                ("☐", "1;33", "0")
            };
            let prefix_w = indent_cols + 2;
            let body_rows = wrap_plain(body, w.saturating_sub(prefix_w));
            for (ri, seg) in body_rows.into_iter().enumerate() {
                let ansi = if ri == 0 {
                    format!("{indent}\x1b[0;{glyph_sgr}m{glyph}\x1b[0m {}", style_inline_ansi(&seg, body_sgr))
                } else {
                    format!("{indent}  {}", style_inline_ansi(&seg, body_sgr))
                };
                rows.push(RenderedRow { src_line: line_idx, ansi, is_checkbox: true });
            }
            continue;
        }

        // Blockquote
        if let Some(rest) = trimmed.strip_prefix('>') {
            let body = rest.trim_start();
            for seg in wrap_plain(body, w.saturating_sub(indent_cols + 2)) {
                rows.push(RenderedRow {
                    src_line: line_idx,
                    ansi: format!("{indent}\x1b[0;32m│ \x1b[0;3;32m{seg}\x1b[0m"),
                    is_checkbox: false,
                });
            }
            continue;
        }

        // Bullet list item
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let prefix_w = indent_cols + 2;
            for (ri, seg) in wrap_plain(rest, w.saturating_sub(prefix_w)).into_iter().enumerate() {
                let ansi = if ri == 0 {
                    format!("{indent}\x1b[0;35m•\x1b[0m {}", style_inline_ansi(&seg, "0"))
                } else {
                    format!("{indent}  {}", style_inline_ansi(&seg, "0"))
                };
                rows.push(RenderedRow { src_line: line_idx, ansi, is_checkbox: false });
            }
            continue;
        }

        // Regular paragraph / blank line
        for seg in wrap_plain(raw, w) {
            rows.push(RenderedRow {
                src_line: line_idx,
                ansi: style_inline_ansi(&seg, "0"),
                is_checkbox: false,
            });
        }
    }
    rows
}

/// Returns `true` for raw HTML / SVG paths whose buffer should be previewed directly.
pub fn is_raw_html_path(path: Option<&std::path::Path>) -> bool {
    path.and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "html" | "htm" | "svg"))
}
