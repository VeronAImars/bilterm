//! End-to-end image tests: lifecycle round trips, CLI, provisioning with a simulated guest, and
//! the ignored real-QEMU smoke.

use std::fs;

use super::build::tests_support::{Tiny, fake_boot16};
use super::build::{BuildOpts, build_image};
use super::bootsector::{CaptureCfg, load_cached};
use super::cmd::{ImgCmd, ImgEnv, execute};
use super::fixtures::{fake_livecd, make_zip};
use super::lock::fake::FakeProcs;
use super::provision::{BackendFactory, Provision, iso_path, provision};
use super::sanity::check_image;
use super::store::{ImageStore, MAX_USER_SNAPSHOTS};
use super::{DEFAULT_NAME, ImageError, ImageKind};
use crate::dos::backend::DosBackend;
use crate::dos::manifest::Manifest;
use crate::dos::paths::DosPaths;
use crate::dos::verify::sha256_bytes;

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub store: ImageStore,
}

/// A data dir holding a valid 32 MiB `default` image built from [`Tiny`].
pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = DosPaths::new(dir.path().join("bilterm").join("dos"));
    paths.ensure_layout().unwrap();
    let store = ImageStore::new(paths);
    let out = store.image_path(DEFAULT_NAME).unwrap();
    build_image(&BuildOpts { out: &out, size_bytes: 32 << 20, boot_sector: Some(&fake_boot16()) }, &Tiny).unwrap();
    Fixture { dir, store }
}

fn env<'a>(fx: &'a Fixture, procs: &'a FakeProcs) -> ImgEnv<'a> {
    ImgEnv { paths: Some(&fx.store.paths), procs, is_root: false, now_secs: Some(1234) }
}

fn run(fx: &Fixture, cmd: ImgCmd, input: &str) -> (i32, String) {
    let procs = FakeProcs::new(10);
    let mut out = String::new();
    let code = execute(&cmd, &env(fx, &procs), &mut input.as_bytes(), &mut out);
    (code, out)
}

#[test]
fn snapshot_list_restore_rm_round_trip() {
    let fx = fixture();
    let img = fx.store.image_path("default").unwrap();
    let orig = fs::read(&img).unwrap();
    let (c, o) = run(&fx, ImgCmd::Snapshot { name: "default".into(), tag: Some("before".into()) }, "");
    assert_eq!(c, 0, "{o}");
    // damage the live image, then restore
    let mut bad = orig.clone();
    bad[3 << 20] = 0x42;
    fs::write(&img, &bad).unwrap();
    let (c, o) = run(&fx, ImgCmd::SnapshotList { name: "default".into() }, "");
    assert!(c == 0 && o.contains("before"), "{o}");
    let (c, o) = run(&fx, ImgCmd::Restore { name: "default".into(), tag: "before".into(), yes: false }, "n\n");
    assert_eq!(c, 1, "{o}");
    assert_eq!(fs::read(&img).unwrap(), bad);
    let (c, o) = run(&fx, ImgCmd::Restore { name: "default".into(), tag: "before".into(), yes: false }, "y\n");
    assert_eq!(c, 0, "{o}");
    assert_eq!(fs::read(&img).unwrap(), orig);
    let (c, _) = run(&fx, ImgCmd::SnapshotRm { name: "default".into(), tag: "before".into() }, "");
    assert_eq!(c, 0);
    assert!(fx.store.list("default").unwrap().is_empty());
    let (c, o) = run(&fx, ImgCmd::Restore { name: "default".into(), tag: "before".into(), yes: true }, "");
    assert_eq!(c, 1, "{o}");
}

#[test]
fn default_tag_and_duplicate_and_reserved_and_cap() {
    let fx = fixture();
    let (c, o) = run(&fx, ImgCmd::Snapshot { name: "default".into(), tag: None }, "");
    assert!(c == 0 && o.contains("snap-1234"), "{o}");
    let (c, _) = run(&fx, ImgCmd::Snapshot { name: "default".into(), tag: None }, "");
    assert_eq!(c, 1, "duplicate tag must be refused");
    let (c, o) = run(&fx, ImgCmd::Snapshot { name: "default".into(), tag: Some("pre-run-1".into()) }, "");
    assert!(c == 1 && o.contains("reserved"), "{o}");
    let procs = FakeProcs::new(10);
    for i in 0..MAX_USER_SNAPSHOTS {
        let _ = fx.store.snapshot("default", &format!("t{i}"), &procs);
    }
    assert!(fx.store.snapshot("default", "overflow", &procs).is_err());
}

