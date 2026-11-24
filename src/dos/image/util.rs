//! Small I/O helpers: bounded slice view, no-follow opens, sparse copy.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::{ImageError, Res};
use crate::secure_fs;

/// Read+Seek view of `[start, start+len)` of an inner reader.
pub struct Slice<R> {
    inner: R,
    start: u64,
    len: u64,
    pos: u64,
}

impl<R: Read + Seek> Slice<R> {
    pub fn new(inner: R, start: u64, len: u64) -> Self {
        Self {
            inner,
            start,
            len,
            pos: 0,
        }
    }
}

impl<R: Read + Seek> Read for Slice<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let n = buf.len().min(left.min(usize::MAX as u64) as usize);
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let got = self.inner.read(&mut buf[..n])?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl<R: Read + Seek> Seek for Slice<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let np = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match np {
            Some(p) if p <= self.len => {
                self.pos = p;
                Ok(p)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside slice",
            )),
        }
    }
}

/// Opens an existing regular file read-only without following a final symlink.
pub fn open_regular_read(path: &Path) -> Res<File> {
    let md = fs::symlink_metadata(path)
        .map_err(|e| ImageError::Missing(format!("{}: {e}", path.display())))?;
    if md.file_type().is_symlink() || !md.is_file() {
        return Err(ImageError::Invalid(format!(
            "{} is not a regular file (symlinks are never followed)",
            path.display()
        )));
    }
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    Ok(o.open(path)?)
}

/// Creates a fresh 0600 read+write file (`O_EXCL`, no-follow); a stale file/symlink is removed.
pub fn create_rw_private(path: &Path) -> Res<File> {
    remove_if_exists(path)?;
    Ok(secure_fs::private_open_options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?)
}

/// Removes a file or symlink (never a directory, never following a link).
pub fn remove_if_exists(path: &Path) -> Res<()> {
    match fs::symlink_metadata(path) {
        Ok(md) if md.is_dir() => Err(ImageError::Invalid(format!(
            "{} is a directory",
            path.display()
        ))),
        Ok(_) => Ok(fs::remove_file(path)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Makes `len` bytes of address space that stays sparse where the filesystem allows it.
pub fn set_sparse_len(f: &File, _path: &Path, len: u64) -> Res<()> {
    let _ = mark_sparse(f);
    f.set_len(len)?;
    Ok(())
}

/// NTFS only allocates lazily for files with the sparse attribute (`FSCTL_SET_SPARSE = 0x000900C4`);
/// without it, `File::set_len` physically zeroes all 128 MiB (spike D0c). Best effort: filesystems
/// without sparse support (FAT/exFAT) return 0 from `DeviceIoControl` and still succeed via `set_len`.
#[cfg(windows)]
pub(crate) fn mark_sparse(f: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use std::ptr::{null, null_mut};
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            h_device: *mut std::ffi::c_void,
            dw_io_control_code: u32,
            lp_in_buffer: *const std::ffi::c_void,
            n_in_buffer_size: u32,
            lp_out_buffer: *mut std::ffi::c_void,
            n_out_buffer_size: u32,
            lp_bytes_returned: *mut u32,
            lp_overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }
    let mut returned: u32 = 0;
    // SAFETY: `f.as_raw_handle()` is a valid open writeable file handle for the duration of this
    // synchronous (`lp_overlapped = null`) call; `FSCTL_SET_SPARSE` accepts null input/output
    // buffers when setting the sparse flag to TRUE, and `returned` is a valid stack `u32` pointer.
    unsafe {
        DeviceIoControl(
            f.as_raw_handle(),
            FSCTL_SET_SPARSE,
            null(),
            0,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        ) != 0
    }
}
#[cfg(not(windows))]
pub(crate) fn mark_sparse(_f: &File) -> bool {
    true
}

/// Copies `src` to a new private file `dst`, keeping all-zero 64 KiB blocks as holes.
pub fn copy_sparse(src: &Path, dst: &Path) -> Res<u64> {
    let mut r = open_regular_read(src)?;
    let total = r.metadata()?.len();
    let mut w = create_rw_private(dst)?;
    set_sparse_len(&w, dst, total)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut off = 0u64;
    while off < total {
        let want = buf.len().min((total - off) as usize);
        r.read_exact(&mut buf[..want])?;
        if buf[..want].iter().any(|b| *b != 0) {
            w.seek(SeekFrom::Start(off))?;
            w.write_all(&buf[..want])?;
        }
        off += want as u64;
    }
    w.flush()?;
    w.sync_all()?;
    Ok(total)
}

/// Bytes physically allocated is platform specific; logical length is what caps use.
pub fn logical_len(path: &Path) -> Res<u64> {
    Ok(fs::symlink_metadata(path)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn slice_is_bounded() {
        let mut s = Slice::new(Cursor::new((0u8..100).collect::<Vec<_>>()), 10, 5);
        let mut v = Vec::new();
        s.read_to_end(&mut v).unwrap();
        assert_eq!(v, vec![10, 11, 12, 13, 14]);
        assert!(s.seek(SeekFrom::Start(6)).is_err());
        s.seek(SeekFrom::Start(2)).unwrap();
        let mut b = [0u8; 8];
        assert_eq!(s.read(&mut b).unwrap(), 3);
        assert_eq!(&b[..3], &[12, 13, 14]);
    }

    #[test]
    fn copy_sparse_roundtrips_and_refuses_symlink() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        let mut data = vec![0u8; 300_000];
        data[70_000] = 7;
        data[299_999] = 9;
        fs::write(&a, &data).unwrap();
        let b = d.path().join("b");
        assert_eq!(copy_sparse(&a, &b).unwrap(), 300_000);
        assert_eq!(fs::read(&b).unwrap(), data);
        #[cfg(unix)]
        {
            let l = d.path().join("l");
            std::os::unix::fs::symlink(&a, &l).unwrap();
            assert!(copy_sparse(&l, &d.path().join("c")).is_err());
            assert!(open_regular_read(&l).is_err());
        }
    }
}
