//! Run-time protocol around a persistent image (used by `dos run`, D2a): take the lock, detect an
//! unclean previous exit via the `.dirty` marker, sanity-check before trusting the image, rotate
//! the automatic `pre-run` snapshots, set the marker, and clear it only on a clean exit.

use std::fs;
use std::path::{Path, PathBuf};

use super::lock::{LockGuard, ProcInfo};
use super::sanity::{SanityReport, check_image, find_fsck, fsck_partition};
use super::store::{ImageStore, PRE_RUN_KEEP, PRE_RUN_PREFIX, valid_name};
use super::util::{logical_len, remove_if_exists};
use super::{ImageError, Res};
use crate::secure_fs;

/// Images larger than this (logical) are not auto-snapshotted before a run.
pub const PRE_RUN_MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct StartReport {
    pub was_dirty: bool,
    pub sanity: Option<SanityReport>,
    /// Newest snapshot the user could restore if the image is damaged.
    pub restore_candidate: Option<String>,
    pub pre_run: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug)]
pub struct RunGuard {
    _lock: LockGuard,
    dirty: PathBuf,
}

impl RunGuard {
    /// Clean exit: clear the marker, then release the lock. Dropping without calling this
    /// leaves the marker (a crash or kill looks exactly like that).
    pub fn finish_clean(self) -> Res<()> {
        remove_if_exists(&self.dirty)
    }
}

pub struct RunOpts<'a> {
    /// Start even if a dirty image fails the sanity check.
    pub allow_corrupt: bool,
    /// `fsck.fat` to use for the extra read-only pass; `None` looks it up on `PATH`.
    pub fsck: Option<&'a Path>,
}

fn latest_snapshot(store: &ImageStore, name: &str) -> Option<String> {
    store.list(name).ok()?.into_iter().last().map(|s| s.tag)
}

pub fn begin_run(
    store: &ImageStore,
    name: &str,
    procs: &dyn ProcInfo,
    opts: &RunOpts,
) -> Res<(RunGuard, StartReport)> {
    valid_name(name, "image name")?;
    if !store.exists(name)? {
        return Err(ImageError::Missing(format!("image {name:?} (run `bilterm dos setup`)")));
    }
    let lock = store.lock(name, procs)?;
    let dirty_path = store.dirty_path(name)?;
    let img = store.image_path(name)?;
    let mut rep = StartReport {
        was_dirty: fs::symlink_metadata(&dirty_path).is_ok(),
        restore_candidate: latest_snapshot(store, name),
        ..StartReport::default()
    };
    if rep.was_dirty {
        let mut san = check_image(&img);
        let fsck = opts.fsck.map(Path::to_path_buf).or_else(find_fsck);
        match fsck {
            Some(f) => {
                if let Err(e) = fsck_partition(&img, &f, &store.paths.sessions_dir(), &mut san) {
                    san.notes.push(format!("fsck pass failed to run: {e}"));
                }
            }
            None => san.notes.push("fsck.fat not found: built-in sanity check only".into()),
        }
        let ok = san.ok();
        rep.notes.push("previous session did not exit cleanly".into());
        if !ok && !opts.allow_corrupt {
            let hint = match &rep.restore_candidate {
                Some(t) => format!("restore with `bilterm dos restore --tag {t}` (latest snapshot)"),
                None => "no snapshot exists; `bilterm dos reset` rebuilds a fresh image".to_string(),
            };
            let mut problems = san.problems.clone();
            problems.push(hint);
            return Err(ImageError::Corrupt(problems));
        }
        rep.sanity = Some(san);
    } else if let Err(e) = rotate_pre_run(store, name) {
        rep.notes.push(format!("pre-run snapshot skipped: {e}"));
    } else {
        rep.pre_run = Some(format!("{PRE_RUN_PREFIX}1"));
    }
    set_marker(&dirty_path)?;
    Ok((RunGuard { _lock: lock, dirty: dirty_path }, rep))
}

fn set_marker(p: &Path) -> Res<()> {
    secure_fs::write_private_file(p, b"dirty: a bilterm dos session is or was running\n")?;
    Ok(())
}

