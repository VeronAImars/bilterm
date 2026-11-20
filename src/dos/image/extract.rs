//! Extracts pinned members (FD14BOOT.img, FD14LIVE.iso) from the downloaded Live CD zip by
//! explicit name: size-capped, hash-verified before it becomes visible, atomic rename.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::util::{create_rw_private, open_regular_read, remove_if_exists};
use super::zipread::ZipArchive;
use super::{ImageError, Res};
use crate::dos::manifest::Member;
use crate::dos::verify::verify_file;
use crate::secure_fs;

/// Extracts `m` from `zip_path` into `dest_dir`; a verified copy already there is kept.
pub fn extract_member(zip_path: &Path, m: &Member, dest_dir: &Path) -> Res<PathBuf> {
    secure_fs::validate_plain_file_name(&m.name)
        .map_err(|_| ImageError::Invalid(format!("unsafe member name {:?}", m.name)))?;
    let dest = dest_dir.join(&m.name);
    if verify_file(&dest, &m.sha256, m.bytes).is_ok() {
        return Ok(dest);
    }
    let mut zip = ZipArchive::open(open_regular_read(zip_path)?)?;
    let entry = zip
        .find(&m.name)
        .cloned()
        .ok_or_else(|| ImageError::Missing(format!("{} not in {}", m.name, zip_path.display())))?;
    if entry.size != m.bytes {
        return Err(ImageError::Format(format!(
            "{}: archive says {} bytes, manifest pins {}",
            m.name, entry.size, m.bytes
        )));
    }
    let part = dest_dir.join(format!("{}.part", m.name));
    let result = (|| -> Res<()> {
        let mut out = create_rw_private(&part)?;
        let mut rd = zip.open_entry(&entry, m.bytes)?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = rd.read(&mut buf).map_err(|e| match e.kind() {
                io::ErrorKind::InvalidData => ImageError::Format(e.to_string()),
                _ => ImageError::from(e),
            })?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
        out.flush()?;
        out.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    if let Err(e) = verify_file(&part, &m.sha256, m.bytes) {
        let _ = std::fs::remove_file(&part);
        return Err(ImageError::Format(format!("{}: {e}", m.name)));
    }
    remove_if_exists(&dest)?;
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::make_zip;
    use super::*;
    use crate::dos::verify::sha256_bytes;

    fn member(name: &str, data: &[u8]) -> Member {
        Member {
            name: name.into(),
            sha256: sha256_bytes(data),
            bytes: data.len() as u64,
        }
    }

    #[test]
    fn extracts_verifies_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("l.zip");
        std::fs::write(&z, make_zip(&[("FD14BOOT.img", b"floppy-bytes", true), ("x", b"y", false)])).unwrap();
        let m = member("FD14BOOT.img", b"floppy-bytes");
        let p = extract_member(&z, &m, d.path()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"floppy-bytes");
        assert!(!d.path().join("FD14BOOT.img.part").exists());
        std::fs::remove_file(&z).unwrap(); // second call must not need the zip
        assert_eq!(extract_member(&z, &m, d.path()).unwrap(), p);
    }

    #[test]
    fn rejects_wrong_hash_size_missing_and_unsafe_names() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("l.zip");
        std::fs::write(&z, make_zip(&[("a.bin", b"data", false)])).unwrap();
        let mut m = member("a.bin", b"data");
        m.sha256 = sha256_bytes(b"other");
        assert!(extract_member(&z, &m, d.path()).is_err());
        assert!(!d.path().join("a.bin").exists() && !d.path().join("a.bin.part").exists());
        let mut m = member("a.bin", b"data");
        m.bytes = 5;
        assert!(extract_member(&z, &m, d.path()).is_err());
        assert!(extract_member(&z, &member("nope", b"x"), d.path()).is_err());
        assert!(extract_member(&z, &member("../a.bin", b"data"), d.path()).is_err());
    }
}
