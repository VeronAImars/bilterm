//! `bilterm dos run` core (PR D2a): resolve a run request into a [`LaunchPlan`], enforce the
//! safety defaults, take the C: image lock and dirty marker, run the emulator in the foreground
//! of the current terminal, and release everything on exit.
//!
//! The same code serves two entry points, so there is exactly one path:
//!
//! * `bilterm dos run ...` in a shell runs QEMU `-display curses` directly in that terminal.
//! * `Action::DosRun` in a running bilterm opens an ordinary PTY pane whose program is
//!   `bilterm dos run ...` (see [`pane_argv`]); the pane therefore handles QEMU exit (clear
//!   `.dirty` on a clean exit, release the lock) without any window-manager exit hook, and a
//!   killed pane leaves the marker exactly like a crash does.
//!
//! Backends are looked up in [`REGISTRY`]: one file plus one row per backend (plan section 4.3).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::backend::qemu::{QemuBackend, QemuMode};
use super::backend::{
    BackendKind, DosBackend, GUEST_COLS, GUEST_ROWS, LaunchKind, LaunchPlan,
    Probe, SessionSpec, detect, probe_qemu,
};
use super::doctor::EmulatorProbe;
use super::image::lock::ProcInfo;
use super::image::run::{RunGuard, RunOpts, StartReport, begin_run};
use super::image::store::{ImageStore, valid_name};
use super::image::{DEFAULT_NAME, ImageError};
use super::manifest::Manifest;
use super::paths::{DosPaths, HostOs};
use crate::secure_fs;

/// Smallest terminal/pane that shows the whole guest text screen (QEMU curses draws the 80x25
/// VGA text mode 1:1 and needs no extra border rows).
pub const MIN_COLS: u16 = GUEST_COLS;
pub const MIN_ROWS: u16 = GUEST_ROWS;

pub const RUN_USAGE: &str = "usage: bilterm dos run [--name N | --image FILE] [--drive-c DIR] [--backend qemu-curses] [--print-argv]\n  \
     --name N        persistent C: image (default \"default\"); single writer, lock + dirty marker\n  \
     --image FILE    throwaway run of your own raw disk image (writes are discarded)\n  \
     --drive-c DIR   expose host DIR read-only (QEMU vvfat) as the drive after C:\n  \
     --print-argv    print the emulator command line and exit; nothing is started or locked\n";

/// A parsed `dos run` request (also the payload of `Action::DosRun`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunRequest {
    /// Persistent image name; `None` means [`DEFAULT_NAME`] unless `image` is given.
    pub name: Option<String>,
    /// User-supplied raw image, attached throwaway (`snapshot=on`).
    pub image: Option<PathBuf>,
    /// Host folder exposed read-only through vvfat.
    pub drive_c: Option<PathBuf>,
    /// Backend id (`qemu-curses`); `None` picks by the per-OS order.
    pub backend: Option<String>,
}

impl RunRequest {
    /// Parses the arguments after `dos run`. Returns the request and whether `--print-argv` was set.
    pub fn parse(args: &[String]) -> Result<(Self, bool), String> {
        let mut r = Self::default();
        let mut print = false;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = |what: &str| -> Result<String, String> {
                it.next().cloned().ok_or_else(|| format!("{a} needs {what}\n{RUN_USAGE}"))
            };
            match a.as_str() {
                "--name" => r.name = Some(val("a name")?),
                "--image" => r.image = Some(PathBuf::from(val("a file")?)),
                "--drive-c" => r.drive_c = Some(PathBuf::from(val("a directory")?)),
                "--backend" => r.backend = Some(val("a backend id")?),
                "--print-argv" => print = true,
                other => return Err(format!("unknown option {other:?}\n{RUN_USAGE}")),
            }
        }
        if r.name.is_some() && r.image.is_some() {
            return Err(format!("--name and --image are mutually exclusive\n{RUN_USAGE}"));
        }
        Ok((r, print))
    }
}

/// Process environment facts used to pick `TERM`/locale, injectable for tests.
#[derive(Clone, Debug, Default)]
pub struct TermEnv {
    pub term: Option<String>,
    pub lc_all: Option<String>,
    pub lc_ctype: Option<String>,
    pub lang: Option<String>,
}

impl TermEnv {
    pub fn from_process() -> Self {
        let g = |k: &str| std::env::var(k).ok();
        Self { term: g("TERM"), lc_all: g("LC_ALL"), lc_ctype: g("LC_CTYPE"), lang: g("LANG") }
    }

