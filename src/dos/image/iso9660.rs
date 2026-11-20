//! Minimal bounded ISO 9660 reader (primary volume descriptor, 8.3 names). Reads only what the
//! image builder needs: walk directories and expose file extents. No Rock Ridge, no Joliet.

use std::io::{Read, Seek, SeekFrom};

use super::util::Slice;
use super::{ImageError, Res};

const BLOCK: u64 = 2048;
const MAX_DIR_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES_PER_DIR: usize = 20_000;
const MAX_PATH_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsoEntry {
    /// Name without the `;1` version suffix, original case.
    pub name: String,
    pub lba: u64,
    pub len: u64,
    pub is_dir: bool,
}

pub struct Iso<R> {
    r: R,
    total: u64,
    root: IsoEntry,
}

fn bad(m: impl Into<String>) -> ImageError {
    ImageError::Format(format!("iso9660: {}", m.into()))
}

fn le32(b: &[u8], o: usize) -> u64 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as u64
}

impl<R: Read + Seek> Iso<R> {
    pub fn open(mut r: R) -> Res<Self> {
        let total = r.seek(SeekFrom::End(0))?;
        let mut pvd = None;
        for sector in 16u64..32 {
            let mut d = [0u8; 2048];
            if (sector + 1) * BLOCK > total {
                break;
            }
            r.seek(SeekFrom::Start(sector * BLOCK))?;
            r.read_exact(&mut d)?;
            if &d[1..6] != b"CD001" {
                break;
            }
            match d[0] {
                1 => {
                    pvd = Some(d);
                    break;
                }
                255 => break,
                _ => {}
            }
        }
        let d = pvd.ok_or_else(|| bad("no primary volume descriptor"))?;
        if u16::from_le_bytes([d[128], d[129]]) as u64 != BLOCK {
            return Err(bad("logical block size is not 2048"));
        }
        let root = parse_record(&d[156..190], total, true)?
            .ok_or_else(|| bad("bad root directory record"))?;
        if !root.is_dir {
            return Err(bad("root is not a directory"));
        }
        Ok(Self { r, total, root })
    }

    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    pub fn list(&mut self, dir: &IsoEntry) -> Res<Vec<IsoEntry>> {
        if !dir.is_dir {
            return Err(bad("not a directory"));
        }
        if dir.len > MAX_DIR_BYTES {
            return Err(bad("directory too large"));
        }
        let mut buf = vec![0u8; dir.len as usize];
        self.r.seek(SeekFrom::Start(dir.lba * BLOCK))?;
        self.r.read_exact(&mut buf)?;
        let mut out = Vec::new();
        let mut off = 0usize;
        while off < buf.len() {
            let l = buf[off] as usize;
            if l == 0 {
                // records never straddle a sector: skip padding to the next one
                off = (off / BLOCK as usize + 1) * BLOCK as usize;
                continue;
            }
            if l < 34 || off + l > buf.len() || (off % 2048) + l > 2048 {
                return Err(bad("corrupt directory record"));
            }
            if let Some(e) = parse_record(&buf[off..off + l], self.total, false)? {
                if out.len() >= MAX_ENTRIES_PER_DIR {
                    return Err(bad("too many directory entries"));
                }
                out.push(e);
            }
            off += l;
        }
        Ok(out)
    }

    /// Case-insensitive lookup of a `/`-separated path from the root.
    pub fn find(&mut self, path: &str) -> Res<Option<IsoEntry>> {
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        if comps.is_empty() || comps.len() > MAX_PATH_DEPTH {
            return Err(bad("bad path"));
        }
        let mut cur = self.root.clone();
        for c in comps {
            let found = self
                .list(&cur)?
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(c));
            match found {
                Some(e) => cur = e,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Bounded reader over a file's extent (borrows the image mutably).
    pub fn file(&mut self, e: &IsoEntry) -> Res<Slice<&mut R>> {
        if e.is_dir {
            return Err(bad("is a directory"));
        }
        Ok(Slice::new(&mut self.r, e.lba * BLOCK, e.len))
    }
}

/// Parses one directory record; `.` and `..` records yield `None` unless `keep_dot` (root).
fn parse_record(rec: &[u8], total: u64, keep_dot: bool) -> Res<Option<IsoEntry>> {
    if rec.len() < 34 {
        return Err(bad("short record"));
    }
    let nl = rec[32] as usize;
    if 33 + nl > rec.len() {
        return Err(bad("name overruns record"));
    }
    let id = &rec[33..33 + nl];
    if (id == [0] || id == [1]) && !keep_dot {
        return Ok(None);
    }
    let lba = le32(rec, 2);
    let len = le32(rec, 10);
    let is_dir = rec[25] & 2 != 0;
    if lba.saturating_mul(BLOCK).saturating_add(len) > total {
        return Err(bad("extent lies outside the image"));
    }
    if id == [0] || id == [1] {
        return Ok(Some(IsoEntry { name: String::new(), lba, len, is_dir }));
    }
    let mut name: String = id.iter().map(|b| *b as char).collect();
    if let Some(i) = name.find(';') {
        name.truncate(i);
    }
    if name.ends_with('.') {
        name.pop();
    }
    if name.contains('/') || name.contains('\0') {
        return Err(bad("hostile name"));
    }
    Ok(Some(IsoEntry {
        name,
        lba,
        len,
        is_dir,
    }))
}
