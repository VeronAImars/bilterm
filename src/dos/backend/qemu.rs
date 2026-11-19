//! QEMU backend. Argv construction is pure ([`build_argv`]); processes are spawned with
//! `Command::args` only, never through a shell. Control is QMP on a unix socket inside a 0700
//! per-session directory where possible, else TCP on 127.0.0.1 with an OS-assigned port.

use std::fs::File;
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::qmp::{Conn, Qmp};
use super::screen::{BDA_ADDR, BDA_LEN, TEXT_BUF_ADDR, TEXT_BUF_LEN, TextScreen};
use super::{
    BackendCaps, BackendError, ControlEndpoint, DosBackend, DosKey, LaunchKind, LaunchPlan, Result,
    SessionSpec, random_hex,
};
use crate::secure_fs;

pub const PROGRAM: &str = "qemu-system-i386";
const MEM_RANGE: std::ops::RangeInclusive<u32> = 8..=512;
const MAX_EXTRA_DRIVES: usize = 3;
const MAX_KEYS_PER_CALL: usize = 4096;
/// `sun_path` is 104 bytes on macOS and 108 on Linux; stay below both.
const MAX_UNIX_PATH: usize = 100;
const ATTACH_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QemuMode {
    /// `-display curses`; the host runs the plan in a PTY pane.
    Curses,
    /// `-display none`; bilterm polls the VGA text buffer over QMP.
    TextBuf,
}

/// QEMU option values treat `,` as a separator; a literal comma is written `,,`.
pub fn escape_value(s: &str) -> String {
    s.replace(',', ",,")
}

fn path_arg(p: &Path, what: &str) -> Result<String> {
    let s = p
        .to_str()
        .ok_or_else(|| BackendError::Invalid(format!("{what}: path is not valid UTF-8")))?;
    if s.is_empty() || s.contains('\0') || s.contains('\n') {
        return Err(BackendError::Invalid(format!("{what}: unusable path")));
    }
    if !p.is_absolute() {
        return Err(BackendError::Invalid(format!(
            "{what}: path must be absolute"
        )));
    }
    Ok(escape_value(s))
}

fn drive(file: &str, iface: &str, index: usize, extra: &str) -> Vec<String> {
    vec![
        "-drive".into(),
        format!("file={file},format=raw,if={iface},index={index}{extra}"),
    ]
}

fn qmp_arg(ep: &ControlEndpoint) -> Result<String> {
    match ep {
        ControlEndpoint::Tcp { port } => Ok(format!(
            "tcp:{}:{port},server=on,wait=off",
            Ipv4Addr::LOCALHOST
        )),
        ControlEndpoint::Unix { path } => Ok(format!(
            "unix:{},server=on,wait=off",
            path_arg(path, "qmp socket")?
        )),
        ControlEndpoint::None => Err(BackendError::Invalid("no control endpoint".into())),
    }
}

/// Pure command-line construction (arguments only, program excluded).
pub fn build_argv(mode: QemuMode, ep: &ControlEndpoint, spec: &SessionSpec) -> Result<Vec<String>> {
    if !MEM_RANGE.contains(&spec.mem_mb) {
        return Err(BackendError::Invalid(format!(
            "memory {} MiB outside {}..={}",
            spec.mem_mb,
            MEM_RANGE.start(),
            MEM_RANGE.end()
        )));
    }
    if spec.extra_drives.len() > MAX_EXTRA_DRIVES {
        return Err(BackendError::Invalid(format!(
            "at most {MAX_EXTRA_DRIVES} extra drives"
        )));
    }
    if spec.c_image.is_none() && spec.floppy.is_none() {
        return Err(BackendError::Invalid(
            "need a C: image or a floppy to boot".into(),
        ));
    }
    let ro = if spec.readonly_media {
        ",readonly=on"
    } else {
        ""
    };
    let mut a: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
    push(&["-name", "bilterm-dos"]);
    push(&[
        "-display",
        if mode == QemuMode::Curses {
            "curses"
        } else {
            "none"
        },
    ]);
    push(&["-m", &spec.mem_mb.to_string()]);
    push(&[
        "-monitor",
        "none",
        "-serial",
        "none",
        "-parallel",
        "none",
        "-nic",
        "none",
    ]);
    a.extend(["-qmp".to_string(), qmp_arg(ep)?]);
    let mut idx = 0;
    if let Some(c) = &spec.c_image {
        let snap = if spec.throwaway { ",snapshot=on" } else { "" };
        a.extend(drive(&path_arg(c, "c_image")?, "ide", idx, snap));
        idx += 1;
    }
    for d in &spec.extra_drives {
        a.extend(drive(&path_arg(d, "extra drive")?, "ide", idx, ro));
        idx += 1;
    }
    if let Some(f) = &spec.floppy {
        a.extend(drive(&path_arg(f, "floppy")?, "floppy", 0, ro));
    }
    a.extend([
        "-boot".to_string(),
        format!("order={}", if spec.floppy.is_some() { "a" } else { "c" }),
    ]);
    Ok(a)
}

