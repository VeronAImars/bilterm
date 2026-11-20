//! FreeDOS FAT16 boot-sector capture. The boot code is GPL-2.0-or-later and is NEVER committed:
//! `dos setup` builds a scratch image in Rust, boots the pinned `FD14BOOT.img` once through a
//! [`DosBackend`], types `sys c:`, reads sector 0 of the partition back and caches the 512 bytes
//! in the per-user data dir (0700 dir, 0600 atomic file via `secure_fs`).

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use super::mbr::{PART_START, SECTOR};
use super::util::open_regular_read;
use super::{ImageError, Res};
use crate::dos::backend::keys::keys_for_text;
use crate::dos::backend::{DosBackend, SessionSpec};
use crate::dos::paths::DosPaths;
use crate::secure_fs;

/// Checks that `b` looks like a FreeDOS FAT16 boot sector (not a floppy FAT12 one, not blank).
pub fn validate_boot_sector(b: &[u8]) -> Res<[u8; 512]> {
    let arr: [u8; 512] = b
        .try_into()
        .map_err(|_| ImageError::Format(format!("boot sector must be 512 bytes, got {}", b.len())))?;
    if arr[510] != 0x55 || arr[511] != 0xAA {
        return Err(ImageError::Format("boot sector signature 55AA missing".into()));
    }
    if arr[0] != 0xEB && arr[0] != 0xE9 {
        return Err(ImageError::Format("boot sector does not start with a jump".into()));
    }
    if &arr[0x36..0x3E] != b"FAT16   " {
        return Err(ImageError::Format("boot sector is not a FAT16 boot sector".into()));
    }
    if !super::mbr::has_boot_code(&arr) {
        return Err(ImageError::Format("boot sector has no boot code".into()));
    }
    Ok(arr)
}

pub fn load_cached(paths: &DosPaths) -> Res<Option<[u8; 512]>> {
    let p = paths.boot_sector_path();
    match std::fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(md) if md.len() != 512 => {
            return Err(ImageError::Format(format!(
                "cached boot sector {} has {} bytes; delete it and rerun setup",
                p.display(),
                md.len()
            )));
        }
        Ok(_) => {}
    }
    let mut buf = Vec::with_capacity(512);
    open_regular_read(&p)?.take(513).read_to_end(&mut buf)?;
    validate_boot_sector(&buf).map(Some)
}

pub fn store_cached(paths: &DosPaths, sector: &[u8; 512]) -> Res<()> {
    secure_fs::ensure_private_dir(&paths.boot_cache_dir())?;
    secure_fs::write_private_file(&paths.boot_sector_path(), sector)?;
    Ok(())
}

/// One scripted interaction with the boot floppy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Poll the text screen until it contains the needle.
    Wait(&'static str, u32),
    Type(&'static str),
    /// Let the guest breathe for this many polling intervals.
    Pause(u32),
}

/// Sequence proven by spike D0b against `FD14BOOT.img` (FreeDOS 1.4 install floppy).
pub fn default_steps() -> Vec<Step> {
    vec![
        Step::Wait("English", 90),
        Step::Type("\n"),
        Step::Wait("proceed", 60),
        Step::Type("n"),
        Step::Pause(3),
        Step::Type("\n"),
        Step::Wait("A:\\>", 30),
        Step::Type("sys c:\n"),
        Step::Wait("System transferred", 30),
    ]
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureCfg {
    pub poll: Duration,
}

impl Default for CaptureCfg {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(1),
        }
    }
}

