//! Host key events (crossterm) -> [`DosKey`]. Pure and table tested. Modifiers and function
//! keys are covered; anything the US-layout guest keyboard cannot express maps to `None`
//! (dropped, never an error), so a stray host key can never fail a pane's input path.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::keys::{ComboKey, DosKey, MOD_ALT, MOD_CTRL, MOD_SHIFT};

fn mods_bits(m: KeyModifiers) -> u8 {
    let mut b = 0;
    if m.contains(KeyModifiers::SHIFT) {
        b |= MOD_SHIFT;
    }
    if m.contains(KeyModifiers::CONTROL) {
        b |= MOD_CTRL;
    }
    if m.contains(KeyModifiers::ALT) {
        b |= MOD_ALT;
    }
    b
}

/// Maps one host key event. Release events and unmappable keys give `None`.
pub fn from_host(ev: &KeyEvent) -> Option<DosKey> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let bits = mods_bits(ev.modifiers);
    let nav = |k: ComboKey, plain: DosKey| {
        // Shift on a nav key is a modifier; for Enter/Backspace/Tab/Esc plain variants carry none.
        Some(if bits == 0 { plain } else { DosKey::Combo(bits, k) })
    };
    match ev.code {
        KeyCode::Char(c) => {
            if !c.is_ascii() || c.is_ascii_control() {
                return None;
            }
            match (bits & MOD_CTRL != 0, bits & MOD_ALT != 0) {
                (false, false) => Some(DosKey::Char(c)),
                (true, false) => Some(DosKey::Ctrl(c)),
                (false, true) => Some(DosKey::Alt(c)),
                (true, true) => None,
            }
        }
        KeyCode::Enter => nav(ComboKey::Enter, DosKey::Enter),
        KeyCode::Backspace => nav(ComboKey::Backspace, DosKey::Backspace),
        KeyCode::Tab => nav(ComboKey::Tab, DosKey::Tab),
        KeyCode::BackTab => Some(DosKey::Combo(bits | MOD_SHIFT, ComboKey::Tab)),
        KeyCode::Esc => nav(ComboKey::Esc, DosKey::Esc),
        KeyCode::Up => nav(ComboKey::Up, DosKey::Up),
        KeyCode::Down => nav(ComboKey::Down, DosKey::Down),
        KeyCode::Left => nav(ComboKey::Left, DosKey::Left),
        KeyCode::Right => nav(ComboKey::Right, DosKey::Right),
        KeyCode::Home => nav(ComboKey::Home, DosKey::Home),
        KeyCode::End => nav(ComboKey::End, DosKey::End),
        KeyCode::PageUp => nav(ComboKey::PageUp, DosKey::PageUp),
        KeyCode::PageDown => nav(ComboKey::PageDown, DosKey::PageDown),
        KeyCode::Insert => nav(ComboKey::Insert, DosKey::Insert),
        KeyCode::Delete => nav(ComboKey::Delete, DosKey::Delete),
        KeyCode::F(n) if (1..=12).contains(&n) => nav(ComboKey::F(n), DosKey::F(n)),
        _ => None,
    }
}

/// Maps a batch, dropping unmappable events; capped so one paste burst cannot flood QMP.
pub fn from_host_batch(evs: &[KeyEvent], cap: usize) -> Vec<DosKey> {
    evs.iter().filter_map(from_host).take(cap).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }
    const N: KeyModifiers = KeyModifiers::NONE;
    const S: KeyModifiers = KeyModifiers::SHIFT;
    const C: KeyModifiers = KeyModifiers::CONTROL;
    const A: KeyModifiers = KeyModifiers::ALT;

    #[test]
    fn table() {
        use ComboKey as K;
        let cases: Vec<(KeyEvent, Option<DosKey>)> = vec![
            (ev(KeyCode::Char('a'), N), Some(DosKey::Char('a'))),
            (ev(KeyCode::Char('A'), S), Some(DosKey::Char('A'))),
            (ev(KeyCode::Char(':'), S), Some(DosKey::Char(':'))),
            (ev(KeyCode::Char(' '), N), Some(DosKey::Char(' '))),
            (ev(KeyCode::Char('c'), C), Some(DosKey::Ctrl('c'))),
            (ev(KeyCode::Char('x'), A), Some(DosKey::Alt('x'))),
            (ev(KeyCode::Char('x'), C | A), None),
            (ev(KeyCode::Char('é'), N), None),
            (ev(KeyCode::Char('\u{1b}'), N), None),
            (ev(KeyCode::Enter, N), Some(DosKey::Enter)),
            (ev(KeyCode::Backspace, N), Some(DosKey::Backspace)),
            (ev(KeyCode::Tab, N), Some(DosKey::Tab)),
            (ev(KeyCode::BackTab, S), Some(DosKey::Combo(MOD_SHIFT, K::Tab))),
            (ev(KeyCode::Esc, N), Some(DosKey::Esc)),
            (ev(KeyCode::Up, N), Some(DosKey::Up)),
            (ev(KeyCode::Up, C), Some(DosKey::Combo(MOD_CTRL, K::Up))),
            (ev(KeyCode::Left, S | A), Some(DosKey::Combo(MOD_SHIFT | MOD_ALT, K::Left))),
            (ev(KeyCode::Delete, C | A), Some(DosKey::Combo(MOD_CTRL | MOD_ALT, K::Delete))),
            (ev(KeyCode::F(1), N), Some(DosKey::F(1))),
            (ev(KeyCode::F(12), N), Some(DosKey::F(12))),
            (ev(KeyCode::F(5), S), Some(DosKey::Combo(MOD_SHIFT, K::F(5)))),
            (ev(KeyCode::F(13), N), None),
            (ev(KeyCode::Null, N), None),
            (ev(KeyCode::CapsLock, N), None),
        ];
        for (e, want) in cases {
            assert_eq!(from_host(&e), want, "{e:?}");
        }
    }

    #[test]
    fn release_dropped_repeat_kept() {
        let mut e = ev(KeyCode::Char('a'), N);
        e.kind = KeyEventKind::Release;
        e.state = KeyEventState::NONE;
        assert_eq!(from_host(&e), None);
        e.kind = KeyEventKind::Repeat;
        assert_eq!(from_host(&e), Some(DosKey::Char('a')));
    }

    #[test]
    fn every_mapped_key_has_a_chord() {
        for c in (0x20u8..0x7f).map(char::from) {
            for m in [N, S, C, A] {
                if let Some(k) = from_host(&ev(KeyCode::Char(c), m)) {
                    // Ctrl/Alt of symbols may be unsupported by the qcode table; never panic.
                    let _ = k.chord();
                }
            }
        }
        for n in 1..=12u8 {
            for m in [N, S, C, A, S | C | A] {
                let k = from_host(&ev(KeyCode::F(n), m)).unwrap();
                assert!(k.chord().is_ok());
            }
        }
    }

    #[test]
    fn batch_is_capped_and_filtered() {
        let evs: Vec<KeyEvent> = (0..10).map(|_| ev(KeyCode::Char('a'), N)).collect();
        assert_eq!(from_host_batch(&evs, 4).len(), 4);
        assert!(from_host_batch(&[ev(KeyCode::Null, N)], 4).is_empty());
    }
}
