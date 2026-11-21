//! Deterministic screenshot generator for the README, the visual gallery and the website.
//!
//! Drives a headless `WindowManager` through the same `Action`/colon/mouse paths the real UI uses,
//! renders the real `WorkspaceWidget` into a ratatui `Buffer`, and writes one SVG per scene
//! (`scripts/gen-screenshots.sh` rasterises them to PNG). No PTY, network or clock is involved, so
//! two runs produce byte-identical files.
//!
//! Usage: `cargo run --example gen_screenshots --features dos -- <out_dir> [scene ...]`
//! (`dos` is only needed for the `dos-mode` scene; without it that scene is skipped.)

use std::fmt::Write as _;
use std::path::PathBuf;

use bilterm::colon_command::parse_colon_command;
use bilterm::ui::WorkspaceWidget;
use bilterm::wm::{SplitDirection, WindowManager};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::Widget;

const COLS: u16 = 110;
const ROWS: u16 = 32;
const CW: f32 = 9.0;
const CH: f32 = 18.0;
const FONT: &str = "'DejaVu Sans Mono','Liberation Mono',monospace";

const BUILD: &str = "\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/src/bilterm\x1b[0m$ cargo build --release\r\n\
   \x1b[1;32mCompiling\x1b[0m bilterm v3.4.0\r\n\
   \x1b[1;33mwarning\x1b[0m: unused variable `x`\r\n\
    \x1b[1;32mFinished\x1b[0m release profile in 41.2s\r\n\
\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/src/bilterm\x1b[0m$ ls --color\r\n\
\x1b[34mdocs\x1b[0m  \x1b[34msrc\x1b[0m  \x1b[34mtests\x1b[0m  Cargo.toml  README.md\r\n";
const LOGS: &str = "\x1b[36m[12:01:07]\x1b[0m worker-3 accepted job 4812\r\n\
\x1b[36m[12:01:08]\x1b[0m worker-1 \x1b[33mretry\x1b[0m 2/5 upstream timeout\r\n\
\x1b[36m[12:01:09]\x1b[0m worker-3 \x1b[32mdone\x1b[0m 4812 in 1.8s\r\n\
\x1b[36m[12:01:11]\x1b[0m queue depth 3\r\n";
const AGENT: &str = "\x1b[1;35mclaude\x1b[0m> refactor the drawer reorder into window_manage\r\n\
\x1b[2m  reading src/wm/drawer/ops.rs ...\x1b[0m\r\n\
\x1b[32m  edited 3 files, 14 tests pass\x1b[0m\r\n";

fn colon(wm: &mut WindowManager, cmd: &str) {
    let a = parse_colon_command(cmd).unwrap_or_else(|| panic!("{cmd}: not a command")).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    wm.apply(a).unwrap_or_else(|e| panic!("{cmd}: {e}"));
}

fn feed(wm: &mut WindowManager, id: usize, text: &str) {
    wm.pane_mut(id).unwrap().vt_mut().process(text.as_bytes());
}

fn area() -> Rect {
    Rect::new(0, 0, COLS, ROWS)
}

/// Three tiled windows with realistic output; returns the ids.
fn base() -> (WindowManager, usize, usize, usize) {
    let mut wm = WindowManager::new_headless(ROWS, COLS);
    let a = wm.active_pane_id();
    let b = wm.split_focused(SplitDirection::Vertical).unwrap();
    let c = wm.split_focused(SplitDirection::Horizontal).unwrap();
    wm.pane_mut(a).unwrap().set_title("build");
    wm.pane_mut(b).unwrap().set_title("logs");
    wm.pane_mut(c).unwrap().set_title("agent: claude");
    for id in [a, b, c] {
        wm.pane_mut(id).unwrap().set_prompt_anchor(bilterm::pane::PromptAnchor::Bottom);
    }
    wm.resize_all(area()).unwrap();
    feed(&mut wm, a, BUILD);
    feed(&mut wm, b, LOGS);
    feed(&mut wm, c, AGENT);
    colon(&mut wm, &format!(":focus {a}"));
    (wm, a, b, c)
}

type Scene = (&'static str, &'static str, fn() -> Option<WindowManager>);

fn scene_workspace() -> Option<WindowManager> {
    let (mut wm, _, _, _) = base();
    colon(&mut wm, ":theme global turbo-blue");
    Some(wm)
}

fn scene_drawer() -> Option<WindowManager> {
    let (mut wm, _, _, _) = base();
    colon(&mut wm, ":theme global turbo-blue");
    colon(&mut wm, ":drawer show");
    wm.resize_all(area()).unwrap();
    Some(wm)
}

