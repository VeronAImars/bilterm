//! `bilterm dos doctor`: read-only host checks (never touches the network).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::manifest::Manifest;
use super::paths::{DosPaths, HostOs};
use super::verify::{parse_qemu_version, verify_file};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

/// Seam over the emulator binary (`designing-testable-seams`); no process runs in tests.
pub trait EmulatorProbe {
    fn locate(&self) -> Option<PathBuf>;
    fn version_output(&self) -> Result<String, String>;
    fn display_help(&self) -> Result<String, String>;
}

const OUTPUT_CAP: usize = 8192;

pub struct SystemProbe {
    pub program: String,
}

impl SystemProbe {
    pub fn new(program: &str) -> Self {
        Self {
            program: program.to_string(),
        }
    }

    fn candidates(&self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                v.push(dir.join(&self.program));
                if cfg!(windows) {
                    v.push(dir.join(format!("{}.exe", self.program)));
                }
            }
        }
        if cfg!(windows) {
            for var in ["ProgramFiles", "ProgramW6432"] {
                if let Some(pf) = std::env::var_os(var) {
                    v.push(
                        PathBuf::from(pf)
                            .join("qemu")
                            .join(format!("{}.exe", self.program)),
                    );
                }
            }
        }
        v
    }

    fn run(&self, arg: &str) -> Result<String, String> {
        let bin = self
            .locate()
            .ok_or_else(|| format!("{} not found", self.program))?;
        let out = Command::new(bin)
            .args(arg_for(arg))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        text.truncate(
            text.char_indices()
                .nth(OUTPUT_CAP)
                .map_or(text.len(), |(i, _)| i),
        );
        Ok(text)
    }
}

fn arg_for(which: &str) -> Vec<&'static str> {
    if which == "version" {
        vec!["-version"]
    } else {
        vec!["-display", "help"]
    }
}

impl EmulatorProbe for SystemProbe {
    fn locate(&self) -> Option<PathBuf> {
        self.candidates().into_iter().find(|p| p.is_file())
    }
    fn version_output(&self) -> Result<String, String> {
        self.run("version")
    }
    fn display_help(&self) -> Result<String, String> {
        self.run("display")
    }
}

pub fn platform_check(os: HostOs) -> Check {
    let (status, detail) = match os {
        HostOs::Linux => (
            Status::Ok,
            "linux: supported (backends: qemu-curses, qemu-textbuf, bochs-term)",
        ),
        HostOs::MacOs => (
            Status::Ok,
            "macos: supported (backends: qemu-curses, qemu-textbuf)",
        ),
        HostOs::Windows => (
            Status::Ok,
            "windows: supported (backends: qemu-textbuf, qemu-curses)",
        ),
        HostOs::Other => (
            Status::Fail,
            "this OS is unsupported for DOS mode (supported: Linux, macOS, Windows)",
        ),
    };
    Check {
        name: "platform",
        status,
        detail: detail.into(),
    }
}

pub fn run_doctor(
    os: HostOs,
    paths: Option<&DosPaths>,
    m: &Manifest,
    probe: &dyn EmulatorProbe,
) -> Vec<Check> {
    let mut checks = vec![platform_check(os)];
    if os == HostOs::Other {
        return checks; // explain and stop; no probing, no network
    }
    match probe.locate() {
        None => checks.push(Check {
            name: "emulator",
            status: Status::Fail,
            detail: format!(
                "{} not found on PATH; install QEMU >= {} with your package manager",
                m.emulator.name, m.emulator.min_version
            ),
        }),
        Some(p) => {
            checks.push(Check {
                name: "emulator",
                status: Status::Ok,
                detail: format!("found {}", p.display()),
            });
            let floor = m.min_version();
            checks.push(
                match probe
                    .version_output()
                    .ok()
                    .as_deref()
                    .and_then(parse_qemu_version)
                {
                    Some(v) if v >= floor => Check {
                        name: "emulator-version",
                        status: Status::Ok,
                        detail: format!("{v} (floor {floor})"),
                    },
                    Some(v) => Check {
                        name: "emulator-version",
                        status: Status::Fail,
                        detail: format!("{v} is below the floor {floor}; update QEMU"),
                    },
                    None => Check {
                        name: "emulator-version",
                        status: Status::Fail,
                        detail: "could not parse `-version` output".into(),
                    },
                },
            );
            checks.push(match probe.display_help() {
                Ok(t) if t.lines().any(|l| l.trim() == "curses") => Check {
                    name: "display-curses",
                    status: Status::Ok,
                    detail: "`-display curses` available".into(),
                },
                Ok(_) => Check {
                    name: "display-curses",
                    status: Status::Warn,
                    detail: "curses display not listed; only the textbuf backend can work".into(),
                },
                Err(e) => Check {
                    name: "display-curses",
                    status: Status::Warn,
                    detail: format!("could not query displays: {e}"),
                },
            });
        }
    }
    let Some(paths) = paths else {
        checks.push(Check {
            name: "data-dir",
            status: Status::Fail,
            detail: "cannot determine the per-user data directory".into(),
        });
        return checks;
    };
    checks.push(Check {
        name: "data-dir",
        status: Status::Ok,
        detail: paths.root().display().to_string(),
    });
    for e in m.defaults(os) {
        let Ok(p) = paths.download_path(&e.file) else {
            continue;
        };
        let (status, detail) = if !p.is_file() {
            (
                Status::Fail,
                format!("{} missing; run `bilterm dos setup`", e.file),
            )
        } else {
            match verify_file(&p, &e.sha256, e.bytes) {
                Ok(()) => (Status::Ok, format!("{} present, sha256 verified", e.file)),
                Err(err) => (
                    Status::Fail,
                    format!("{}: {err}; run `bilterm dos setup`", e.file),
                ),
            }
        };
        checks.push(Check {
            name: "image",
            status,
            detail,
        });
    }
    let bs = paths.boot_sector_path();
    checks.push(
        if std::fs::metadata(&bs).is_ok_and(|m| m.is_file() && m.len() == 512) {
            Check {
                name: "boot-sector",
                status: Status::Ok,
                detail: format!("cached at {}", bs.display()),
            }
        } else {
            Check {
                name: "boot-sector",
                status: Status::Warn,
                detail: format!(
                    "not cached yet (captured by `dos setup` in a later release): {}",
                    bs.display()
                ),
            }
        },
    );
    checks
}