/// Boots `floppy` with `scratch` as C: through `backend`, runs `steps`, shuts down, and returns the
/// partition boot sector the guest's `sys c:` wrote into `scratch`.
pub fn capture_via_guest(
    backend: &mut dyn DosBackend,
    floppy: &Path,
    scratch: &Path,
    steps: &[Step],
    cfg: CaptureCfg,
    progress: &mut dyn FnMut(&str),
) -> Res<[u8; 512]> {
    let spec = SessionSpec {
        c_image: Some(scratch.to_path_buf()),
        floppy: Some(floppy.to_path_buf()),
        readonly_media: true,
        ..SessionSpec::default()
    };
    backend.launch(&spec)?;
    let run = drive(backend, steps, cfg, progress);
    let _ = backend.shutdown();
    run?;
    let mut f = open_regular_read(scratch)?;
    let mut sec = [0u8; 512];
    f.seek(SeekFrom::Start(PART_START * SECTOR))?;
    f.read_exact(&mut sec)?;
    validate_boot_sector(&sec)
}

fn drive(
    be: &mut dyn DosBackend,
    steps: &[Step],
    cfg: CaptureCfg,
    progress: &mut dyn FnMut(&str),
) -> Res<()> {
    for s in steps {
        match s {
            Step::Type(t) => be.send_keys(&keys_for_text(t)?)?,
            Step::Pause(n) => std::thread::sleep(cfg.poll * *n),
            Step::Wait(needle, polls) => {
                progress(&format!("waiting for {needle:?} in the guest"));
                let mut last = String::new();
                let mut seen = false;
                for _ in 0..*polls {
                    if let Ok(scr) = be.read_text() {
                        if scr.contains(needle) {
                            seen = true;
                            break;
                        }
                        last = scr.lines().join("\n");
                    }
                    std::thread::sleep(cfg.poll);
                }
                if !seen {
                    let tail: String = last.trim_end().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
                    return Err(ImageError::Backend(format!(
                        "timed out waiting for {needle:?}; last screen: {tail}"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod sim {
    //! A backend that behaves like the FreeDOS boot floppy: after `sys c:` it writes the known
    //! FAT16 boot sector into the scratch image, exactly where the real guest would.
    use super::super::mbr::{PART_START, SECTOR};
    use crate::dos::backend::*;
    use std::io::{Seek, SeekFrom, Write};

    pub struct SysSim {
        pub sector: [u8; 512],
        pub image: Option<std::path::PathBuf>,
        pub typed: String,
        pub screen: &'static str,
        pub shutdowns: u32,
    }

    impl SysSim {
        pub fn new(sector: [u8; 512]) -> Self {
            Self { sector, image: None, typed: String::new(), screen: "A:\\> System transferred English proceed", shutdowns: 0 }
        }
    }

    impl DosBackend for SysSim {
        fn id(&self) -> &'static str {
            "syssim"
        }
        fn caps(&self) -> BackendCaps {
            BackendCaps::default()
        }
        fn plan(&mut self, _: &SessionSpec) -> Result<LaunchPlan> {
            Err(BackendError::Unsupported("sim"))
        }
        fn launch(&mut self, s: &SessionSpec) -> Result<()> {
            assert!(s.floppy.is_some() && s.readonly_media);
            self.image = s.c_image.clone();
            Ok(())
        }
        fn attach(&mut self) -> Result<()> {
            Ok(())
        }
        fn send_keys(&mut self, keys: &[DosKey]) -> Result<()> {
            for k in keys {
                match k {
                    DosKey::Char(c) => self.typed.push(*c),
                    DosKey::Enter => {
                        if self.typed.ends_with("sys c:") {
                            let mut f = std::fs::OpenOptions::new().write(true).open(self.image.as_ref().unwrap())?;
                            f.seek(SeekFrom::Start(PART_START * SECTOR))?;
                            f.write_all(&self.sector)?;
                        }
                        self.typed.push('\n');
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        fn read_text(&mut self) -> Result<TextScreen> {
            let mut s = TextScreen::blank();
            for (i, ch) in self.screen.chars().enumerate() {
                s.cells[i].ch = ch;
            }
            Ok(s)
        }
        fn resize(&mut self, _: u16, _: u16) -> Result<()> {
            Ok(())
        }
        fn snapshot(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn restore(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn is_running(&mut self) -> bool {
            self.image.is_some()
        }
        fn shutdown(&mut self) -> Result<()> {
            self.shutdowns += 1;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::build::tests_support::{Tiny, fake_boot16};
    use super::super::build::{BuildOpts, build_image};
    use super::sim::SysSim;
    use super::*;
    use crate::dos::backend::mock::MockBackend;

    #[test]
    fn validation() {
        assert!(validate_boot_sector(&fake_boot16()).is_ok());
        assert!(validate_boot_sector(&[0u8; 512]).is_err());
        assert!(validate_boot_sector(&[0u8; 100]).is_err());
        let mut fat12 = fake_boot16();
        fat12[0x36..0x3E].copy_from_slice(b"FAT12   ");
        assert!(validate_boot_sector(&fat12).is_err());
        let mut nocode = fake_boot16();
        nocode[0x3E..0x1FE].fill(0);
        assert!(validate_boot_sector(&nocode).is_err());
    }

    #[test]
    fn cache_roundtrip_private_and_rejects_bad_files() {
        let d = tempfile::tempdir().unwrap();
        let p = DosPaths::new(d.path().join("dos"));
        p.ensure_layout().unwrap();
        assert_eq!(load_cached(&p).unwrap(), None);
        store_cached(&p, &fake_boot16()).unwrap();
        assert_eq!(load_cached(&p).unwrap().unwrap(), fake_boot16());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(p.boot_sector_path()).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o600);
        }
        std::fs::write(p.boot_sector_path(), b"short").unwrap();
        assert!(load_cached(&p).is_err());
    }

    #[test]
    fn capture_through_sim_backend_returns_what_the_guest_wrote() {
        let d = tempfile::tempdir().unwrap();
        let scratch = d.path().join("scratch.img");
        build_image(&BuildOpts { out: &scratch, size_bytes: 32 << 20, boot_sector: None }, &Tiny).unwrap();
        let floppy = d.path().join("boot.img");
        std::fs::write(&floppy, b"floppy").unwrap();
        let mut be = SysSim::new(fake_boot16());
        let mut log = Vec::new();
        let got = capture_via_guest(
            &mut be,
            &floppy,
            &scratch,
            &default_steps(),
            CaptureCfg { poll: Duration::ZERO },
            &mut |m| log.push(m.to_string()),
        )
        .unwrap();
        assert_eq!(got, fake_boot16());
        assert!(be.typed.contains("sys c:"), "{:?}", be.typed);
        assert_eq!(be.shutdowns, 1);
        assert!(!log.is_empty());
    }

    #[test]
    fn capture_times_out_cleanly_on_mock_backend_and_still_shuts_down() {
        let d = tempfile::tempdir().unwrap();
        let scratch = d.path().join("scratch.img");
        build_image(&BuildOpts { out: &scratch, size_bytes: 32 << 20, boot_sector: None }, &Tiny).unwrap();
        let mut be = MockBackend::new();
        let err = capture_via_guest(
            &mut be,
            &d.path().join("boot.img"),
            &scratch,
            &[Step::Wait("English", 3)],
            CaptureCfg { poll: Duration::ZERO },
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(!be.is_running());
    }

    #[test]
    fn capture_rejects_a_guest_that_wrote_nothing() {
        let d = tempfile::tempdir().unwrap();
        let scratch = d.path().join("scratch.img");
        build_image(&BuildOpts { out: &scratch, size_bytes: 32 << 20, boot_sector: None }, &Tiny).unwrap();
        let mut be = SysSim::new(fake_boot16());
        // never types sys c: -> sector 0 stays the blank fatfs one
        let r = capture_via_guest(&mut be, &d.path().join("b"), &scratch, &[Step::Wait("A:\\>", 1)], CaptureCfg { poll: Duration::ZERO }, &mut |_| {});
        assert!(matches!(r, Err(ImageError::Format(_))), "{r:?}");
    }
}
