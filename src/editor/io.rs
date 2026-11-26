//! Safe file IO for the editor (`EDIT-02`, `SEC-R01`): plain UTF-8 text only, regular files only,
//! no clobber without confirmation, atomic replace that keeps the file's mode.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Largest file the editor opens or saves.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Identity of the on-disk bytes at load/save time, used to notice external changes.
pub type DiskStamp = (SystemTime, u64);

#[derive(Debug)]
pub struct Loaded {
    pub text: String,
    pub crlf: bool,
    pub trailing_newline: bool,
    pub stamp: Option<DiskStamp>,
}

/// Why a save did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The target exists (save-as) or changed on disk since it was read: needs `force`.
    NeedsConfirm(String),
    Failed(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedsConfirm(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

/// Expands a leading `~/` and resolves relative paths against `cwd`; rejects empty or NUL paths.
pub fn resolve_path(raw: &str, cwd: &Path) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\0') {
        return Err("invalid path".into());
    }
    let p = if let Some(rest) = raw.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(rest)
    } else {
        PathBuf::from(raw)
    };
    Ok(if p.is_absolute() { p } else { cwd.join(p) })
}

/// `true` when a path component names a Windows device (`CON`, `NUL`, `COM1`, ...), with or without
/// an extension or trailing dots/spaces (`nul.txt`, `CON.`): such a name opens the device on Windows.
fn is_device_component(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches([' ', ':']);
    let up = stem.to_ascii_uppercase();
    matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|p| up.strip_prefix(p).is_some_and(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit()))
}

/// Confines a host `path` to `root` for API callers (`SEC-R01`). The deepest existing ancestor of
/// `path` is canonicalised (resolving every symlink and `..` along it), the not-yet-existing tail
/// must be plain names, and the result must stay inside the canonical `root`. A target that exists
/// is canonicalised too, so a symlink pointing outside is refused, and a dangling symlink is refused
/// because writing through it would create its target elsewhere. Windows device names, `\\.\` and
/// `\\?\` prefixes and UNC shares are refused. Returns the confined absolute path.
pub fn confine_to_root(path: &Path, root: &Path) -> Result<PathBuf, String> {
    use std::path::Component;
    let deny = |why: &str| format!("{}: {why}; API callers may only use files under {}", path.display(), root.display());
    let root = fs::canonicalize(root).map_err(|e| format!("{}: workspace root unavailable: {e}", root.display()))?;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut base = path.to_path_buf();
    // Walk up to the deepest ancestor that exists (a dangling symlink counts as existing).
    while fs::symlink_metadata(&base).is_err() {
        let Some(name) = base.file_name().map(|n| n.to_os_string()) else { return Err(deny("no existing ancestor (or '..' in a path that does not exist yet)")) };
        tail.push(name);
        if !base.pop() {
            return Err(deny("no existing ancestor"));
        }
    }
    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                let s = p.as_os_str().to_string_lossy();
                if s.starts_with("\\\\") || s.starts_with("//") {
                    return Err(deny("UNC, device and verbatim paths are refused"));
                }
            }
            Component::Normal(n) if is_device_component(&n.to_string_lossy()) => return Err(deny("reserved device name")),
            _ => {}
        }
    }
    let mut full = fs::canonicalize(&base).map_err(|e| deny(&format!("cannot resolve ({e})")))?;
    for n in tail.iter().rev() {
        full.push(n);
    }
    if !full.starts_with(&root) {
        return Err(deny("outside the workspace root"));
    }
    Ok(full)
}

fn stamp_of(md: &fs::Metadata) -> Option<DiskStamp> {
    md.modified().ok().map(|m| (m, md.len()))
}

/// Fast `stat`-only stamp for detecting external file updates on disk.
pub fn stat_stamp(path: &Path) -> Option<DiskStamp> {
    let md = fs::metadata(path).ok()?;
    md.is_file().then(|| stamp_of(&md)).flatten()
}

/// `true` when `path` does not exist yet (a new file); errors for anything that is not a regular file.
pub fn check_regular(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::metadata(path) {
        Ok(md) if md.is_file() => Ok(Some(md)),
        Ok(_) => Err(format!("{}: not a regular file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Reads `path` as UTF-8 text. `Ok(None)` means the file does not exist (open as a new file).
pub fn load(path: &Path) -> Result<Option<Loaded>, String> {
    let Some(md) = check_regular(path)? else { return Ok(None) };
    if md.len() > MAX_FILE_BYTES {
        return Err(format!("{}: file is larger than {} MiB", path.display(), MAX_FILE_BYTES >> 20));
    }
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.contains(&0) {
        return Err(format!("{}: binary file (contains NUL); the editor opens text and markdown only", path.display()));
    }
    let mut text = String::from_utf8(bytes).map_err(|_| format!("{}: not valid UTF-8", path.display()))?;
    let crlf = text.contains("\r\n");
    if crlf {
        text = text.replace("\r\n", "\n");
    }
    let trailing_newline = text.ends_with('\n');
    if trailing_newline {
        text.pop();
    }
    Ok(Some(Loaded { text, crlf, trailing_newline, stamp: stamp_of(&md) }))
}

/// Atomically writes `contents` to `path`.
///
/// - `must_not_exist`: save-as semantics; an existing target needs `force`.
/// - `expect`: the stamp from the last read/write; a different on-disk stamp needs `force`.
///
/// Symlinks are followed to their regular-file target; directories, devices and sockets are refused.
pub fn save(path: &Path, contents: &str, must_not_exist: bool, expect: Option<DiskStamp>, force: bool) -> Result<DiskStamp, SaveError> {
    let fail = |m: String| SaveError::Failed(format!("{}: {m}", path.display()));
    if contents.len() as u64 > MAX_FILE_BYTES {
        return Err(fail(format!("larger than {} MiB", MAX_FILE_BYTES >> 20)));
    }
    let existing = check_regular(path).map_err(SaveError::Failed)?;
    if let Some(md) = &existing {
        if must_not_exist && !force {
            return Err(SaveError::NeedsConfirm(format!("{} exists: confirm to overwrite", path.display())));
        }
        if !must_not_exist && !force && expect.is_some() && stamp_of(md) != expect {
            return Err(SaveError::NeedsConfirm(format!("{} changed on disk since it was read: confirm to overwrite", path.display())));
        }
    }
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(fail("parent directory does not exist".into()));
    }
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let tmp = parent.join(format!(".{name}.bilterm-tmp.{}", crate::secure_fs::unique_suffix()));
    let mut f = crate::secure_fs::create_private_file(&tmp).map_err(|e| fail(e.to_string()))?;
    let wrote = f.write_all(contents.as_bytes()).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = wrote {
        let _ = fs::remove_file(&tmp);
        return Err(fail(e.to_string()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = existing.as_ref().map_or(0o644, |m| m.permissions().mode() & 0o7777);
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
    }
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(fail(e.to_string()));
    }
    let md = fs::metadata(&target).map_err(|e| fail(e.to_string()))?;
    stamp_of(&md).ok_or_else(|| fail("no modification time".into()))
}

/// Directory holding scratchpad buffers, created private (0700).
pub fn ensure_scratch_dir(dir: &Path) -> std::io::Result<()> {
    crate::secure_fs::create_private_dir_all(dir)?;
    crate::secure_fs::ensure_private_dir(dir)
}

/// File backing the scratchpad `name` inside `dir`.
pub fn scratch_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    crate::secure_fs::validate_safe_name(name)?;
    Ok(dir.join(format!("{name}.md")))
}
