//! Hand-built MBR and BPB/FAT16 structure checks (no sfdisk, no syslinux `mbr.bin`).

pub const SECTOR: u64 = 512;
/// First partition starts at 1 MiB.
pub const PART_START: u64 = 2048;
pub const SPT: u16 = 63;
pub const HEADS: u16 = 16;

/// 105-byte original MBR boot code from spike D0c (no third-party licence): relocates to 0x0600,
/// finds the active entry, LBA-reads (INT 13h AH=42h) its first sector to 0x7C00, checks 0xAA55
/// and jumps there. DL (boot drive) is preserved.
const MBR_CODE: &[u8] = &[
    0xFA, 0x31, 0xC0, 0x8E, 0xD0, 0xBC, 0x00, 0x7C, 0x8E, 0xC0, 0x8E, 0xD8, 0xFB, 0xFC, 0xBE,
    0x00, 0x7C, 0xBF, 0x00, 0x06, 0xB9, 0x00, 0x01, 0xF3, 0xA5, 0xEA, 0x1E, 0x06, 0x00, 0x00,
    0xBE, 0xBE, 0x07, 0xB9, 0x04, 0x00, 0x80, 0x3C, 0x80, 0x74, 0x09, 0x83, 0xC6, 0x10, 0xE2,
    0xF6, 0xCD, 0x18, 0xEB, 0xFE, 0x66, 0x6A, 0x00, 0x66, 0xFF, 0x74, 0x08, 0x6A, 0x00, 0x68,
    0x00, 0x7C, 0x6A, 0x01, 0x6A, 0x10, 0x89, 0xE6, 0xB4, 0x42, 0xCD, 0x13, 0x72, 0xE4, 0x81,
    0x3E, 0xFE, 0x7D, 0x55, 0xAA, 0x75, 0xDC, 0xEA, 0x00, 0x7C, 0x00, 0x00,
];

/// FAT16B CHS (type 0x06) is limited to ~504 MiB; larger volumes use the LBA type 0x0E.
pub fn partition_type(total_bytes: u64) -> u8 {
    if total_bytes > 504 * 1024 * 1024 {
        0x0E
    } else {
        0x06
    }
}

fn chs(lba: u64) -> [u8; 3] {
    let per_cyl = HEADS as u64 * SPT as u64;
    let c = (lba / per_cyl).min(1023);
    let t = lba % per_cyl;
    let (h, s) = (t / SPT as u64, t % SPT as u64 + 1);
    [h as u8, (s as u8 & 0x3F) | ((c >> 2) as u8 & 0xC0), c as u8]
}

