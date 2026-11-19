//! Lifecycle of persistent C: images: paths, snapshot / restore / list / rm and reset.
//! Every name goes through `validate_safe_name`, every directory through `secure_fs`, and no
//! operation follows a symlink; mutating operations take the single-writer lock.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use super::build::{BuildOpts, Populate, PopulateStats, build_image};
use super::lock::{LockGuard, ProcInfo, acquire, holder};
use super::sanity::check_image;
use super::util::{copy_sparse, logical_len, remove_if_exists};
use super::{ImageError, MAX_IMAGE_BYTES, Res};
use crate::dos::paths::DosPaths;
use crate::secure_fs;

/// At most this many user snapshots per image, and this much logical size per image's snapshots.
pub const MAX_USER_SNAPSHOTS: usize = 16;
pub const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Automatic pre-run snapshots kept per image (rotation N).
pub const PRE_RUN_KEEP: usize = 2;
pub const PRE_RUN_PREFIX: &str = "pre-run-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapInfo {
    pub tag: String,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

pub struct ImageStore {
    pub paths: DosPaths,
}

pub fn valid_name(s: &str, what: &str) -> Res<()> {
    secure_fs::validate_safe_name(s).map_err(|e| ImageError::Invalid(format!("{what}: {e}")))
}

impl ImageStore {
    pub fn new(paths: DosPaths) -> Self {
        Self { paths }
    }

    pub fn image_path(&self, name: &str) -> Res<PathBuf> {
        valid_name(name, "image name")?;
        Ok(self.paths.c_dir().join(format!("{name}.img")))
    }
    pub fn lock_path(&self, name: &str) -> Res<PathBuf> {
        valid_name(name, "image name")?;
        Ok(self.paths.c_dir().join(format!("{name}.lock")))
    }
    pub fn dirty_path(&self, name: &str) -> Res<PathBuf> {
        valid_name(name, "image name")?;
        Ok(self.paths.c_dir().join(format!("{name}.dirty")))
    }
    pub fn snap_dir(&self, name: &str) -> Res<PathBuf> {
        valid_name(name, "image name")?;
        Ok(self.paths.snapshots_dir().join(name))
    }
    pub fn snap_path(&self, name: &str, tag: &str) -> Res<PathBuf> {
        valid_name(tag, "snapshot tag")?;
        Ok(self.snap_dir(name)?.join(format!("{tag}.img")))
    }

    pub fn ensure_dirs(&self) -> Res<()> {
        self.paths.ensure_layout()?;
        Ok(())
    }

    pub fn lock(&self, name: &str, procs: &dyn ProcInfo) -> Res<LockGuard> {
        self.ensure_dirs()?;
        acquire(&self.lock_path(name)?, procs)
    }

    pub fn is_locked(&self, name: &str, procs: &dyn ProcInfo) -> Res<Option<u32>> {
        Ok(holder(&self.lock_path(name)?, procs))
    }

    pub fn exists(&self, name: &str) -> Res<bool> {
        Ok(fs::symlink_metadata(self.image_path(name)?).is_ok_and(|m| m.is_file()))
    }

    /// Lists the snapshots of `name`, oldest first.
    pub fn list(&self, name: &str) -> Res<Vec<SnapInfo>> {
        let dir = self.snap_dir(name)?;
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for ent in rd.flatten().take(4096) {
            let Some(tag) = ent.file_name().to_str().and_then(|n| n.strip_suffix(".img")).map(str::to_string) else {
                continue;
            };
            let Ok(md) = fs::symlink_metadata(ent.path()) else { continue };
            if !md.is_file() || secure_fs::validate_safe_name(&tag).is_err() {
                continue;
            }
            out.push(SnapInfo { tag, bytes: md.len(), modified: md.modified().ok() });
        }
        out.sort_by(|a, b| a.modified.cmp(&b.modified).then(a.tag.cmp(&b.tag)));
        Ok(out)
    }

    fn total_snapshot_bytes(&self, name: &str) -> Res<u64> {
        Ok(self.list(name)?.iter().map(|s| s.bytes).sum())
    }