/// Chooses the control endpoint: unix socket in `dir` when the platform and path length allow,
/// else a loopback TCP port assigned by the OS (bound and released; QEMU rebinds it immediately).
pub fn allocate_endpoint(dir: &Path, prefer_unix: bool) -> std::io::Result<ControlEndpoint> {
    let sock = dir.join("qmp.sock");
    if prefer_unix && sock.as_os_str().len() <= MAX_UNIX_PATH {
        return Ok(ControlEndpoint::Unix { path: sock });
    }
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(ControlEndpoint::Tcp {
        port: l.local_addr()?.port(),
    })
}

pub struct QemuBackend {
    mode: QemuMode,
    program: PathBuf,
    /// Pre-existing private base (for example `secure_fs::private_runtime_dir()`).
    base: PathBuf,
    session_dir: Option<PathBuf>,
    endpoint: ControlEndpoint,
    child: Option<Child>,
    qmp: Option<Qmp<Conn, Conn>>,
    size: (u16, u16),
}

impl QemuBackend {
    pub fn new(mode: QemuMode, program: PathBuf, base: PathBuf) -> Self {
        Self {
            mode,
            program,
            base,
            session_dir: None,
            endpoint: ControlEndpoint::None,
            child: None,
            qmp: None,
            size: (super::GUEST_COLS, super::GUEST_ROWS),
        }
    }

    fn ensure_session_dir(&mut self) -> Result<PathBuf> {
        if let Some(d) = &self.session_dir {
            return Ok(d.clone());
        }
        let d = secure_fs::private_dir_in(&self.base, &format!("dos-{}", random_hex(8)))?;
        self.session_dir = Some(d.clone());
        Ok(d)
    }

    fn qmp_mut(&mut self) -> Result<&mut Qmp<Conn, Conn>> {
        self.qmp.as_mut().ok_or(BackendError::NotRunning)
    }

    fn dump(&mut self, addr: u64, len: usize, name: &str) -> Result<Vec<u8>> {
        let path = self
            .session_dir
            .as_ref()
            .ok_or(BackendError::NotRunning)?
            .join(name);
        self.qmp_mut()?.pmemsave(addr, len, &path)?;
        let mut buf = Vec::with_capacity(len);
        File::open(&path)?
            .take(len as u64 + 1)
            .read_to_end(&mut buf)?;
        if buf.len() != len {
            return Err(BackendError::Qmp(format!(
                "dump {name} was {} bytes, expected {len}",
                buf.len()
            )));
        }
        Ok(buf)
    }
}

