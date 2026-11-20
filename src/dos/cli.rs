//! `bilterm dos setup|remove|doctor`. Logic lives here; `main.rs` only dispatches. These are
//! host-setup commands that run before/outside any WindowManager, so they do not lower to
//! `Action` (the pane-facing `dos run` surface gets `Action::Dos*` in D3).

use std::fs;
use std::io::BufRead;

use super::doctor::{self, EmulatorProbe, SystemProbe};
use super::download::{fetch_entry, CurlDownloader, Downloader, FetchOutcome};
use super::image::cmd::{self as image_cmd, ImgCmd, ImgEnv};
use super::image::lock::{ProcInfo, SysProc};
use super::image::provision::{BackendFactory, Provision, QemuFactory, provision};
use super::image::ImageKind;
use super::manifest::Manifest;
use super::paths::{DosPaths, HostOs};

pub const USAGE: &str =
    "usage: bilterm dos <setup [--yes] [--full] [--entry ID]... | remove [--all] [--yes] | doctor | run [--name N | --image FILE] [--drive-c DIR] [--print-argv] | reset | snapshot | restore>\n  (reset/snapshot/restore: see `bilterm dos reset --help`-style usage in the error text)\n";

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Setup { yes: bool, full: bool, entries: Vec<String> },
    Remove { all: bool, yes: bool },
    Doctor,
    /// `run` (D2a): start the emulator in this terminal; `print_argv` only prints the command.
    Run { req: super::launch::RunRequest, print_argv: bool },
    /// `reset`, `snapshot`, `restore` (persistent C: image lifecycle, D1c).
    Image(ImgCmd),
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let (sub, rest) = args.split_first().ok_or_else(|| USAGE.to_string())?;
    match sub.as_str() {
        "doctor" if rest.is_empty() => Ok(Cmd::Doctor),
        "run" => super::launch::RunRequest::parse(rest).map(|(req, print_argv)| Cmd::Run { req, print_argv }),
        "setup" => {
            let (mut yes, mut full, mut entries, mut it) = (false, false, Vec::new(), rest.iter());
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--yes" | "-y" => yes = true,
                    "--full" => full = true,
                    "--entry" => entries.push(it.next().ok_or("--entry needs an ID")?.clone()),
                    other => return Err(format!("unknown option {other:?}\n{USAGE}")),
                }
            }
            Ok(Cmd::Setup { yes, full, entries })
        }
        "remove" => {
            let (mut all, mut yes) = (false, false);
            for a in rest {
                match a.as_str() {
                    "--all" => all = true,
                    "--yes" | "-y" => yes = true,
                    other => return Err(format!("unknown option {other:?}\n{USAGE}")),
                }
            }
            Ok(Cmd::Remove { all, yes })
        }
        other => match image_cmd::parse(other, rest) {
            Some(r) => r.map(Cmd::Image),
            None => Err(USAGE.to_string()),
        },
    }
}

/// Everything a command needs from the host, injectable for tests.
pub struct Ctx<'a> {
    pub os: HostOs,
    pub paths: Option<DosPaths>,
    pub manifest: &'a Manifest,
    pub probe: &'a dyn EmulatorProbe,
    pub downloader: &'a dyn Downloader,
    pub is_root: bool,
    pub procs: &'a dyn ProcInfo,
    /// Creates the emulator backend for the one-off boot-sector capture; `None` skips image
    /// provisioning in `setup` (downloads only).
    pub factory: Option<&'a dyn BackendFactory>,
}