    /// Copies the image to `snapshots/<name>/<tag>.img`. Needs the lock (guest must be stopped).
    pub fn snapshot(&self, name: &str, tag: &str, procs: &dyn ProcInfo) -> Res<SnapInfo> {
        if tag.starts_with(PRE_RUN_PREFIX) {
            return Err(ImageError::Invalid(format!("tags starting with {PRE_RUN_PREFIX:?} are reserved")));
        }
        let _g = self.lock(name, procs)?;
        let user = self.list(name)?.iter().filter(|s| !s.tag.starts_with(PRE_RUN_PREFIX)).count();
        if user >= MAX_USER_SNAPSHOTS {
            return Err(ImageError::Invalid(format!("at most {MAX_USER_SNAPSHOTS} snapshots per image; remove one first")));
        }
        self.copy_in(name, tag, false)
    }

    pub(crate) fn copy_in(&self, name: &str, tag: &str, replace: bool) -> Res<SnapInfo> {
        let src = self.image_path(name)?;
        if !self.exists(name)? {
            return Err(ImageError::Missing(format!("image {name:?} (run `bilterm dos setup`)")));
        }
        let dst = self.snap_path(name, tag)?;
        secure_fs::ensure_private_dir(&self.paths.snapshots_dir())?;
        secure_fs::ensure_private_dir(&self.snap_dir(name)?)?;
        if !replace && fs::symlink_metadata(&dst).is_ok() {
            return Err(ImageError::Invalid(format!("snapshot {tag:?} already exists; remove it first")));
        }
        let len = logical_len(&src)?;
        if self.total_snapshot_bytes(name)? + len > MAX_SNAPSHOT_TOTAL_BYTES {
            return Err(ImageError::Invalid("snapshot size cap reached; remove old snapshots".into()));
        }
        let part = dst.with_extension("img.part");
        copy_sparse(&src, &part).inspect_err(|_| {
            let _ = fs::remove_file(&part);
        })?;
        remove_if_exists(&dst)?;
        fs::rename(&part, &dst)?;
        Ok(SnapInfo { tag: tag.to_string(), bytes: len, modified: fs::metadata(&dst)?.modified().ok() })
    }

    /// Replaces the image with snapshot `tag` after validating it. Clears the dirty marker.
    pub fn restore(&self, name: &str, tag: &str, procs: &dyn ProcInfo) -> Res<()> {
        let _g = self.lock(name, procs)?;
        let src = self.snap_path(name, tag)?;
        if !fs::symlink_metadata(&src).is_ok_and(|m| m.is_file()) {
            return Err(ImageError::Missing(format!("snapshot {tag:?} of {name:?}")));
        }
        let len = logical_len(&src)?;
        if len > MAX_IMAGE_BYTES {
            return Err(ImageError::Invalid("snapshot exceeds the image size cap".into()));
        }
        let rep = check_image(&src);
        if !rep.ok() {
            return Err(ImageError::Corrupt(rep.problems));
        }
        let dst = self.image_path(name)?;
        let part = dst.with_extension("img.part");
        copy_sparse(&src, &part).inspect_err(|_| {
            let _ = fs::remove_file(&part);
        })?;
        remove_if_exists(&dst)?;
        fs::rename(&part, &dst)?;
        remove_if_exists(&self.dirty_path(name)?)?;
        Ok(())
    }

    pub fn remove_snapshot(&self, name: &str, tag: &str, procs: &dyn ProcInfo) -> Res<()> {
        let _g = self.lock(name, procs)?;
        let p = self.snap_path(name, tag)?;
        match fs::symlink_metadata(&p) {
            Ok(m) if m.is_file() => Ok(fs::remove_file(&p)?),
            Ok(_) => Err(ImageError::Invalid(format!("{} is not a regular file", p.display()))),
            Err(_) => Err(ImageError::Missing(format!("snapshot {tag:?} of {name:?}"))),
        }
    }

    /// Rebuilds the image from `content`. The old image stays until the new one is complete;
    /// snapshots are kept; the dirty marker is cleared.
    pub fn reset(
        &self,
        name: &str,
        size_bytes: u64,
        boot: &[u8; 512],
        content: &dyn Populate,
        procs: &dyn ProcInfo,
    ) -> Res<PopulateStats> {
        let _g = self.lock(name, procs)?;
        let out = self.image_path(name)?;
        let st = build_image(&BuildOpts { out: &out, size_bytes, boot_sector: Some(boot) }, content)?;
        remove_if_exists(&self.dirty_path(name)?)?;
        Ok(st)
    }

    /// Names of images present in `c/` (for CLI messages).
    pub fn image_names(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.paths.c_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".img")).map(str::to_string))
            .collect();
        v.sort();
        v
    }
}
