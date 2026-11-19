//! Downloader seam. Production uses the system `curl` (argv vector, HTTPS only, no shell); tests use
//! [`MockDownloader`] so no test touches the network.

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::manifest::Entry;
use super::paths::DosPaths;
use super::verify::verify_file;
use crate::secure_fs;

pub trait Downloader {
    /// Streams the body of `url` into `out`, writing at most `max_bytes`. Returns bytes written.
    fn fetch(&self, url: &str, max_bytes: u64, out: &mut File) -> Result<u64, String>;
}

/// Shells out to `curl` with a fixed argv. `--proto =https` forbids plain HTTP including redirects.
pub struct CurlDownloader {
    pub program: String,
}

impl Default for CurlDownloader {
    fn default() -> Self {
        Self {
            program: "curl".into(),
        }
    }
}

impl CurlDownloader {
    pub fn argv(url: &str, max_bytes: u64) -> Vec<String> {
        [
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--max-redirs",
            "5",
            "--max-filesize",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([max_bytes.to_string(), "--".into(), url.to_string()])
        .collect()
    }
}

impl Downloader for CurlDownloader {
    fn fetch(&self, url: &str, max_bytes: u64, out: &mut File) -> Result<u64, String> {
        let sink = out
            .try_clone()
            .map_err(|e| format!("cannot clone download file: {e}"))?;
        let status = Command::new(&self.program)
            .args(Self::argv(url, max_bytes))
            .env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| {
                // Keep only what curl needs to find CA certs / proxies / a PATH-less system root.
                let k = k.to_string_lossy().to_ascii_uppercase();
                matches!(
                    k.as_str(),
                    "PATH"
                        | "SYSTEMROOT"
                        | "HOME"
                        | "CURL_CA_BUNDLE"
                        | "SSL_CERT_FILE"
                        | "SSL_CERT_DIR"
                ) || k.ends_with("_PROXY")
            }))
            .stdin(Stdio::null())
            .stdout(Stdio::from(sink))
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("cannot run {}: {e} (install curl)", self.program))?;
        if !status.success() {
            return Err(format!("{} failed ({status})", self.program));
        }
        out.metadata().map(|m| m.len()).map_err(|e| e.to_string())
    }
}

/// Test downloader: serves canned bodies by URL; records every requested URL.
pub struct MockDownloader {
    pub bodies: Vec<(String, Vec<u8>)>,
    pub requests: std::cell::RefCell<Vec<String>>,
}

impl MockDownloader {
    pub fn new(bodies: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            bodies,
            requests: Default::default(),
        }
    }
}

impl Downloader for MockDownloader {
    fn fetch(&self, url: &str, max_bytes: u64, out: &mut File) -> Result<u64, String> {
        use std::io::Write;
        self.requests.borrow_mut().push(url.to_string());
        let (_, body) = self
            .bodies
            .iter()
            .find(|(u, _)| u == url)
            .ok_or_else(|| format!("mock: no body for {url}"))?;
        let n = body.len().min(max_bytes as usize);
        out.write_all(&body[..n]).map_err(|e| e.to_string())?;
        Ok(n as u64)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// A verified copy was already present; nothing downloaded.
    AlreadyPresent(PathBuf),
    Downloaded(PathBuf),
}

/// Downloads one pinned entry: refuse unpinned/disallowed, download into a 0600 `.part`, verify
/// size + SHA256 *before* it becomes visible, then atomically rename into place.
pub fn fetch_entry(
    entry: &Entry,
    paths: &DosPaths,
    dl: &dyn Downloader,
) -> Result<FetchOutcome, String> {
    if let Some(why) = entry.refusal() {
        return Err(why);
    }
    let dest = paths.download_path(&entry.file)?;
    if dest.is_file() && verify_file(&dest, &entry.sha256, entry.bytes).is_ok() {
        return Ok(FetchOutcome::AlreadyPresent(dest));
    }
    let part = paths.part_path(&entry.file)?;
    let _ = fs::remove_file(&part);
    let mut file = secure_fs::create_private_file(&part)
        .map_err(|e| format!("{}: cannot create {}: {e}", entry.id, part.display()))?;
    let cap = entry.size_cap();
    let result = dl.fetch(&entry.url, cap, &mut file);
    drop(file);
    let checked = result.and_then(|n| {
        if n > cap {
            return Err(format!("downloaded {n} bytes, over the cap of {cap}"));
        }
        verify_file(&part, &entry.sha256, entry.bytes).map_err(|e| e.to_string())
    });
    if let Err(e) = checked {
        let _ = fs::remove_file(&part);
        return Err(format!("{}: {e}", entry.id));
    }
    // A planted symlink/dir at `dest` must not be followed: remove a stale file first.
    if let Ok(md) = fs::symlink_metadata(&dest) {
        if md.is_dir() {
            let _ = fs::remove_file(&part);
            return Err(format!("{}: {} is a directory", entry.id, dest.display()));
        }
        let _ = fs::remove_file(&dest);
    }
    fs::rename(&part, &dest).map_err(|e| {
        let _ = fs::remove_file(&part);
        format!("{}: rename failed: {e}", entry.id)
    })?;
    Ok(FetchOutcome::Downloaded(dest))
}

#[cfg(test)]
mod tests {
    use super::super::manifest::Manifest;
    use super::super::verify::sha256_bytes;
    use super::*;

