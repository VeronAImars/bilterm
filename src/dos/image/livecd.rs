//! Content source for the C: image: KERNEL.SYS / COMMAND.COM and the chosen package sets, read
//! from the pinned `FD14LIVE.iso` (ISO 9660 -> package zips -> files) with bounded extraction.

use std::path::PathBuf;

use super::build::{FatSink, Populate, PopulateStats};
use super::iso9660::Iso;
use super::util::open_regular_read;
use super::zipread::{ZipArchive, fat_component_ok, safe_components};
use super::{ImageError, Res};

/// Per-file and whole-image extraction caps.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FILES: u64 = 40_000;
const MAX_TOTAL_BYTES: u64 = 1536 * 1024 * 1024;

pub struct LiveIsoSource {
    pub iso: PathBuf,
    pub sets: Vec<String>,
}

impl LiveIsoSource {
    pub fn new(iso: PathBuf, sets: &[&str]) -> Self {
        Self {
            iso,
            sets: sets.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// `FDCONFIG.SYS` for the generated image (HIMEMX only when it was actually installed).
pub fn fdconfig(himemx: bool) -> String {
    let mut s = String::from("!SWITCHES=/N\r\nFILES=40\r\n");
    if himemx {
        s.push_str("DEVICE=C:\\FREEDOS\\BIN\\HIMEMX.EXE\r\nDOS=HIGH\r\n");
    }
    s.push_str("SHELL=C:\\COMMAND.COM C:\\ /E:1024 /P=C:\\AUTOEXEC.BAT\r\n");
    s
}

pub const AUTOEXEC: &str = "@ECHO OFF\r\nSET DOSDIR=C:\\FREEDOS\r\nSET PATH=%DOSDIR%\\BIN\r\nSET TEMP=C:\\FREEDOS\\TEMP\r\nSET NLSPATH=%DOSDIR%\\NLS\r\nPROMPT $P$G\r\nECHO BILTERM-READY\r\n";

fn put(sink: &mut dyn FatSink, path: &str, data: &[u8], st: &mut PopulateStats) -> Res<()> {
    let n = sink.write_file(path, &mut &data[..], MAX_FILE_BYTES)?;
    st.files += 1;
    st.bytes += n;
    Ok(())
}

impl Populate for LiveIsoSource {
    fn populate(&self, sink: &mut dyn FatSink) -> Res<PopulateStats> {
        let mut iso = Iso::open(open_regular_read(&self.iso)?)?;
        let mut st = PopulateStats::default();
        // KERNEL.SYS first: the FreeDOS boot sector looks it up in the root directory.
        for name in ["KERNEL.SYS", "COMMAND.COM"] {
            let e = iso
                .find(name)?
                .ok_or_else(|| ImageError::Missing(format!("{name} not on the Live CD")))?;
            if e.is_dir {
                return Err(ImageError::Format(format!("{name} is a directory")));
            }
            let n = sink.write_file(name, &mut iso.file(&e)?, MAX_FILE_BYTES)?;
            st.files += 1;
            st.bytes += n;
        }
        sink.mkdir_all("FREEDOS/TEMP")?;
        let mut himemx = false;
        for set in &self.sets {
            safe_components(set).map_err(ImageError::Invalid)?;
            let dir = iso
                .find(&format!("packages/{set}"))?
                .filter(|d| d.is_dir)
                .ok_or_else(|| ImageError::Missing(format!("package set {set:?} not on the Live CD")))?;
            let mut zips: Vec<_> = iso
                .list(&dir)?
                .into_iter()
                .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".zip"))
                .collect();
            zips.sort_by(|a, b| a.name.cmp(&b.name));
            for z in zips {
                let slice = iso.file(&z)?;
                himemx |= install_zip(slice, sink, &mut st)?;
            }
        }
        put(sink, "FDCONFIG.SYS", fdconfig(himemx).as_bytes(), &mut st)?;
        put(sink, "AUTOEXEC.BAT", AUTOEXEC.as_bytes(), &mut st)?;
        Ok(st)
    }
}

/// Installs one package zip below `FREEDOS/`, dropping `SOURCE/`. Returns true if HIMEMX landed.
fn install_zip<R: std::io::Read + std::io::Seek>(
    r: R,
    sink: &mut dyn FatSink,
    st: &mut PopulateStats,
) -> Res<bool> {
    let mut zip = ZipArchive::open(r)?;
    let entries = zip.entries().to_vec();
    let mut himemx = false;
    for e in entries {
        let comps = safe_components(&e.name).map_err(ImageError::Format)?; // traversal: hard error
        if e.is_symlink {
            return Err(ImageError::Format(format!("{}: symlink member", e.name)));
        }
        if comps[0].eq_ignore_ascii_case("SOURCE") {
            continue;
        }
        if !comps.iter().all(|c| fat_component_ok(c)) {
            st.skipped += 1;
            continue;
        }
        let dest = format!("FREEDOS/{}", comps.join("/"));
        if e.is_dir {
            sink.mkdir_all(&dest)?;
            continue;
        }
        if st.files >= MAX_FILES || st.bytes.saturating_add(e.size) > MAX_TOTAL_BYTES {
            return Err(ImageError::Format("package content exceeds the extraction caps".into()));
        }
        let n = {
            let mut rd = zip.open_entry(&e, MAX_FILE_BYTES)?;
            sink.write_file(&dest, &mut rd, MAX_FILE_BYTES)?
        };
        st.files += 1;
        st.bytes += n;
        himemx |= dest.eq_ignore_ascii_case("FREEDOS/BIN/HIMEMX.EXE");
    }
    Ok(himemx)
}

#[cfg(test)]
mod tests {
    use super::super::build::{BuildOpts, build_image};
    use super::super::fixtures::*;
    use super::super::sanity::check_image;
    use super::*;

