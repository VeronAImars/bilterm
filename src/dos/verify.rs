//! SHA256 verification and emulator version parsing/floor comparison.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Lowercase hex SHA256 of everything `r` yields.
pub fn sha256_reader(mut r: impl Read) -> io::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    sha256_reader(File::open(path)?)
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    sha256_reader(bytes).expect("reading a slice cannot fail")
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    Io(String),
    SizeMismatch { expected: u64, actual: u64 },
    HashMismatch { expected: String, actual: String },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::SizeMismatch { expected, actual } => {
                write!(f, "size mismatch: expected {expected} bytes, got {actual}")
            }
            Self::HashMismatch { expected, actual } => {
                write!(f, "sha256 mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

/// Checks exact size, then SHA256 (case-insensitive compare on the pinned value).
pub fn verify_file(
    path: &Path,
    expected_sha256: &str,
    expected_bytes: u64,
) -> Result<(), VerifyError> {
    let actual = std::fs::metadata(path)
        .map_err(|e| VerifyError::Io(e.to_string()))?
        .len();
    if actual != expected_bytes {
        return Err(VerifyError::SizeMismatch {
            expected: expected_bytes,
            actual,
        });
    }
    let got = sha256_file(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    if !got.eq_ignore_ascii_case(expected_sha256) {
        return Err(VerifyError::HashMismatch {
            expected: expected_sha256.to_string(),
            actual: got,
        });
    }
    Ok(())
}

/// `major.minor.patch` (missing parts are 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.trim().split('.');
        let a = it.next()?.parse().ok()?;
        let b = it.next().map_or(Some(0), |x| x.parse().ok())?;
        let c = it.next().map_or(Some(0), |x| x.parse().ok())?;
        Some(Self(a, b, c))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Extracts the version from `qemu-system-* -version` output
/// ("QEMU emulator version 10.2.1 (Debian ...)"). Only the first 512 bytes are examined.
pub fn parse_qemu_version(out: &str) -> Option<Version> {
    let head: String = out.chars().take(512).collect();
    let rest = head.split("version").nth(1)?;
    let tok = rest.split_whitespace().next()?;
    let clean: String = tok
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    Version::parse(&clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_sha256() {
        assert_eq!(
            sha256_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_good_bad_truncated() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        let good = sha256_bytes(b"abc");
        assert!(verify_file(&p, &good, 3).is_ok());
        assert!(matches!(
            verify_file(&p, &good, 4),
            Err(VerifyError::SizeMismatch { .. })
        ));
        assert!(matches!(
            verify_file(&p, &sha256_bytes(b"abd"), 3),
            Err(VerifyError::HashMismatch { .. })
        ));
        std::fs::write(&p, b"ab").unwrap();
        assert!(matches!(
            verify_file(&p, &good, 3),
            Err(VerifyError::SizeMismatch { .. })
        ));
        assert!(matches!(
            verify_file(&d.path().join("nope"), &good, 3),
            Err(VerifyError::Io(_))
        ));
    }

    #[test]
    fn qemu_version_parse_and_floor() {
        let v =
            parse_qemu_version("QEMU emulator version 10.2.1 (Debian 1:10.2.1+ds-1)\nCopyright")
                .unwrap();
        assert_eq!(v, Version(10, 2, 1));
        assert!(v >= Version::parse("10.2.0").unwrap());
        assert!(parse_qemu_version("QEMU emulator version 9.2.4").unwrap() < Version(10, 2, 0));
        assert_eq!(
            parse_qemu_version("QEMU emulator version 11.1.3p0 x"),
            Some(Version(11, 1, 3))
        );
        assert_eq!(parse_qemu_version("garbage"), None);
        assert!(Version::parse("x.y").is_none());
    }
}