    fn fixture(body: &[u8]) -> (tempfile::TempDir, DosPaths, Entry) {
        let d = tempfile::tempdir().unwrap();
        let paths = DosPaths::new(d.path().join("bilterm").join("dos"));
        paths.ensure_layout().unwrap();
        let mut e = Manifest::embedded()
            .unwrap()
            .entry("freedos-14-floppy")
            .unwrap()
            .clone();
        e.sha256 = sha256_bytes(body);
        e.bytes = body.len() as u64;
        (d, paths, e)
    }

    #[test]
    fn happy_path_then_idempotent() {
        let body = b"hello freedos".to_vec();
        let (_d, paths, e) = fixture(&body);
        let dl = MockDownloader::new(vec![(e.url.clone(), body.clone())]);
        let out = fetch_entry(&e, &paths, &dl).unwrap();
        let FetchOutcome::Downloaded(p) = out else {
            panic!()
        };
        assert_eq!(fs::read(&p).unwrap(), body);
        assert!(!paths.part_path(&e.file).unwrap().exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(matches!(
            fetch_entry(&e, &paths, &dl).unwrap(),
            FetchOutcome::AlreadyPresent(_)
        ));
        assert_eq!(dl.requests.borrow().len(), 1);
    }

    #[test]
    fn bad_hash_leaves_nothing_visible() {
        let (_d, paths, mut e) = fixture(b"good");
        e.sha256 = sha256_bytes(b"other");
        let dl = MockDownloader::new(vec![(e.url.clone(), b"good".to_vec())]);
        assert!(fetch_entry(&e, &paths, &dl)
            .unwrap_err()
            .contains("sha256 mismatch"));
        assert!(!paths.download_path(&e.file).unwrap().exists());
        assert!(!paths.part_path(&e.file).unwrap().exists());
    }

    #[test]
    fn truncated_and_oversized_bodies_rejected() {
        let (_d, paths, e) = fixture(b"0123456789");
        let dl = MockDownloader::new(vec![(e.url.clone(), b"0123".to_vec())]);
        assert!(fetch_entry(&e, &paths, &dl)
            .unwrap_err()
            .contains("size mismatch"));
        let dl = MockDownloader::new(vec![(e.url.clone(), vec![b'x'; 5000])]);
        assert!(fetch_entry(&e, &paths, &dl).is_err());
        assert!(!paths.download_path(&e.file).unwrap().exists());
    }

    #[test]
    fn unpinned_entry_refused_without_network() {
        let (_d, paths, _) = fixture(b"x");
        let e = Manifest::parse(&crate::dos::manifest::EMBEDDED.replace(
            "f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f",
            "TODO",
        ))
        .unwrap()
        .entry("qemu-windows-x64")
            .unwrap()
            .clone();
        let dl = MockDownloader::new(vec![]);
        assert!(fetch_entry(&e, &paths, &dl)
            .unwrap_err()
            .contains("not pinned"));
        assert!(dl.requests.borrow().is_empty());
    }

    #[test]
    fn curl_argv_is_https_only_and_url_after_separator() {
        let a = CurlDownloader::argv("https://download.freedos.org/x", 99);
        assert!(a.windows(2).any(|w| w == ["--proto", "=https"]));
        assert!(a.windows(2).any(|w| w == ["--proto-redir", "=https"]));
        assert_eq!(a[a.len() - 2], "--");
        assert_eq!(a.last().unwrap(), "https://download.freedos.org/x");
        assert!(a.contains(&"99".to_string()));
    }
}
