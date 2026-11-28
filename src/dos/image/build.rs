//! Pure-Rust image builder: sparse raw file, hand-built MBR, `fatfs` FAT16 volume, optional
//! captured boot-sector code. Built as `<out>.part` then renamed (atomic, 0600).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use fscommon::{BufStream, StreamSlice};

use super::mbr::{HEADS, PART_START, SECTOR, SPT, build_mbr};
use super::util::{create_rw_private, remove_if_exists, set_sparse_len};
use super::zipread::fat_component_ok;
use super::{ImageError, MAX_IMAGE_BYTES, MIN_IMAGE_BYTES, Res};

type Stream = BufStream<StreamSlice<File>>;
type FatDir<'a> = fatfs::Dir<'a, Stream>;

/// Destination the content source writes into; keeps sources independent of `fatfs`.
pub trait FatSink {
    /// Creates every directory of a `/`-separated path (existing ones are fine).
    fn mkdir_all(&mut self, path: &str) -> Res<()>;
    /// Writes (or replaces) a file, creating parent directories. Returns bytes written.
    fn write_file(&mut self, path: &str, data: &mut dyn Read, max: u64) -> Res<u64>;
}

/// Content of an image: files to place after the (empty) FAT16 volume is formatted.
pub trait Populate {
    fn populate(&self, sink: &mut dyn FatSink) -> Res<PopulateStats>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopulateStats {
    pub files: u64,
    pub bytes: u64,
    /// Members skipped because FAT cannot store the name (never a security skip).
    pub skipped: u64,
}

struct Sink<'a> {
    root: FatDir<'a>,
}

fn fat_err(e: impl std::fmt::Display) -> ImageError {
    ImageError::Format(format!("fat: {e}"))
}

impl FatSink for Sink<'_> {
    fn mkdir_all(&mut self, path: &str) -> Res<()> {
        let mut cur = self.root.clone();
        for c in path.split('/').filter(|c| !c.is_empty()) {
            if !fat_component_ok(c) {
                return Err(ImageError::Invalid(format!("bad directory name {c:?}")));
            }
            cur = cur.create_dir(c).map_err(fat_err)?;
        }
        Ok(())
    }

    fn write_file(&mut self, path: &str, data: &mut dyn Read, max: u64) -> Res<u64> {
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        let (name, dirs) = comps
            .split_last()
            .ok_or_else(|| ImageError::Invalid("empty path".into()))?;
        let mut cur = self.root.clone();
        for c in dirs {
            if !fat_component_ok(c) {
                return Err(ImageError::Invalid(format!("bad directory name {c:?}")));
            }
            cur = cur.create_dir(c).map_err(fat_err)?;
        }
        if !fat_component_ok(name) {
            return Err(ImageError::Invalid(format!("bad file name {name:?}")));
        }
        let mut f = cur.create_file(name).map_err(fat_err)?;
        f.truncate().map_err(fat_err)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > max {
                return Err(ImageError::Format(format!("{path}: exceeds {max} bytes")));
            }
            f.write_all(&buf[..n]).map_err(fat_err)?;
        }
        f.flush().map_err(fat_err)?;
        Ok(total)
    }
}

pub struct BuildOpts<'a> {
    pub out: &'a Path,
    pub size_bytes: u64,
    /// Captured FreeDOS FAT16 boot sector (see `bootsector`); `None` builds a non-bootable volume.
    pub boot_sector: Option<&'a [u8; 512]>,
}

