//! Loopback-only Artifact Studio & HTML/SVG preview server for `:editor preview` / `:board` (`EDIT-07`).
//!
//! Serves either:
//! - The interactive Shared Artifact Studio web app (`studio_web.rs`) wired live to `ContextBus`
//!   (`/api/state`, `/api/save`, `/api/create`, `/api/delete`, `/api/broadcast`), OR
//! - Raw `.html`/`.htm`/`.svg` buffer content directly for standalone HTML/JS/SVG artifacts
//!   (with `/api/*` disabled and `Content-Security-Policy: sandbox allow-scripts`).
//!
//! Bound strictly to `127.0.0.1:0` with a 256-bit OS CSPRNG path token verified in constant time,
//! loopback `Host` and same-origin `Origin` checks, `Referrer-Policy: no-referrer`, and
//! `Cache-Control: no-store`. Shuts down cleanly on drop.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::json::JsonValue;
use crate::editor::artifact::{ARTIFACT_KEY_PREFIX, artifact_bus_key, artifact_template, checklist_progress};
use crate::editor::buffer::TextBuffer;
use crate::editor::io as editor_io;
use crate::editor::studio_web::studio_app_html;
use crate::wm::context_bus::ContextBus;

const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_PENDING_BROADCASTS: usize = 32;
const MAX_BROADCAST_NOTE_CHARS: usize = 512;
const REQUEST_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct SharedPreview {
    bus: ContextBus,
    primary_name: Mutex<String>,
    active_seq: AtomicU64,
    raw_html: Mutex<Option<(String, &'static str)>>,
    pending_broadcasts: Mutex<Vec<(String, String)>>,
    persist_dir: Option<PathBuf>,
    stop: AtomicBool,
}

/// Live handle to a loopback preview / Artifact Studio server owned by an editor window or `:board`.
#[derive(Debug)]
pub struct PreviewServer {
    url: String,
    shared: Arc<SharedPreview>,
}

/// Generates a 256-bit hex token from the OS CSPRNG (`/dev/urandom` on Unix, `BCryptGenRandom` on Windows).
fn generate_preview_token() -> std::io::Result<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
        Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
    }
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                h_algorithm: *mut std::ffi::c_void,
                pb_buffer: *mut u8,
                cb_buffer: u32,
                dw_flags: u32,
            ) -> i32;
        }
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
        let mut buf = [0u8; 32];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(std::io::Error::other(format!("BCryptGenRandom failed: {status:#x}")));
        }
        Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "OS CSPRNG unavailable on this platform",
        ))
    }
}

