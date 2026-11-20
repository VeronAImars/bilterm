use std::cell::RefCell;
use std::fs;

use super::*;
use crate::dos::image::lock::fake::FakeProcs;

pub(crate) struct FakeProbe {
    pub locate: Option<&'static str>,
    pub version: &'static str,
    pub display: &'static str,
}

impl FakeProbe {
    pub fn good() -> Self {
        Self {
            locate: Some("/usr/bin/qemu-system-i386"),
            version: "QEMU emulator version 10.2.1",
            display: "Available display backend types:\nnone\ncurses\n",
        }
    }
}

impl EmulatorProbe for FakeProbe {
    fn locate(&self) -> Option<PathBuf> {
        self.locate.map(PathBuf::from)
    }
    fn version_output(&self) -> Result<String, String> {
        Ok(self.version.into())
    }
    fn display_help(&self) -> Result<String, String> {
        Ok(self.display.into())
    }
}

pub(crate) struct Fx {
    pub dir: tempfile::TempDir,
    pub paths: DosPaths,
    pub manifest: Manifest,
    pub procs: FakeProcs,
}

/// Data dir with a (fake, 1 MiB) `default` and `work` C: image.
pub(crate) fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let paths = DosPaths::new(dir.path().join("dos"));
    paths.ensure_layout().unwrap();
    for n in ["default", "work"] {
        fs::write(paths.c_dir().join(format!("{n}.img")), vec![0u8; 1 << 20]).unwrap();
    }
    Fx { dir, paths, manifest: Manifest::embedded().unwrap(), procs: FakeProcs::new(10) }
}

