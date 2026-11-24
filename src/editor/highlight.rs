//! Markdown syntax classification for the editor (`EDIT-04`): line-oriented, no parser crate.

/// Style class of one character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Plain,
    Heading,
    Code,
    Quote,
    Marker,
    Bold,
    Italic,
    Link,
    Rule,
    CheckOpen,
    CheckDone,
    CheckDoneText,
}

impl Kind {
    /// SGR parameters for this class.
    pub fn sgr(self) -> &'static str {
        match self {
            Self::Plain => "0",
            Self::Heading => "1;36",
            Self::Code => "33",
            Self::Quote => "32",
            Self::Marker => "35",
            Self::Bold => "1",
            Self::Italic => "3",
            Self::Link => "4;34",
            Self::Rule => "2;36",
            Self::CheckOpen => "1;33",
            Self::CheckDone => "1;32",
            Self::CheckDoneText => "2;9",
        }
    }
}

/// `true` for a line that opens or closes a fenced code block.
pub fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Per-line `in_fence` state at the start of every line of `lines`.
pub fn fence_states<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<bool> {
    let mut open = false;
    lines
        .map(|l| {
            let s = open;
            if is_fence(l) {
                open = !open;
            }
            s
        })
        .collect()
}

/// Classifies every char of `line`. `in_fence` is the state at the start of the line.
pub fn classify(line: &str, in_fence: bool) -> Vec<Kind> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    if in_fence || is_fence(line) {
        return vec![Kind::Code; n];
    }
    let trimmed = line.trim_start();
    let indent = n - trimmed.chars().count();
    let mut out = vec![Kind::Plain; n];
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && trimmed.chars().nth(hashes).is_none_or(|c| c == ' ') {
        return vec![Kind::Heading; n];
    }
    let t: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_')) {
        return vec![Kind::Rule; n];
    }
    if trimmed.starts_with('>') {
        return vec![Kind::Quote; n];
    }
    let mut i = indent;
    // list markers: `- `, `* `, `+ `, `1. `
    let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let marker = if matches!(chars.get(i), Some('-' | '*' | '+')) && chars.get(i + 1) == Some(&' ') {
        Some(2)
    } else if digits > 0 && chars.get(i + digits) == Some(&'.') && chars.get(i + digits + 1) == Some(&' ') {
        Some(digits + 2)
    } else {
        None
    };
    if let Some(m) = marker {
        out[i..i + m].fill(Kind::Marker);
        i += m;
        if let Some(cb) = super::artifact::find_checkbox(line) {
            out[cb.box_col..cb.end_col].fill(if cb.checked { Kind::CheckDone } else { Kind::CheckOpen });
            if cb.checked {
                out[cb.end_col..n].fill(Kind::CheckDoneText);
                return out;
            }
            i = cb.end_col;
        }
    }
    while i < n {
        match chars[i] {
            '`' => {
                if let Some(j) = chars[i + 1..].iter().position(|&c| c == '`') {
                    out[i..=i + 1 + j].fill(Kind::Code);
                    i += j + 2;
                    continue;
                }
            }
            '*' | '_' => {
                let c = chars[i];
                let double = chars.get(i + 1) == Some(&c);
                let w = if double { 2 } else { 1 };
                let open_ok = chars.get(i + w).is_some_and(|x| !x.is_whitespace());
                if open_ok && let Some(j) = (i + w + 1..n).find(|&j| chars[j] == c && (!double || chars.get(j + 1) == Some(&c))) {
                    out[i..j + w].fill(if double { Kind::Bold } else { Kind::Italic });
                    i = j + w;
                    continue;
                }
            }
            '[' => {
                if let Some(j) = chars[i..].iter().position(|&c| c == ']')
                    && chars.get(i + j + 1) == Some(&'(')
                    && let Some(k) = chars[i + j + 2..].iter().position(|&c| c == ')')
                {
                    out[i..i + j + 2 + k + 1].fill(Kind::Link);
                    i += j + k + 3;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_markdown() {
        assert!(classify("# Title", false).iter().all(|k| *k == Kind::Heading));
        assert!(classify("#hashtag", false).iter().all(|k| *k == Kind::Plain));
        assert!(classify("> quote", false).iter().all(|k| *k == Kind::Quote));
        assert!(classify("---", false).iter().all(|k| *k == Kind::Rule));
        let k = classify("- a `x` **b** *c* [t](u)", false);
        assert_eq!(k[0], Kind::Marker);
        assert_eq!(k[4], Kind::Code);
        assert_eq!(k[10], Kind::Bold);
        assert_eq!(k[16], Kind::Italic);
        assert_eq!(k[20], Kind::Link);
        let k = classify("1. x", false);
        assert_eq!(&k[..3], &[Kind::Marker; 3]);
    }

    #[test]
    fn fences_span_lines() {
        let lines = ["a", "```rs", "# not heading", "```", "# heading"];
        let st = fence_states(lines.iter().copied());
        assert_eq!(st, vec![false, false, true, true, false]);
        assert!(classify(lines[2], st[2]).iter().all(|k| *k == Kind::Code));
        assert!(classify(lines[4], st[4]).iter().all(|k| *k == Kind::Heading));
    }
}
