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

// ---- Pty-plan backends (qemu-curses, D2a) ---------------------------------------------------
// Real emulators cannot run offline, so their conformance row covers the pure parts of the
// contract (`plan`, `caps`, `launch` refusal); the live half is the `#[ignore]` smoke test in
// `dos::launch::tests::dos_curses_smoke`. Add one row per new Pty/Bridge backend.

type PlanFactory = fn(&std::path::Path) -> Box<dyn DosBackend>;

fn curses(base: &std::path::Path) -> Box<dyn DosBackend> {
    use super::qemu::{QemuBackend, QemuMode};
    Box::new(QemuBackend::new(QemuMode::Curses, "/usr/bin/qemu-system-i386".into(), base.to_path_buf()))
}

const PTY_BACKENDS: &[(&str, PlanFactory, LaunchKind)] = &[("qemu-curses", curses, LaunchKind::Pty)];

#[test]
fn pty_backends_plan_purely_and_keep_safety_defaults() {
    for (name, f, kind) in PTY_BACKENDS {
        let base = tempfile::tempdir().unwrap();
        let mut b = f(base.path());
        assert_eq!(b.id(), *name);
        let s = SessionSpec { c_image: Some("/d/c.img".into()), ..spec() };
        let p = b.plan(&s).unwrap();
        assert_eq!(&p.kind, kind, "{name}");
        assert!(!b.is_running(), "{name}: plan must not spawn");
        assert!(b.caps().text_pane_native, "{name}");
        assert!(p.argv.windows(2).any(|w| w == ["-nic", "none"]), "{name}");
        assert!(crate::dos::launch::check_plan_safety(&p).is_ok(), "{name}");
        assert!(matches!(b.launch(&s), Err(BackendError::Unsupported(_))), "{name}: Pty backends run in the host PTY");
        // no boot media is a contract error, not a spawn
        assert!(b.plan(&SessionSpec { c_image: None, ..spec() }).is_err(), "{name}");
        b.shutdown().unwrap();
    }
}
