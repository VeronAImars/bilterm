//! Pinned download manifest (`manifest.toml`, embedded with `include_str!`).

use serde::Deserialize;

use super::paths::HostOs;
use super::verify::Version;

pub const EMBEDDED: &str = include_str!("manifest.toml");

/// Hosts downloads may come from (HTTPS only).
pub const HOST_ALLOWLIST: &[&str] = &["download.freedos.org"];

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub emulator: EmulatorSpec,
    pub freedos: FreedosSpec,
    #[serde(rename = "entry")]
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EmulatorSpec {
    pub name: String,
    pub min_version: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FreedosSpec {
    pub version: String,
    pub notice: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Member {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Entry {
    pub id: String,
    pub file: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub default: bool,
    /// `all`, `linux`, `macos` or `windows`.
    pub platform: String,
    pub license_note: String,
    pub source_url: String,
    #[serde(default)]
    pub members: Vec<Member>,
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Entry {
    /// True only when the pin is a real lowercase SHA256 and a non-zero size.
    pub fn is_pinned(&self) -> bool {
        is_sha256_hex(&self.sha256) && self.bytes > 0
    }

    pub fn applies_to(&self, os: HostOs) -> bool {
        self.platform == "all" || self.platform == os.name()
    }

    /// HTTPS + allowlisted host, no userinfo/port tricks.
    pub fn check_url(&self) -> Result<(), String> {
        let rest = self
            .url
            .strip_prefix("https://")
            .ok_or_else(|| format!("{}: only https:// URLs are allowed", self.id))?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if !HOST_ALLOWLIST.contains(&authority) {
            return Err(format!(
                "{}: host {authority:?} is not on the download allowlist",
                self.id
            ));
        }
        Ok(())
    }

    /// Why this entry cannot be downloaded, if it cannot.
    pub fn refusal(&self) -> Option<String> {
        if !self.is_pinned() {
            return Some(format!(
                "{}: not pinned (sha256/bytes are TODO); refusing to download",
                self.id
            ));
        }
        self.check_url().err()
    }

    /// Size cap for a download: pinned bytes plus 1 percent.
    pub fn size_cap(&self) -> u64 {
        self.bytes.saturating_add(self.bytes / 100)
    }
}

impl Manifest {
    pub fn embedded() -> Result<Self, String> {
        Self::parse(EMBEDDED)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("manifest parse error: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err(format!("unsupported manifest schema {}", self.schema));
        }
        Version::parse(&self.emulator.min_version)
            .ok_or_else(|| format!("bad emulator min_version {:?}", self.emulator.min_version))?;
        let mut seen = std::collections::HashSet::new();
        for e in &self.entries {
            if !seen.insert(e.id.as_str()) {
                return Err(format!("duplicate entry id {}", e.id));
            }
            crate::secure_fs::validate_safe_name(&e.id)
                .map_err(|m| format!("entry id {:?}: {m}", e.id))?;
            crate::secure_fs::validate_plain_file_name(&e.file)
                .map_err(|_| format!("{}: unsafe file name", e.id))?;
            if e.sha256 != "TODO" && !is_sha256_hex(&e.sha256) {
                return Err(format!(
                    "{}: sha256 must be 64 lowercase hex chars or TODO",
                    e.id
                ));
            }
            for m in &e.members {
                crate::secure_fs::validate_plain_file_name(&m.name)
                    .map_err(|_| format!("{}: unsafe member name", e.id))?;
                if !is_sha256_hex(&m.sha256) {
                    return Err(format!("{}: member {} sha256 invalid", e.id, m.name));
                }
            }
            // A pinned entry must also be downloadable under the policy.
            if e.is_pinned() {
                e.check_url()?;
            }
        }
        Ok(())
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Default entries for `os`.
    pub fn defaults(&self, os: HostOs) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |e| e.default && e.applies_to(os))
    }

    pub fn min_version(&self) -> Version {
        Version::parse(&self.emulator.min_version).expect("validated")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_parses_and_pins() {
        let m = Manifest::embedded().unwrap();
        assert!(m.min_version() >= Version::parse("10.2.0").unwrap());
        assert_eq!(m.freedos.version, "1.4");
        assert!(m.entry("freedos-14-livecd").unwrap().is_pinned());
        assert!(m.entry("freedos-14-floppy").unwrap().is_pinned());
        let w = m.entry("qemu-windows-x64").unwrap();
        assert!(!w.is_pinned());
        assert!(w.refusal().unwrap().contains("not pinned"));
        assert!(m.freedos.notice.contains("DOS mode -- runs FreeDOS"));
    }

    #[test]
    fn defaults_only_pinned_for_every_os() {
        let m = Manifest::embedded().unwrap();
        for os in [HostOs::Linux, HostOs::MacOs, HostOs::Windows] {
            for e in m.defaults(os) {
                assert!(e.refusal().is_none(), "{} default but refused", e.id);
            }
        }
    }

    fn with(sha: &str, url: &str) -> String {
        EMBEDDED
            .replace(
                "2020ff6bb681967fd6eff8f51ad2e5cd5ab4421165948cef4246e4f7fcaf6339",
                sha,
            )
            .replacen("https://download.freedos.org/1.4/FD14-LiveCD.zip", url, 1)
    }

    #[test]
    fn rejects_bad_hash_and_hosts() {
        assert!(Manifest::parse(&with("ABC", "https://download.freedos.org/x")).is_err());
        assert!(Manifest::parse(&with(&"a".repeat(64), "http://download.freedos.org/x")).is_err());
        assert!(Manifest::parse(&with(&"a".repeat(64), "https://evil.example/x")).is_err());
        assert!(Manifest::parse(&with(
            &"a".repeat(64),
            "https://download.freedos.org@evil.example/x"
        ))
        .is_err());
        assert!(Manifest::parse(&with(&"a".repeat(64), "https://download.freedos.org/x")).is_ok());
    }

    #[test]
    fn size_cap_is_one_percent_over() {
        let m = Manifest::embedded().unwrap();
        let e = m.entry("freedos-14-floppy").unwrap();
        assert_eq!(e.size_cap(), 23_624_258 + 236_242);
    }
}