#[test]
fn names_and_tags_are_validated_and_stay_inside_the_private_dir() {
    let fx = fixture();
    let procs = FakeProcs::new(10);
    for bad in ["../evil", "a/b", "a\\b", "..", ".", "", "a b", "x\0y", &"z".repeat(65)] {
        assert!(fx.store.snapshot(bad, "t", &procs).is_err(), "name {bad:?}");
        assert!(fx.store.snapshot("default", bad, &procs).is_err(), "tag {bad:?}");
        assert!(fx.store.restore("default", bad, &procs).is_err());
        assert!(fx.store.remove_snapshot("default", bad, &procs).is_err());
        assert!(fx.store.image_path(bad).is_err());
    }
    assert!(!fx.dir.path().join("evil").exists());
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed() {
    use std::os::unix::fs::symlink;
    let fx = fixture();
    let procs = FakeProcs::new(10);
    // image replaced by a symlink to a victim file
    let victim = fx.dir.path().join("victim");
    fs::write(&victim, b"precious").unwrap();
    let img = fx.store.image_path("default").unwrap();
    fs::remove_file(&img).unwrap();
    symlink(&victim, &img).unwrap();
    assert!(fx.store.snapshot("default", "s", &procs).is_err());
    assert!(!check_image(&img).ok());
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    // snapshot file replaced by a symlink: restore must refuse, rm must not touch the target
    fs::remove_file(&img).unwrap();
    build_image(&BuildOpts { out: &img, size_bytes: 32 << 20, boot_sector: Some(&fake_boot16()) }, &Tiny).unwrap();
    fx.store.snapshot("default", "s", &procs).unwrap();
    let sp = fx.store.snap_path("default", "s").unwrap();
    fs::remove_file(&sp).unwrap();
    symlink(&victim, &sp).unwrap();
    assert!(fx.store.restore("default", "s", &procs).is_err());
    assert!(fx.store.remove_snapshot("default", "s", &procs).is_err());
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    // a symlinked snapshot dir is rejected by ensure_private_dir
    let sd = fx.store.snap_dir("other").unwrap();
    let elsewhere = fx.dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, &sd).unwrap();
    fs::copy(&img, fx.store.image_path("other").unwrap()).unwrap();
    assert!(fx.store.snapshot("other", "s", &procs).is_err());
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
}

#[test]
fn restore_refuses_a_corrupt_snapshot_and_keeps_the_image() {
    let fx = fixture();
    let procs = FakeProcs::new(10);
    fx.store.snapshot("default", "s", &procs).unwrap();
    let sp = fx.store.snap_path("default", "s").unwrap();
    let mut raw = fs::read(&sp).unwrap();
    raw[510] = 0; // kill the MBR signature
    fs::write(&sp, raw).unwrap();
    let before = fs::read(fx.store.image_path("default").unwrap()).unwrap();
    assert!(matches!(fx.store.restore("default", "s", &procs), Err(ImageError::Corrupt(_))));
    assert_eq!(fs::read(fx.store.image_path("default").unwrap()).unwrap(), before);
}

#[test]
fn mutations_are_refused_while_another_process_holds_the_lock() {
    let fx = fixture();
    let a = FakeProcs::new(10);
    let _held = fx.store.lock("default", &a).unwrap();
    let mut b = FakeProcs::new(11);
    b.procs.insert(10, "100".into());
    assert!(matches!(fx.store.snapshot("default", "s", &b), Err(ImageError::LockHeld { pid: 10 })));
    assert!(matches!(fx.store.restore("default", "s", &b), Err(ImageError::LockHeld { .. })));
}

#[test]
fn root_is_refused_and_missing_image_has_a_hint() {
    let fx = fixture();
    let procs = FakeProcs::new(10);
    let mut e = env(&fx, &procs);
    e.is_root = true;
    let mut out = String::new();
    assert_eq!(execute(&ImgCmd::SnapshotList { name: "default".into() }, &e, &mut "".as_bytes(), &mut out), 1);
    assert!(out.contains("root"));
    let (c, o) = run(&fx, ImgCmd::Snapshot { name: "ghost".into(), tag: None }, "");
    assert!(c == 1 && o.contains("dos setup"), "{o}");
}