impl DosBackend for QemuBackend {
    fn id(&self) -> &'static str {
        match self.mode {
            QemuMode::Curses => "qemu-curses",
            QemuMode::TextBuf => "qemu-textbuf",
        }
    }

    fn caps(&self) -> BackendCaps {
        BackendCaps {
            text_pane_native: self.mode == QemuMode::Curses,
            textbuf: true,
            graphics_dump: false,
            snapshots: false,
            host_folder: false,
        }
    }

    fn plan(&mut self, spec: &SessionSpec) -> Result<LaunchPlan> {
        let dir = self.ensure_session_dir()?;
        if self.endpoint == ControlEndpoint::None {
            self.endpoint = allocate_endpoint(&dir, cfg!(unix))?;
        }
        Ok(LaunchPlan {
            kind: if self.mode == QemuMode::Curses {
                LaunchKind::Pty
            } else {
                LaunchKind::Bridge
            },
            program: self.program.clone(),
            argv: build_argv(self.mode, &self.endpoint, spec)?,
            env: Vec::new(),
            control: self.endpoint.clone(),
        })
    }

    fn launch(&mut self, spec: &SessionSpec) -> Result<()> {
        if self.mode == QemuMode::Curses {
            return Err(BackendError::Unsupported(
                "curses mode runs in a PTY pane: run plan() there, then attach()",
            ));
        }
        if self.child.is_some() {
            return Err(BackendError::Process("already launched".into()));
        }
        self.size = (spec.size_cols, spec.size_rows);
        let plan = self.plan(spec)?;
        let child = Command::new(&plan.program)
            .args(&plan.argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| BackendError::Process(format!("spawn {}: {e}", plan.program.display())))?;
        self.child = Some(child);
        if let Err(e) = self.attach() {
            let _ = self.shutdown();
            return Err(e);
        }
        Ok(())
    }

    fn attach(&mut self) -> Result<()> {
        let ep = self.endpoint.clone();
        let start = Instant::now();
        let conn = loop {
            if let Some(c) = self.child.as_mut()
                && let Ok(Some(st)) = c.try_wait()
            {
                return Err(BackendError::Process(format!(
                    "emulator exited early: {st}"
                )));
            }
            match Conn::connect(&ep) {
                Ok(c) => break c,
                Err(e) if start.elapsed() >= ATTACH_WAIT => return Err(e.into()),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        self.qmp = Some(Qmp::open(conn)?);
        Ok(())
    }

    fn send_keys(&mut self, ks: &[DosKey]) -> Result<()> {
        if ks.len() > MAX_KEYS_PER_CALL {
            return Err(BackendError::Invalid("too many keys in one call".into()));
        }
        let chords = ks.iter().map(|k| k.chord()).collect::<Result<Vec<_>>>()?;
        let q = self.qmp_mut()?;
        for c in chords {
            q.send_key(&c)?;
        }
        Ok(())
    }

    fn read_text(&mut self) -> Result<TextScreen> {
        let raw = self.dump(TEXT_BUF_ADDR, TEXT_BUF_LEN, "vga.bin")?;
        let bda = self.dump(BDA_ADDR, BDA_LEN, "bda.bin")?;
        TextScreen::from_vga(&raw, Some(&bda))
    }

    fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        if cols == 0 || rows == 0 {
            return Err(BackendError::Invalid("zero-sized resize".into()));
        }
        // The guest screen is fixed 80x25; curses clips to the PTY size, which the pane
        // forwards itself (SIGWINCH). Only record the request.
        self.size = (cols, rows);
        Ok(())
    }

    fn snapshot(&mut self, _tag: &str) -> Result<()> {
        Err(BackendError::Unsupported("snapshots land in D1c/D5"))
    }

    fn restore(&mut self, _tag: &str) -> Result<()> {
        Err(BackendError::Unsupported("snapshots land in D1c/D5"))
    }

    fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => self.qmp.is_some(),
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(mut q) = self.qmp.take() {
            let _ = q.quit();
        }
        if let Some(mut c) = self.child.take() {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(2) {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if matches!(c.try_wait(), Ok(None)) {
                let _ = c.kill();
            }
            let _ = c.wait();
        }
        if let Some(d) = self.session_dir.take() {
            let _ = std::fs::remove_dir_all(d);
        }
        self.endpoint = ControlEndpoint::None;
        Ok(())
    }
}

impl Drop for QemuBackend {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SessionSpec {
        SessionSpec {
            c_image: Some("/d/c.img".into()),
            ..SessionSpec::default()
        }
    }
    fn tcp() -> ControlEndpoint {
        ControlEndpoint::Tcp { port: 4555 }
    }

    #[test]
    fn curses_argv_exact() {
        let a = build_argv(QemuMode::Curses, &tcp(), &spec()).unwrap();
        let want = [
            "-name",
            "bilterm-dos",
            "-display",
            "curses",
            "-m",
            "32",
            "-monitor",
            "none",
            "-serial",
            "none",
            "-parallel",
            "none",
            "-nic",
            "none",
            "-qmp",
            "tcp:127.0.0.1:4555,server=on,wait=off",
            "-drive",
            "file=/d/c.img,format=raw,if=ide,index=0",
            "-boot",
            "order=c",
        ];
        assert_eq!(a, want);
    }

    #[test]
    fn textbuf_floppy_unix_throwaway() {
        let mut s = spec();
        s.floppy = Some("/f/boot.img".into());
        s.throwaway = true;
        s.extra_drives = vec!["/x/d.img".into()];
        let ep = ControlEndpoint::Unix {
            path: "/run/u/dos-ab/qmp.sock".into(),
        };
        let a = build_argv(QemuMode::TextBuf, &ep, &s).unwrap();
        assert!(a.windows(2).any(|w| w == ["-display", "none"]));
        assert!(a.contains(&"unix:/run/u/dos-ab/qmp.sock,server=on,wait=off".to_string()));
        assert!(a.contains(&"file=/d/c.img,format=raw,if=ide,index=0,snapshot=on".to_string()));
        assert!(a.contains(&"file=/x/d.img,format=raw,if=ide,index=1,readonly=on".to_string()));
        assert!(
            a.contains(&"file=/f/boot.img,format=raw,if=floppy,index=0,readonly=on".to_string())
        );
        assert_eq!(a.last().unwrap(), "order=a");
    }