pub(crate) fn confirm(prompt: &str, input: &mut dyn BufRead, out: &mut String) -> bool {
    out.push_str(prompt);
    let mut line = String::new();
    input.read_line(&mut line).is_ok()
        && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Runs a parsed command; returns the exit code and appends human output to `out`.
pub fn execute(cmd: &Cmd, ctx: &Ctx, input: &mut dyn BufRead, out: &mut String) -> i32 {
    match cmd {
        Cmd::Doctor => {
            let checks = doctor::run_doctor(ctx.os, ctx.paths.as_ref(), ctx.manifest, ctx.probe);
            out.push_str(&doctor::render(&checks));
            doctor::exit_code(&checks)
        }
        Cmd::Run { req, print_argv } => {
            let Some(paths) = ctx.paths.as_ref() else {
                out.push_str("cannot determine the per-user data directory\n");
                return 1;
            };
            let env = super::launch::RunEnv {
                os: ctx.os,
                paths,
                manifest: ctx.manifest,
                probe: ctx.probe,
                procs: ctx.procs,
                is_root: ctx.is_root,
                term_size: crossterm::terminal::size().ok(),
                term_env: super::launch::TermEnv::from_process(),
            };
            super::launch::cli_run(req, *print_argv, &env, &super::launch::SystemSpawner, out)
        }
        Cmd::Setup { yes, full, entries } => setup(ctx, *yes, *full, entries, input, out),
        Cmd::Image(c) => {
            let env = ImgEnv {
                paths: ctx.paths.as_ref(),
                procs: ctx.procs,
                is_root: ctx.is_root,
                now_secs: None,
            };
            image_cmd::execute(c, &env, input, out)
        }
        Cmd::Remove { all, yes } => remove(ctx, *all, *yes, input, out),
    }
}

fn setup(
    ctx: &Ctx,
    yes: bool,
    full: bool,
    ids: &[String],
    input: &mut dyn BufRead,
    out: &mut String,
) -> i32 {
    if ctx.os == HostOs::Other {
        out.push_str("DOS mode is unsupported on this OS; see `bilterm dos doctor`\n");
        return 1;
    }
    if ctx.is_root {
        out.push_str(
            "refusing to run as root: DOS mode keeps per-user data and must not run privileged\n",
        );
        return 1;
    }
    let Some(paths) = &ctx.paths else {
        out.push_str("cannot determine the per-user data directory\n");
        return 1;
    };
    let m = ctx.manifest;
    let selected: Vec<_> = if ids.is_empty() {
        m.defaults(ctx.os).collect()
    } else {
        let mut v = Vec::new();
        for id in ids {
            match m.entry(id) {
                Some(e) => v.push(e),
                None => {
                    out.push_str(&format!("unknown manifest entry {id:?}\n"));
                    return 2;
                }
            }
        }
        v
    };
    // Refuse unpinned/disallowed entries before any prompt or network use.
    for e in &selected {
        if let Some(why) = e.refusal() {
            out.push_str(&format!("{why}\n"));
            return 1;
        }
    }
    out.push_str(&format!("{}\n\n", m.freedos.notice));
    for e in &selected {
        out.push_str(&format!(
            "  {} {} ({} bytes)\n    from {}\n    license: {}\n",
            e.id, e.version, e.bytes, e.url, e.license_note
        ));
    }
    if !yes
        && !confirm(
            "\nDownload these into your user data directory? [y/N] ",
            input,
            out,
        )
    {
        out.push_str("\naborted; nothing downloaded\n");
        return 1;
    }
    if let Err(e) = paths.ensure_layout() {
        out.push_str(&format!("cannot create {}: {e}\n", paths.root().display()));
        return 1;
    }
    for e in selected {
        match fetch_entry(e, paths, ctx.downloader) {
            Ok(FetchOutcome::AlreadyPresent(p)) => out.push_str(&format!(
                "{}: already present and verified ({})\n",
                e.id,
                p.display()
            )),
            Ok(FetchOutcome::Downloaded(p)) => out.push_str(&format!(
                "{}: downloaded and verified ({})\n",
                e.id,
                p.display()
            )),
            Err(err) => {
                out.push_str(&format!("{err}\n"));
                return 1;
            }
        }
    }
    let Some(factory) = ctx.factory else {
        out.push_str("downloads complete (image provisioning skipped)\n");
        return 0;
    };
    if !paths
        .download_path(&m.entry(super::image::provision::LIVECD_ENTRY).map_or(String::new(), |e| e.file.clone()))
        .is_ok_and(|p| p.is_file())
    {
        out.push_str("downloads complete; the Live CD was not selected, so no C: image was built\n");
        return 0;
    }
    let kind = if full { ImageKind::Full } else { ImageKind::Base };
    let p = Provision::new(paths, m, factory, ctx.procs);
    // Progress goes to stderr immediately: the capture step takes about a minute.
    match provision(&p, kind, &mut |s| eprintln!("bilterm dos: {s}")) {
        Ok(msg) => {
            out.push_str(&msg);
            out.push_str("\nsetup complete (run `bilterm dos doctor` to check the emulator)\n");
            0
        }
        Err(e) => {
            out.push_str(&format!(
                "downloads are complete but the C: image was not built: {e}\nfix the problem and rerun `bilterm dos setup`\n"
            ));
            1
        }
    }
}

fn remove(ctx: &Ctx, all: bool, yes: bool, input: &mut dyn BufRead, out: &mut String) -> i32 {
    let Some(paths) = &ctx.paths else {
        out.push_str("cannot determine the per-user data directory\n");
        return 1;
    };
    // Never follow a symlinked root: only delete real directories we own the layout of.
    match fs::symlink_metadata(paths.root()) {
        Err(_) => {
            out.push_str("nothing to remove\n");
            return 0;
        }
        Ok(md) if !md.is_dir() => {
            out.push_str(&format!(
                "{} is not a directory; refusing\n",
                paths.root().display()
            ));
            return 1;
        }
        Ok(_) => {}
    }
    let targets = if all {
        vec![paths.root().to_path_buf()]
    } else {
        vec![paths.downloads_dir(), paths.images_dir()]
    };
    let what = if all {
        "ALL DOS mode data (downloads, images, C: disks, boot sector)"
    } else {
        "downloaded files and images (C: disks are kept)"
    };
    if !yes
        && !confirm(
            &format!("Delete {what} under {}? [y/N] ", paths.root().display()),
            input,
            out,
        )
    {
        out.push_str("\naborted\n");
        return 1;
    }
    let mut code = 0;
    for t in targets {
        match fs::symlink_metadata(&t) {
            Ok(md) if md.is_dir() => {
                if let Err(e) = fs::remove_dir_all(&t) {
                    out.push_str(&format!("cannot remove {}: {e}\n", t.display()));
                    code = 1;
                } else {
                    out.push_str(&format!("removed {}\n", t.display()));
                }
            }
            Ok(_) => {
                out.push_str(&format!("{} is not a directory; skipped\n", t.display()));
                code = 1;
            }
            Err(_) => {}
        }
    }
    code
}

/// Process entry point used by `main.rs`: returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let cmd = match parse(args) {
        Ok(c) => c,
        Err(msg) => {
            eprint!("{msg}");
            return 2;
        }
    };
    let manifest = match Manifest::embedded() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("bilterm: {e}");
            return 1;
        }
    };
    let probe = SystemProbe::new(&manifest.emulator.name);
    let dl = CurlDownloader::default();
    let paths = DosPaths::from_process();
    let bp = super::backend::probe_qemu(&probe, manifest.min_version());
    let factory = QemuFactory {
        program: probe.locate(),
        problem: (!bp.usable()).then(|| {
            if bp.found { format!("emulator unusable: {}", bp.detail) } else { "qemu-system-i386 not found (see `bilterm dos doctor`)".to_string() }
        }),
        base: paths.as_ref().map(DosPaths::sessions_dir).unwrap_or_default(),
    };
    let ctx = Ctx {
        os: HostOs::current(),
        paths,
        manifest: &manifest,
        probe: &probe,
        downloader: &dl,
        is_root: cfg!(unix) && crate::secure_fs::current_euid() == 0,
        procs: &SysProc,
        factory: Some(&factory),
    };
    let mut out = String::new();
    let code = execute(&cmd, &ctx, &mut std::io::stdin().lock(), &mut out);
    if code == 0 {
        print!("{out}");
    } else {
        eprint!("{out}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::super::doctor::Status;
    use super::super::download::MockDownloader;
    use super::super::verify::sha256_bytes;
    use super::*;

    struct NoProbe;
    impl EmulatorProbe for NoProbe {
        fn locate(&self) -> Option<std::path::PathBuf> {
            None
        }
        fn version_output(&self) -> Result<String, String> {
            Err("none".into())
        }
        fn display_help(&self) -> Result<String, String> {
            Err("none".into())
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn manifest_with_body(body: &[u8]) -> Manifest {
        let mut m = Manifest::embedded().unwrap();
        for e in m.entries.iter_mut().filter(|e| e.default) {
            e.sha256 = sha256_bytes(body);
            e.bytes = body.len() as u64;
        }
        m
    }

    #[test]
    fn parse_forms() {
        assert_eq!(parse(&s(&["doctor"])).unwrap(), Cmd::Doctor);
        assert_eq!(
            parse(&s(&["setup", "-y", "--entry", "a"])).unwrap(),
            Cmd::Setup {
                yes: true,
                full: false,
                entries: vec!["a".into()]
            }
        );
        assert_eq!(
            parse(&s(&["remove", "--all"])).unwrap(),
            Cmd::Remove {
                all: true,
                yes: false
            }
        );
        assert!(
            parse(&s(&[])).is_err()
                && parse(&s(&["setup", "--bogus"])).is_err()
                && parse(&s(&["run", "--bogus"])).is_err()
        );
    }

    fn ctx<'a>(
        t: &tempfile::TempDir,
        m: &'a Manifest,
        dl: &'a dyn Downloader,
        root: bool,
    ) -> Ctx<'a> {
        Ctx {
            os: HostOs::Linux,
            paths: Some(DosPaths::new(t.path().join("dos"))),
            manifest: m,
            probe: &NoProbe,
            downloader: dl,
            is_root: root,
            procs: &SysProc,
            factory: None,
        }
    }

    #[test]
    fn setup_requires_consent_then_downloads_then_remove() {
        let t = tempfile::tempdir().unwrap();
        let body = b"fake livecd".to_vec();
        let m = manifest_with_body(&body);
        let url = m.entry("freedos-14-livecd").unwrap().url.clone();
        let dl = MockDownloader::new(vec![(url, body)]);
        let c = ctx(&t, &m, &dl, false);
        let setup = Cmd::Setup {
            yes: false,
            full: false,
            entries: vec![],
        };
        let mut out = String::new();
        assert_eq!(execute(&setup, &c, &mut "n\n".as_bytes(), &mut out), 1);
        assert!(out.contains("runs FreeDOS") && out.contains("nothing downloaded"));
        assert!(dl.requests.borrow().is_empty());
        let mut out = String::new();
        assert_eq!(
            execute(&setup, &c, &mut "y\n".as_bytes(), &mut out),
            0,
            "{out}"
        );
        assert!(c
            .paths
            .as_ref()
            .unwrap()
            .download_path("FD14-LiveCD.zip")
            .unwrap()
            .is_file());
        // doctor now sees the image as verified
        let mut out = String::new();
        execute(&Cmd::Doctor, &c, &mut "".as_bytes(), &mut out);
        assert!(out.contains("present, sha256 verified"), "{out}");
        // remove without --all keeps root, with --all deletes it
        let mut out = String::new();
        assert_eq!(
            execute(
                &Cmd::Remove {
                    all: false,
                    yes: true
                },
                &c,
                &mut "".as_bytes(),
                &mut out
            ),
            0
        );
        assert!(
            !c.paths.as_ref().unwrap().downloads_dir().exists()
                && c.paths.as_ref().unwrap().c_dir().exists()
        );
        assert_eq!(
            execute(
                &Cmd::Remove {
                    all: true,
                    yes: true
                },
                &c,
                &mut "".as_bytes(),
                &mut out
            ),
            0
        );
        assert!(!c.paths.as_ref().unwrap().root().exists());
    }

    #[test]
    fn setup_refuses_unpinned_entry_without_network_or_dirs() {
        let t = tempfile::tempdir().unwrap();
        let m = Manifest::parse(&crate::dos::manifest::EMBEDDED.replace(
            "f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f",
            "TODO",
        ))
        .unwrap();
        let dl = MockDownloader::new(vec![]);
        let c = ctx(&t, &m, &dl, false);
        let mut out = String::new();
        let cmd = Cmd::Setup {
            yes: true,
            full: false,
            entries: vec!["qemu-windows-x64".into()],
        };
        assert_eq!(execute(&cmd, &c, &mut "".as_bytes(), &mut out), 1);
        assert!(out.contains("not pinned"));
        assert!(dl.requests.borrow().is_empty() && !c.paths.as_ref().unwrap().root().exists());
    }

    #[test]
    fn setup_refuses_root_unknown_entry_and_unsupported_os() {
        let t = tempfile::tempdir().unwrap();
        let m = Manifest::embedded().unwrap();
        let dl = MockDownloader::new(vec![]);
        let cmd = Cmd::Setup {
            yes: true,
            full: false,
            entries: vec![],
        };
        let mut out = String::new();
        assert_eq!(
            execute(&cmd, &ctx(&t, &m, &dl, true), &mut "".as_bytes(), &mut out),
            1
        );
        assert!(out.contains("root"));
        let bad = Cmd::Setup {
            yes: true,
            full: false,
            entries: vec!["nope".into()],
        };
        assert_eq!(
            execute(&bad, &ctx(&t, &m, &dl, false), &mut "".as_bytes(), &mut out),
            2
        );
        let mut other = ctx(&t, &m, &dl, false);
        other.os = HostOs::Other;
        assert_eq!(execute(&cmd, &other, &mut "".as_bytes(), &mut out), 1);
    }

    #[test]
    fn remove_refuses_symlinked_root() {
        #[cfg(unix)]
        {
            let t = tempfile::tempdir().unwrap();
            let real = t.path().join("real");
            fs::create_dir(&real).unwrap();
            fs::write(real.join("keep"), b"x").unwrap();
            std::os::unix::fs::symlink(&real, t.path().join("dos")).unwrap();
            let m = Manifest::embedded().unwrap();
            let dl = MockDownloader::new(vec![]);
            let mut out = String::new();
            let c = ctx(&t, &m, &dl, false);
            assert_eq!(
                execute(
                    &Cmd::Remove {
                        all: true,
                        yes: true
                    },
                    &c,
                    &mut "".as_bytes(),
                    &mut out
                ),
                1
            );
            assert!(real.join("keep").exists());
        }
    }

    #[test]
    fn doctor_without_emulator_fails() {
        let t = tempfile::tempdir().unwrap();
        let m = Manifest::embedded().unwrap();
        let dl = MockDownloader::new(vec![]);
        let mut out = String::new();
        assert_eq!(
            execute(
                &Cmd::Doctor,
                &ctx(&t, &m, &dl, false),
                &mut "".as_bytes(),
                &mut out
            ),
            1
        );
        let _ = Status::Ok;
        assert!(out.contains("[FAIL] emulator"));
    }
}
