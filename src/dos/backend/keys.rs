//! Host key -> QEMU QMP `send-key` qcode chords (US layout). Pure and table tested.

use super::{BackendError, Result};

/// A key press delivered to the guest. `Char` covers printable ASCII.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DosKey {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1..=F12.
    F(u8),
    Ctrl(char),
    Alt(char),
}

/// Keys pressed together (down in order, released together) as one `send-key` command.
pub type Chord = Vec<&'static str>;

fn plain(c: char) -> Option<&'static str> {
    Some(match c {
        'a'..='z' => return Some(LETTERS[(c as u8 - b'a') as usize]),
        '0'..='9' => return Some(DIGITS[(c as u8 - b'0') as usize]),
        ' ' => "spc",
        '-' => "minus",
        '=' => "equal",
        '[' => "bracket_left",
        ']' => "bracket_right",
        '\\' => "backslash",
        ';' => "semicolon",
        '\'' => "apostrophe",
        '`' => "grave_accent",
        ',' => "comma",
        '.' => "dot",
        '/' => "slash",
        _ => return None,
    })
}

const LETTERS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];
const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Unshifted character a shifted ASCII symbol is typed with (US layout).
fn shifted_base(c: char) -> Option<char> {
    Some(match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '~' => '`',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return None,
    })
}

fn fkey(n: u8) -> Option<&'static str> {
    const F: [&str; 12] = [
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
    ];
    (1..=12).contains(&n).then(|| F[n as usize - 1])
}

impl DosKey {
    /// The qcode chord for this key.
    pub fn chord(self) -> Result<Chord> {
        let bad = |what: String| BackendError::Invalid(format!("no DOS key for {what}"));
        Ok(match self {
            Self::Char(c) if c.is_ascii_uppercase() => {
                vec!["shift", plain(c.to_ascii_lowercase()).expect("letter")]
            }
            Self::Char(c) => {
                if let Some(q) = plain(c) {
                    vec![q]
                } else if let Some(q) = shifted_base(c).and_then(plain) {
                    vec!["shift", q]
                } else {
                    return Err(bad(format!("{c:?}")));
                }
            }
            Self::Enter => vec!["ret"],
            Self::Backspace => vec!["backspace"],
            Self::Tab => vec!["tab"],
            Self::Esc => vec!["esc"],
            Self::Up => vec!["up"],
            Self::Down => vec!["down"],
            Self::Left => vec!["left"],
            Self::Right => vec!["right"],
            Self::Home => vec!["home"],
            Self::End => vec!["end"],
            Self::PageUp => vec!["pgup"],
            Self::PageDown => vec!["pgdn"],
            Self::Insert => vec!["insert"],
            Self::Delete => vec!["delete"],
            Self::F(n) => vec![fkey(n).ok_or_else(|| bad(format!("F{n}")))?],
            Self::Ctrl(c) | Self::Alt(c) => {
                let base = plain(c.to_ascii_lowercase()).ok_or_else(|| bad(format!("{c:?}")))?;
                vec![
                    if matches!(self, Self::Ctrl(_)) {
                        "ctrl"
                    } else {
                        "alt"
                    },
                    base,
                ]
            }
        })
    }
}

/// Converts typed text to keys; `\n` becomes Enter, `\t` Tab, anything non-ASCII is rejected.
pub fn keys_for_text(text: &str) -> Result<Vec<DosKey>> {
    text.chars()
        .map(|c| match c {
            '\n' => Ok(DosKey::Enter),
            '\t' => Ok(DosKey::Tab),
            c if c.is_ascii() && !c.is_ascii_control() => Ok(DosKey::Char(c)),
            c => Err(BackendError::Invalid(format!("cannot type {c:?} into DOS"))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        let cases: &[(DosKey, &[&str])] = &[
            (DosKey::Char('a'), &["a"]),
            (DosKey::Char('A'), &["shift", "a"]),
            (DosKey::Char('7'), &["7"]),
            (DosKey::Char(':'), &["shift", "semicolon"]),
            (DosKey::Char('>'), &["shift", "dot"]),
            (DosKey::Char('_'), &["shift", "minus"]),
            (DosKey::Char('\\'), &["backslash"]),
            (DosKey::Char(' '), &["spc"]),
            (DosKey::Enter, &["ret"]),
            (DosKey::F(10), &["f10"]),
            (DosKey::Ctrl('C'), &["ctrl", "c"]),
            (DosKey::Alt('x'), &["alt", "x"]),
            (DosKey::PageDown, &["pgdn"]),
        ];
        for (k, want) in cases {
            assert_eq!(&k.chord().unwrap(), want, "{k:?}");
        }
    }

    #[test]
    fn every_printable_ascii_maps() {
        for b in 0x20u8..0x7f {
            assert!(DosKey::Char(b as char).chord().is_ok(), "{}", b as char);
        }
    }

    #[test]
    fn rejects_unmappable() {
        assert!(DosKey::Char('é').chord().is_err());
        assert!(DosKey::F(0).chord().is_err());
        assert!(DosKey::F(13).chord().is_err());
        assert!(DosKey::Ctrl('é').chord().is_err());
        assert!(keys_for_text("hi\u{1b}").is_err());
        assert_eq!(
            keys_for_text("a\n").unwrap(),
            vec![DosKey::Char('a'), DosKey::Enter]
        );
    }
}
