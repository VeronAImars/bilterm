//! Minimal bounded ZIP reader (central directory, stored + deflate, CRC-32 checked).
//! Hostile-archive rules (plan section 8): no zip64, no encryption, no symlinks, bounded
//! entry count and sizes, and member names must pass [`safe_components`].

use std::io::{self, Read, Seek, SeekFrom};

use flate2::Crc;
use flate2::read::DeflateDecoder;

use super::{ImageError, Res};

const EOCD_SIG: u32 = 0x0605_4b50;
const CEN_SIG: u32 = 0x0201_4b50;
const LOC_SIG: u32 = 0x0403_4b50;
pub const MAX_ENTRIES: usize = 20_000;
const MAX_CEN_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub crc: u32,
    pub comp: u64,
    pub size: u64,
    pub offset: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub encrypted: bool,
}

pub struct ZipArchive<R> {
    r: R,
    total: u64,
    entries: Vec<ZipEntry>,
}

fn bad(m: impl Into<String>) -> ImageError {
    ImageError::Format(format!("zip: {}", m.into()))
}
fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl<R: Read + Seek> ZipArchive<R> {
    pub fn open(mut r: R) -> Res<Self> {
        let total = r.seek(SeekFrom::End(0))?;
        if total < 22 {
            return Err(bad("too small"));
        }
        let tail_len = total.min(22 + 65_535);
        r.seek(SeekFrom::Start(total - tail_len))?;
        let mut tail = vec![0u8; tail_len as usize];
        r.read_exact(&mut tail)?;
        let pos = (0..=tail.len() - 22)
            .rev()
            .find(|i| le32(&tail, *i) == EOCD_SIG)
            .ok_or_else(|| bad("no end-of-central-directory record"))?;
        let e = &tail[pos..];
        let (n, cen_size, cen_off) = (le16(e, 10) as usize, le32(e, 12) as u64, le32(e, 16) as u64);
        if le16(e, 4) != 0 || le16(e, 6) != 0 || le16(e, 8) != le16(e, 10) {
            return Err(bad("multi-disk archives are not supported"));
        }
        if n == 0xFFFF || cen_size == 0xFFFF_FFFF || cen_off == 0xFFFF_FFFF {
            return Err(bad("zip64 is not supported"));
        }
        if n > MAX_ENTRIES || cen_size > MAX_CEN_BYTES || cen_off + cen_size > total {
            return Err(bad("central directory out of bounds"));
        }
        r.seek(SeekFrom::Start(cen_off))?;
        let mut cen = vec![0u8; cen_size as usize];
        r.read_exact(&mut cen)?;
        let mut entries = Vec::with_capacity(n);
        let mut o = 0usize;
        for _ in 0..n {
            if o + 46 > cen.len() || le32(&cen, o) != CEN_SIG {
                return Err(bad("corrupt central directory"));
            }
            let (nl, xl, cl) = (
                le16(&cen, o + 28) as usize,
                le16(&cen, o + 30) as usize,
                le16(&cen, o + 32) as usize,
            );
            if o + 46 + nl + xl + cl > cen.len() {
                return Err(bad("central directory entry overruns"));
            }
            let flags = le16(&cen, o + 8);
            let made_by_unix = cen[o + 5] == 3;
            let mode = le32(&cen, o + 38) >> 16;
            let name = String::from_utf8_lossy(&cen[o + 46..o + 46 + nl]).into_owned();
            let (comp, size, offset) = (
                le32(&cen, o + 20) as u64,
                le32(&cen, o + 24) as u64,
                le32(&cen, o + 42) as u64,
            );
            if comp == 0xFFFF_FFFF || size == 0xFFFF_FFFF || offset == 0xFFFF_FFFF {
                return Err(bad("zip64 entry is not supported"));
            }
            entries.push(ZipEntry {
                is_dir: name.ends_with('/'),
                name,
                method: le16(&cen, o + 10),
                crc: le32(&cen, o + 16),
                comp,
                size,
                offset,
                is_symlink: made_by_unix && (mode & 0o170000) == 0o120000,
                encrypted: flags & 1 != 0,
            });
            o += 46 + nl + xl + cl;
        }
        Ok(Self { r, total, entries })
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Opens a member for reading. Refuses encryption, symlinks, unknown methods and entries
    /// whose declared size exceeds `max_size`. The returned reader yields at most `size` bytes
    /// and fails with `InvalidData` on a length or CRC-32 mismatch.
    pub fn open_entry(&mut self, e: &ZipEntry, max_size: u64) -> Res<impl Read + '_> {
        if e.encrypted {
            return Err(bad(format!("{}: encrypted", e.name)));
        }
        if e.is_symlink {
            return Err(bad(format!("{}: symlink member", e.name)));
        }
        if e.is_dir {
            return Err(bad(format!("{}: directory", e.name)));
        }
        if e.size > max_size {
            return Err(bad(format!(
                "{}: {} bytes exceeds the cap of {max_size}",
                e.name, e.size
            )));
        }
        if e.method != 0 && e.method != 8 {
            return Err(bad(format!("{}: unsupported method {}", e.name, e.method)));
        }
        self.r.seek(SeekFrom::Start(e.offset))?;
        let mut h = [0u8; 30];
        self.r.read_exact(&mut h)?;
        if le32(&h, 0) != LOC_SIG {
            return Err(bad(format!("{}: bad local header", e.name)));
        }
        let data = e.offset + 30 + le16(&h, 26) as u64 + le16(&h, 28) as u64;
        if data + e.comp > self.total {
            return Err(bad(format!("{}: data runs past the archive", e.name)));
        }
        self.r.seek(SeekFrom::Start(data))?;
        let raw = (&mut self.r).take(e.comp);
        let inner: Box<dyn Read + '_> = if e.method == 0 {
            if e.comp != e.size {
                return Err(bad(format!("{}: stored size mismatch", e.name)));
            }
            Box::new(raw)
        } else {
            Box::new(DeflateDecoder::new(raw).take(e.size + 1))
        };
        Ok(Verified {
            inner,
            crc: Crc::new(),
            want_crc: e.crc,
            want_len: e.size,
            got: 0,
            name: e.name.clone(),
        })
    }
}

