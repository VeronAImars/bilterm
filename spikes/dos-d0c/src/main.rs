//! Spike D0c: pure-Rust builder for a bootable FreeDOS C: image (MBR + one FAT16 partition).
//!
//! usage: dos-d0c build --out c.img --kernel KERNEL.SYS --command COMMAND.COM
//!                      [--size-mib 128] [--boot-sector bs.bin]
//!        dos-d0c extract-bs --image c.img --out bs.bin   (capture FreeDOS FAT16 boot sector)
//!
//! No mtools, no sfdisk, no dd. The MBR boot code below is original (written for this spike,
//! hand-assembled, no third-party licence). The FreeDOS FAT16 boot sector code is NOT embedded
//! here; it is supplied as a 512-byte blob (see docs/plans/dos-mode.md, D0c).
use fscommon::StreamSlice;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

const PART_START: u64 = 2048; // sectors (1 MiB alignment)
const SPT: u16 = 63;
const HEADS: u16 = 16;

/// 105-byte MBR: relocate to 0x0600, find the active entry, LBA-read its first sector
/// (INT 13h AH=42h) to 0x7C00, check 0xAA55, jump. DL (boot drive) is preserved.
const MBR_CODE: &[u8] = &[
    0xFA, 0x31, 0xC0, 0x8E, 0xD0, 0xBC, 0x00, 0x7C, 0x8E, 0xC0, 0x8E, 0xD8, 0xFB, 0xFC, 0xBE,
    0x00, 0x7C, 0xBF, 0x00, 0x06, 0xB9, 0x00, 0x01, 0xF3, 0xA5, 0xEA, 0x1E, 0x06, 0x00, 0x00,
    // 0x1E: scan 4 entries for 0x80
    0xBE, 0xBE, 0x07, 0xB9, 0x04, 0x00, 0x80, 0x3C, 0x80, 0x74, 0x09, 0x83, 0xC6, 0x10, 0xE2,
    0xF6, 0xCD, 0x18, 0xEB, 0xFE,
    // found: build DAP on the stack, read, verify, jump
    0x66, 0x6A, 0x00, 0x66, 0xFF, 0x74, 0x08, 0x6A, 0x00, 0x68, 0x00, 0x7C, 0x6A, 0x01, 0x6A,
    0x10, 0x89, 0xE6, 0xB4, 0x42, 0xCD, 0x13, 0x72, 0xE4, 0x81, 0x3E, 0xFE, 0x7D, 0x55, 0xAA,
    0x75, 0xDC, 0xEA, 0x00, 0x7C, 0x00, 0x00,
];

fn chs(lba: u64) -> [u8; 3] {
    let c = lba / (HEADS as u64 * SPT as u64);
    let t = lba % (HEADS as u64 * SPT as u64);
    let (h, s) = (t / SPT as u64, t % SPT as u64 + 1);
    let c = c.min(1023);
    [h as u8, (s as u8 & 0x3F) | ((c >> 2) as u8 & 0xC0), c as u8]
}

fn build_mbr(total_sectors: u64) -> [u8; 512] {
    let mut m = [0u8; 512];
    m[..MBR_CODE.len()].copy_from_slice(MBR_CODE);
    let len = total_sectors - PART_START;
    let e = &mut m[0x1BE..0x1CE];
    e[0] = 0x80; // active
    e[1..4].copy_from_slice(&chs(PART_START));
    e[4] = 0x06; // FAT16B
    e[5..8].copy_from_slice(&chs(total_sectors - 1));
    e[8..12].copy_from_slice(&(PART_START as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(len as u32).to_le_bytes());
    m[510] = 0x55;
    m[511] = 0xAA;
    m
}

fn die(m: impl std::fmt::Display) -> ! {
    eprintln!("error: {m}");
    std::process::exit(1)
}

/// NTFS does not allocate lazily unless the file carries the sparse attribute (measured on
/// win-devvm: plain set_len => 128 MiB on disk). Spike uses `fsutil`; the real code should call
/// DeviceIoControl(FSCTL_SET_SPARSE) via windows-sys (already a bilterm dependency candidate).
#[cfg(windows)]
fn mark_sparse(path: &str) {
    let _ = std::process::Command::new("fsutil").args(["sparse", "setflag", path]).output();
}
#[cfg(not(windows))]
fn mark_sparse(_path: &str) {}

fn arg(a: &[String], k: &str) -> Option<String> {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1).cloned())
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("build") => build(&a),
        Some("extract-bs") => extract(&a),
        _ => die("usage: build|extract-bs (see source header)"),
    }
}

