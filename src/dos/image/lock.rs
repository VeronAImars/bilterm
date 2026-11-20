//! Single-writer lock for a C: image (controlling-processes-safely rules): the lock file records
//! `pid` AND the process start-time token; a holder counts as alive only if the pid exists and its
//! start-time token still matches (a recycled pid is a stale lock). `kill(0)` / pid 0 are never
//! used on unvalidated numbers.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::util::open_regular_read;
use super::{ImageError, Res};
use crate::secure_fs;

const HEADER: &str = "bilterm-dos-lock v1";
/// A lock file this young that we cannot parse may be another process mid-write.
const PARSE_GRACE: Duration = Duration::from_secs(2);

/// Process facts, injectable so contention and pid-reuse are testable.
pub trait ProcInfo {
    fn alive(&self, pid: u32) -> bool;
    /// Opaque token identifying this incarnation of `pid` (None when the platform cannot tell).
    fn start_token(&self, pid: u32) -> Option<String>;
    fn current_pid(&self) -> u32 {
        std::process::id()
    }
}

pub struct SysProc;

impl ProcInfo for SysProc {
    fn alive(&self, pid: u32) -> bool {
        if pid == 0 || pid > i32::MAX as u32 {
            return false;
        }
        #[cfg(unix)]
        {
            // SAFETY: signal 0 only checks existence; pid is validated to be a positive i32.
            let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
            r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
        #[cfg(windows)]
        {
            std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
                .unwrap_or(false)
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    fn start_token(&self, pid: u32) -> Option<String> {
        if pid == 0 || pid > i32::MAX as u32 {
            return None;
        }
        #[cfg(target_os = "linux")]
        {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // field 22 (starttime); the comm field may contain spaces/parens, so split after the last ')'
            let rest = stat.rsplit_once(')')?.1;
            rest.split_whitespace().nth(19).map(str::to_string)
        }
        #[cfg(target_os = "macos")]
        {
            let o = std::process::Command::new("ps")
                .args(["-o", "lstart=", "-p", &pid.to_string()])
                .output()
                .ok()?;
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            (o.status.success() && !s.is_empty()).then_some(s)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            None
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Holder {
    pid: u32,
    start: Option<String>,
}

fn render(h: &Holder) -> String {
    format!(
        "{HEADER}\npid={}\nstart={}\n",
        h.pid,
        h.start.as_deref().unwrap_or("-")
    )
}

fn parse(text: &str) -> Option<Holder> {
    let mut l = text.lines();
    if l.next()? != HEADER {
        return None;
    }
    let pid = l.next()?.strip_prefix("pid=")?.parse().ok()?;
    let start = l.next()?.strip_prefix("start=")?;
    Some(Holder {
        pid,
        start: (start != "-").then(|| start.to_string()),
    })
}

/// Held lock; removed on drop if the file still names this process.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    mine: String,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path).is_ok_and(|t| t == self.mine) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

enum Verdict {
    Held(u32),
    Stale,
}

fn judge(path: &Path, procs: &dyn ProcInfo) -> Verdict {
    let text = open_regular_read(path).ok().and_then(|mut f| {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::Read::take(&mut f, 512), &mut s).ok()?;
        Some(s)
    });
    let Some(h) = text.as_deref().and_then(parse) else {
        let young = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < PARSE_GRACE);
        return if young { Verdict::Held(0) } else { Verdict::Stale };
    };
    if !procs.alive(h.pid) {
        return Verdict::Stale;
    }
    match (&h.start, procs.start_token(h.pid)) {
        (Some(rec), Some(now)) if *rec != now => Verdict::Stale, // pid was recycled
        _ => Verdict::Held(h.pid),
    }
}

/// Takes the lock at `path`. A live holder yields [`ImageError::LockHeld`]; a stale one is
/// replaced. Creation uses `O_EXCL`, so two racing acquirers cannot both win.
pub fn acquire(path: &Path, procs: &dyn ProcInfo) -> Res<LockGuard> {
    let me = Holder {
        pid: procs.current_pid(),
        start: procs.start_token(procs.current_pid()),
    };
    let mine = render(&me);
    for _ in 0..2 {
        match secure_fs::create_private_file(path) {
            Ok(mut f) => {
                f.write_all(mine.as_bytes())?;
                f.sync_all()?;
                return Ok(LockGuard {
                    path: path.to_path_buf(),
                    mine,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match judge(path, procs) {
                Verdict::Held(pid) => return Err(ImageError::LockHeld { pid }),
                Verdict::Stale => {
                    let _ = fs::remove_file(path);
                }
            },
            Err(e) => return Err(e.into()),
        }
    }
    Err(ImageError::LockHeld { pid: 0 })
}

/// Read-only: who holds the lock right now (None when free or stale).
pub fn holder(path: &Path, procs: &dyn ProcInfo) -> Option<u32> {
    if !path.exists() {
        return None;
    }
    match judge(path, procs) {
        Verdict::Held(p) => Some(p),
        Verdict::Stale => None,
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::HashMap;

    pub struct FakeProcs {
        pub me: u32,
        pub procs: HashMap<u32, String>,
    }
    impl FakeProcs {
        pub fn new(me: u32) -> Self {
            Self { me, procs: HashMap::from([(me, "100".to_string())]) }
        }
    }
    impl ProcInfo for FakeProcs {
        fn alive(&self, pid: u32) -> bool {
            self.procs.contains_key(&pid)
        }
        fn start_token(&self, pid: u32) -> Option<String> {
            self.procs.get(&pid).cloned()
        }
        fn current_pid(&self) -> u32 {
            self.me
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeProcs;
    use super::*;

    #[test]
    fn contention_release_and_reacquire() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.lock");
        let a = FakeProcs::new(10);
        let mut b = FakeProcs::new(20);
        b.procs.insert(10, "100".into());
        let g = acquire(&p, &a).unwrap();
        assert!(matches!(acquire(&p, &b), Err(ImageError::LockHeld { pid: 10 })));
        assert_eq!(holder(&p, &b), Some(10));
        drop(g);
        assert!(!p.exists());
        assert!(acquire(&p, &b).is_ok());
    }

    #[test]
    fn dead_holder_and_recycled_pid_are_stale() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.lock");
        let a = FakeProcs::new(10);
        let g = acquire(&p, &a).unwrap();
        std::mem::forget(g); // simulate a crash: the file stays
        // holder died
        let b = FakeProcs::new(20);
        assert_eq!(holder(&p, &b), None);
        let g2 = acquire(&p, &b).unwrap();
        drop(g2);
        // pid 10 alive again but with a different start token (recycled pid)
        std::mem::forget(acquire(&p, &a).unwrap());
        let mut c = FakeProcs::new(30);
        c.procs.insert(10, "999".into());
        assert!(acquire(&p, &c).is_ok());
    }

    #[test]
    fn same_incarnation_blocks_and_garbage_is_handled() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.lock");
        std::fs::write(&p, b"garbage").unwrap();
        // freshly written garbage might be a racing writer: held
        let b = FakeProcs::new(20);
        assert!(matches!(acquire(&p, &b), Err(ImageError::LockHeld { .. })));
        // age it: stale
        let old = SystemTime::now() - Duration::from_secs(60);
        fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        assert!(acquire(&p, &b).is_ok());
    }

    #[test]
    fn drop_does_not_remove_a_lock_taken_over_by_someone_else() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.lock");
        let a = FakeProcs::new(10);
        let g = acquire(&p, &a).unwrap();
        fs::write(&p, render(&Holder { pid: 77, start: None })).unwrap();
        drop(g);
        assert!(p.exists());
    }

    #[test]
    fn pid_validation_and_real_process_info() {
        let s = SysProc;
        assert!(!s.alive(0) && !s.alive(u32::MAX));
        assert!(s.alive(std::process::id()));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let t = s.start_token(std::process::id()).expect("start token");
            assert_eq!(Some(t), s.start_token(std::process::id()));
        }
        assert_eq!(parse(&render(&Holder { pid: 5, start: Some("a b".into()) })).unwrap().pid, 5);
        assert!(parse("pid=5").is_none());
    }

    #[test]
    fn real_lock_blocks_second_acquire_in_same_process() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.lock");
        let _g = acquire(&p, &SysProc).unwrap();
        assert!(matches!(acquire(&p, &SysProc), Err(ImageError::LockHeld { .. })));
    }
}