/// Manifest whose Live CD entry pins a tiny synthetic zip.
fn fake_manifest(zip: &[u8], iso: &[u8], floppy: &[u8]) -> Manifest {
    let mut m = Manifest::embedded().unwrap();
    let e = m.entries.iter_mut().find(|e| e.id == "freedos-14-livecd").unwrap();
    e.sha256 = sha256_bytes(zip);
    e.bytes = zip.len() as u64;
    for mem in &mut e.members {
        let data = if mem.name == "FD14LIVE.iso" { iso } else { floppy };
        mem.sha256 = sha256_bytes(data);
        mem.bytes = data.len() as u64;
    }
    m
}

struct SimFactory;
impl BackendFactory for SimFactory {
    fn make(&self) -> Result<Box<dyn DosBackend>, String> {
        Ok(Box::new(super::bootsector::sim::SysSim::new(fake_boot16())))
    }
}
struct NoFactory;
impl BackendFactory for NoFactory {
    fn make(&self) -> Result<Box<dyn DosBackend>, String> {
        Err("qemu-system-i386 not found".into())
    }
}

#[test]
fn provision_extracts_captures_builds_then_reset_rebuilds() {
    let d = tempfile::tempdir().unwrap();
    let paths = DosPaths::new(d.path().join("dos"));
    paths.ensure_layout().unwrap();
    let (iso, floppy) = (fake_livecd(), b"floppy-image".to_vec());
    let zip = make_zip(&[("FD14BOOT.img", &floppy, false), ("FD14LIVE.iso", &iso, true), ("readme.txt", b"hi", false)]);
    let m = fake_manifest(&zip, &iso, &floppy);
    fs::write(paths.download_path(&m.entry("freedos-14-livecd").unwrap().file).unwrap(), &zip).unwrap();
    let procs = FakeProcs::new(10);
    let mut p = Provision::new(&paths, &m, &SimFactory, &procs);
    p.cfg = CaptureCfg { poll: std::time::Duration::ZERO };
    let mut log = Vec::new();
    let msg = provision(&p, ImageKind::Base, &mut |s| log.push(s.to_string())).unwrap();
    assert!(msg.contains("built C: image"), "{msg}");
    assert!(log.iter().any(|l| l.contains("capturing")));
    assert_eq!(fs::read(iso_path(&paths)).unwrap(), iso);
    assert_eq!(load_cached(&paths).unwrap().unwrap(), fake_boot16());
    let store = ImageStore::new(paths.clone());
    let rep = check_image(&store.image_path("default").unwrap());
    assert!(rep.ok(), "{:?}", rep.problems);
    assert!(fs::read_dir(paths.sessions_dir()).unwrap().next().is_none(), "scratch image must be removed");
    // second run: nothing to redo, no emulator needed
    let p2 = Provision::new(&paths, &m, &NoFactory, &procs);
    assert!(provision(&p2, ImageKind::Base, &mut |_| {}).unwrap().contains("already exists"));
    // reset via the CLI rebuilds from the extracted iso with the cached boot sector
    fs::write(store.image_path("default").unwrap(), b"trashed").unwrap();
    let e = ImgEnv { paths: Some(&paths), procs: &procs, is_root: false, now_secs: None };
    let mut out = String::new();
    let cmd = ImgCmd::Reset { name: "default".into(), full: false, yes: false };
    assert_eq!(execute(&cmd, &e, &mut "n\n".as_bytes(), &mut out), 1);
    assert_eq!(fs::read(store.image_path("default").unwrap()).unwrap(), b"trashed");
    assert_eq!(execute(&cmd, &e, &mut "y\n".as_bytes(), &mut out), 0, "{out}");
    assert!(check_image(&store.image_path("default").unwrap()).ok());
}