impl PreviewServer {
    /// Starts a loopback server on an ephemeral port with a 256-bit OS CSPRNG path token.
    ///
    /// When `raw_body` is `Some((body, content_type))`, serves raw HTML/SVG directly with `/api/*`
    /// disabled; when `None`, serves the interactive Shared Artifact Studio web app backed by `bus`.
    /// When `persist_dir` is `Some`, Studio saves/creates/deletes also persist to `0600` `.md` files.
    pub fn start(
        bus: ContextBus,
        primary_name: String,
        raw_body: Option<(String, &'static str)>,
        persist_dir: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token = generate_preview_token()?;
        let url = format!("http://127.0.0.1:{port}/{token}/");
        let shared = Arc::new(SharedPreview {
            bus,
            primary_name: Mutex::new(primary_name),
            active_seq: AtomicU64::new(1),
            raw_html: Mutex::new(raw_body),
            pending_broadcasts: Mutex::new(Vec::new()),
            persist_dir,
            stop: AtomicBool::new(false),
        });
        let bg = Arc::clone(&shared);
        let tok = token;
        std::thread::Builder::new()
            .name("bilterm-editor-preview".into())
            .spawn(move || serve_loop(listener, tok, bg))?;
        Ok(Self { url, shared })
    }

    /// Loopback URL (`http://127.0.0.1:<port>/<token>/`).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Updates raw HTML/SVG body or primary artifact name in memory and bumps `active_seq` when switching docs.
    pub fn update(&self, primary_name: String, raw_body: Option<(String, &'static str)>) {
        if let Ok(mut g) = self.shared.primary_name.lock()
            && *g != primary_name
        {
            *g = primary_name;
            self.shared.active_seq.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(mut g) = self.shared.raw_html.lock() {
            *g = raw_body;
        }
    }

    /// Drains any `(artifact_name, note)` broadcast requests clicked inside the web UI.
    pub fn take_pending_broadcasts(&self) -> Vec<(String, String)> {
        self.shared
            .pending_broadcasts
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}

impl Drop for PreviewServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// Normalizes a user-entered title or filename (e.g. `"UI Spec 2"`, `"README.md"`) into a
/// `validate_safe_name`-compliant artifact slug (`[a-z0-9_-]{1,64}`, no dots).
pub fn slugify_artifact_name(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

fn next_auto_doc_name(bus: &ContextBus) -> String {
    let existing = bus.list(Some(ARTIFACT_KEY_PREFIX));
    for n in 1..=999 {
        let cand = format!("doc-{n}");
        let key = artifact_bus_key(&cand);
        if !existing.iter().any(|e| e.key == key) {
            return cand;
        }
    }
    slugify_artifact_name(&format!("doc-{}", crate::secure_fs::unique_suffix()))
}

fn host_only(hostport: &str) -> &str {
    if let Some(rest) = hostport.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
}

fn is_loopback_host(h: &str) -> bool {
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    host: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

enum ParseError {
    Empty,
    Status(&'static str, &'static str),
}

fn read_http_request<R: Read>(stream: &mut R) -> Result<HttpRequest, ParseError> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    let mut content_len: usize = 0;
    let mut scan_from: usize = 0;
    let deadline = Instant::now() + REQUEST_DEADLINE;

    loop {
        if Instant::now() >= deadline {
            return Err(ParseError::Status("408 Request Timeout", "request timed out"));
        }
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if header_end.is_none() {
                    let search_start = scan_from.saturating_sub(3);
                    if let Some(rel) = buf[search_start..].windows(4).position(|w| w == b"\r\n\r\n") {
                        let pos = search_start + rel;
                        if pos > MAX_HEAD_BYTES {
                            return Err(ParseError::Status(
                                "431 Request Header Fields Too Large",
                                "headers too large",
                            ));
                        }
                        header_end = Some(pos + 4);
                        let hdrs = std::str::from_utf8(&buf[..pos])
                            .map_err(|_| ParseError::Status("400 Bad Request", "invalid header encoding"))?;
                        let mut saw_cl = false;
                        for line in hdrs.lines().skip(1) {
                            let Some((k, v)) = line.split_once(':') else { continue };
                            let key = k.trim();
                            if key.eq_ignore_ascii_case("transfer-encoding") {
                                return Err(ParseError::Status(
                                    "400 Bad Request",
                                    "transfer-encoding not supported",
                                ));
                            }
                            if key.eq_ignore_ascii_case("content-length") {
                                if saw_cl {
                                    return Err(ParseError::Status(
                                        "400 Bad Request",
                                        "duplicate content-length",
                                    ));
                                }
                                saw_cl = true;
                                let parsed = v
                                    .trim()
                                    .parse::<usize>()
                                    .map_err(|_| ParseError::Status("400 Bad Request", "invalid content-length"))?;
                                if parsed > MAX_BODY_BYTES {
                                    return Err(ParseError::Status(
                                        "413 Payload Too Large",
                                        "payload too large",
                                    ));
                                }
                                content_len = parsed;
                            }
                        }
                    } else {
                        scan_from = buf.len();
                        if buf.len() > MAX_HEAD_BYTES {
                            return Err(ParseError::Status(
                                "431 Request Header Fields Too Large",
                                "headers too large",
                            ));
                        }
                    }
                }
                if let Some(he) = header_end {
                    if buf.len() >= he + content_len {
                        break;
                    }
                    if buf.len() > he + MAX_BODY_BYTES {
                        return Err(ParseError::Status("413 Payload Too Large", "payload too large"));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if Instant::now() >= deadline {
                    return Err(ParseError::Status("408 Request Timeout", "request timed out"));
                }
                continue;
            }
            Err(_) => break,
        }
    }

    if buf.is_empty() {
        return Err(ParseError::Empty);
    }
    let he = header_end.ok_or(ParseError::Status("400 Bad Request", "incomplete headers"))?;
    if buf.len() < he + content_len {
        return Err(ParseError::Status("400 Bad Request", "incomplete request body"));
    }
    let head = std::str::from_utf8(&buf[..he - 4])
        .map_err(|_| ParseError::Status("400 Bad Request", "invalid header encoding"))?;
    let mut lines = head.lines();
    let first = lines.next().ok_or(ParseError::Status("400 Bad Request", "empty request"))?;
    let mut parts = first.split_whitespace();
    let method = parts
        .next()
        .ok_or(ParseError::Status("400 Bad Request", "missing method"))?
        .to_string();
    let raw_path = parts
        .next()
        .ok_or(ParseError::Status("400 Bad Request", "missing path"))?;
    let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();

    let mut host = None;
    let mut origin = None;
    let mut content_type = None;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim();
        let val = v.trim();
        if key.eq_ignore_ascii_case("host") {
            host = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("origin") {
            origin = Some(val.to_string());
        } else if key.eq_ignore_ascii_case("content-type") {
            content_type = Some(val.to_string());
        }
    }

    Ok(HttpRequest {
        method,
        path,
        host,
        origin,
        content_type,
        body: buf[he..he + content_len].to_vec(),
    })
}

fn write_response<W: Write>(
    stream: &mut W,
    status: &str,
    ct: &str,
    body: &[u8],
    head_only: bool,
    extra_headers: &str,
) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n{extra_headers}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
}

fn persist_artifact(shared: &SharedPreview, name: &str, text: &str) {
    let Some(dir) = &shared.persist_dir else { return };
    if let Ok(path) = editor_io::scratch_file(dir, name)
        && editor_io::ensure_scratch_dir(dir).is_ok()
    {
        let _ = crate::secure_fs::write_private_file(&path, text.as_bytes());
    }
}

fn remove_persisted_artifact(shared: &SharedPreview, name: &str) {
    let Some(dir) = &shared.persist_dir else { return };
    if let Ok(path) = editor_io::scratch_file(dir, name) {
        let _ = std::fs::remove_file(path);
    }
}

fn handle_api(sub: &str, method: &str, body: &[u8], shared: &SharedPreview) -> (&'static str, String) {
    match (method, sub) {
        ("GET", "/api/state") => {
            let entries = shared.bus.list(Some(ARTIFACT_KEY_PREFIX));
            let mut docs = Vec::with_capacity(entries.len());
            for e in entries {
                let name = e.key.strip_prefix(ARTIFACT_KEY_PREFIX).unwrap_or(&e.key).to_string();
                let tb = TextBuffer::from_text(&e.value);
                let (done, total) = checklist_progress(&tb);
                docs.push(JsonValue::obj([
                    ("name", JsonValue::String(name)),
                    ("key", JsonValue::String(e.key)),
                    ("text", JsonValue::String(e.value)),
                    ("revision", JsonValue::Number(e.revision as i64)),
                    ("done", JsonValue::Number(done as i64)),
                    ("total", JsonValue::Number(total as i64)),
                ]));
            }
            let active = shared.primary_name.lock().map(|g| g.clone()).unwrap_or_else(|_| "shared".into());
            let active_seq = shared.active_seq.load(Ordering::Relaxed);
            let payload = JsonValue::obj([
                ("ok", JsonValue::Bool(true)),
                ("active", JsonValue::String(active)),
                ("active_seq", JsonValue::Number(active_seq as i64)),
                ("docs", JsonValue::Array(docs)),
            ]);
            ("200 OK", payload.to_json_string())
        }
        ("POST", "/api/save") => {
            let Some(v) = std::str::from_utf8(body).ok().and_then(JsonValue::parse) else {
                return ("400 Bad Request", r#"{"ok":false,"error":"invalid json"}"#.into());
            };
            let name = v.get("name").and_then(JsonValue::as_str).unwrap_or("");
            let text = v.get("text").and_then(JsonValue::as_str).unwrap_or("");
            if let Err(e) = crate::secure_fs::validate_safe_name(name) {
                return (
                    "400 Bad Request",
                    JsonValue::obj([("ok", JsonValue::Bool(false)), ("error", JsonValue::String(e))]).to_json_string(),
                );
            }
            match shared.bus.set(&artifact_bus_key(name), text, None) {
                Ok(rev) => {
                    persist_artifact(shared, name, text);
                    (
                        "200 OK",
                        JsonValue::obj([("ok", JsonValue::Bool(true)), ("revision", JsonValue::Number(rev as i64))]).to_json_string(),
                    )
                }
                Err(e) => (
                    "400 Bad Request",
                    JsonValue::obj([("ok", JsonValue::Bool(false)), ("error", JsonValue::String(e.to_string()))]).to_json_string(),
                ),
            }
        }
        ("POST", "/api/create") => {
            let Some(v) = std::str::from_utf8(body).ok().and_then(JsonValue::parse) else {
                return ("400 Bad Request", r#"{"ok":false,"error":"invalid json"}"#.into());
            };
            let raw_name = v.get("name").and_then(JsonValue::as_str).unwrap_or("");
            let name = {
                let slug = slugify_artifact_name(raw_name);
                if slug.is_empty() { next_auto_doc_name(&shared.bus) } else { slug }
            };
            if let Err(e) = crate::secure_fs::validate_safe_name(&name) {
                return (
                    "400 Bad Request",
                    JsonValue::obj([("ok", JsonValue::Bool(false)), ("error", JsonValue::String(e))]).to_json_string(),
                );
            }
            let key = artifact_bus_key(&name);
            if shared.bus.get(&key).is_none() {
                let initial = artifact_template(&name);
                if let Err(e) = shared.bus.set(&key, &initial, None) {
                    return (
                        "400 Bad Request",
                        JsonValue::obj([("ok", JsonValue::Bool(false)), ("error", JsonValue::String(e.to_string()))]).to_json_string(),
                    );
                }
                persist_artifact(shared, &name, &initial);
            }
            ("200 OK", JsonValue::obj([("ok", JsonValue::Bool(true)), ("name", JsonValue::String(name))]).to_json_string())
        }
        ("POST", "/api/delete") => {
            let Some(v) = std::str::from_utf8(body).ok().and_then(JsonValue::parse) else {
                return ("400 Bad Request", r#"{"ok":false,"error":"invalid json"}"#.into());
            };
            let name = v.get("name").and_then(JsonValue::as_str).unwrap_or("");
            if let Err(e) = crate::secure_fs::validate_safe_name(name) {
                return (
                    "400 Bad Request",
                    JsonValue::obj([("ok", JsonValue::Bool(false)), ("error", JsonValue::String(e))]).to_json_string(),
                );
            }
            let deleted = shared.bus.delete(&artifact_bus_key(name));
            if deleted {
                remove_persisted_artifact(shared, name);
            }
            ("200 OK", JsonValue::obj([("ok", JsonValue::Bool(deleted))]).to_json_string())
        }
        ("POST", "/api/broadcast") => {
            let Some(v) = std::str::from_utf8(body).ok().and_then(JsonValue::parse) else {
                return ("400 Bad Request", r#"{"ok":false,"error":"invalid json"}"#.into());
            };
            let name = v.get("name").and_then(JsonValue::as_str).unwrap_or("");
            if let Err(e) = crate::secure_fs::validate_safe_name(name) {
                return (
                    "400 Bad Request",
                    JsonValue::obj([("ok", JsonValue::Bool(false)), ("error", JsonValue::String(e))]).to_json_string(),
                );
            }
            let raw_note = v.get("note").and_then(JsonValue::as_str).unwrap_or("");
            let note: String = raw_note
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_BROADCAST_NOTE_CHARS)
                .collect();
            if let Ok(mut q) = shared.pending_broadcasts.lock() {
                if q.len() >= MAX_PENDING_BROADCASTS {
                    return ("429 Too Many Requests", r#"{"ok":false,"error":"broadcast queue full"}"#.into());
                }
                q.push((name.to_string(), note));
            }
            ("200 OK", r#"{"ok":true}"#.into())
        }
        _ => ("404 Not Found", r#"{"ok":false,"error":"not found"}"#.into()),
    }
}

/// Validates loopback `Host`, constant-time path token, same-origin CSRF defenses, and dispatches one HTTP exchange.
fn handle_http_exchange<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    token: &str,
    shared: &SharedPreview,
) {
    let req = match read_http_request(reader) {
        Ok(r) => r,
        Err(ParseError::Empty) => return,
        Err(ParseError::Status(status, msg)) => {
            write_response(writer, status, "text/plain; charset=utf-8", msg.as_bytes(), false, "");
            return;
        }
    };
    let head_only = req.method == "HEAD";

    // DNS-rebinding defense: Host must be present and loopback.
    let Some(host) = req.host.as_deref() else {
        write_response(writer, "403 Forbidden", "text/plain; charset=utf-8", b"missing Host header", head_only, "");
        return;
    };
    if !is_loopback_host(host_only(host)) {
        write_response(writer, "403 Forbidden", "text/plain; charset=utf-8", b"Host not allowed", head_only, "");
        return;
    }

    // Constant-time comparison of the first path segment against the CSPRNG token.
    let Some(after_slash) = req.path.strip_prefix('/') else {
        write_response(writer, "404 Not Found", "text/plain; charset=utf-8", b"Not Found", head_only, "");
        return;
    };
    let (seg, sub) = match after_slash.find('/') {
        Some(idx) => (&after_slash[..idx], &after_slash[idx..]),
        None => (after_slash, ""),
    };
    if !crate::secure_fs::constant_time_eq(seg.as_bytes(), token.as_bytes()) {
        write_response(writer, "404 Not Found", "text/plain; charset=utf-8", b"Not Found", head_only, "");
        return;
    }

    let raw = shared.raw_html.lock().ok().and_then(|g| g.clone());
    if sub.is_empty() || sub == "/" {
        if req.method != "GET" && req.method != "HEAD" {
            write_response(writer, "405 Method Not Allowed", "text/plain; charset=utf-8", b"", head_only, "");
            return;
        }
        if let Some((html, ct)) = raw {
            // Isolate raw previewed HTML/SVG in a sandboxed opaque origin (no same-origin API access).
            write_response(
                writer,
                "200 OK",
                ct,
                html.as_bytes(),
                head_only,
                "Content-Security-Policy: sandbox allow-scripts\r\n",
            );
        } else {
            let init = shared.primary_name.lock().map(|g| g.clone()).unwrap_or_else(|_| "shared".into());
            let base_path = format!("/{token}");
            let page = studio_app_html(&base_path, &init);
            write_response(writer, "200 OK", "text/html; charset=utf-8", page.as_bytes(), head_only, "");
        }
    } else if sub.starts_with("/api/") {
        // Never expose ContextBus mutation/read APIs alongside raw HTML/SVG previews.
        if raw.is_some() {
            write_response(writer, "404 Not Found", "application/json; charset=utf-8", br#"{"ok":false,"error":"not found"}"#, head_only, "");
            return;
        }
        if req.method == "POST" {
            if let Some(origin) = req.origin.as_deref() {
                let same_origin = origin
                    .strip_prefix("http://")
                    .is_some_and(|o| o.eq_ignore_ascii_case(host));
                if !same_origin {
                    write_response(
                        writer,
                        "403 Forbidden",
                        "application/json; charset=utf-8",
                        br#"{"ok":false,"error":"cross-origin request refused"}"#,
                        head_only,
                        "",
                    );
                    return;
                }
            }
            let is_json = req
                .content_type
                .as_deref()
                .and_then(|ct| ct.split(';').next())
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"));
            if !is_json {
                write_response(
                    writer,
                    "415 Unsupported Media Type",
                    "application/json; charset=utf-8",
                    br#"{"ok":false,"error":"Content-Type must be application/json"}"#,
                    head_only,
                    "",
                );
                return;
            }
        }
        let (status, json) = handle_api(sub, &req.method, &req.body, shared);
        write_response(writer, status, "application/json; charset=utf-8", json.as_bytes(), head_only, "");
    } else {
        write_response(writer, "404 Not Found", "text/plain; charset=utf-8", b"Not Found", head_only, "");
    }
}

fn serve_loop(listener: TcpListener, token: String, shared: Arc<SharedPreview>) {
    while !shared.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // On macOS/BSD, accepted streams inherit O_NONBLOCK from the listener.
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));
                let Ok(mut reader) = stream.try_clone() else { continue };
                handle_http_exchange(&mut reader, &mut stream, &token, &shared);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_shared(raw: Option<(String, &'static str)>) -> SharedPreview {
        let bus = ContextBus::new();
        bus.set(&artifact_bus_key("plan"), "# Plan\nParagraph here\n- [ ] Task 1", None).unwrap();
        SharedPreview {
            bus,
            primary_name: Mutex::new("plan".into()),
            active_seq: AtomicU64::new(1),
            raw_html: Mutex::new(raw),
            pending_broadcasts: Mutex::new(Vec::new()),
            persist_dir: None,
            stop: AtomicBool::new(false),
        }
    }

    fn exchange(raw_req: &[u8], token: &str, shared: &SharedPreview) -> String {
        let mut reader = Cursor::new(raw_req);
        let mut out = Vec::new();
        handle_http_exchange(&mut reader, &mut out, token, shared);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn http_request_parser_handles_get_post_and_rejects_malformed() {
        let get = b"GET /tok/api/state?x=1 HTTP/1.1\r\nHost: 127.0.0.1:9999\r\n\r\n";
        let req = read_http_request(&mut Cursor::new(&get[..])).unwrap_or_else(|_| panic!("GET failed"));
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/tok/api/state");
        assert!(req.body.is_empty());

        let post = b"POST /tok/api/create HTTP/1.1\r\nHost: 127.0.0.1:9999\r\nContent-Type: application/json\r\nContent-Length: 20\r\n\r\n{\"name\":\"UI Spec 1\"}";
        let req = read_http_request(&mut Cursor::new(&post[..])).unwrap_or_else(|_| panic!("POST failed"));
        assert_eq!(req.method, "POST");
        assert_eq!(req.body, br#"{"name":"UI Spec 1"}"#);

        // Truncated body -> 400
        let trunc = b"POST /tok/api/save HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 50\r\n\r\nshort";
        assert!(matches!(
            read_http_request(&mut Cursor::new(&trunc[..])),
            Err(ParseError::Status("400 Bad Request", _))
        ));

        // Oversized Content-Length -> 413
        let big = b"POST /tok/api/save HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 999999\r\n\r\n";
        assert!(matches!(
            read_http_request(&mut Cursor::new(&big[..])),
            Err(ParseError::Status("413 Payload Too Large", _))
        ));

        // Duplicate Content-Length -> 400
        let dup = b"POST /tok/api/save HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        assert!(matches!(
            read_http_request(&mut Cursor::new(&dup[..])),
            Err(ParseError::Status("400 Bad Request", _))
        ));

        // Transfer-Encoding -> 400
        let chunked = b"POST /tok/api/save HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
        assert!(matches!(
            read_http_request(&mut Cursor::new(&chunked[..])),
            Err(ParseError::Status("400 Bad Request", _))
        ));
    }

    #[test]
    fn security_guards_enforce_token_host_origin_content_type_and_raw_isolation() {
        let shared = test_shared(None);
        let tok = "deadbeef01234567";

        // Wrong token -> 404
        let res = exchange(b"GET /wrong/api/state HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n\r\n", tok, &shared);
        assert!(res.starts_with("HTTP/1.1 404"), "{res}");

        // Non-loopback Host -> 403 (DNS rebinding defense)
        let res = exchange(b"GET /deadbeef01234567/api/state HTTP/1.1\r\nHost: evil.example:8080\r\n\r\n", tok, &shared);
        assert!(res.starts_with("HTTP/1.1 403"), "{res}");

        // Cross-origin POST -> 403
        let body = br#"{"name":"plan","text":"pwn"}"#;
        let req = format!(
            "POST /{tok}/api/save HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nOrigin: http://evil.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let res = exchange(req.as_bytes(), tok, &shared);
        assert!(res.starts_with("HTTP/1.1 403"), "{res}");

        // Non-JSON Content-Type on POST -> 415
        let req = format!(
            "POST /{tok}/api/save HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let res = exchange(req.as_bytes(), tok, &shared);
        assert!(res.starts_with("HTTP/1.1 415"), "{res}");

        // Studio page sets Referrer-Policy: no-referrer
        let res = exchange(b"GET /deadbeef01234567/ HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n\r\n", tok, &shared);
        assert!(res.starts_with("HTTP/1.1 200"), "{res}");
        assert!(res.contains("Referrer-Policy: no-referrer"), "{res}");

        // Raw HTML preview disables /api/* and applies sandbox CSP
        let raw_shared = test_shared(Some(("<h1>Hi</h1>".into(), "text/html; charset=utf-8")));
        let res = exchange(b"GET /deadbeef01234567/ HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n\r\n", tok, &raw_shared);
        assert!(res.starts_with("HTTP/1.1 200"), "{res}");
        assert!(res.contains("Content-Security-Policy: sandbox allow-scripts"), "{res}");
        let api_res = exchange(b"GET /deadbeef01234567/api/state HTTP/1.1\r\nHost: 127.0.0.1:8080\r\n\r\n", tok, &raw_shared);
        assert!(api_res.starts_with("HTTP/1.1 404"), "{api_res}");
    }

    #[test]
    fn slugify_strips_dots_and_api_crud_and_broadcast_work_end_to_end() {
        assert_eq!(slugify_artifact_name("README.md"), "readme-md");
        assert_eq!(slugify_artifact_name("v1.0 spec"), "v1-0-spec");
        assert!(crate::secure_fs::validate_safe_name(&slugify_artifact_name("v1.0 spec")).is_ok());

        let shared = test_shared(None);
        // Auto-name when empty
        let (st, res) = handle_api("/api/create", "POST", br#"{"name":""}"#, &shared);
        assert_eq!(st, "200 OK");
        assert!(res.contains("\"name\":\"doc-1\""), "{res}");

        // Dotted title slugifies to a valid safe name and saves cleanly
        let (st, res) = handle_api("/api/create", "POST", br#"{"name":"v1.0 Spec"}"#, &shared);
        assert_eq!(st, "200 OK");
        assert!(res.contains("\"name\":\"v1-0-spec\""), "{res}");

        // Save edited paragraph + multiline text
        let payload = br##"{"name":"plan","text":"# Plan\nUpdated instruction paragraph!\n- [x] Task 1"}"##;
        let (st, res) = handle_api("/api/save", "POST", payload, &shared);
        assert_eq!(st, "200 OK");
        assert!(res.contains("\"ok\":true"), "{res}");

        let (st, state) = handle_api("/api/state", "GET", b"", &shared);
        assert_eq!(st, "200 OK");
        assert!(state.contains("Updated instruction paragraph!"), "{state}");
        assert!(state.contains("\"done\":1"), "{state}");
        assert!(state.contains("\"active\":\"plan\""), "{state}");
        assert!(state.contains("\"active_seq\":1"), "{state}");

        // Broadcast validates JSON + safe name and queues sanitized note
        let (st, _) = handle_api("/api/broadcast", "POST", b"not-json", &shared);
        assert_eq!(st, "400 Bad Request");
        let (st, _) = handle_api("/api/broadcast", "POST", br#"{"name":"../bad","note":"hi"}"#, &shared);
        assert_eq!(st, "400 Bad Request");
        let (st, _) = handle_api("/api/broadcast", "POST", br#"{"name":"plan","note":"ready\u001b[31m"}"#, &shared);
        assert_eq!(st, "200 OK");
        let queued = shared.pending_broadcasts.lock().unwrap().clone();
        assert_eq!(queued, vec![("plan".to_string(), "ready[31m".to_string())]);

        // Delete removes the doc
        let (st, del) = handle_api("/api/delete", "POST", br#"{"name":"doc-1"}"#, &shared);
        assert_eq!(st, "200 OK");
        assert!(del.contains("\"ok\":true"), "{del}");
    }

    #[test]
    fn csprng_token_has_64_hex_chars_and_varies() {
        let a = generate_preview_token().unwrap();
        let b = generate_preview_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