    fn write_iso(d: &std::path::Path, iso: Vec<u8>) -> PathBuf {
        let p = d.join("live.iso");
        std::fs::write(&p, iso).unwrap();
        p
    }

    #[test]
    fn builds_from_fake_livecd_and_drops_source() {
        let d = tempfile::tempdir().unwrap();
        let src = LiveIsoSource::new(write_iso(d.path(), fake_livecd()), &["base", "net"]);
        let out = d.path().join("c.img");
        let st = build_image(&BuildOpts { out: &out, size_bytes: 32 << 20, boot_sector: None }, &src).unwrap();
        // 2 root + 4 base members (HELLO.COM, HIMEMX.EXE, README.TXT, HELLO.LSM) + PING + 2 generated
        assert_eq!(st.files, 2 + 4 + 1 + 2, "{st:?}");
        let rep = check_image(&out);
        assert_eq!(rep.root_names[0], "KERNEL.SYS");
        for n in ["COMMAND.COM", "FREEDOS", "FDCONFIG.SYS", "AUTOEXEC.BAT"] {
            assert!(rep.root_names.contains(&n.to_string()), "{n}: {:?}", rep.root_names);
        }
        // read back through fatfs-independent route: search the raw bytes
        let raw = std::fs::read(&out).unwrap();
        let has = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"hello-com") && has(b"ping") && has(b"KERNEL-BYTES"));
        assert!(!has(b"source-must-be-dropped"));
        assert!(has(b"HIMEMX.EXE\r\nDOS=HIGH"));
        // regression: fatfs with LFN support writes bogus '.'/'..' entries that fsck.fat rejects
        if let Some(fsck) = super::super::sanity::find_fsck() {
            let mut rep = super::super::sanity::SanityReport::default();
            super::super::sanity::fsck_partition(&out, &fsck, d.path(), &mut rep).unwrap();
            assert!(rep.problems.is_empty(), "{:?}", rep.problems);
        }
    }

    #[test]
    fn missing_set_and_traversal_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("c.img");
        let opts = BuildOpts { out: &out, size_bytes: 32 << 20, boot_sector: None };
        let src = LiveIsoSource::new(write_iso(d.path(), fake_livecd()), &["games"]);
        assert!(matches!(build_image(&opts, &src), Err(ImageError::Missing(_))));
        let evil = make_zip(&[("../../escape.txt", b"x", false)]);
        let iso = make_iso(vec![
            file("KERNEL.SYS", b"k"),
            file("COMMAND.COM", b"c"),
            dir("packages", vec![dir("base", vec![file("evil.zip", &evil)])]),
        ]);
        let src = LiveIsoSource::new(write_iso(d.path(), iso), &["base"]);
        let err = build_image(&opts, &src).unwrap_err();
        assert!(err.to_string().contains("traversal"), "{err}");
        assert!(!out.exists());
    }

    #[test]
    fn unstorable_names_are_skipped_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        let z = make_zip(&[("BIN/OK.EXE", b"ok", false), ("BIN/BAD*NAME.EXE", b"no", false)]);
        let iso = make_iso(vec![
            file("KERNEL.SYS", b"k"),
            file("COMMAND.COM", b"c"),
            dir("packages", vec![dir("base", vec![file("a.zip", &z)])]),
        ]);
        let src = LiveIsoSource::new(write_iso(d.path(), iso), &["base"]);
        let out = d.path().join("c.img");
        let st = build_image(&BuildOpts { out: &out, size_bytes: 32 << 20, boot_sector: None }, &src).unwrap();
        assert_eq!(st.skipped, 1);
    }
}