#[test]
fn provision_without_emulator_fails_cleanly_and_leaves_no_image() {
    let d = tempfile::tempdir().unwrap();
    let paths = DosPaths::new(d.path().join("dos"));
    paths.ensure_layout().unwrap();
    let (iso, floppy) = (fake_livecd(), b"f".to_vec());
    let zip = make_zip(&[("FD14BOOT.img", &floppy, false), ("FD14LIVE.iso", &iso, false)]);
    let m = fake_manifest(&zip, &iso, &floppy);
    fs::write(paths.download_path(&m.entry("freedos-14-livecd").unwrap().file).unwrap(), &zip).unwrap();
    let procs = FakeProcs::new(10);
    let err = provision(&Provision::new(&paths, &m, &NoFactory, &procs), ImageKind::Base, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    assert!(!ImageStore::new(paths.clone()).exists("default").unwrap());
    assert!(load_cached(&paths).unwrap().is_none());
    assert!(fs::read_dir(paths.sessions_dir()).unwrap().next().is_none());
}

#[test]
fn reset_without_setup_artifacts_explains_itself() {
    let d = tempfile::tempdir().unwrap();
    let paths = DosPaths::new(d.path().join("dos"));
    paths.ensure_layout().unwrap();
    let procs = FakeProcs::new(10);
    let e = ImgEnv { paths: Some(&paths), procs: &procs, is_root: false, now_secs: None };
    let mut out = String::new();
    let code = execute(&ImgCmd::Reset { name: "default".into(), full: false, yes: true }, &e, &mut "".as_bytes(), &mut out);
    assert!(code == 1 && out.contains("dos setup"), "{out}");
}

/// Real run: capture the boot sector from the install floppy, build the BASE image with the
/// pure-Rust builder, boot it and wait for the FreeCom prompt. Needs `qemu-system-i386` and the
/// pinned `FD14LIVE.iso` + `FD14BOOT.img` in `$BILTERM_DOS_SMOKE_DIR`.
/// Run: `BILTERM_DOS_SMOKE=1 BILTERM_DOS_SMOKE_DIR=<dir> cargo test --features dos --lib -- --ignored dos_smoke`
#[test]
#[ignore = "boots real QEMU; set BILTERM_DOS_SMOKE=1 and BILTERM_DOS_SMOKE_DIR"]
fn dos_smoke_builds_and_boots_to_freecom() {
    use super::bootsector::{capture_via_guest, default_steps};
    use super::livecd::LiveIsoSource;
    use crate::dos::backend::qemu::{QemuBackend, QemuMode};
    use crate::dos::backend::{DosKey, SessionSpec};
    use std::path::PathBuf;
    if std::env::var_os("BILTERM_DOS_SMOKE").is_none() {
        eprintln!("BILTERM_DOS_SMOKE not set; skipping");
        return;
    }
    let src = PathBuf::from(std::env::var_os("BILTERM_DOS_SMOKE_DIR").expect("BILTERM_DOS_SMOKE_DIR"));
    let (iso, floppy) = (src.join("FD14LIVE.iso"), src.join("FD14BOOT.img"));
    let tmp = tempfile::tempdir().unwrap();
    let qbase = crate::secure_fs::private_dir_in(tmp.path(), "q").unwrap();
    let mk = || QemuBackend::new(QemuMode::TextBuf, "qemu-system-i386".into(), qbase.clone());

    let scratch = tmp.path().join("scratch.img");
    let none = LiveIsoSource::new(iso.clone(), &[]);
    build_image(&BuildOpts { out: &scratch, size_bytes: super::BASE_BYTES, boot_sector: None }, &none).unwrap();
    let mut be = mk();
    let sector = capture_via_guest(&mut be, &floppy, &scratch, &default_steps(), CaptureCfg::default(), &mut |m| eprintln!("capture: {m}")).unwrap();

    let c = tmp.path().join("c.img");
    let base = LiveIsoSource::new(iso, ImageKind::Base.sets());
    let t = std::time::Instant::now();
    let st = build_image(&BuildOpts { out: &c, size_bytes: super::BASE_BYTES, boot_sector: Some(&sector) }, &base).unwrap();
    eprintln!("built {} files / {} bytes in {:?}", st.files, st.bytes, t.elapsed());
    let mut rep = check_image(&c);
    if let Some(fsck) = super::sanity::find_fsck() {
        super::sanity::fsck_partition(&c, &fsck, tmp.path(), &mut rep).unwrap();
    }
    assert!(rep.ok(), "{:?}", rep.problems);

    let mut be = mk();
    be.launch(&SessionSpec { c_image: Some(c), throwaway: true, ..SessionSpec::default() }).unwrap();
    let mut last = String::new();
    let mut ok = false;
    for _ in 0..120 {
        let scr = be.read_text().unwrap();
        last = scr.lines().join("\n");
        if scr.contains("BILTERM-READY") {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    assert!(ok, "never reached the prompt:\n{last}");
    be.send_keys(&[DosKey::Char('v'), DosKey::Char('e'), DosKey::Char('r'), DosKey::Enter]).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let scr = be.read_text().unwrap();
    assert!(scr.contains("FreeCom"), "ver did not answer:\n{}", scr.lines().join("\n"));
    be.shutdown().unwrap();
}

