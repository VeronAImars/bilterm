//! `bilterm dos reset|snapshot|restore` (host-side lifecycle commands; they run outside any
//! WindowManager, so like `setup`/`remove` they do not lower to `Action` -- the pane-facing
//! `dos run` surface gets `Action::Dos*` in D3).

use std::io::BufRead;
use std::time::{SystemTime, UNIX_EPOCH};

use super::bootsector::load_cached;
use super::livecd::LiveIsoSource;
use super::lock::ProcInfo;
use super::provision::iso_path;
use super::store::ImageStore;
use super::{DEFAULT_NAME, ImageKind, ImageError};
use crate::dos::cli::confirm;
use crate::dos::paths::DosPaths;

pub const USAGE: &str = "usage: bilterm dos reset [--name N] [--full] [--yes] | snapshot [--name N] [--tag T] | snapshot list [--name N] | snapshot rm --tag T [--name N] | restore --tag T [--name N] [--yes]\n";

#[derive(Debug, PartialEq, Eq)]
pub enum ImgCmd {
    Reset { name: String, full: bool, yes: bool },
    Snapshot { name: String, tag: Option<String> },
    SnapshotList { name: String },
    SnapshotRm { name: String, tag: String },
    Restore { name: String, tag: String, yes: bool },
}

struct Flags {
    name: String,
    tag: Option<String>,
    yes: bool,
    full: bool,
}

fn flags(rest: &[String], allow: &[&str]) -> Result<Flags, String> {
    let mut f = Flags { name: DEFAULT_NAME.into(), tag: None, yes: false, full: false };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if !allow.contains(&a.as_str()) {
            return Err(format!("unknown option {a:?}\n{USAGE}"));
        }
        match a.as_str() {
            "--name" => f.name = it.next().ok_or("--name needs a value")?.clone(),
            "--tag" => f.tag = Some(it.next().ok_or("--tag needs a value")?.clone()),
            "--yes" | "-y" => f.yes = true,
            "--full" => f.full = true,
            _ => unreachable!(),
        }
    }
    Ok(f)
}

/// `None` when `sub` is not an image command.
pub fn parse(sub: &str, rest: &[String]) -> Option<Result<ImgCmd, String>> {
    Some(match sub {
        "reset" => flags(rest, &["--name", "--full", "--yes", "-y"])
            .map(|f| ImgCmd::Reset { name: f.name, full: f.full, yes: f.yes }),
        "restore" => flags(rest, &["--name", "--tag", "--yes", "-y"]).and_then(|f| {
            let tag = f.tag.ok_or_else(|| format!("restore needs --tag\n{USAGE}"))?;
            Ok(ImgCmd::Restore { name: f.name, tag, yes: f.yes })
        }),
        "snapshot" => match rest.first().map(String::as_str) {
            Some("list") => flags(&rest[1..], &["--name"]).map(|f| ImgCmd::SnapshotList { name: f.name }),
            Some("rm") => flags(&rest[1..], &["--name", "--tag"]).and_then(|f| {
                let tag = f.tag.ok_or_else(|| format!("snapshot rm needs --tag\n{USAGE}"))?;
                Ok(ImgCmd::SnapshotRm { name: f.name, tag })
            }),
            _ => flags(rest, &["--name", "--tag"]).map(|f| ImgCmd::Snapshot { name: f.name, tag: f.tag }),
        },
        _ => return None,
    })
}

pub struct ImgEnv<'a> {
    pub paths: Option<&'a DosPaths>,
    pub procs: &'a dyn ProcInfo,
    pub is_root: bool,
    /// Injected for tests; production uses the wall clock.
    pub now_secs: Option<u64>,
}