fn scene_session_find() -> Option<WindowManager> {
    let (mut wm, _a, b, _c) = base();
    colon(&mut wm, ":theme global turbo-blue");
    // `Ctrl+X &` prefills the prompt with `:find `; the located pane is flashed.
    colon(&mut wm, ":find-flash --text retry");
    let _ = b;
    wm.active_pane_mut().prompt_mut().set_text(":find --text retry ");
    wm.active_pane_mut().prompt_mut().force_command_prompt = true;
    Some(wm)
}

fn scene_editor() -> Option<WindowManager> {
    let mut wm = WindowManager::new_headless(ROWS, COLS);
    colon(&mut wm, ":theme global turbo-blue");
    let first = wm.active_pane_id();
    colon(&mut wm, ":scratch");
    let id = wm.active_pane_id();
    wm.remove_pane(first);
    wm.pane_mut(id).unwrap().set_title("scratchpad (autosaved)");
    wm.resize_all(area()).unwrap();
    for ch in "# Release notes\n\n- drawer drag and drop\n- session find (Ctrl+X &)\n- DOS EDIT style menu bar\n".chars() {
        let _ = wm.editor_paste(&ch.to_string());
    }
    // Alt+H opens the Help drop-down (F10 opens File), so the text stays readable next to it.
    let ev = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('h'), crossterm::event::KeyModifiers::ALT);
    let _ = bilterm::input::handle_wm_key_event(&mut wm, ev);
    Some(wm)
}

fn scene_pane_mail() -> Option<WindowManager> {
    let (mut wm, a, b, _c) = base();
    colon(&mut wm, ":theme global turbo-blue");
    // Mail arrives at the logs pane (closed, blinking envelope) and the agent pane.
    let _ = a;
    wm.config_mut().ui.pane_mail_flash = false; // steady closed envelope: no clock-dependent blink
    let now = std::time::Instant::now();
    wm.note_pane_message(b, "agent-2", now);
    wm.note_pane_message(b, "agent-2", now);
    wm.note_pane_message(_c, "api", now);
    wm.pane_mail_ack(Some(_c), now); // read: open envelope
    Some(wm)
}

fn scene_remote() -> Option<WindowManager> {
    let (mut wm, a, b, c) = base();
    colon(&mut wm, ":theme global turbo-blue");
    wm.pane_mut(b).unwrap().set_remote_target(Some("root@web1".into()));
    wm.pane_mut(c).unwrap().set_remote_target(Some("deploy@build-farm".into()));
    wm.pane_mut(b).unwrap().set_title("web1: journalctl -f");
    let _ = a;
    colon(&mut wm, ":drawer show");
    wm.resize_all(area()).unwrap();
    Some(wm)
}

fn scene_drag() -> Option<WindowManager> {
    let (mut wm, a, b, _c) = base();
    colon(&mut wm, ":theme global turbo-blue");
    let ws = bilterm::ui::shell::ShellLayout::compute(area(), wm.config(), wm.pane_count()).workspace;
    let rects = wm.compute_workspace_rects(ws);
    let r = rects.iter().find(|(id, _)| *id == b).map(|(_, r)| *r).unwrap();
    // Drag `build` over the right edge band of `logs`.
    wm.window_drag_update(a, r.x + r.width - 1, r.y + r.height / 2, ws, None);
    Some(wm)
}

fn scene_theme(name: &'static str) -> Option<WindowManager> {
    let (mut wm, _, _, _) = base();
    if name == "tron" {
        colon(&mut wm, ":tron on");
    } else {
        colon(&mut wm, &format!(":theme global {name}"));
    }
    Some(wm)
}

fn scene_browser() -> Option<WindowManager> {
    // SAFETY: single-threaded generator; selects the deterministic placeholder engine.
    unsafe { std::env::set_var("BILTERM_BROWSER_SYNTHETIC", "1") };
    let mut wm = WindowManager::new_headless(ROWS, COLS);
    colon(&mut wm, ":theme global turbo-blue");
    let id = wm.active_pane_id();
    wm.pane_mut(id).unwrap().open_browser("https://example.com");
    wm.resize_all(area()).unwrap();
    Some(wm)
}

