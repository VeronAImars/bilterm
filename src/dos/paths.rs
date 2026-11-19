//! Per-user data-dir layout for DOS mode. All directories are created through
//! [`crate::secure_fs`] (0700 dirs); files go through its 0600 atomic writers.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::secure_fs;

/// Host operating systems DOS mode distinguishes (support matrix, plan section 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl HostOs {
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }
}

/// Environment inputs for path resolution (injectable so tests never read the process env).
#[derive(Clone, Debug, Default)]
pub struct PathEnv {
    pub home: Option<OsString>,
    pub xdg_data_home: Option<OsString>,
    pub local_app_data: Option<OsString>,
}

impl PathEnv {
    pub fn from_process() -> Self {
        Self {
            home: std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            local_app_data: std::env::var_os("LOCALAPPDATA"),
        }
    }
}

fn absolute(v: &Option<OsString>) -> Option<PathBuf> {
    v.as_ref().map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `<data>/bilterm/dos` for `os`; `None` when no absolute base can be determined.
pub fn data_root(os: HostOs, env: &PathEnv) -> Option<PathBuf> {
    let base = match os {
        HostOs::Windows => absolute(&env.local_app_data)?,
        HostOs::MacOs => absolute(&env.home)?
            .join("Library")
            .join("Application Support"),
        _ => absolute(&env.xdg_data_home)
            .or_else(|| absolute(&env.home).map(|h| h.join(".local").join("share")))?,
    };
    Some(base.join("bilterm").join("dos"))
}

/// Concrete locations below the DOS data root.
#[derive(Clone, Debug)]
pub struct DosPaths {
    root: PathBuf,
}

impl DosPaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_process() -> Option<Self> {
        data_root(HostOs::current(), &PathEnv::from_process()).map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }
    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }
    /// Persistent C: images (D1c).
    pub fn c_dir(&self) -> PathBuf {
        self.root.join("c")
    }
    /// Cache for the FreeDOS FAT16 boot sector captured at setup (D1c). GPL-2.0+: never committed.
    pub fn boot_cache_dir(&self) -> PathBuf {
        self.root.join("boot")
    }
    /// Reserved cache path for the captured boot sector (512 bytes).
    pub fn boot_sector_path(&self) -> PathBuf {
        self.boot_cache_dir().join("fat16-boot-sector.bin")
    }
    /// Final, hash-verified location of a downloaded manifest file.
    pub fn download_path(&self, file: &str) -> Result<PathBuf, String> {
        secure_fs::validate_plain_file_name(file)
            .map_err(|_| format!("unsafe file name in manifest: {file:?}"))?;
        Ok(self.downloads_dir().join(file))
    }
    /// In-progress download (renamed to [`Self::download_path`] only after hash verification).
    pub fn part_path(&self, file: &str) -> Result<PathBuf, String> {
        secure_fs::validate_plain_file_name(file)
            .map_err(|_| format!("unsafe file name in manifest: {file:?}"))?;
        Ok(self.downloads_dir().join(format!("{file}.part")))
    }
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("manifest.lock.toml")
    }

    /// Creates the root and every subdirectory with 0700 permissions.
    pub fn ensure_layout(&self) -> io::Result<()> {
        if let Some(parent) = self.root.parent() {
            secure_fs::create_private_dir_all(parent)?;
        }
        secure_fs::ensure_private_dir(&self.root)?;
        for d in [
            self.downloads_dir(),
            self.images_dir(),
            self.c_dir(),
            self.boot_cache_dir(),
        ] {
            secure_fs::ensure_private_dir(&d)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &str, xdg: Option<&str>, lad: Option<&str>) -> PathEnv {
        PathEnv {
            home: Some(home.into()),
            xdg_data_home: xdg.map(Into::into),
            local_app_data: lad.map(Into::into),
        }
    }

    #[test]
    fn per_os_roots() {
        assert_eq!(
            data_root(HostOs::Linux, &env("/h", None, None)).unwrap(),
            Path::new("/h/.local/share/bilterm/dos")
        );
        assert_eq!(
            data_root(HostOs::Linux, &env("/h", Some("/x"), None)).unwrap(),
            Path::new("/x/bilterm/dos")
        );
        assert_eq!(
            data_root(HostOs::MacOs, &env("/Users/u", None, None)).unwrap(),
            Path::new("/Users/u/Library/Application Support/bilterm/dos")
        );
        assert!(data_root(HostOs::Windows, &env("/h", None, None)).is_none());
    }

    #[test]
    fn relative_bases_rejected() {
        assert!(data_root(HostOs::Linux, &env("rel", Some("also/rel"), None)).is_none());
    }

    #[test]
    fn unsafe_manifest_names_rejected() {
        let p = DosPaths::new(PathBuf::from("/r"));
        assert!(p.download_path("../evil").is_err());
        assert!(p.part_path("a/b").is_err());
        assert!(p.download_path("ok.zip").is_ok());
    }

    #[test]
    fn layout_is_private_and_reserves_boot_cache() {
        let t = tempfile::tempdir().unwrap();
        let p = DosPaths::new(t.path().join("bilterm").join("dos"));
        p.ensure_layout().unwrap();
        assert!(p.boot_cache_dir().is_dir() && p.c_dir().is_dir());
        assert!(!p.boot_sector_path().exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(p.root()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        p.ensure_layout().unwrap(); // idempotent
    }
}