fn now_secs(env: &ImgEnv) -> u64 {
    env.now_secs.unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

pub fn execute(cmd: &ImgCmd, env: &ImgEnv, input: &mut dyn BufRead, out: &mut String) -> i32 {
    if env.is_root {
        out.push_str("refusing to run as root: DOS mode keeps per-user data and must not run privileged\n");
        return 1;
    }
    let Some(paths) = env.paths else {
        out.push_str("cannot determine the per-user data directory\n");
        return 1;
    };
    let store = ImageStore::new(paths.clone());
    match run(cmd, env, &store, input, out) {
        Ok(code) => code,
        Err(e) => {
            out.push_str(&format!("{e}\n"));
            if let ImageError::Missing(_) = e {
                out.push_str("hint: run `bilterm dos setup` first\n");
            }
            1
        }
    }
}

fn run(cmd: &ImgCmd, env: &ImgEnv, store: &ImageStore, input: &mut dyn BufRead, out: &mut String) -> Result<i32, ImageError> {
    match cmd {
        ImgCmd::Snapshot { name, tag } => {
            let tag = tag.clone().unwrap_or_else(|| format!("snap-{}", now_secs(env)));
            let s = store.snapshot(name, &tag, env.procs)?;
            out.push_str(&format!("snapshot {:?} of {name:?} saved ({} bytes logical)\n", s.tag, s.bytes));
            Ok(0)
        }
        ImgCmd::SnapshotList { name } => {
            let l = store.list(name)?;
            if l.is_empty() {
                out.push_str("no snapshots\n");
            }
            for s in l {
                out.push_str(&format!("{}\t{} bytes\n", s.tag, s.bytes));
            }
            Ok(0)
        }
        ImgCmd::SnapshotRm { name, tag } => {
            store.remove_snapshot(name, tag, env.procs)?;
            out.push_str(&format!("removed snapshot {tag:?} of {name:?}\n"));
            Ok(0)
        }
        ImgCmd::Restore { name, tag, yes } => {
            if !store.snap_path(name, tag)?.is_file() {
                return Err(ImageError::Missing(format!("snapshot {tag:?} of {name:?}")));
            }
            if !yes && !confirm(&format!("Replace C: image {name:?} with snapshot {tag:?}? Current contents are lost. [y/N] "), input, out) {
                out.push_str("\naborted\n");
                return Ok(1);
            }
            store.restore(name, tag, env.procs)?;
            out.push_str(&format!("restored {name:?} from snapshot {tag:?}\n"));
            Ok(0)
        }
        ImgCmd::Reset { name, full, yes } => {
            let paths = &store.paths;
            let boot = load_cached(paths)?.ok_or_else(|| ImageError::Missing("cached FAT16 boot sector".into()))?;
            if !iso_path(paths).is_file() {
                return Err(ImageError::Missing(iso_path(paths).display().to_string()));
            }
            let kind = if *full { ImageKind::Full } else { ImageKind::Base };
            if store.exists(name)?
                && !yes
                && !confirm(&format!("Delete and rebuild C: image {name:?} from the pinned base? All data on C: is lost (snapshots are kept). [y/N] "), input, out)
            {
                out.push_str("\naborted\n");
                return Ok(1);
            }
            let src = LiveIsoSource::new(iso_path(paths), kind.sets());
            let st = store.reset(name, kind.bytes(), &boot, &src, env.procs)?;
            out.push_str(&format!("rebuilt {name:?}: {} files, {} bytes\n", st.files, st.bytes));
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_forms() {
        let p = |sub: &str, a: &[&str]| parse(sub, &s(a)).unwrap();
        assert_eq!(p("reset", &[]).unwrap(), ImgCmd::Reset { name: "default".into(), full: false, yes: false });
        assert_eq!(p("reset", &["--name", "x", "--full", "-y"]).unwrap(), ImgCmd::Reset { name: "x".into(), full: true, yes: true });
        assert_eq!(p("snapshot", &[]).unwrap(), ImgCmd::Snapshot { name: "default".into(), tag: None });
        assert_eq!(p("snapshot", &["--tag", "t1"]).unwrap(), ImgCmd::Snapshot { name: "default".into(), tag: Some("t1".into()) });
        assert_eq!(p("snapshot", &["list", "--name", "w"]).unwrap(), ImgCmd::SnapshotList { name: "w".into() });
        assert_eq!(p("snapshot", &["rm", "--tag", "t"]).unwrap(), ImgCmd::SnapshotRm { name: "default".into(), tag: "t".into() });
        assert_eq!(p("restore", &["--tag", "t"]).unwrap(), ImgCmd::Restore { name: "default".into(), tag: "t".into(), yes: false });
        assert!(p("restore", &[]).is_err());
        assert!(p("snapshot", &["rm"]).is_err());
        assert!(p("reset", &["--bogus"]).is_err());
        assert!(p("reset", &["--name"]).is_err());
        assert!(parse("setup", &[]).is_none());
    }
}