    /// Environment additions for the emulator. QEMU's curses UI maps CP437 box drawing through
    /// ncursesw, which needs a UTF-8 locale; a missing or `dumb` TERM would break curses at start.
    pub fn emulator_env(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        match self.term.as_deref() {
            Some(t) if !t.trim().is_empty() && t != "dumb" && t != "unknown" => {}
            _ => out.push(("TERM".to_string(), "xterm-256color".to_string())),
        }
        // POSIX precedence: LC_ALL, then LC_CTYPE, then LANG.
        let effective = [&self.lc_all, &self.lc_ctype, &self.lang]
            .into_iter()
            .flatten()
            .find(|v| !v.is_empty());
        let utf8 = effective.is_some_and(|v| {
            let u = v.to_ascii_lowercase();
            u.contains("utf-8") || u.contains("utf8")
        });
        if !utf8 {
            out.push(("LC_ALL".to_string(), "C.UTF-8".to_string()));
        }
        out
    }
}

/// Everything a run needs from the host, injectable for tests.
pub struct RunEnv<'a> {
    pub os: HostOs,
    pub paths: &'a DosPaths,
    pub manifest: &'a Manifest,
    pub probe: &'a dyn EmulatorProbe,
    pub procs: &'a dyn ProcInfo,
    pub is_root: bool,
    /// Size of the terminal/pane the emulator will draw into; `None` when unknown (not a tty).
    pub term_size: Option<(u16, u16)>,
    pub term_env: TermEnv,
}

/// Inputs for constructing a registered backend.
pub struct BackendCtx {
    pub program: PathBuf,
    /// Private directory below which the backend creates its per-session dir.
    pub base: PathBuf,
}

type Ctor = fn(&BackendCtx) -> Box<dyn DosBackend>;

fn make_qemu_curses(c: &BackendCtx) -> Box<dyn DosBackend> {
    Box::new(QemuBackend::new(QemuMode::Curses, c.program.clone(), c.base.clone()))
}

/// Backends `dos run` can start as an ordinary PTY (one row per backend; plan section 4.3).
pub const REGISTRY: &[(BackendKind, Ctor)] = &[(BackendKind::QemuCurses, make_qemu_curses)];

/// Maps `--backend` text to a kind. Unknown ids are an error, not a silent fallback.
pub fn parse_backend_id(id: &str) -> Result<BackendKind, String> {
    match id {
        "qemu-curses" => Ok(BackendKind::QemuCurses),
        "qemu-textbuf" => Ok(BackendKind::QemuTextbuf),
        other => Err(format!("unknown backend {other:?} (known: qemu-curses, qemu-textbuf)")),
    }
}