struct Verified<'a> {
    inner: Box<dyn Read + 'a>,
    crc: Crc,
    want_crc: u32,
    want_len: u64,
    got: u64,
    name: String,
}

impl Read for Verified<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let err = |m: &str| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {m}", self.name));
        if n == 0 {
            if self.got != self.want_len {
                return Err(err("length mismatch"));
            }
            if self.crc.sum() != self.want_crc {
                return Err(err("CRC-32 mismatch"));
            }
            return Ok(0);
        }
        self.crc.update(&buf[..n]);
        self.got += n as u64;
        if self.got > self.want_len {
            return Err(err("data longer than declared"));
        }
        Ok(n)
    }
}

/// Splits a member name into safe path components. Rejects absolute paths, `..`, backslashes,
/// drive letters/colons, NUL and control characters, empty components. `.` is dropped.
pub fn safe_components(name: &str) -> Result<Vec<&str>, String> {
    if name.is_empty() || name.len() > 1024 {
        return Err(format!("bad member name length {}", name.len()));
    }
    if name.starts_with('/') || name.contains('\\') || name.contains(':') {
        return Err(format!("unsafe member name {name:?}"));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("control character in member name {name:?}"));
    }
    let mut out = Vec::new();
    let body = name.strip_suffix('/').unwrap_or(name);
    for c in body.split('/') {
        match c {
            "" => return Err(format!("empty component in {name:?}")),
            ".." => return Err(format!("path traversal in {name:?}")),
            "." => {}
            c => out.push(c),
        }
    }
    if out.is_empty() {
        return Err(format!("empty member name {name:?}"));
    }
    Ok(out)
}

/// Components FAT can store: no reserved characters, no trailing dot/space, length <= 255.
pub fn fat_component_ok(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 255
        && !c.ends_with('.')
        && !c.ends_with(' ')
        && !c.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::make_zip;
    use super::*;
    use std::io::Cursor;

    fn read_all(z: &mut ZipArchive<Cursor<Vec<u8>>>, name: &str) -> io::Result<Vec<u8>> {
        let e = z.find(name).unwrap().clone();
        let mut v = Vec::new();
        z.open_entry(&e, 1 << 20)
            .map_err(|e| io::Error::other(e.to_string()))?
            .read_to_end(&mut v)?;
        Ok(v)
    }

    #[test]
    fn stored_and_deflated_roundtrip() {
        let big = vec![b'x'; 50_000];
        let zip = make_zip(&[("a.txt", b"hello", false), ("dir/b.bin", &big, true)]);
        let mut z = ZipArchive::open(Cursor::new(zip)).unwrap();
        assert_eq!(z.entries().len(), 2);
        assert_eq!(read_all(&mut z, "a.txt").unwrap(), b"hello");
        assert_eq!(read_all(&mut z, "dir/b.bin").unwrap(), big);
    }

    #[test]
    fn size_cap_and_corruption() {
        let zip = make_zip(&[("a.txt", b"hello world", false)]);
        let mut z = ZipArchive::open(Cursor::new(zip.clone())).unwrap();
        let e = z.find("a.txt").unwrap().clone();
        assert!(z.open_entry(&e, 5).is_err());
        // flip a data byte: CRC check must fail
        let mut bad = zip.clone();
        let at = 30 + "a.txt".len() + 2;
        bad[at] ^= 0xFF;
        let mut z = ZipArchive::open(Cursor::new(bad)).unwrap();
        assert!(read_all(&mut z, "a.txt").is_err());
        // truncated archive
        assert!(ZipArchive::open(Cursor::new(zip[..zip.len() - 30].to_vec())).is_err());
        assert!(ZipArchive::open(Cursor::new(vec![0u8; 10])).is_err());
    }

    #[test]
    fn declared_size_lies_are_caught() {
        // deflate bomb: claims 10 bytes but expands to 5000
        let mut zip = make_zip(&[("b", &vec![b'a'; 5000], true)]);
        let cen = zip.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
        zip[cen + 24..cen + 28].copy_from_slice(&10u32.to_le_bytes());
        let mut z = ZipArchive::open(Cursor::new(zip)).unwrap();
        assert!(read_all(&mut z, "b").is_err());
    }

    #[test]
    fn symlink_and_traversal_names() {
        let mut zip = make_zip(&[("link", b"/etc/passwd", false)]);
        let cen = zip.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
        zip[cen + 5] = 3; // made by unix
        zip[cen + 38..cen + 42].copy_from_slice(&((0o120777u32) << 16).to_le_bytes());
        let mut z = ZipArchive::open(Cursor::new(zip)).unwrap();
        let e = z.find("link").unwrap().clone();
        assert!(e.is_symlink);
        assert!(z.open_entry(&e, 100).is_err());
        for n in ["../x", "a/../../x", "/abs", "a\\b", "C:x", "a//b", "", "a\0b", "a\n"] {
            assert!(safe_components(n).is_err(), "{n:?}");
        }
        assert_eq!(safe_components("./a/b/").unwrap(), vec!["a", "b"]);
        assert!(!fat_component_ok("a*b") && !fat_component_ok("x.") && fat_component_ok("README.TXT"));
    }
}