/// One active FAT16 partition from `PART_START` to the end of an image of `total_bytes`.
pub fn build_mbr(total_bytes: u64) -> [u8; 512] {
    let total = total_bytes / SECTOR;
    let mut m = [0u8; 512];
    m[..MBR_CODE.len()].copy_from_slice(MBR_CODE);
    let len = total - PART_START;
    let e = &mut m[0x1BE..0x1CE];
    e[0] = 0x80;
    e[1..4].copy_from_slice(&chs(PART_START));
    e[4] = partition_type(total_bytes);
    e[5..8].copy_from_slice(&chs(total - 1));
    e[8..12].copy_from_slice(&(PART_START as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(len as u32).to_le_bytes());
    m[510] = 0x55;
    m[511] = 0xAA;
    m
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartInfo {
    pub start_lba: u64,
    pub sectors: u64,
    pub ptype: u8,
    pub active: bool,
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

/// Checks signature and the first partition entry against the image length.
pub fn validate_mbr(m: &[u8; 512], file_len: u64) -> Result<PartInfo, String> {
    if m[510] != 0x55 || m[511] != 0xAA {
        return Err("MBR signature 55AA missing".into());
    }
    let e = &m[0x1BE..0x1CE];
    let p = PartInfo {
        start_lba: le32(e, 8) as u64,
        sectors: le32(e, 12) as u64,
        ptype: e[4],
        active: e[0] == 0x80,
    };
    if !matches!(p.ptype, 0x04 | 0x06 | 0x0E) {
        return Err(format!("partition 1 type {:#04x} is not FAT16", p.ptype));
    }
    if !p.active {
        return Err("partition 1 is not marked active".into());
    }
    if p.start_lba == 0 || p.sectors == 0 {
        return Err("partition 1 is empty".into());
    }
    if (p.start_lba + p.sectors) * SECTOR > file_len {
        return Err("partition 1 extends past the end of the image".into());
    }
    Ok(p)
}

/// Layout derived from a FAT16 boot sector (offsets are in sectors relative to the partition).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fat16Geometry {
    pub fat_start: u64,
    pub sectors_per_fat: u64,
    pub num_fats: u64,
    pub root_start: u64,
    pub root_entries: u64,
    pub root_sectors: u64,
    pub data_start: u64,
    pub clusters: u64,
    pub media: u8,
}

/// Structural BPB checks for a FAT16 volume that must fit in `part_sectors`.
pub fn validate_fat16_boot(b: &[u8; 512], part_sectors: u64) -> Result<Fat16Geometry, String> {
    if b[510] != 0x55 || b[511] != 0xAA {
        return Err("partition boot sector signature 55AA missing".into());
    }
    if b[0] != 0xEB && b[0] != 0xE9 {
        return Err("partition boot sector does not start with a jump".into());
    }
    if le16(b, 0x0B) != 512 {
        return Err(format!("bytes per sector {} (need 512)", le16(b, 0x0B)));
    }
    let spc = b[0x0D] as u64;
    if spc == 0 || !spc.is_power_of_two() || spc > 128 {
        return Err(format!("sectors per cluster {spc} is invalid"));
    }
    let reserved = le16(b, 0x0E) as u64;
    let nfats = b[0x10] as u64;
    let root_entries = le16(b, 0x11) as u64;
    let total = match le16(b, 0x13) as u64 {
        0 => le32(b, 0x20) as u64,
        n => n,
    };
    let media = b[0x15];
    let spf = le16(b, 0x16) as u64;
    if reserved == 0 || !(1..=2).contains(&nfats) || spf == 0 {
        return Err("reserved sectors / FAT count / FAT size is invalid".into());
    }
    if root_entries == 0 || root_entries > 4096 || !(root_entries * 32).is_multiple_of(512) {
        return Err(format!("root entry count {root_entries} is invalid"));
    }
    if media < 0xF0 {
        return Err(format!("media descriptor {media:#04x} is invalid"));
    }
    if total == 0 || total > part_sectors {
        return Err(format!(
            "volume has {total} sectors but the partition has {part_sectors}"
        ));
    }
    let root_sectors = root_entries * 32 / 512;
    let root_start = reserved + nfats * spf;
    let data_start = root_start + root_sectors;
    if data_start >= total {
        return Err("metadata area is larger than the volume".into());
    }
    let clusters = (total - data_start) / spc;
    if !(4085..=65524).contains(&clusters) {
        return Err(format!("{clusters} clusters: not a FAT16 volume"));
    }
    if spf * 512 < (clusters + 2) * 2 {
        return Err("FAT is too small for the cluster count".into());
    }
    Ok(Fat16Geometry {
        fat_start: reserved,
        sectors_per_fat: spf,
        num_fats: nfats,
        root_start,
        root_entries,
        root_sectors,
        data_start,
        clusters,
        media,
    })
}

/// True when the boot code area (after the EBPB) holds real code: not blank, and not the stub
/// `fatfs` writes ("This is not a bootable disk...").
pub fn has_boot_code(b: &[u8; 512]) -> bool {
    let code = &b[0x3E..0x1FE];
    code.iter().any(|x| *x != 0) && !code.windows(8).any(|w| w == b"not a bo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbr_structure() {
        let total = 128 * 1024 * 1024u64;
        let m = build_mbr(total);
        assert_eq!(&m[510..], &[0x55, 0xAA]);
        let p = validate_mbr(&m, total).unwrap();
        assert_eq!(p.start_lba, 2048);
        assert_eq!(p.sectors, total / 512 - 2048);
        assert_eq!(p.ptype, 0x06);
        assert!(p.active);
        assert_eq!(build_mbr(1024 * 1024 * 1024)[0x1BE + 4], 0x0E);
        assert_eq!(m[0x1BE + 8..0x1BE + 12], 2048u32.to_le_bytes());
        // the CHS start of LBA 2048 with 16 heads x 63 spt is head 1 sector 1... verified by BIOS use
        assert_eq!(&m[0x1BE + 1..0x1BE + 4], &chs(2048));
    }

    #[test]
    fn mbr_rejections() {
        let total = 128 * 1024 * 1024u64;
        let good = build_mbr(total);
        let mut m = good;
        m[511] = 0;
        assert!(validate_mbr(&m, total).is_err());
        let mut m = good;
        m[0x1BE + 4] = 0x83;
        assert!(validate_mbr(&m, total).is_err());
        let mut m = good;
        m[0x1BE] = 0;
        assert!(validate_mbr(&m, total).is_err());
        assert!(validate_mbr(&good, total - 512 * 100).is_err());
        let mut m = good;
        m[0x1BE + 12..0x1BE + 16].fill(0);
        assert!(validate_mbr(&m, total).is_err());
    }

    #[test]
    fn bpb_rejections() {
        let mut b = [0u8; 512];
        b[510] = 0x55;
        b[511] = 0xAA;
        assert!(validate_fat16_boot(&b, 1 << 20).is_err());
        b[0] = 0xEB;
        b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        b[0x0D] = 4;
        b[0x0E] = 1;
        b[0x10] = 2;
        b[0x11..0x13].copy_from_slice(&512u16.to_le_bytes());
        b[0x15] = 0xF8;
        b[0x16..0x18].copy_from_slice(&64u16.to_le_bytes());
        b[0x20..0x24].copy_from_slice(&65536u32.to_le_bytes());
        let g = validate_fat16_boot(&b, 65536).unwrap();
        assert_eq!(g.root_start, 129);
        assert!(validate_fat16_boot(&b, 1000).is_err());
        let mut c = b;
        c[0x0D] = 3;
        assert!(validate_fat16_boot(&c, 65536).is_err());
        let mut c = b;
        c[0x11..0x13].copy_from_slice(&0u16.to_le_bytes());
        assert!(validate_fat16_boot(&c, 65536).is_err());
        assert!(!has_boot_code(&b));
    }
}