/// Builds `opts.out`. The destination is only replaced after the new image is complete.
pub fn build_image(opts: &BuildOpts, content: &dyn Populate) -> Res<PopulateStats> {
    let size = opts.size_bytes;
    if !(MIN_IMAGE_BYTES..=MAX_IMAGE_BYTES).contains(&size) || !size.is_multiple_of(SECTOR) {
        return Err(ImageError::Invalid(format!(
            "image size {size} must be a multiple of 512 within {MIN_IMAGE_BYTES}..={MAX_IMAGE_BYTES}"
        )));
    }
    let part = opts.out.with_extension("img.part");
    let res = build_into(&part, opts, content);
    match res {
        Ok(stats) => {
            remove_if_exists(opts.out)?;
            std::fs::rename(&part, opts.out)?;
            Ok(stats)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn build_into(part: &Path, opts: &BuildOpts, content: &dyn Populate) -> Res<PopulateStats> {
    let size = opts.size_bytes;
    let mut f = create_rw_private(part)?;
    set_sparse_len(&f, part, size)?;
    f.write_all(&build_mbr(size))?;
    f.flush()?;

    let shared = f.try_clone()?;
    let slice = || -> Res<Stream> {
        let s = StreamSlice::new(shared.try_clone()?, PART_START * SECTOR, size).map_err(fat_err)?;
        Ok(BufStream::new(s))
    };
    let mut vol = slice()?;
    let fmt = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat16)
        .bytes_per_sector(512)
        .volume_label(*b"FREEDOS C  ");
    fatfs::format_volume(&mut vol, fmt).map_err(fat_err)?;
    vol.flush()?;
    drop(vol);

    // BPB geometry / hidden sectors / drive number, then (optionally) the boot code.
    let mut s0 = [0u8; 512];
    f.seek(SeekFrom::Start(PART_START * SECTOR))?;
    f.read_exact(&mut s0)?;
    s0[0x18..0x1A].copy_from_slice(&SPT.to_le_bytes());
    s0[0x1A..0x1C].copy_from_slice(&HEADS.to_le_bytes());
    s0[0x1C..0x20].copy_from_slice(&(PART_START as u32).to_le_bytes());
    s0[0x24] = 0x80;
    if let Some(b) = opts.boot_sector {
        s0[0..3].copy_from_slice(&b[0..3]);
        s0[3..11].copy_from_slice(&b[3..11]);
        s0[0x3E..0x1FE].copy_from_slice(&b[0x3E..0x1FE]);
    }
    f.seek(SeekFrom::Start(PART_START * SECTOR))?;
    f.write_all(&s0)?;
    f.flush()?;

    let fs = fatfs::FileSystem::new(slice()?, fatfs::FsOptions::new()).map_err(fat_err)?;
    let stats = {
        let mut sink = Sink { root: fs.root_dir() };
        content.populate(&mut sink)?
    };
    fs.unmount().map_err(fat_err)?;
    f.sync_all()?;
    Ok(stats)
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// Minimal content: the two files FreeDOS needs plus a nested directory.
    pub struct Tiny;
    impl Populate for Tiny {
        fn populate(&self, s: &mut dyn FatSink) -> Res<PopulateStats> {
            s.write_file("KERNEL.SYS", &mut &b"kernel"[..], 100)?;
            s.write_file("COMMAND.COM", &mut &b"command"[..], 100)?;
            s.write_file("FREEDOS/BIN/X.EXE", &mut &b"xx"[..], 100)?;
            s.mkdir_all("FREEDOS/TEMP")?;
            Ok(PopulateStats { files: 3, ..Default::default() })
        }
    }

    /// A stand-in FAT16 boot sector (NOT FreeDOS code).
    pub fn fake_boot16() -> [u8; 512] {
        let mut b = [0u8; 512];
        b[0] = 0xEB;
        b[1] = 0x3C;
        b[2] = 0x90;
        b[3..11].copy_from_slice(b"FDOS    ");
        b[0x36..0x3E].copy_from_slice(b"FAT16   ");
        for (i, x) in b[0x3E..0x1FE].iter_mut().enumerate() {
            *x = (i % 251) as u8 | 1;
        }
        b[510] = 0x55;
        b[511] = 0xAA;
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dos::image::mbr::{has_boot_code, validate_fat16_boot, validate_mbr};
    use crate::dos::image::sanity::check_image;

    use super::tests_support::{Tiny, fake_boot16 as fake_boot};

    #[test]
    fn builds_structurally_valid_image() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("c.img");
        let boot = fake_boot();
        let size = 64 * 1024 * 1024;
        build_image(&BuildOpts { out: &out, size_bytes: size, boot_sector: Some(&boot) }, &Tiny).unwrap();
        assert!(!d.path().join("c.img.part").exists());
        let raw = std::fs::read(&out).unwrap();
        assert_eq!(raw.len() as u64, size);
        let mbr: [u8; 512] = raw[..512].try_into().unwrap();
        let part = validate_mbr(&mbr, size).unwrap();
        let off = (part.start_lba * 512) as usize;
        let bs: [u8; 512] = raw[off..off + 512].try_into().unwrap();
        let geo = validate_fat16_boot(&bs, part.sectors).unwrap();
        assert!(has_boot_code(&bs));
        assert_eq!(&bs[0x36..0x3B], b"FAT16");
        assert_eq!(&bs[3..11], b"FDOS    ");
        assert_eq!(u16::from_le_bytes([bs[0x18], bs[0x19]]), 63);
        assert_eq!(u32::from_le_bytes(bs[0x1C..0x20].try_into().unwrap()), 2048);
        assert_eq!(bs[0x24], 0x80);
        assert!(geo.clusters >= 4085);
        let rep = check_image(&out);
        assert!(rep.ok(), "{:?}", rep.problems);
        assert_eq!(rep.root_names[0], "KERNEL.SYS");
        assert!(rep.root_names.contains(&"COMMAND.COM".to_string()));
        assert!(rep.root_names.contains(&"FREEDOS".to_string()));
    }

    #[test]
    fn without_boot_sector_is_flagged_unbootable() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("c.img");
        build_image(&BuildOpts { out: &out, size_bytes: 32 << 20, boot_sector: None }, &Tiny).unwrap();
        let rep = check_image(&out);
        assert!(rep.problems.iter().any(|p| p.contains("no boot code")), "{:?}", rep.problems);
    }

    #[test]
    fn size_validation_and_failed_build_leaves_no_part() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("c.img");
        for bad in [0, 1 << 20, MAX_IMAGE_BYTES + 512, (32 << 20) + 1] {
            assert!(build_image(&BuildOpts { out: &out, size_bytes: bad, boot_sector: None }, &Tiny).is_err());
        }
        struct Fail;
        impl Populate for Fail {
            fn populate(&self, _: &mut dyn FatSink) -> Res<PopulateStats> {
                Err(ImageError::Format("boom".into()))
            }
        }
        std::fs::write(&out, b"keep me").unwrap();
        assert!(build_image(&BuildOpts { out: &out, size_bytes: 32 << 20, boot_sector: None }, &Fail).is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"keep me");
        assert!(!d.path().join("c.img.part").exists());
    }

    #[cfg(unix)]
    #[test]
    fn image_is_private_and_sparse() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("c.img");
        build_image(&BuildOpts { out: &out, size_bytes: 128 << 20, boot_sector: None }, &Tiny).unwrap();
        let md = std::fs::metadata(&out).unwrap();
        assert_eq!(md.permissions().mode() & 0o777, 0o600);
        assert!(md.blocks() * 512 < 64 << 20, "image not sparse: {} blocks", md.blocks());
    }

    #[cfg(windows)]
    #[test]
    fn image_is_sparse_on_windows() {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("c.img");
        build_image(&BuildOpts { out: &out, size_bytes: 128 << 20, boot_sector: None }, &Tiny).unwrap();
        let md = std::fs::metadata(&out).unwrap();
        assert_eq!(md.len(), 128 << 20);
        assert_ne!(md.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE, 0, "expected sparse attribute on NTFS: {:#x}", md.file_attributes());
    }
}
