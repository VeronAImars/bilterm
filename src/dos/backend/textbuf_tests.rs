//! Textbuf tests against an in-process fake QMP server (real sockets, real `Qmp`, real
//! `QemuBackend::attach`), plus the `#[ignore]` real-QEMU smoke.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::qemu::{PROGRAM, QemuBackend, QemuMode};
use super::screen::{BDA_LEN, TEXT_BUF_LEN};
use super::textbuf::{MAX_CONSECUTIVE_ERRORS, Poll, TextPoller, run_loop};
use super::*;

#[derive(Default)]
struct Guest {
    vga: Vec<u8>,
    bda: Vec<u8>,
    keys: Vec<Vec<String>>,
    pmemsaves: Vec<u64>,
    /// "short" / "long" / "nofile" make pmemsave misbehave.
    fault: Option<&'static str>,
    typed: usize,
}

fn fixture(name: &str) -> (Vec<u8>, Vec<u8>) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/dos/backend/fixtures/");
    (
        std::fs::read(format!("{dir}{name}.vga")).unwrap(),
        std::fs::read(format!("{dir}{name}.bda")).unwrap(),
    )
}

fn guest(name: &str) -> Arc<Mutex<Guest>> {
    let (vga, bda) = fixture(name);
    Arc::new(Mutex::new(Guest {
        vga,
        bda,
        ..Guest::default()
    }))
}

