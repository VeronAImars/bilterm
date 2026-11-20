//! Read-only image sanity: built-in MBR/BPB/FAT/root-directory checks, plus an optional
//! `fsck.fat -n` pass when the tool exists. Images are guest-controlled data, so this reader is
//! bounded (fixed small reads, no recursion) and never mounts the volume on the host.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::mbr::{
    Fat16Geometry, PartInfo, SECTOR, has_boot_code, validate_fat16_boot, validate_mbr,
};
use super::util::{open_regular_read, set_sparse_len};
use super::{MAX_IMAGE_BYTES, MIN_IMAGE_BYTES, Res};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SanityReport {
    pub problems: Vec<String>,
    pub notes: Vec<String>,
    /// Names found in the root directory (8.3, upper case, bounded).
    pub root_names: Vec<String>,
}

impl SanityReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn read_at(f: &mut std::fs::File, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
    f.seek(SeekFrom::Start(off))?;
    f.read_exact(buf)
}

/// Built-in structural check of a raw C: image.
pub fn check_image(path: &Path) -> SanityReport {
    let mut rep = SanityReport::default();
    let mut f = match open_regular_read(path) {
        Ok(f) => f,
        Err(e) => {
            rep.problems.push(e.to_string());
            return rep;
        }
    };
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            rep.problems.push(format!("cannot stat image: {e}"));
            return rep;
        }
    };
    if !(MIN_IMAGE_BYTES..=MAX_IMAGE_BYTES).contains(&len) {
        rep.problems.push(format!(
            "image size {len} outside {MIN_IMAGE_BYTES}..={MAX_IMAGE_BYTES}"
        ));
        return rep;
    }
    let mut mbr = [0u8; 512];
    if let Err(e) = read_at(&mut f, 0, &mut mbr) {
        rep.problems.push(format!("cannot read MBR: {e}"));
        return rep;
    }
    let part: PartInfo = match validate_mbr(&mbr, len) {
        Ok(p) => p,
        Err(e) => {
            rep.problems.push(e);
            return rep;
        }
    };
    let mut bs = [0u8; 512];
    if let Err(e) = read_at(&mut f, part.start_lba * SECTOR, &mut bs) {
        rep.problems.push(format!("cannot read partition boot sector: {e}"));
        return rep;
    }
    let geo = match validate_fat16_boot(&bs, part.sectors) {
        Ok(g) => g,
        Err(e) => {
            rep.problems.push(e);
            return rep;
        }
    };
    if !has_boot_code(&bs) {
        rep.problems
            .push("partition boot sector has no boot code (image will not boot)".into());
    }
    check_fat_and_root(&mut f, part, geo, &mut rep);
    rep
}

fn check_fat_and_root(
    f: &mut std::fs::File,
    part: PartInfo,
    geo: Fat16Geometry,
    rep: &mut SanityReport,
) {
    let base = part.start_lba * SECTOR;
    let mut fat0 = [0u8; 4];
    if read_at(f, base + geo.fat_start * SECTOR, &mut fat0).is_err() {
        rep.problems.push("cannot read FAT".into());
        return;
    }
    if fat0[0] != geo.media || fat0[1] != 0xFF {
        rep.problems
            .push("FAT entry 0 does not match the media descriptor".into());
    }
    if geo.num_fats == 2 {
        let mut fat1 = [0u8; 4];
        let off = base + (geo.fat_start + geo.sectors_per_fat) * SECTOR;
        if read_at(f, off, &mut fat1).is_err() || fat1 != fat0 {
            rep.problems.push("second FAT copy disagrees with the first".into());
        }
    }
    let mut root = vec![0u8; (geo.root_sectors * SECTOR) as usize];
    if read_at(f, base + geo.root_start * SECTOR, &mut root).is_err() {
        rep.problems.push("cannot read root directory".into());
        return;
    }
    for e in root.chunks_exact(32) {
        match e[0] {
            0x00 => break,
            0xE5 => continue,
            _ => {}
        }
        if e[11] == 0x0F || e[11] & 0x08 != 0 {
            continue; // long-name piece or volume label
        }
        let name = String::from_utf8_lossy(&e[0..8]).trim_end().to_string();
        let ext = String::from_utf8_lossy(&e[8..11]).trim_end().to_string();
        let full = if ext.is_empty() {
            name
        } else {
            format!("{name}.{ext}")
        };
        if rep.root_names.len() < 4096 {
            rep.root_names.push(full);
        }
    }
    for need in ["KERNEL.SYS", "COMMAND.COM"] {
        if !rep.root_names.iter().any(|n| n == need) {
            rep.problems.push(format!("{need} missing from the root directory"));
        }
    }
}

/// Finds `fsck.fat` (or `dosfsck`) on `PATH`.
pub fn find_fsck() -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for n in ["fsck.fat", "dosfsck"] {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Runs `fsck -n` (read-only) on a sparse copy of the partition. `scratch` must be a private
/// directory; the copy is removed afterwards. Returns notes/problems appended to `rep`.
pub fn fsck_partition(img: &Path, fsck: &Path, scratch: &Path, rep: &mut SanityReport) -> Res<()> {
    let mut f = open_regular_read(img)?;
    let mut mbr = [0u8; 512];
    read_at(&mut f, 0, &mut mbr)?;
    let Ok(part) = validate_mbr(&mbr, f.metadata()?.len()) else {
        return Ok(()); // already reported by check_image
    };
    let copy = scratch.join(format!("fsck-{}.part", crate::secure_fs::unique_suffix()));
    let len = part.sectors * SECTOR;
    let mut out = super::util::create_rw_private(&copy)?;
    set_sparse_len(&out, &copy, len)?;
    f.seek(SeekFrom::Start(part.start_lba * SECTOR))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    while done < len {
        let n = buf.len().min((len - done) as usize);
        f.read_exact(&mut buf[..n])?;
        if buf[..n].iter().any(|b| *b != 0) {
            out.seek(SeekFrom::Start(done))?;
            std::io::Write::write_all(&mut out, &buf[..n])?;
        }
        done += n as u64;
    }
    drop(out);
    let res = std::process::Command::new(fsck).arg("-n").arg(&copy).output();
    let _ = std::fs::remove_file(&copy);
    match res {
        Ok(o) if o.status.success() => rep.notes.push("fsck.fat -n: clean".into()),
        Ok(o) => {
            let tail: String = String::from_utf8_lossy(&o.stdout)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ");
            let tail: String = tail.chars().take(300).collect();
            rep.problems.push(format!("fsck.fat -n reported errors: {tail}"));
        }
        Err(e) => rep.notes.push(format!("fsck.fat not runnable: {e}")),
    }
    Ok(())
}
