//! Launch of the system default browser in its own window (`:web external`, `web.open_external`).
//!
//! The in-pane browser canvas is CDP based; this is the escape hatch to the user's real browser.
//! URL validation and command construction are pure functions so they are testable without ever
//! spawning a process. The launcher passes the URL as a single argv element, never through a shell.

use std::process::{Command, Stdio};

/// Longest URL accepted, in bytes.
pub const MAX_EXTERNAL_URL_BYTES: usize = 8 * 1024;

/// Host platform flavour that selects the opener program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenerPlatform {
    Linux,
    MacOs,
    Windows,
}

impl OpenerPlatform {
    /// Platform this binary was compiled for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Validates `input` for hand-off to the system browser and returns the exact URL to open.
///
/// Only `http://`, `https://` and `file://` URLs with a non-empty remainder are accepted. Whitespace,
/// control characters, a leading `-`, and over-long input are refused.
pub fn validate_external_url(input: &str) -> Result<String, String> {
    let url = input.trim();
    if url.is_empty() {
        return Err("empty URL".to_string());
    }
    if url.len() > MAX_EXTERNAL_URL_BYTES {
        return Err(format!("URL longer than {MAX_EXTERNAL_URL_BYTES} bytes"));
    }
    if url.starts_with('-') {
        return Err("URL must not start with '-'".to_string());
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URL contains a control character or space".to_string());
    }
    let lower = url.to_ascii_lowercase();
    let rest = ["http://", "https://", "file://"].iter().find_map(|p| lower.strip_prefix(p));
    match rest {
        Some(r) if !r.is_empty() => Ok(url.to_string()),
        Some(_) => Err("URL has no host or path".to_string()),
        None => Err("only http, https and file URLs can be opened in the system browser".to_string()),
    }
}

/// Program and argv used to open `url` on `platform`. Pure; nothing is spawned.
///
/// Windows uses `rundll32 url.dll,FileProtocolHandler`, which receives the URL as one argument and
/// never passes it through `cmd.exe` (where `&` or `^` in a URL would be command injection).
pub fn opener_command(platform: OpenerPlatform, url: &str) -> (&'static str, Vec<String>) {
    match platform {
        OpenerPlatform::Linux => ("xdg-open", vec![url.to_string()]),
        OpenerPlatform::MacOs => ("open", vec![url.to_string()]),
        OpenerPlatform::Windows => ("rundll32", vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]),
    }
}

/// Spawns the system opener for an already validated `url`, detached from bilterm.
///
/// The child gets null stdio and its own process group (Unix) or a detached console (Windows), so
/// closing the pane or quitting bilterm never kills the browser. `BILTERM_NO_BROWSER` disables it.
pub fn launch_detached(url: &str) -> std::io::Result<()> {
    if std::env::var_os("BILTERM_NO_BROWSER").is_some() {
        return Err(std::io::Error::other("BILTERM_NO_BROWSER is set"));
    }
    let (program, args) = opener_command(OpenerPlatform::current(), url);
    let mut cmd = Command::new(program);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let mut child = cmd.spawn()?;
    // Reap in the background so the short-lived opener never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_https_file() {
        assert_eq!(validate_external_url(" https://example.com/a?b=1&c=2 ").unwrap(), "https://example.com/a?b=1&c=2");
        assert!(validate_external_url("HTTP://localhost:8080").is_ok());
        assert!(validate_external_url("file:///tmp/x.html").is_ok());
    }

    #[test]
    fn rejects_dangerous_or_malformed() {
        for bad in [
            "", "   ", "example.com", "javascript:alert(1)", "data:text/html,x", "chrome://settings", "ftp://x", "-flag",
            "--new-window https://x", "https://", "http://a b", "https://a\nb", "https://x\u{7}", "view-source:https://x",
            "file://", "mailto:a@b",
        ] {
            assert!(validate_external_url(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(validate_external_url(&format!("https://{}", "a".repeat(MAX_EXTERNAL_URL_BYTES))).is_err());
    }

    #[test]
    fn command_construction_is_one_argv_element_and_never_a_shell() {
        let url = "https://example.com/?a=1&b=2;rm -rf ~";
        let (p, a) = opener_command(OpenerPlatform::Linux, url);
        assert_eq!((p, a), ("xdg-open", vec![url.to_string()]));
        assert_eq!(opener_command(OpenerPlatform::MacOs, url), ("open", vec![url.to_string()]));
        let (p, a) = opener_command(OpenerPlatform::Windows, url);
        assert_eq!(p, "rundll32");
        assert_eq!(a, vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]);
        for plat in [OpenerPlatform::Linux, OpenerPlatform::MacOs, OpenerPlatform::Windows] {
            let (p, _) = opener_command(plat, url);
            assert!(!matches!(p, "sh" | "bash" | "cmd" | "cmd.exe"));
        }
    }
}