/// Serves one QMP client on `l` until it disconnects.
fn serve(l: TcpListener, g: Arc<Mutex<Guest>>, hang_up_after: Option<usize>) {
    std::thread::spawn(move || {
        let Ok((s, _)) = l.accept() else { return };
        let mut w = s.try_clone().unwrap();
        let mut r = BufReader::new(s);
        writeln!(w, r#"{{"QMP":{{"version":{{"qemu":{{"major":10}}}},"capabilities":[]}}}}"#).unwrap();
        let mut served = 0usize;
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if hang_up_after.is_some_and(|n| served >= n) {
                return;
            }
            served += 1;
            let v: Value = serde_json::from_str(&line).unwrap();
            let args = &v["arguments"];
            let mut gm = g.lock().unwrap();
            // an async event before every reply, as real QEMU does around state changes
            writeln!(w, r#"{{"event":"RESUME"}}"#).unwrap();
            match v["execute"].as_str().unwrap() {
                "pmemsave" => {
                    let addr = args["val"].as_u64().unwrap();
                    let size = args["size"].as_u64().unwrap() as usize;
                    gm.pmemsaves.push(addr);
                    let data: Vec<u8> = match addr {
                        0xB8000 => gm.vga[..size].to_vec(),
                        0x400 => gm.bda[..size].to_vec(),
                        _ => vec![],
                    };
                    let data = match gm.fault {
                        Some("short") => data[..data.len() / 2].to_vec(),
                        Some("long") => [data, vec![0; 100_000]].concat(),
                        _ => data,
                    };
                    if gm.fault != Some("nofile") {
                        std::fs::write(args["filename"].as_str().unwrap(), data).unwrap();
                    }
                    writeln!(w, r#"{{"return":{{}}}}"#).unwrap();
                }
                "send-key" => {
                    let qs: Vec<String> = args["keys"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|k| k["data"].as_str().unwrap().to_string())
                        .collect();
                    if qs.len() == 1 && qs[0].len() == 1 {
                        let i = gm.typed * 2;
                        gm.vga[i] = qs[0].as_bytes()[0];
                        gm.vga[i + 1] = 0x07;
                        gm.typed += 1;
                    }
                    gm.keys.push(qs);
                    writeln!(w, r#"{{"return":{{}}}}"#).unwrap();
                }
                "quit" => {
                    writeln!(w, r#"{{"return":{{}}}}"#).unwrap();
                    return;
                }
                _ => writeln!(w, "{}", json!({"return": {}})).unwrap(),
            }
        }
    });
}

/// Backend over TCP whose QMP endpoint is served by the fake.
fn attached(g: Arc<Mutex<Guest>>, hang_up_after: Option<usize>) -> (QemuBackend, tempfile::TempDir) {
    let (mut b, t, l) = planned_with_listener();
    serve(l, g, hang_up_after);
    b.attach().unwrap();
    (b, t)
}

/// The bind-and-release port pick races with parallel tests exactly as it can with other
/// processes in production, so retry with a fresh plan until the bind wins.
fn planned_with_listener() -> (QemuBackend, tempfile::TempDir, TcpListener) {
    for _ in 0..20 {
        let t = tempfile::tempdir().unwrap();
        let mut b =
            QemuBackend::new(QemuMode::TextBuf, PROGRAM.into(), t.path().into()).prefer_tcp();
        let plan = b
            .plan(&SessionSpec {
                c_image: Some("/d/c.img".into()),
                ..SessionSpec::default()
            })
            .unwrap();
        let ControlEndpoint::Tcp { port } = plan.control else {
            panic!("tcp expected")
        };
        if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            return (b, t, l);
        }
    }
    panic!("no free port");
}

#[test]
fn reads_real_guest_dump_through_qmp() {
    let g = guest("installer");
    let (mut b, _t) = attached(g.clone(), None);
    let s = b.read_text().unwrap();
    assert!(s.contains("What is your preferred language?"));
    assert!(s.is_text_mode() && !s.cursor_visible);
    assert_eq!(g.lock().unwrap().pmemsaves, vec![0x400, 0xB8000]);
}

#[test]
fn graphics_mode_skips_buffer_dump_and_shows_notice() {
    let g = guest("boot");
    g.lock().unwrap().bda[0x49] = 0x13;
    let (mut b, _t) = attached(g.clone(), None);
    let s = b.read_text().unwrap();
    assert!(!s.is_text_mode());
    assert!(s.for_display().contains("graphics mode 0x13"));
    assert_eq!(g.lock().unwrap().pmemsaves, vec![0x400], "no text dump in graphics mode");
}

#[test]
fn hostile_dump_sizes_and_missing_files_are_errors() {
    for fault in ["short", "long", "nofile"] {
        let g = guest("boot");
        g.lock().unwrap().fault = Some(fault);
        let (mut b, _t) = attached(g, None);
        assert!(b.read_text().is_err(), "{fault}");
    }
    assert_eq!(TEXT_BUF_LEN, 4000);
    assert_eq!(BDA_LEN, 256);
}

#[test]
fn poller_emits_frames_only_on_change() {
    let g = guest("boot");
    let (mut b, _t) = attached(g.clone(), None);
    let mut p = TextPoller::new();
    let first = p.poll(&mut b).unwrap();
    assert!(matches!(&first, Poll::Frame { ansi, .. } if ansi.starts_with(b"\x1b[0m\x1b[2J")));
    for _ in 0..3 {
        assert_eq!(p.poll(&mut b).unwrap(), Poll::Unchanged);
    }
    b.send_keys(&[DosKey::Char('x')]).unwrap();
    let Poll::Frame { ansi, screen } = p.poll(&mut b).unwrap() else {
        panic!("expected a delta frame")
    };
    assert!(!ansi.windows(4).any(|w| w == b"\x1b[2J"), "delta, not repaint");
    assert_eq!(screen.cell(0, 0).unwrap().ch, 'x');
    assert_eq!(g.lock().unwrap().keys, vec![vec!["x".to_string()]]);
}

#[test]
fn run_loop_forwards_keys_and_streams_frames() {
    let g = guest("boot");
    let (mut b, _t) = attached(g.clone(), None);
    let (tx, rx) = channel::<Vec<DosKey>>();
    let stop = AtomicBool::new(false);
    let (ftx, frx) = channel::<String>();
    let h = std::thread::spawn(move || {
        run_loop(&mut b, &rx, &stop, |_, s| {
            let _ = ftx.send(s.lines()[0].clone());
        })
    });
    tx.send(vec![DosKey::Char('h'), DosKey::Char('i')]).unwrap();
    // Condition wait on the frame channel (no sleep polling); generous bound only for hangs.
    let mut frames = Vec::<String>::new();
    loop {
        let l = frx
            .recv_timeout(Duration::from_secs(30))
            .expect("frame with typed text never arrived");
        let done = l.starts_with("hi");
        frames.push(l);
        if done {
            break;
        }
    }
    drop(tx);
    assert!(h.join().unwrap().is_ok());
    frames.extend(frx.try_iter());
    let n = frames.len();
    assert!((2..=6).contains(&n), "{n} frames: initial + changes only");
    assert_eq!(g.lock().unwrap().keys.len(), 2);
}

#[test]
fn stop_flag_ends_the_loop() {
    let (mut b, _t) = attached(guest("boot"), None);
    let (_tx, rx) = channel::<Vec<DosKey>>();
    let stop = AtomicBool::new(true);
    assert!(run_loop(&mut b, &rx, &stop, |_, _| panic!("no poll after stop")).is_ok());
    stop.store(false, Ordering::Relaxed);
}

#[test]
fn repeated_read_failures_end_the_loop() {
    // server hangs up after the handshake's capabilities command
    let (mut b, _t) = attached(guest("boot"), Some(1));
    let mut p = TextPoller::new();
    for _ in 0..MAX_CONSECUTIVE_ERRORS - 1 {
        assert_eq!(p.poll(&mut b).unwrap(), Poll::Unchanged, "transient errors are tolerated");
    }
    assert!(p.poll(&mut b).is_err());
}

#[test]
fn non_qmp_squatter_on_the_port_is_rejected() {
    let (mut b, _t, l) = planned_with_listener();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let _ = s.write_all(b"SSH-2.0-squatter\r\n");
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    assert!(matches!(b.attach(), Err(BackendError::Qmp(_))));
}

#[test]
fn qemu_textbuf_offline_conformance_row() {
    let t = tempfile::tempdir().unwrap();
    let mut b: Box<dyn DosBackend> = Box::new(QemuBackend::new(
        QemuMode::TextBuf,
        "/nonexistent/qemu".into(),
        t.path().into(),
    ));
    assert_eq!(b.id(), "qemu-textbuf");
    assert!(b.caps().textbuf && !b.caps().text_pane_native);
    assert!(!b.is_running());
    assert!(matches!(b.read_text(), Err(BackendError::NotRunning)));
    assert!(matches!(b.send_keys(&[DosKey::Enter]), Err(BackendError::NotRunning)));
    let spec = SessionSpec {
        c_image: Some("/d/c.img".into()),
        ..SessionSpec::default()
    };
    let p = b.plan(&spec).unwrap();
    assert_eq!(p.kind, LaunchKind::Bridge);
    assert!(p.argv.windows(2).any(|w| w == ["-display", "none"]));
    assert!(!b.is_running(), "plan must not spawn");
    // launching a missing emulator fails cleanly and leaves nothing behind
    assert!(matches!(b.launch(&spec), Err(BackendError::Process(_))));
    assert!(!b.is_running());
    b.shutdown().unwrap();
    b.shutdown().unwrap();
}

/// Real boot of the pinned FreeDOS boot floppy. Run with
/// `BILTERM_DOS_SMOKE=1 BILTERM_DOS_FLOPPY=/path/FD14BOOT.img cargo test --features dos \
///  textbuf_tests::smoke -- --ignored`.
#[test]
#[ignore]
fn smoke_boots_freedos_floppy_and_types() {
    let (Some(_), Some(fl)) = (
        std::env::var_os("BILTERM_DOS_SMOKE"),
        std::env::var_os("BILTERM_DOS_FLOPPY"),
    ) else {
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let c = t.path().join("c.img");
    std::fs::write(&c, vec![0u8; 1024 * 1024]).unwrap();
    for tcp in [false, true] {
        let mut b = QemuBackend::new(QemuMode::TextBuf, PROGRAM.into(), t.path().into());
        if tcp {
            b = b.prefer_tcp();
        }
        b.launch(&SessionSpec {
            c_image: Some(c.clone()),
            floppy: Some(fl.clone().into()),
            throwaway: true,
            ..SessionSpec::default()
        })
        .unwrap();
        let mut p = TextPoller::new();
        let t0 = Instant::now();
        let mut last = None;
        let mut frames = 0;
        while t0.elapsed() < Duration::from_secs(60) {
            if let Poll::Frame { screen, .. } = p.poll(&mut b).unwrap() {
                frames += 1;
                if screen.contains("preferred language") {
                    last = Some(screen);
                    break;
                }
            }
            std::thread::sleep(p.interval());
        }
        let s = last.expect("FreeDOS installer screen never appeared");
        assert!(frames >= 2 && s.is_text_mode());
        // Down moves the menu highlight, which changes cell attributes on screen.
        let before = s.cells.clone();
        b.send_keys(&[DosKey::Down]).unwrap();
        let t0 = Instant::now();
        loop {
            if let Poll::Frame { screen, .. } = p.poll(&mut b).unwrap()
                && screen.cells != before
            {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "keys had no visible effect");
            std::thread::sleep(p.interval());
        }
        b.shutdown().unwrap();
        assert!(!b.is_running());
    }
}