#[derive(Debug)]
pub enum RunError {
    Refused(String),
    Image(ImageError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(s) => f.write_str(s),
            Self::Image(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<ImageError> for RunError {
    fn from(e: ImageError) -> Self {
        Self::Image(e)
    }
}

fn refuse<T>(s: impl Into<String>) -> Result<T, RunError> {
    Err(RunError::Refused(s.into()))
}

/// Result of every check that has no side effects.
#[derive(Debug)]
pub struct Resolved {
    pub kind: BackendKind,
    pub program: PathBuf,
    pub spec: SessionSpec,
    /// `Some(name)` when the persistent image (lock + dirty marker) is in play.
    pub persistent: Option<String>,
    pub host_folder: Option<PathBuf>,
    pub title: String,
}

/// A run ready to start: the plan, plus the held lock/marker for a persistent image.
pub struct PreparedRun {
    pub plan: LaunchPlan,
    pub title: String,
    pub notes: Vec<String>,
    pub report: Option<StartReport>,
    guard: Option<RunGuard>,
    work_dir: PathBuf,
    /// Kept alive for the whole run: dropping a QEMU backend removes its session dir, which holds
    /// the QMP socket the emulator binds at startup.
    _backend: Box<dyn DosBackend>,
}

impl std::fmt::Debug for PreparedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRun").field("plan", &self.plan).field("title", &self.title).finish_non_exhaustive()
    }
}

impl PreparedRun {
    /// Ends the run. A clean emulator exit clears the dirty marker; any other outcome leaves it so
    /// the next start runs the sanity check. The lock is always released.
    pub fn finish(mut self, clean_exit: bool) -> Result<(), ImageError> {
        let guard = self.guard.take();
        let _ = std::fs::remove_dir_all(&self.work_dir);
        match guard {
            Some(g) if clean_exit => g.finish_clean(),
            _ => Ok(()), // dropping the guard releases the lock; the marker stays
        }
    }
}

impl Drop for PreparedRun {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

/// Checks that every plan the host would run keeps the safety defaults (plan section 5): no
/// network, no USB, no host audio, no host file sharing, no extra control channels.
pub fn check_plan_safety(plan: &LaunchPlan) -> Result<(), String> {
    let a = &plan.argv;
    let pairs = |flag: &str| -> Vec<&str> {
        a.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()).collect()
    };
    if !pairs("-nic").iter().any(|v| *v == "none") {
        return Err("plan lacks `-nic none`".into());
    }
    for bad in [
        "-net", "-netdev", "-usb", "-usbdevice", "-audiodev", "-audio", "-soundhw", "-virtfs",
        "-fsdev", "-smb", "-redir", "-chardev", "-device", "-serial", "-parallel", "-monitor",
        "-vnc", "-spice", "-incoming", "-S", "-gdb", "-s", "-kernel", "-initrd", "-append",
    ] {
        match bad {
            // The backend turns these off explicitly with `none`; anything else is a leak.
            "-serial" | "-parallel" | "-monitor" => {
                if pairs(bad).iter().any(|v| *v != "none") {
                    return Err(format!("plan enables {bad}"));
                }
            }
            _ => {
                if a.contains(&bad.to_string()) {
                    return Err(format!("plan contains forbidden option {bad}"));
                }
            }
        }
    }
    let qmp_ok = pairs("-qmp").iter().all(|v| v.starts_with("unix:") || v.starts_with("tcp:127.0.0.1:"));
    if !qmp_ok {
        return Err("QMP endpoint is not loopback/unix".into());
    }
    Ok(())
}

fn validate_host_folder(dir: &Path) -> Result<PathBuf, RunError> {
    let canon = std::fs::canonicalize(dir)
        .map_err(|e| RunError::Refused(format!("--drive-c {}: {e}", dir.display())))?;
    if !std::fs::metadata(&canon).is_ok_and(|m| m.is_dir()) {
        return refuse(format!("--drive-c {}: not a directory", dir.display()));
    }
    let s = canon.to_str().ok_or_else(|| RunError::Refused("--drive-c: path is not valid UTF-8".into()))?;
    if s.contains(['\0', '\n', '\r']) {
        return refuse("--drive-c: unusable path");
    }
    Ok(canon)
}

fn validate_image_file(p: &Path) -> Result<PathBuf, RunError> {
    let canon = std::fs::canonicalize(p)
        .map_err(|e| RunError::Refused(format!("--image {}: {e}", p.display())))?;
    let md = std::fs::metadata(&canon).map_err(|e| RunError::Refused(format!("--image: {e}")))?;
    if !md.is_file() {
        return refuse(format!("--image {}: not a regular file", p.display()));
    }
    if md.len() < 512 {
        return refuse(format!("--image {}: smaller than one sector", p.display()));
    }
    Ok(canon)
}

/// Appends the read-only vvfat host folder drive after the IDE drives already in `spec`.
pub fn append_host_folder(argv: &mut Vec<String>, spec: &SessionSpec, dir: &Path) -> Result<(), String> {
    let used = usize::from(spec.c_image.is_some()) + spec.extra_drives.len();
    if used >= 4 {
        return Err("no free IDE slot for the host folder".into());
    }
    let d = dir.to_str().ok_or("host folder path is not valid UTF-8")?;
    argv.push("-drive".into());
    argv.push(format!(
        "file=fat:ro:{},format=raw,if=ide,index={used},readonly=on",
        super::backend::qemu::escape_value(d)
    ));
    Ok(())
}

/// All checks that need no side effects: root, OS, terminal size, emulator, backend, image.
pub fn resolve(req: &RunRequest, env: &RunEnv) -> Result<Resolved, RunError> {
    if env.is_root {
        return refuse("refusing to run as root: DOS mode must not run privileged");
    }
    if env.os == HostOs::Other {
        return refuse("DOS mode is unsupported on this OS; see `bilterm dos doctor`");
    }
    if let Some((c, r)) = env.term_size
        && (c < MIN_COLS || r < MIN_ROWS)
    {
        return refuse(format!(
            "terminal is {c}x{r}; the DOS screen needs at least {MIN_COLS}x{MIN_ROWS} (enlarge or zoom the window)"
        ));
    }
    let want = req.backend.as_deref().map(parse_backend_id).transpose().map_err(RunError::Refused)?;
    let program = env.probe.locate().ok_or_else(|| {
        RunError::Refused("qemu-system-i386 not found; run `bilterm dos doctor` for install guidance".into())
    })?;
    let probe: Probe = probe_qemu(env.probe, env.manifest.min_version());
    if !probe.usable() {
        return refuse(format!("emulator unusable: {}", probe.detail));
    }
    let kind = match want {
        Some(k) if k == BackendKind::QemuCurses && !probe.curses => {
            return refuse("this QEMU build has no `-display curses` (see `bilterm dos doctor`)");
        }
        Some(k) => k,
        None => detect(env.os, &probe, None)
            .ok_or_else(|| RunError::Refused("no usable backend for this host (see `bilterm dos doctor`)".into()))?,
    };
    if !REGISTRY.iter().any(|(k, _)| *k == kind) {
        return refuse(format!(
            "backend {} cannot run as a plain PTY pane yet (it needs the text-buffer pane bridge); use --backend qemu-curses",
            kind.id()
        ));
    }
    let store = ImageStore::new(env.paths.clone());
    let mut spec = SessionSpec::default();
    let (persistent, label) = match &req.image {
        Some(f) => {
            spec.c_image = Some(validate_image_file(f)?);
            spec.throwaway = true;
            (None, format!("{} (throwaway)", f.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())))
        }
        None => {
            let name = req.name.clone().unwrap_or_else(|| DEFAULT_NAME.to_string());
            valid_name(&name, "image name")?;
            if !store.exists(&name)? {
                return refuse(format!(
                    "no C: image {name:?}: run `bilterm dos setup` first (DOS mode never downloads at run time)"
                ));
            }
            spec.c_image = Some(store.image_path(&name)?);
            (Some(name.clone()), name)
        }
    };
    let host_folder = req.drive_c.as_deref().map(validate_host_folder).transpose()?;
    Ok(Resolved {
        kind,
        program,
        spec,
        persistent,
        host_folder,
        title: format!("DOS:{label}"),
    })
}

