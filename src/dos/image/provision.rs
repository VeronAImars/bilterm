//! `dos setup` image provisioning: extract the pinned members, capture the FAT16 boot sector once
//! through a [`DosBackend`] (cached, never committed), and build the default C: image.

use std::path::PathBuf;

use super::bootsector::{CaptureCfg, Step, capture_via_guest, default_steps, load_cached, store_cached};
use super::build::{BuildOpts, build_image};
use super::extract::extract_member;
use super::livecd::LiveIsoSource;
use super::lock::ProcInfo;
use super::store::ImageStore;
use super::util::remove_if_exists;
use super::{DEFAULT_NAME, ImageKind, ImageError, Res};
use crate::dos::backend::qemu::{QemuBackend, QemuMode};
use crate::dos::backend::{DosBackend, random_hex};
use crate::dos::manifest::Manifest;
use crate::dos::paths::DosPaths;

pub const LIVECD_ENTRY: &str = "freedos-14-livecd";
pub const ISO_NAME: &str = "FD14LIVE.iso";
pub const FLOPPY_NAME: &str = "FD14BOOT.img";

/// Creates the emulator backend used for the one-off boot-sector capture.
pub trait BackendFactory {
    fn make(&self) -> Result<Box<dyn DosBackend>, String>;
}

/// Production factory: QEMU in text-buffer mode (`-display none`, QMP) so no terminal is needed.
pub struct QemuFactory {
    pub program: Option<PathBuf>,
    /// Why the emulator is unusable (not found, below the version floor).
    pub problem: Option<String>,
    pub base: PathBuf,
}

impl BackendFactory for QemuFactory {
    fn make(&self) -> Result<Box<dyn DosBackend>, String> {
        if let Some(p) = &self.problem {
            return Err(p.clone());
        }
        let prog = self.program.clone().ok_or("qemu-system-i386 not found")?;
        Ok(Box::new(QemuBackend::new(QemuMode::TextBuf, prog, self.base.clone())))
    }
}

pub struct Provision<'a> {
    pub paths: &'a DosPaths,
    pub manifest: &'a Manifest,
    pub factory: &'a dyn BackendFactory,
    pub procs: &'a dyn ProcInfo,
    pub steps: Vec<Step>,
    pub cfg: CaptureCfg,
}

impl<'a> Provision<'a> {
    pub fn new(paths: &'a DosPaths, manifest: &'a Manifest, factory: &'a dyn BackendFactory, procs: &'a dyn ProcInfo) -> Self {
        Self { paths, manifest, factory, procs, steps: default_steps(), cfg: CaptureCfg::default() }
    }
}

pub fn iso_path(paths: &DosPaths) -> PathBuf {
    paths.images_dir().join(ISO_NAME)
}
pub fn floppy_path(paths: &DosPaths) -> PathBuf {
    paths.images_dir().join(FLOPPY_NAME)
}

/// Extracts the pinned members from the downloaded Live CD zip into `images/`.
pub fn extract_members(paths: &DosPaths, manifest: &Manifest, progress: &mut dyn FnMut(&str)) -> Res<()> {
    let e = manifest
        .entry(LIVECD_ENTRY)
        .ok_or_else(|| ImageError::Missing(format!("manifest entry {LIVECD_ENTRY}")))?;
    let zip = paths.download_path(&e.file).map_err(ImageError::Invalid)?;
    if !zip.is_file() {
        return Err(ImageError::Missing(format!("{} (run `bilterm dos setup`)", zip.display())));
    }
    for m in &e.members {
        progress(&format!("extracting {} ({} bytes)", m.name, m.bytes));
        extract_member(&zip, m, &paths.images_dir())?;
    }
    Ok(())
}

/// Returns the cached FAT16 boot sector, capturing it through the backend on first use.
pub fn ensure_boot_sector(p: &Provision, progress: &mut dyn FnMut(&str)) -> Res<[u8; 512]> {
    if let Some(b) = load_cached(p.paths)? {
        return Ok(b);
    }
    progress("capturing the FreeDOS FAT16 boot sector once (boots the install floppy, runs `sys c:`)");
    let mut backend = p.factory.make().map_err(ImageError::Backend)?;
    let scratch = p.paths.sessions_dir().join(format!("capture-{}.img", random_hex(6)));
    let src = LiveIsoSource::new(iso_path(p.paths), &[]);
    let built = build_image(&BuildOpts { out: &scratch, size_bytes: super::BASE_BYTES, boot_sector: None }, &src);
    let res = built.and_then(|_| {
        capture_via_guest(backend.as_mut(), &floppy_path(p.paths), &scratch, &p.steps, p.cfg, progress)
    });
    let _ = remove_if_exists(&scratch);
    let sector = res?;
    store_cached(p.paths, &sector)?;
    Ok(sector)
}

/// Full setup tail: extract, capture, build `default.img` unless it already exists.
/// Returns a human summary line.
pub fn provision(p: &Provision, kind: ImageKind, progress: &mut dyn FnMut(&str)) -> Res<String> {
    p.paths.ensure_layout()?;
    extract_members(p.paths, p.manifest, progress)?;
    let boot = ensure_boot_sector(p, progress)?;
    let store = ImageStore::new(p.paths.clone());
    if store.exists(DEFAULT_NAME)? {
        return Ok(format!("C: image {DEFAULT_NAME:?} already exists (`bilterm dos reset` rebuilds it)"));
    }
    progress(&format!("building the {} MiB C: image", kind.bytes() >> 20));
    let src = LiveIsoSource::new(iso_path(p.paths), kind.sets());
    let st = store.reset(DEFAULT_NAME, kind.bytes(), &boot, &src, p.procs)?;
    Ok(format!(
        "built C: image {DEFAULT_NAME:?}: {} files, {} bytes of content, {} unstorable names skipped",
        st.files, st.bytes, st.skipped
    ))
}
