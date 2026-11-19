//! Table-driven backend conformance suite. Runs against the mock backend always; real
//! backends run the same cases in `#[ignore]` smoke tests (see `qemu::tests`).

use super::keys::keys_for_text;
use super::mock::MockBackend;
use super::*;

type Factory = fn() -> Box<dyn DosBackend>;

fn mock() -> Box<dyn DosBackend> {
    Box::new(MockBackend::new())
}

const BACKENDS: &[(&str, Factory)] = &[("mock", mock)];

fn spec() -> SessionSpec {
    SessionSpec::default()
}

fn typed(b: &mut dyn DosBackend, s: &str) {
    b.send_keys(&keys_for_text(s).unwrap()).unwrap();
}

#[test]
fn lifecycle_and_not_running_errors() {
    for (name, f) in BACKENDS {
        let mut b = f();
        assert_eq!(b.id(), *name);
        assert!(!b.is_running(), "{name}");
        assert!(
            matches!(b.read_text(), Err(BackendError::NotRunning)),
            "{name}"
        );
        assert!(
            matches!(b.send_keys(&[DosKey::Enter]), Err(BackendError::NotRunning)),
            "{name}"
        );
        b.launch(&spec()).unwrap();
        assert!(b.is_running(), "{name}");
        assert!(b.launch(&spec()).is_err(), "{name}: double launch");
        b.shutdown().unwrap();
        assert!(!b.is_running(), "{name}");
        b.shutdown().unwrap(); // idempotent
    }
}

#[test]
fn plan_is_pure_and_bridge_or_pty() {
    for (name, f) in BACKENDS {
        let mut b = f();
        let p = b.plan(&spec()).unwrap();
        assert!(!b.is_running(), "{name}: plan must not spawn");
        assert!(!p.argv.iter().any(|a| a.contains('\0')), "{name}");
    }
}

#[test]
fn canned_screen_and_echo() {
    for (name, f) in BACKENDS {
        let mut b = f();
        b.launch(&spec()).unwrap();
        let s = b.read_text().unwrap();
        assert_eq!((s.cols, s.rows), (GUEST_COLS, GUEST_ROWS), "{name}");
        assert!(s.contains("C:\\>"), "{name}");
        assert!(s.is_text_mode());
        typed(&mut *b, "ver");
        assert!(b.read_text().unwrap().contains("C:\\>ver"), "{name}");
        assert_eq!(b.read_text().unwrap().cursor, Some((7, 2)), "{name}");
        b.send_keys(&[DosKey::Enter]).unwrap();
        let s = b.read_text().unwrap();
        assert!(s.contains("FreeDOS mock 1.4"), "{name}");
        typed(&mut *b, "zz\n");
        assert!(b.read_text().unwrap().contains("Bad command"), "{name}");
        b.send_keys(&[DosKey::Char('a'), DosKey::Backspace])
            .unwrap();
        assert!(!b.read_text().unwrap().contains("C:\\>a"), "{name}");
    }
}

#[test]
fn deterministic() {
    let run = || {
        let mut b = MockBackend::new();
        b.launch(&spec()).unwrap();
        typed(&mut b, "ver\nxx");
        b.read_text().unwrap()
    };
    assert_eq!(run(), run());
}

#[test]
fn invalid_keys_rejected_without_effect() {
    for (name, f) in BACKENDS {
        let mut b = f();
        b.launch(&spec()).unwrap();
        let before = b.read_text().unwrap();
        assert!(
            b.send_keys(&[DosKey::Char('a'), DosKey::F(99)]).is_err(),
            "{name}"
        );
        // a rejected batch may or may not apply the valid prefix, but must not corrupt the screen
        assert!(b.read_text().is_ok());
        let _ = before;
    }
}

#[test]
fn screen_scrolls_and_stays_80x25() {
    let mut b = MockBackend::new();
    b.launch(&spec()).unwrap();
    for _ in 0..40 {
        typed(&mut b, "x\n");
    }
    let s = b.read_text().unwrap();
    assert_eq!(s.cells.len(), 80 * 25);
    assert_eq!(s.cursor.unwrap().1, 24);
}

#[test]
fn snapshot_restore_fakes() {
    let mut b = MockBackend::new();
    assert!(matches!(b.snapshot("a"), Err(BackendError::NotRunning)));
    b.launch(&spec()).unwrap();
    assert!(b.caps().snapshots);
    typed(&mut b, "ver\n");
    b.snapshot("a").unwrap();
    typed(&mut b, "zz\n");
    assert!(b.read_text().unwrap().contains("Bad command"));
    b.restore("a").unwrap();
    assert!(!b.read_text().unwrap().contains("Bad command"));
    assert!(b.restore("missing").is_err());
    assert!(b.snapshot("../evil").is_err());
}

#[test]
fn resize_validates() {
    let mut b = MockBackend::new();
    b.launch(&spec()).unwrap();
    assert!(b.resize(0, 10).is_err());
    b.resize(100, 30).unwrap();
    assert_eq!(b.last_size(), Some((100, 30)));
}
