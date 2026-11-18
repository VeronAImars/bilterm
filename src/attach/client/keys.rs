//! Key handling for the attach client: bytes for the pane, plus the `Ctrl-]` command prefix.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::input::encode::encode_key_to_pty_bytes;

/// Detach prefix. `Ctrl-]` is rarely bound by shells/editors (telnet's escape character).
pub const PREFIX_BYTE: u8 = 0x1d;

/// Human-readable summary shown in the status line and `--help`.
pub const HELP: &str = "Ctrl-] d detach | n/p pane | Ctrl-] Ctrl-] literal";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Bytes for the selected pane.
    Send(Vec<u8>),
    Detach,
    NextPane,
    PrevPane,
    Nothing,
}

#[derive(Default)]
pub struct KeyMap {
    prefix: bool,
}

fn is_prefix(key: &KeyEvent) -> bool {
    // Legacy terminals send Ctrl-] as byte 0x1d, which crossterm reports as Ctrl-'5'.
    matches!(key.code, KeyCode::Char(']' | '5')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

impl KeyMap {
    pub fn prefix_pending(&self) -> bool {
        self.prefix
    }

    pub fn handle(&mut self, key: KeyEvent) -> KeyAction {
        if key.kind == KeyEventKind::Release {
            return KeyAction::Nothing;
        }
        if self.prefix {
            self.prefix = false;
            if is_prefix(&key) {
                return KeyAction::Send(vec![PREFIX_BYTE]);
            }
            return match key.code {
                KeyCode::Char('d' | 'q' | 'D' | 'Q') => KeyAction::Detach,
                KeyCode::Char('n' | 'N') | KeyCode::Right => KeyAction::NextPane,
                KeyCode::Char('p' | 'P') | KeyCode::Left => KeyAction::PrevPane,
                _ => KeyAction::Nothing,
            };
        }
        if is_prefix(&key) {
            self.prefix = true;
            return KeyAction::Nothing;
        }
        let bytes = encode_key_to_pty_bytes(key);
        if bytes.is_empty() {
            KeyAction::Nothing
        } else {
            KeyAction::Send(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: char, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), m)
    }

    #[test]
    fn plain_keys_become_bytes() {
        let mut m = KeyMap::default();
        assert_eq!(m.handle(k('a', KeyModifiers::NONE)), KeyAction::Send(b"a".to_vec()));
        assert_eq!(m.handle(k('c', KeyModifiers::CONTROL)), KeyAction::Send(vec![3]));
        assert_eq!(m.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), KeyAction::Send(b"\r".to_vec()));
    }

    #[test]
    fn prefix_commands() {
        let mut m = KeyMap::default();
        let p = k(']', KeyModifiers::CONTROL);
        assert_eq!(m.handle(p), KeyAction::Nothing);
        assert!(m.prefix_pending());
        assert_eq!(m.handle(k('d', KeyModifiers::NONE)), KeyAction::Detach);
        assert!(!m.prefix_pending());
        m.handle(p);
        assert_eq!(m.handle(k('n', KeyModifiers::NONE)), KeyAction::NextPane);
        m.handle(p);
        assert_eq!(m.handle(k('p', KeyModifiers::NONE)), KeyAction::PrevPane);
        m.handle(p);
        assert_eq!(m.handle(p), KeyAction::Send(vec![PREFIX_BYTE]));
        m.handle(p);
        assert_eq!(m.handle(k('z', KeyModifiers::NONE)), KeyAction::Nothing, "unknown prefix key is swallowed");
        assert_eq!(m.handle(k('z', KeyModifiers::NONE)), KeyAction::Send(b"z".to_vec()), "and the prefix is cleared");
    }

    #[test]
    fn releases_are_ignored() {
        let mut m = KeyMap::default();
        let mut e = k('a', KeyModifiers::NONE);
        e.kind = KeyEventKind::Release;
        assert_eq!(m.handle(e), KeyAction::Nothing);
    }
}