    #[test]
    fn commas_are_escaped_and_shell_metachars_stay_one_arg() {
        let mut s = spec();
        s.c_image = Some("/d/a,file=/etc/passwd;$(rm -rf ~) `x`.img".into());
        let a = build_argv(QemuMode::TextBuf, &tcp(), &s).unwrap();
        let d = a.iter().find(|x| x.starts_with("file=")).unwrap();
        assert_eq!(
            d,
            "file=/d/a,,file=/etc/passwd;$(rm -rf ~) `x`.img,format=raw,if=ide,index=0"
        );
        // exactly one -drive argument pair; the injection attempt created no new option.
        assert_eq!(a.iter().filter(|x| *x == "-drive").count(), 1);
        assert_eq!(escape_value("a,b,,c"), "a,,b,,,,c");
    }

    #[test]
    fn rejects_bad_specs() {
        let ok = |f: &dyn Fn(&mut SessionSpec)| {
            let mut s = spec();
            f(&mut s);
            build_argv(QemuMode::TextBuf, &tcp(), &s).is_err()
        };
        assert!(ok(&|s| s.mem_mb = 4));
        assert!(ok(&|s| s.mem_mb = 4096));
        assert!(ok(&|s| s.c_image = Some("rel/c.img".into())));
        assert!(ok(&|s| s.c_image = Some("/a\nb".into())));
        assert!(ok(&|s| s.extra_drives = vec!["/a".into(); 4]));
        assert!(ok(&|s| s.c_image = None));
        assert!(build_argv(QemuMode::TextBuf, &ControlEndpoint::None, &spec()).is_err());
    }

    #[test]
    fn endpoint_allocation() {
        let ep = allocate_endpoint(Path::new("/short"), true).unwrap();
        assert_eq!(
            ep,
            ControlEndpoint::Unix {
                path: "/short/qmp.sock".into()
            }
        );
        let long = PathBuf::from(format!("/{}", "a".repeat(120)));
        assert!(
            matches!(allocate_endpoint(&long, true).unwrap(), ControlEndpoint::Tcp { port } if port != 0)
        );
        assert!(matches!(
            allocate_endpoint(Path::new("/s"), false).unwrap(),
            ControlEndpoint::Tcp { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn plan_uses_private_session_dir_and_does_not_spawn() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let mut b = QemuBackend::new(
            QemuMode::Curses,
            "/nonexistent/qemu".into(),
            t.path().into(),
        );
        let p = b.plan(&spec()).unwrap();
        assert_eq!(p.kind, LaunchKind::Pty);
        assert!(!b.is_running());
        let ControlEndpoint::Unix { path } = &p.control else {
            panic!("unix endpoint")
        };
        let dir = path.parent().unwrap();
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // curses launch is refused (host owns the PTY); nothing is spawned.
        assert!(matches!(
            b.launch(&spec()),
            Err(BackendError::Unsupported(_))
        ));
        // stable endpoint on re-plan, cleanup on shutdown
        assert_eq!(b.plan(&spec()).unwrap().control, p.control);
        b.shutdown().unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn stubs_and_not_running() {
        let t = tempfile::tempdir().unwrap();
        let mut b = QemuBackend::new(QemuMode::TextBuf, PROGRAM.into(), t.path().into());
        assert!(matches!(b.snapshot("t"), Err(BackendError::Unsupported(_))));
        assert!(matches!(b.restore("t"), Err(BackendError::Unsupported(_))));
        assert!(matches!(
            b.send_keys(&[DosKey::Enter]),
            Err(BackendError::NotRunning)
        ));
        assert!(matches!(b.read_text(), Err(BackendError::NotRunning)));
        assert!(b.resize(0, 5).is_err());
        assert!(b.send_keys(&[DosKey::Char('é')]).is_err());
        assert_eq!(b.id(), "qemu-textbuf");
        let _ = crate::dos::backend::keys::keys_for_text("x");
    }

    /// Real boot smoke (no media needed: SeaBIOS banner). `BILTERM_DOS_SMOKE=1 cargo test --features dos -- --ignored`.
    #[test]
    #[ignore]
    fn smoke_boots_and_reads_seabios_banner() {
        if std::env::var_os("BILTERM_DOS_SMOKE").is_none() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let img = t.path().join("c.img");
        std::fs::write(&img, vec![0u8; 1024 * 1024]).unwrap();
        let mut b = QemuBackend::new(QemuMode::TextBuf, PROGRAM.into(), t.path().into());
        let s = SessionSpec {
            c_image: Some(img),
            throwaway: true,
            ..SessionSpec::default()
        };
        b.launch(&s).unwrap();
        let start = Instant::now();
        let mut seen = false;
        while start.elapsed() < Duration::from_secs(15) {
            if b.read_text().unwrap().contains("SeaBIOS") {
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        b.send_keys(&crate::dos::backend::keys::keys_for_text("dir\n").unwrap())
            .unwrap();
        assert!(b.is_running());
        b.shutdown().unwrap();
        assert!(!b.is_running());
        assert!(seen, "SeaBIOS banner never appeared");
    }
}