pub fn render(checks: &[Check]) -> String {
    let mut s = String::new();
    for c in checks {
        let tag = match c.status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        s.push_str(&format!("[{tag}] {}: {}\n", c.name, c.detail));
    }
    s
}

pub fn exit_code(checks: &[Check]) -> i32 {
    i32::from(checks.iter().any(|c| c.status == Status::Fail))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub struct FakeProbe {
        pub found: bool,
        pub version: &'static str,
        pub displays: &'static str,
    }
    impl EmulatorProbe for FakeProbe {
        fn locate(&self) -> Option<PathBuf> {
            self.found
                .then(|| PathBuf::from("/usr/bin/qemu-system-i386"))
        }
        fn version_output(&self) -> Result<String, String> {
            Ok(self.version.to_string())
        }
        fn display_help(&self) -> Result<String, String> {
            Ok(self.displays.to_string())
        }
    }

    fn names(c: &[Check], s: Status) -> Vec<&'static str> {
        c.iter().filter(|c| c.status == s).map(|c| c.name).collect()
    }

    #[test]
    fn unsupported_os_stops_early_nonzero() {
        let m = Manifest::embedded().unwrap();
        let p = FakeProbe {
            found: true,
            version: "",
            displays: "",
        };
        let c = run_doctor(HostOs::Other, None, &m, &p);
        assert_eq!(c.len(), 1);
        assert_eq!(exit_code(&c), 1);
    }

    #[test]
    fn missing_emulator_old_version_and_good() {
        let m = Manifest::embedded().unwrap();
        let t = tempfile::tempdir().unwrap();
        let paths = DosPaths::new(t.path().join("dos"));
        let missing = FakeProbe {
            found: false,
            version: "",
            displays: "",
        };
        assert!(names(
            &run_doctor(HostOs::Linux, Some(&paths), &m, &missing),
            Status::Fail
        )
        .contains(&"emulator"));
        let old = FakeProbe {
            found: true,
            version: "QEMU emulator version 9.2.4",
            displays: "curses",
        };
        assert!(names(
            &run_doctor(HostOs::Linux, Some(&paths), &m, &old),
            Status::Fail
        )
        .contains(&"emulator-version"));
        let good = FakeProbe {
            found: true,
            version: "QEMU emulator version 10.2.1 (x)",
            displays: "none\ncurses\n",
        };
        let c = run_doctor(HostOs::Linux, Some(&paths), &m, &good);
        let fails = names(&c, Status::Fail);
        assert!(!fails.contains(&"emulator") && !fails.contains(&"emulator-version"));
        assert!(fails.contains(&"image")); // not set up yet
        assert!(names(&c, Status::Warn).contains(&"boot-sector"));
        assert!(names(&c, Status::Ok).contains(&"display-curses"));
    }

    #[test]
    fn no_curses_is_a_warning_and_cached_boot_sector_is_ok() {
        let m = Manifest::embedded().unwrap();
        let t = tempfile::tempdir().unwrap();
        let paths = DosPaths::new(t.path().join("dos"));
        paths.ensure_layout().unwrap();
        std::fs::write(paths.boot_sector_path(), [0u8; 512]).unwrap();
        let p = FakeProbe {
            found: true,
            version: "QEMU emulator version 11.1.0",
            displays: "none\ngtk\n",
        };
        let c = run_doctor(HostOs::Windows, Some(&paths), &m, &p);
        assert!(names(&c, Status::Warn).contains(&"display-curses"));
        assert!(names(&c, Status::Ok).contains(&"boot-sector"));
        assert!(render(&c).contains("[warn] display-curses"));
    }
}