fn extract(a: &[String]) {
    let img = arg(a, "--image").unwrap_or_else(|| die("--image"));
    let out = arg(a, "--out").unwrap_or_else(|| die("--out"));
    let mut f = File::open(img).unwrap_or_else(|e| die(e));
    let mut b = [0u8; 512];
    f.seek(SeekFrom::Start(PART_START * 512)).unwrap();
    f.read_exact(&mut b).unwrap();
    std::fs::write(out, b).unwrap();
}

fn build(a: &[String]) {
    let out = arg(a, "--out").unwrap_or_else(|| die("--out"));
    let kernel = std::fs::read(arg(a, "--kernel").unwrap_or_else(|| die("--kernel"))).unwrap_or_else(|e| die(e));
    let command = std::fs::read(arg(a, "--command").unwrap_or_else(|| die("--command"))).unwrap_or_else(|e| die(e));
    let mib: u64 = arg(a, "--size-mib").map(|s| s.parse().unwrap_or_else(|e| die(e))).unwrap_or(128);
    let bs = arg(a, "--boot-sector").map(|p| std::fs::read(p).unwrap_or_else(|e| die(e)));
    if let Some(b) = &bs {
        if b.len() != 512 || b[510..] != [0x55, 0xAA] {
            die("boot sector blob must be 512 bytes ending 55 AA");
        }
    }
    let total = mib * 1024 * 1024 / 512;
    // Build as .part then rename (the real implementation uses secure_fs atomic write).
    let part = format!("{out}.part");
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&part).unwrap_or_else(|e| die(e));
    mark_sparse(&part); // NTFS only; no-op elsewhere
    f.set_len(total * 512).unwrap(); // sparse on ext4/APFS; on NTFS only after the sparse flag
    f.write_all(&build_mbr(total)).unwrap();

    let slice = StreamSlice::new(f.try_clone().unwrap(), PART_START * 512, total * 512).unwrap();
    let mut slice = fscommon::BufStream::new(slice);
    let opts = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat16)
        .bytes_per_sector(512)
        .volume_label(*b"FREEDOS C  ");
    fatfs::format_volume(&mut slice, opts).unwrap_or_else(|e| die(e));
    slice.flush().unwrap();
    drop(slice);

    // Patch BPB geometry/hidden sectors + (optionally) boot code into the partition's sector 0.
    let mut s0 = [0u8; 512];
    f.seek(SeekFrom::Start(PART_START * 512)).unwrap();
    f.read_exact(&mut s0).unwrap();
    s0[0x18..0x1A].copy_from_slice(&SPT.to_le_bytes());
    s0[0x1A..0x1C].copy_from_slice(&HEADS.to_le_bytes());
    s0[0x1C..0x20].copy_from_slice(&(PART_START as u32).to_le_bytes());
    s0[0x24] = 0x80;
    if let Some(b) = &bs {
        s0[0..3].copy_from_slice(&b[0..3]); // jmp
        s0[3..11].copy_from_slice(&b[3..11]); // OEM id
        s0[0x3E..0x1FE].copy_from_slice(&b[0x3E..0x1FE]); // boot code (FAT16 EBPB ends at 0x3E)
    }
    f.seek(SeekFrom::Start(PART_START * 512)).unwrap();
    f.write_all(&s0).unwrap();
    f.flush().unwrap();

    let slice = StreamSlice::new(f.try_clone().unwrap(), PART_START * 512, total * 512).unwrap();
    let fs = fatfs::FileSystem::new(fscommon::BufStream::new(slice), fatfs::FsOptions::new()).unwrap_or_else(|e| die(e));
    let root = fs.root_dir();
    // KERNEL.SYS first: FreeDOS boot sectors look it up in the root directory.
    for (name, data) in [
        ("KERNEL.SYS", kernel.as_slice()),
        ("COMMAND.COM", command.as_slice()),
        ("FDCONFIG.SYS", b"!SWITCHES=/N\r\nDOS=HIGH\r\nFILES=40\r\nSHELL=C:\\COMMAND.COM C:\\ /E:256 /P\r\n".as_slice()),
        ("AUTOEXEC.BAT", b"@ECHO OFF\r\nPROMPT $P$G\r\nECHO BILTERM-D0C AUTOEXEC RAN\r\n".as_slice()),
    ] {
        let mut file = root.create_file(name).unwrap_or_else(|e| die(format!("{name}: {e}")));
        file.truncate().unwrap();
        file.write_all(data).unwrap();
        file.flush().unwrap();
    }
    drop(root);
    fs.unmount().unwrap_or_else(|e| die(e));
    f.sync_all().unwrap();
    drop(f);
    std::fs::rename(&part, &out).unwrap();
    println!("built {out} ({mib} MiB, bootsector={})", if bs.is_some() { "freedos blob" } else { "fatfs default (not bootable)" });
}