/// pre-run-1 is the newest; older ones shift up and the oldest beyond `PRE_RUN_KEEP` is dropped.
fn rotate_pre_run(store: &ImageStore, name: &str) -> Res<()> {
    let len = logical_len(&store.image_path(name)?)?;
    if len > PRE_RUN_MAX_BYTES {
        return Err(ImageError::Invalid(format!("image is {len} bytes, over the pre-run snapshot cap")));
    }
    let tag = |i: usize| format!("{PRE_RUN_PREFIX}{i}");
    let path = |i: usize| store.snap_path(name, &tag(i));
    secure_fs::ensure_private_dir(&store.paths.snapshots_dir())?;
    secure_fs::ensure_private_dir(&store.snap_dir(name)?)?;
    remove_if_exists(&path(PRE_RUN_KEEP)?)?;
    for i in (1..PRE_RUN_KEEP).rev() {
        let from = path(i)?;
        if fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
            fs::rename(&from, path(i + 1)?)?;
        }
    }
    store.copy_in(name, &tag(1), true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::build::tests_support::fake_boot16;
    use super::super::lock::fake::FakeProcs;
    use super::super::tests_e2e::{fixture, Fixture};
    use super::*;

    fn opts() -> RunOpts<'static> {
        RunOpts { allow_corrupt: false, fsck: None }
    }

    #[test]
    fn clean_cycle_sets_and_clears_marker_and_rotates() {
        let fx = fixture();
        let procs = FakeProcs::new(10);
        let (g, rep) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        assert!(!rep.was_dirty && rep.pre_run.is_some());
        assert!(fx.store.dirty_path("default").unwrap().exists());
        // second runner is refused while the first holds the lock
        let other = FakeProcs::new(11);
        let mut other = other;
        other.procs.insert(10, "100".into());
        assert!(matches!(begin_run(&fx.store, "default", &other, &opts()), Err(ImageError::LockHeld { .. })));
        g.finish_clean().unwrap();
        assert!(!fx.store.dirty_path("default").unwrap().exists());
        assert!(!fx.store.lock_path("default").unwrap().exists());
    }

    #[test]
    fn pre_run_rotation_keeps_n_and_never_more() {
        let fx = fixture();
        let procs = FakeProcs::new(10);
        for _ in 0..5 {
            let (g, _) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
            g.finish_clean().unwrap();
        }
        let tags: Vec<String> = fx.store.list("default").unwrap().into_iter().map(|s| s.tag).collect();
        assert_eq!(tags.len(), PRE_RUN_KEEP, "{tags:?}");
        assert!(tags.iter().all(|t| t.starts_with(PRE_RUN_PREFIX)));
    }

    #[test]
    fn crash_leaves_marker_and_next_start_runs_sanity() {
        let fx = fixture();
        let procs = FakeProcs::new(10);
        let (g, _) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        drop(g); // crash: lock released by drop, marker stays
        assert!(fx.store.dirty_path("default").unwrap().exists());
        let (g, rep) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        assert!(rep.was_dirty && rep.pre_run.is_none());
        assert!(rep.sanity.as_ref().unwrap().ok());
        g.finish_clean().unwrap();
    }

    #[test]
    fn dirty_and_corrupt_is_refused_with_restore_hint() {
        let fx = fixture();
        let procs = FakeProcs::new(10);
        fx.store.snapshot("default", "good", &procs).unwrap();
        let (g, _) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        drop(g);
        // smash the partition boot sector
        let img = fx.store.image_path("default").unwrap();
        let mut raw = fs::read(&img).unwrap();
        raw[2048 * 512..2048 * 512 + 512].fill(0xAA);
        fs::write(&img, &raw).unwrap();
        let err = begin_run(&fx.store, "default", &procs, &opts()).unwrap_err();
        let ImageError::Corrupt(p) = &err else { panic!("{err}") };
        assert!(p.iter().any(|l| l.contains("restore")), "{p:?}");
        // lock must be released after the refusal
        assert!(!fx.store.lock_path("default").unwrap().exists());
        // restore from the snapshot repairs it and clears the marker
        fx.store.restore("default", "good", &procs).unwrap();
        assert!(!fx.store.dirty_path("default").unwrap().exists());
        let (g, rep) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        assert!(!rep.was_dirty);
        g.finish_clean().unwrap();
        let _: &Fixture = &fx;
        let _ = fake_boot16();
    }

    #[test]
    fn allow_corrupt_overrides_and_unknown_image_errors() {
        let fx = fixture();
        let procs = FakeProcs::new(10);
        let (g, _) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        drop(g);
        let img = fx.store.image_path("default").unwrap();
        let mut raw = fs::read(&img).unwrap();
        raw[510] = 0;
        fs::write(&img, &raw).unwrap();
        let o = RunOpts { allow_corrupt: true, fsck: None };
        let (g, rep) = begin_run(&fx.store, "default", &procs, &o).unwrap();
        assert!(!rep.sanity.unwrap().ok());
        drop(g);
        assert!(matches!(begin_run(&fx.store, "nope", &procs, &opts()), Err(ImageError::Missing(_))));
        assert!(begin_run(&fx.store, "../x", &procs, &opts()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn fsck_pass_uses_the_tool_when_present() {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture();
        let procs = FakeProcs::new(10);
        let (g, _) = begin_run(&fx.store, "default", &procs, &opts()).unwrap();
        drop(g);
        let tool = fx.dir.path().join("fake-fsck");
        fs::write(&tool, "#!/bin/sh\necho 'bad cluster chain'\nexit 1\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        let o = RunOpts { allow_corrupt: false, fsck: Some(&tool) };
        let err = begin_run(&fx.store, "default", &procs, &o).unwrap_err();
        assert!(err.to_string().contains("fsck.fat -n reported errors"), "{err}");
        fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        let (g, rep) = begin_run(&fx.store, "default", &procs, &o).unwrap();
        assert!(rep.sanity.unwrap().notes.iter().any(|n| n.contains("clean")));
        g.finish_clean().unwrap();
        // no scratch copies left behind
        assert_eq!(fs::read_dir(fx.store.paths.sessions_dir()).unwrap().count(), 0);
    }
}