impl Fx {
    pub fn env<'a>(&'a self, probe: &'a FakeProbe) -> RunEnv<'a> {
        RunEnv {
            os: HostOs::Linux,
            paths: &self.paths,
            manifest: &self.manifest,
            probe,
            procs: &self.procs,
            is_root: false,
            term_size: Some((120, 40)),
            term_env: TermEnv {
                term: Some("xterm-256color".into()),
                lang: Some("en_US.UTF-8".into()),
                ..TermEnv::default()
            },
        }
    }
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn req(v: &[&str]) -> RunRequest {
    RunRequest::parse(&args(v)).unwrap().0
}

fn err(r: Result<impl std::fmt::Debug, RunError>) -> String {
    r.unwrap_err().to_string()
}

/// Replaces volatile absolute paths so argv snapshots are stable.
fn normalize(prep: &PreparedRun, fx: &Fx) -> String {
    let mut cmd = render_command(&prep.plan);
    if let crate::dos::backend::ControlEndpoint::Unix { path } = &prep.plan.control {
        cmd = cmd.replace(path.parent().unwrap().to_str().unwrap(), "<SESSION>");
    }
    cmd.replace(prep.work_dir.to_str().unwrap(), "<WORK>")
        .replace(fx.dir.path().to_str().unwrap(), "<TMP>")
        .split(" -")
        .collect::<Vec<_>>()
        .join("\n -")
}

#[test]
fn parse_request_forms() {
    let (r, p) = RunRequest::parse(&args(&["--name", "w", "--drive-c", "/d", "--print-argv"])).unwrap();
    assert_eq!(r.name.as_deref(), Some("w"));
    assert_eq!(r.drive_c, Some(PathBuf::from("/d")));
    assert!(p && r.image.is_none());
    assert!(RunRequest::parse(&args(&["--name", "a", "--image", "b"])).is_err());
    assert!(RunRequest::parse(&args(&["--name"])).is_err());
    assert!(RunRequest::parse(&args(&["--wat"])).is_err());
    assert_eq!(pane_argv(&req(&["--name", "w", "--backend", "qemu-curses"])), args(&["dos", "run", "--name", "w", "--backend", "qemu-curses"]));
}

#[test]
fn argv_snapshot_persistent_c() {
    let fx = fx();
    let probe = FakeProbe::good();
    let prep = prepare(&req(&[]), &fx.env(&probe), true).unwrap();
    insta::assert_snapshot!("persistent_c", normalize(&prep, &fx));
    assert_eq!(prep.plan.kind, LaunchKind::Pty);
    assert_eq!(prep.title, "DOS:default");
}

#[test]
fn argv_snapshot_throwaway_image_and_host_folder() {
    let fx = fx();
    let probe = FakeProbe::good();
    let img = fx.dir.path().join("my disk,v1.img");
    fs::write(&img, vec![0u8; 4096]).unwrap();
    let share = fx.dir.path().join("share");
    fs::create_dir(&share).unwrap();
    let r = RunRequest { image: Some(img), drive_c: Some(share), ..RunRequest::default() };
    let prep = prepare(&r, &fx.env(&probe), true).unwrap();
    insta::assert_snapshot!("throwaway_image_host_folder", normalize(&prep, &fx));
    assert!(prep.title.ends_with("(throwaway)"));
}

#[test]
fn plans_never_enable_network_usb_audio() {
    let fx = fx();
    let probe = FakeProbe::good();
    let prep = prepare(&req(&[]), &fx.env(&probe), true).unwrap();
    let a = &prep.plan.argv;
    assert!(a.windows(2).any(|w| w == ["-nic", "none"]));
    for bad in ["-netdev", "-net", "-usb", "-device", "-audiodev", "-audio", "-soundhw", "-virtfs", "-vnc"] {
        assert!(!a.iter().any(|x| x == bad), "{bad}");
    }
    assert!(a.windows(2).any(|w| w == ["-display", "curses"]));
    check_plan_safety(&prep.plan).unwrap();
}

#[test]
fn safety_check_rejects_leaks() {
    let fx = fx();
    let probe = FakeProbe::good();
    let base = prepare(&req(&[]), &fx.env(&probe), true).unwrap().plan.clone();
    for extra in [
        vec!["-netdev", "user,id=n"],
        vec!["-usb"],
        vec!["-device", "usb-host"],
        vec!["-audiodev", "pa,id=a"],
        vec!["-virtfs", "local,path=/"],
        vec!["-serial", "tcp::1234"],
        vec!["-vnc", ":1"],
    ] {
        let mut p = base.clone();
        p.argv.extend(extra.iter().map(|s| s.to_string()));
        assert!(check_plan_safety(&p).is_err(), "{extra:?}");
    }
    let mut p = base.clone();
    p.argv.retain(|a| a != "-nic" && a != "none");
    assert!(check_plan_safety(&p).is_err());
    let mut p = base;
    for a in p.argv.iter_mut() {
        if a.starts_with("unix:") {
            *a = "tcp:0.0.0.0:4444,server=on,wait=off".into();
        }
    }
    assert!(check_plan_safety(&p).unwrap_err().contains("QMP"));
}

#[test]
fn refusals_are_clear_and_side_effect_free() {
    let fx = fx();
    let good = FakeProbe::good();
    let mut env = fx.env(&good);
    env.is_root = true;
    assert!(err(prepare(&req(&[]), &env, true)).contains("root"));
    let mut env = fx.env(&good);
    env.os = HostOs::Other;
    assert!(err(prepare(&req(&[]), &env, true)).contains("unsupported"));
    let mut env = fx.env(&good);
    env.term_size = Some((79, 40));
    assert!(err(prepare(&req(&[]), &env, true)).contains("80x25"));
    env.term_size = Some((120, 24));
    assert!(err(prepare(&req(&[]), &env, true)).contains("80x25"));
    let none = FakeProbe { locate: None, ..FakeProbe::good() };
    assert!(err(prepare(&req(&[]), &fx.env(&none), true)).contains("not found"));
    let old = FakeProbe { version: "QEMU emulator version 9.0.0", ..FakeProbe::good() };
    assert!(err(prepare(&req(&[]), &fx.env(&old), true)).contains("unusable"));
    let nocurses = FakeProbe { display: "none\n", ..FakeProbe::good() };
    // auto-selection falls to textbuf, which has no PTY runner in D2a
    assert!(err(prepare(&req(&[]), &fx.env(&nocurses), true)).contains("text-buffer"));
    assert!(err(prepare(&req(&["--backend", "qemu-curses"]), &fx.env(&nocurses), true)).contains("curses"));
    assert!(err(prepare(&req(&["--backend", "qemu-textbuf"]), &fx.env(&good), true)).contains("text-buffer"));
    assert!(err(prepare(&req(&["--backend", "bogus"]), &fx.env(&good), true)).contains("unknown backend"));
    // missing / unsafe image names never reach the network or a process
    assert!(err(prepare(&req(&["--name", "nope"]), &fx.env(&good), true)).contains("dos setup"));
    assert!(prepare(&req(&["--name", "../evil"]), &fx.env(&good), true).is_err());
    assert!(err(prepare(&req(&["--image", "/definitely/not/here.img"]), &fx.env(&good), true)).contains("--image"));
    assert!(err(prepare(&req(&["--drive-c", "/definitely/not/here"]), &fx.env(&good), true)).contains("--drive-c"));
    let file = fx.dir.path().join("f");
    fs::write(&file, b"x").unwrap();
    assert!(err(prepare(&req(&["--drive-c", file.to_str().unwrap()]), &fx.env(&good), true)).contains("not a directory"));
    assert!(err(prepare(&req(&["--image", file.to_str().unwrap()]), &fx.env(&good), true)).contains("sector"));
    // refusals leave no scratch dirs, locks, or markers behind
    assert_eq!(fs::read_dir(fx.paths.sessions_dir()).unwrap().count(), 0);
    assert!(!fx.paths.c_dir().join("default.lock").exists() && !fx.paths.c_dir().join("default.dirty").exists());
}

#[test]
fn print_argv_is_dry() {
    let fx = fx();
    let probe = FakeProbe::good();
    let mut out = String::new();
    let sp = FakeSpawner::new(Ok(true));
    assert_eq!(cli_run(&req(&[]), true, &fx.env(&probe), &sp, &mut out), 0);
    assert!(out.contains("qemu-system-i386") && out.contains("-display curses"), "{out}");
    assert!(sp.calls.borrow().is_empty());
    assert!(!fx.paths.c_dir().join("default.dirty").exists());
    assert!(!fx.paths.c_dir().join("default.lock").exists());
    assert_eq!(fs::read_dir(fx.paths.sessions_dir()).unwrap().count(), 0, "work dir cleaned");
}

#[test]
fn print_argv_ignores_small_terminal() {
    let fx = fx();
    let probe = FakeProbe::good();
    let mut env = fx.env(&probe);
    env.term_size = Some((80, 24));
    let sp = FakeSpawner::new(Ok(true));
    let mut out = String::new();
    assert_eq!(cli_run(&req(&[]), true, &env, &sp, &mut out), 0, "{out}");
    let mut out = String::new();
    assert_eq!(cli_run(&req(&[]), false, &env, &sp, &mut out), 1);
    assert!(out.contains("terminal is 80x24"), "{out}");
}

struct FakeSpawner {
    result: RefCell<Option<std::io::Result<bool>>>,
    calls: RefCell<Vec<LaunchPlan>>,
    /// Marker/lock state observed while the "emulator" runs.
    seen: RefCell<Vec<(bool, bool)>>,
    c_dir: RefCell<Option<PathBuf>>,
}

impl FakeSpawner {
    fn new(r: std::io::Result<bool>) -> Self {
        Self { result: RefCell::new(Some(r)), calls: RefCell::default(), seen: RefCell::default(), c_dir: RefCell::default() }
    }
}

impl Spawner for FakeSpawner {
    fn run(&self, plan: &LaunchPlan) -> std::io::Result<bool> {
        self.calls.borrow_mut().push(plan.clone());
        if let Some(c) = &*self.c_dir.borrow() {
            self.seen.borrow_mut().push((c.join("default.dirty").exists(), c.join("default.lock").exists()));
        }
        self.result.borrow_mut().take().unwrap()
    }
}

#[test]
fn clean_exit_clears_marker_and_releases_lock() {
    let fx = fx();
    let probe = FakeProbe::good();
    let sp = FakeSpawner::new(Ok(true));
    *sp.c_dir.borrow_mut() = Some(fx.paths.c_dir());
    let mut out = String::new();
    assert_eq!(cli_run(&req(&[]), false, &fx.env(&probe), &sp, &mut out), 0, "{out}");
    assert_eq!(*sp.seen.borrow(), vec![(true, true)], "marker + lock held while running");
    assert!(!fx.paths.c_dir().join("default.dirty").exists());
    assert!(!fx.paths.c_dir().join("default.lock").exists());
    assert_eq!(fs::read_dir(fx.paths.sessions_dir()).unwrap().count(), 0);
    assert_eq!(sp.calls.borrow()[0].env, vec![], "TERM and UTF-8 locale already fine");
}

#[test]
fn abnormal_exit_keeps_marker_but_releases_lock_and_next_start_checks() {
    let fx = fx();
    let probe = FakeProbe::good();
    let sp = FakeSpawner::new(Ok(false));
    let mut out = String::new();
    assert_eq!(cli_run(&req(&[]), false, &fx.env(&probe), &sp, &mut out), 1);
    assert!(out.contains("dirty marker"), "{out}");
    assert!(fx.paths.c_dir().join("default.dirty").exists());
    assert!(!fx.paths.c_dir().join("default.lock").exists());
    // the next start sees a dirty image and runs the sanity pass (zeros are not a valid disk)
    let e = err(prepare(&req(&[]), &fx.env(&probe), false));
    assert!(e.contains("sanity") || e.contains("restore") || e.contains("reset"), "{e}");
    assert!(!fx.paths.c_dir().join("default.lock").exists(), "refusal releases the lock");
}

#[test]
fn spawn_failure_reports_and_releases() {
    let fx = fx();
    let probe = FakeProbe::good();
    let sp = FakeSpawner::new(Err(std::io::Error::other("boom")));
    let mut out = String::new();
    assert_eq!(cli_run(&req(&[]), false, &fx.env(&probe), &sp, &mut out), 1);
    assert!(out.contains("cannot start the emulator"), "{out}");
    assert!(!fx.paths.c_dir().join("default.lock").exists());
}

#[test]
fn second_writer_is_refused_while_running() {
    let fx = fx();
    let probe = FakeProbe::good();
    let first = prepare(&req(&[]), &fx.env(&probe), false).unwrap();
    let mut other = FakeProcs::new(11);
    other.procs.insert(10, "100".into());
    let mut env = fx.env(&probe);
    env.procs = &other;
    let e = err(prepare(&req(&[]), &env, false));
    assert!(e.contains("in use"), "{e}");
    // a different image is independent
    let w = prepare(&req(&["--name", "work"]), &env, false).unwrap();
    w.finish(true).unwrap();
    first.finish(true).unwrap();
}

#[test]
fn throwaway_run_takes_no_lock_and_never_marks_a_persistent_image() {
    let fx = fx();
    let probe = FakeProbe::good();
    let img = fx.dir.path().join("x.img");
    fs::write(&img, vec![0u8; 4096]).unwrap();
    let r = RunRequest { image: Some(img), ..RunRequest::default() };
    let prep = prepare(&r, &fx.env(&probe), false).unwrap();
    assert!(prep.plan.argv.iter().any(|a| a.contains("snapshot=on")));
    assert!(!fx.paths.c_dir().join("default.lock").exists());
    prep.finish(true).unwrap();
}

#[test]
fn term_env_rules() {
    let e = |t: Option<&str>, a: Option<&str>, c: Option<&str>, l: Option<&str>| {
        TermEnv { term: t.map(Into::into), lc_all: a.map(Into::into), lc_ctype: c.map(Into::into), lang: l.map(Into::into) }
            .emulator_env()
    };
    let fixed = |t: &str, l: &str| vec![("TERM".to_string(), t.to_string()), ("LC_ALL".to_string(), l.to_string())];
    assert_eq!(e(None, None, None, None), fixed("xterm-256color", "C.UTF-8"));
    assert_eq!(e(Some("dumb"), None, None, Some("C")), fixed("xterm-256color", "C.UTF-8"));
    assert_eq!(e(Some("screen"), None, None, Some("en_US.UTF-8")), vec![]);
    // LC_ALL wins over LANG
    assert_eq!(e(Some("xterm"), Some("C"), None, Some("en_US.UTF-8")), vec![("LC_ALL".to_string(), "C.UTF-8".to_string())]);
    assert_eq!(e(Some("xterm"), None, Some("en_US.utf8"), Some("C")), vec![]);
}

#[test]
fn render_command_quotes_for_display() {
    let p = LaunchPlan {
        kind: LaunchKind::Pty,
        program: "/opt/q emu/qemu".into(),
        argv: vec!["-name".into(), "it's".into(), "".into()],
        env: vec![("TERM".into(), "xterm-256color".into())],
        control: crate::dos::backend::ControlEndpoint::None,
    };
    assert_eq!(render_command(&p), "TERM=xterm-256color '/opt/q emu/qemu' -name 'it'\\''s' ''");
}

#[test]
fn registry_rows_construct_pty_backends() {
    for (kind, ctor) in REGISTRY {
        let d = tempfile::tempdir().unwrap();
        let b = ctor(&BackendCtx { program: "/q".into(), base: d.path().to_path_buf() });
        assert_eq!(b.id(), kind.id());
        assert!(b.caps().text_pane_native);
    }
}

/// Real QEMU in a real PTY (`cargo test --features dos -- --ignored dos_curses_smoke`). Boots a
/// blank disk, so the screen shows QEMU's own "no bootable device" text; proves the argv, curses
/// and the PTY wiring, not FreeDOS.
#[test]
#[ignore = "needs qemu-system-i386 with curses"]
fn dos_curses_smoke() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read;
    let fx = fx();
    let probe = crate::dos::doctor::SystemProbe::new("qemu-system-i386");
    let stub = FakeProbe::good();
    let mut env = fx.env(&stub);
    env.probe = &probe;
    let img = fx.dir.path().join("blank.img");
    fs::write(&img, vec![0u8; 1 << 20]).unwrap();
    let r = RunRequest { image: Some(img), ..RunRequest::default() };
    let prep = prepare(&r, &env, true).unwrap();
    let pair = native_pty_system().openpty(PtySize { rows: 25, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(&prep.plan.program);
    cmd.args(&prep.plan.argv);
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut rd = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = rd.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Ok(b) = rx.recv_timeout(std::time::Duration::from_millis(200)) {
            seen.extend(b);
        }
        if String::from_utf8_lossy(&seen).contains("Booting from") || String::from_utf8_lossy(&seen).contains("No bootable") {
            break;
        }
    }
    let _ = child.kill();
    let text = String::from_utf8_lossy(&seen).to_string();
    assert!(text.contains("Booting from") || text.contains("No bootable"), "no SeaBIOS text in curses output: {text:?}");
}