#[cfg(feature = "dos")]
fn scene_dos() -> Option<WindowManager> {
    use bilterm::dos::backend::render::render_ansi;
    use bilterm::dos::backend::screen::{TextCell, TextScreen};
    let mut wm = WindowManager::new_headless(ROWS, COLS);
    colon(&mut wm, ":theme global turbo-blue");
    let id = wm.active_pane_id();
    wm.pane_mut(id).unwrap().set_title("DOS:default [#1]");
    wm.resize_all(area()).unwrap();
    let mut s = TextScreen::blank();
    let put = |s: &mut TextScreen, r: usize, c: usize, t: &str, attr: u8| {
        for (i, b) in t.bytes().enumerate() {
            s.cells[r * 80 + c + i] = TextCell::from_raw(b, attr);
        }
    };
    put(&mut s, 0, 0, "Starting FreeDOS...", 0x07);
    put(&mut s, 2, 0, "FreeCom version 0.85a - WATCOMC - Aug  5 2022 XMS_Swap [Oct  8 2025 00:06:37]", 0x07);
    put(&mut s, 4, 0, "C:\\>ver", 0x07);
    put(&mut s, 5, 0, "FreeDOS kernel 2043 (build 2043 OEM:0xfd) [compiled Oct  8 2025]", 0x07);
    put(&mut s, 7, 0, "C:\\>dir", 0x07);
    put(&mut s, 8, 0, " Volume in drive C is BILTERM", 0x07);
    put(&mut s, 10, 0, "COMMAND  COM        87,772  08-05-22  12:00a", 0x07);
    put(&mut s, 11, 0, "KERNEL   SYS        45,892  10-08-25  12:00a", 0x07);
    put(&mut s, 12, 0, "AUTOEXEC BAT            62  11-21-26  12:00a", 0x07);
    put(&mut s, 14, 0, "C:\\>", 0x0f);
    s.cursor = Some((4, 14));
    let bytes = render_ansi(&s, None);
    wm.pane_mut(id).unwrap().vt_mut().process(&bytes);
    Some(wm)
}
#[cfg(not(feature = "dos"))]
fn scene_dos() -> Option<WindowManager> {
    None
}

fn scenes() -> Vec<Scene> {
    vec![
        ("workspace", "Multi-pane workspace (Turbo Blue)", scene_workspace),
        ("drawer", "Navigator drawer with sessions and agent status", scene_drawer),
        ("session-find", "Session find: Ctrl+X & prefills :find, the match flashes", scene_session_find),
        ("editor-menu", "Built-in editor with the DOS EDIT style menu bar (Alt+H, Help menu)", scene_editor),
        ("pane-mail", "Pane-mail envelope on a pane title", scene_pane_mail),
        ("remote-badge", "Remote [R:host] badge on title and drawer", scene_remote),
        ("drag-drop", "Drag grip and drop-zone overlay mid-drag", scene_drag),
        ("dos-mode", "DOS mode pane (FreeDOS text screen)", scene_dos),
        ("browser", "Browser pane (deterministic placeholder engine)", scene_browser),
        ("theme-3278", "IBM 3278 phosphor green", || scene_theme("3278")),
        ("theme-3279", "IBM 3279 amber", || scene_theme("3279")),
        ("theme-tron", "TRON", || scene_theme("tron")),
        ("theme-cyberpunk", "Cyberpunk neon", || scene_theme("cyberpunk")),
    ]
}