/// `dry` skips the lock/marker/snapshot side effects (`--print-argv`).
pub fn prepare(req: &RunRequest, env: &RunEnv, dry: bool) -> Result<PreparedRun, RunError> {
    let r = resolve(req, env)?;
    let store = ImageStore::new(env.paths.clone());
    let (guard, report) = match (&r.persistent, dry) {
        (Some(name), false) => {
            let (g, rep) = begin_run(&store, name, env.procs, &RunOpts { allow_corrupt: false, fsck: None })?;
            (Some(g), Some(rep))
        }
        _ => (None, None),
    };
    match build_plan(&r, env) {
        Ok((plan, work_dir, backend)) => {
            let mut notes = Vec::new();
            if let Some(rep) = &report {
                notes.extend(rep.notes.iter().cloned());
            }
            Ok(PreparedRun { plan, title: r.title, notes, report, guard, work_dir, _backend: backend })
        }
        // The guard drops here: lock released, marker left (the emulator never started, but the
        // marker only costs one read-only sanity pass next time).
        Err(e) => Err(e),
    }
}

type Built = (LaunchPlan, PathBuf, Box<dyn DosBackend>);

fn build_plan(r: &Resolved, env: &RunEnv) -> Result<Built, RunError> {
    let sessions = env.paths.sessions_dir();
    secure_fs::create_private_dir_all(&sessions).map_err(ImageError::from)?;
    let work_dir = secure_fs::private_dir_in(&sessions, &format!("run-{}", super::backend::random_hex(8)))
        .map_err(ImageError::from)?;
    let go = || -> Result<(LaunchPlan, Box<dyn DosBackend>), RunError> {
        let ctor = REGISTRY
            .iter()
            .find(|(k, _)| *k == r.kind)
            .map(|(_, c)| *c)
            .ok_or_else(|| RunError::Refused("backend not registered".into()))?;
        let mut backend = ctor(&BackendCtx { program: r.program.clone(), base: work_dir.clone() });
        let mut plan = backend.plan(&r.spec).map_err(|e| RunError::Refused(e.to_string()))?;
        if plan.kind != LaunchKind::Pty {
            return refuse("backend did not produce a PTY plan");
        }
        if let Some(dir) = &r.host_folder {
            append_host_folder(&mut plan.argv, &r.spec, dir).map_err(RunError::Refused)?;
        }
        plan.env.extend(env.term_env.emulator_env());
        check_plan_safety(&plan).map_err(RunError::Refused)?;
        Ok((plan, backend))
    };
    match go() {
        Ok((p, b)) => Ok((p, work_dir, b)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work_dir);
            Err(e)
        }
    }
}