fn named(c: u8, bright: bool) -> (u8, u8, u8) {
    const N: [(u8, u8, u8); 8] =
        [(0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (204, 204, 204)];
    const B: [(u8, u8, u8); 8] =
        [(102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255)];
    if bright { B[c as usize] } else { N[c as usize] }
}

fn rgb(c: Color, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Reset => default,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => named(0, false),
        Color::Red => named(1, false),
        Color::Green => named(2, false),
        Color::Yellow => named(3, false),
        Color::Blue => named(4, false),
        Color::Magenta => named(5, false),
        Color::Cyan => named(6, false),
        Color::Gray => named(7, false),
        Color::DarkGray => named(0, true),
        Color::LightRed => named(1, true),
        Color::LightGreen => named(2, true),
        Color::LightYellow => named(3, true),
        Color::LightBlue => named(4, true),
        Color::LightMagenta => named(5, true),
        Color::LightCyan => named(6, true),
        Color::White => named(7, true),
        Color::Indexed(i) if i < 8 => named(i, false),
        Color::Indexed(i) if i < 16 => named(i - 8, true),
        Color::Indexed(i) if i >= 232 => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
        Color::Indexed(i) => {
            let i = i - 16;
            let f = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            (f(i / 36), f((i / 6) % 6), f(i % 6))
        }
    }
}

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Buffer -> SVG: merged background runs plus one `<text>` per same-style run with explicit glyph x positions.
fn buffer_to_svg(buf: &Buffer, title: &str) -> String {
    const DBG: (u8, u8, u8) = (12, 12, 12);
    const DFG: (u8, u8, u8) = (204, 204, 204);
    let (w, h) = (buf.area.width, buf.area.height);
    let pad = 12.0;
    let tw = f32::from(w) * CW + pad * 2.0;
    let th = f32::from(h) * CH + pad * 2.0;
    let mut s = String::new();
    let _ = writeln!(s, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{tw}" height="{th}" viewBox="0 0 {tw} {th}" role="img" aria-label="{}">"#, esc(title));
    let _ = writeln!(s, r##"<rect width="{tw}" height="{th}" fill="#0c0c0c"/>"##);
    let _ = writeln!(s, r#"<g font-family="{FONT}" font-size="15" xml:space="preserve">"#);
    struct Run {
        x0: u16,
        fg: (u8, u8, u8),
        bold: bool,
        italic: bool,
        glyphs: Vec<(u16, String)>,
    }
    for y in 0..h {
        let fy = pad + f32::from(y) * CH;
        // backgrounds
        let mut x = 0;
        while x < w {
            let cell = &buf[(x, y)];
            let rev = cell.modifier.contains(Modifier::REVERSED);
            let bg = if rev { rgb(cell.fg, DFG) } else { rgb(cell.bg, DBG) };
            let mut e = x + 1;
            while e < w {
                let c2 = &buf[(e, y)];
                let r2 = c2.modifier.contains(Modifier::REVERSED);
                let b2 = if r2 { rgb(c2.fg, DFG) } else { rgb(c2.bg, DBG) };
                if b2 != bg {
                    break;
                }
                e += 1;
            }
            if bg != DBG {
                let _ = writeln!(s, r#"<rect x="{}" y="{fy}" width="{}" height="{CH}" fill="{}"/>"#, pad + f32::from(x) * CW, f32::from(e - x) * CW, hex(bg));
            }
            x = e;
        }
        // text
        let mut runs: Vec<Run> = Vec::new();
        for x in 0..w {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            if sym.trim().is_empty() {
                continue;
            }
            let rev = cell.modifier.contains(Modifier::REVERSED);
            let mut fg = if rev { rgb(cell.bg, DBG) } else { rgb(cell.fg, DFG) };
            if cell.modifier.contains(Modifier::DIM) {
                fg = (fg.0 / 2 + DBG.0 / 2, fg.1 / 2 + DBG.1 / 2, fg.2 / 2 + DBG.2 / 2);
            }
            let bold = cell.modifier.contains(Modifier::BOLD);
            let italic = cell.modifier.contains(Modifier::ITALIC);
            match runs.last_mut() {
                Some(r) if r.fg == fg && r.bold == bold && r.italic == italic => r.glyphs.push((x, sym.to_string())),
                _ => runs.push(Run { x0: x, fg, bold, italic, glyphs: vec![(x, sym.to_string())] }),
            }
        }
        for r in runs {
            let xs: Vec<String> = r.glyphs.iter().map(|(x, _)| format!("{}", pad + f32::from(*x) * CW)).collect();
            let text: String = r.glyphs.iter().map(|(_, g)| esc(g)).collect();
            let _ = r.x0;
            let wt = if r.bold { r#" font-weight="bold""# } else { "" };
            let st = if r.italic { r#" font-style="italic""# } else { "" };
            let _ = writeln!(s, r#"<text x="{}" y="{}" fill="{}"{wt}{st}>{text}</text>"#, xs.join(" "), fy + CH * 0.78, hex(r.fg));
        }
    }
    s.push_str("</g>\n</svg>\n");
    s
}

fn render(wm: &WindowManager) -> Buffer {
    let a = area();
    let mut buf = Buffer::empty(a);
    WorkspaceWidget::new(wm).render(a, &mut buf);
    buf
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out: PathBuf = args.first().map(PathBuf::from).expect("usage: gen_screenshots <out_dir> [scene ...]");
    let only: Vec<&String> = args.iter().skip(1).collect();
    std::fs::create_dir_all(&out).unwrap();
    // Hermetic: never read or write the user's config, state or sessions.
    let home = out.join(".scratch");
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: single-threaded generator, set before any WindowManager exists.
    unsafe {
        for (k, d) in [("HOME", "home"), ("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data")] {
            let p = home.join(d);
            std::fs::create_dir_all(&p).unwrap();
            std::env::set_var(k, &p);
        }
        std::env::set_var("BILTERM_SESSION_DIR", home.join("sessions"));
    }
    for (name, title, build) in scenes() {
        if !only.is_empty() && !only.iter().any(|o| o.as_str() == name) {
            continue;
        }
        let Some(wm) = build() else {
            eprintln!("skip {name} (feature not enabled)");
            continue;
        };
        let mut wm = wm;
        for id in wm.ordered_pane_ids() {
            if let Some(p) = wm.pane_mut(id) {
                p.set_status_message(None);
                if p.editor().is_none() && p.browser_canvas().is_none() {
                    p.set_prompt_anchor(bilterm::pane::PromptAnchor::Bottom);
                }
            }
        }
        let svg = buffer_to_svg(&render(&wm), title);
        std::fs::write(out.join(format!("screenshot-{name}.svg")), svg).unwrap();
        eprintln!("wrote screenshot-{name}.svg");
    }
    let _ = std::fs::remove_dir_all(&home);
}