/// Runs the emulator; production uses [`SystemSpawner`], tests a fake.
pub trait Spawner {
    /// Blocks until the emulator exits. `Ok(true)` means a clean (zero) exit.
    fn run(&self, plan: &LaunchPlan) -> std::io::Result<bool>;
}

/// Inherits the current terminal (stdin/stdout/stderr); never uses a shell.
pub struct SystemSpawner;

impl Spawner for SystemSpawner {
    fn run(&self, plan: &LaunchPlan) -> std::io::Result<bool> {
        let st = Command::new(&plan.program).args(&plan.argv).envs(plan.env.iter().cloned()).status()?;
        Ok(st.success())
    }
}

/// Foreground run: returns the process exit code for `bilterm dos run`.
pub fn run_foreground(prep: PreparedRun, spawner: &dyn Spawner, out: &mut String) -> i32 {
    for n in &prep.notes {
        let _ = writeln!(out, "note: {n}");
    }
    let outcome = spawner.run(&prep.plan);
    let clean = matches!(outcome, Ok(true));
    let fin = prep.finish(clean);
    let mut code = if clean { 0 } else { 1 };
    match outcome {
        Ok(true) => {}
        Ok(false) => out.push_str("emulator exited abnormally; the image keeps its dirty marker (checked on next start)\n"),
        Err(e) => {
            let _ = writeln!(out, "cannot start the emulator: {e}");
            code = 1;
        }
    }
    if let Err(e) = fin {
        let _ = writeln!(out, "cleanup failed: {e}");
        code = 1;
    }
    code
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./:=,+@%".contains(&b)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// One display line for `--print-argv` (informational; bilterm itself never uses a shell).
pub fn render_command(plan: &LaunchPlan) -> String {
    let mut s = String::new();
    for (k, v) in &plan.env {
        let _ = write!(s, "{k}={} ", shell_quote(v));
    }
    s.push_str(&shell_quote(&plan.program.to_string_lossy()));
    for a in &plan.argv {
        s.push(' ');
        s.push_str(&shell_quote(a));
    }
    s
}

/// CLI entry: `--print-argv` prints and exits 0; otherwise runs in the foreground.
pub fn cli_run(req: &RunRequest, print_argv: bool, env: &RunEnv, spawner: &dyn Spawner, out: &mut String) -> i32 {
    match prepare(req, env, print_argv) {
        Err(e) => {
            let _ = writeln!(out, "{e}");
            1
        }
        Ok(prep) if print_argv => {
            let _ = writeln!(out, "{}", render_command(&prep.plan));
            0
        }
        Ok(prep) => run_foreground(prep, spawner, out),
    }
}

/// Argv (after the program) for the PTY pane that `Action::DosRun` opens: this same executable
/// running `dos run`, so the pane gets lock/dirty/cleanup handling for free.
pub fn pane_argv(req: &RunRequest) -> Vec<String> {
    let mut v = vec!["dos".to_string(), "run".to_string()];
    if let Some(n) = &req.name {
        v.extend(["--name".into(), n.clone()]);
    }
    if let Some(i) = &req.image {
        v.extend(["--image".into(), i.to_string_lossy().into_owned()]);
    }
    if let Some(d) = &req.drive_c {
        v.extend(["--drive-c".into(), d.to_string_lossy().into_owned()]);
    }
    if let Some(b) = &req.backend {
        v.extend(["--backend".into(), b.clone()]);
    }
    v
}

/// Side-effect free validation for callers that only need to know "would this run start".
pub fn preflight(req: &RunRequest, env: &RunEnv) -> Result<Resolved, RunError> {
    resolve(req, env)
}

/// Builds a [`RunEnv`] for the real host (used by the CLI and by `Action::DosRun`).
pub fn with_host_env<R>(
    term_size: Option<(u16, u16)>,
    f: impl FnOnce(&RunEnv) -> R,
) -> Result<R, RunError> {
    let manifest = Manifest::embedded().map_err(|e| RunError::Refused(e.to_string()))?;
    let paths = DosPaths::from_process()
        .ok_or_else(|| RunError::Refused("cannot determine the per-user data directory".into()))?;
    let probe = super::doctor::SystemProbe::new(&manifest.emulator.name);
    let env = RunEnv {
        os: HostOs::current(),
        paths: &paths,
        manifest: &manifest,
        probe: &probe,
        procs: &super::image::lock::SysProc,
        is_root: cfg!(unix) && secure_fs::current_euid() == 0,
        term_size,
        term_env: TermEnv::from_process(),
    };
    Ok(f(&env))
}

#[cfg(test)]
pub(crate) mod tests;
