//! Live session transfer between two session hosts (unix): same child pid, no output lost or
//! duplicated, terminal state preserved, and rollback leaves the old session working.
#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bilterm::daemon::transfer::wire::{self, Msg};
use bilterm::daemon::transfer::{serve_connection, transfer_sessions, SocketKind, TransferListener, TransferOpts};
use bilterm::wm::WindowManager;

const T: Duration = Duration::from_secs(20);

fn tmp_dir(tag: &str) -> PathBuf {
    let d = common::short_root().join(format!("bt-xfer-{tag}-{}", bilterm::secure_fs::unique_suffix()));
    bilterm::secure_fs::create_private_dir_all(&d).unwrap();
    d
}

fn counters(bytes: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for part in text.split("CNT ").skip(1) {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            out.push(n);
        }
    }
    out
}

/// Visible text of a replay: escape sequences removed, blank lines dropped.
fn text_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() || n == '~' || n == '@' {
                        break;
                    }
                }
            } else {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out.split(['\r', '\n']).map(str::trim_end).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

fn pump_until(wm: &mut WindowManager, what: &str, mut f: impl FnMut(&mut WindowManager) -> bool) {
    let end = Instant::now() + T;
    loop {
        wm.poll_all_ptys();
        if f(wm) {
            return;
        }
        assert!(Instant::now() < end, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_in_active(wm: &mut WindowManager, cmd: &str) {
    wm.active_pane_mut().submit_command(cmd).expect("submit");
}

fn active_dump(wm: &mut WindowManager) -> (Option<u32>, Vec<u8>) {
    let name = wm.live_session_names()[0].clone();
    let d = wm.session_window_dump(&name);
    (d[0].1, d[0].2.clone())
}

type Dump = Vec<(String, Vec<(usize, Option<u32>, Vec<u8>)>)>;
type Served = Vec<Result<Option<(Vec<String>, bool)>, String>>;

enum Cmd {
    Dump(mpsc::Sender<Dump>),
    Stop,
}

/// A receiving host on its own thread (a `WindowManager` is built where it lives).
struct Receiver {
    tx: mpsc::Sender<Cmd>,
    join: Option<std::thread::JoinHandle<Served>>,
    path: PathBuf,
}

fn spawn_receiver(dir: &std::path::Path, kind: SocketKind) -> Receiver {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let (ready_tx, ready_rx) = mpsc::channel();
    let d = dir.to_path_buf();
    let join = std::thread::spawn(move || {
        let mut wm = WindowManager::new_host(24, 80).expect("receiver host");
        let l = TransferListener::bind(&d, "recv", kind).expect("bind");
        ready_tx.send(l.path().to_path_buf()).unwrap();
        let mut results = Vec::new();
        loop {
            if let Some(s) = l.poll_accept() {
                results.push(serve_connection(&mut wm, s, kind));
            }
            wm.poll_all_ptys();
            match rx.try_recv() {
                Ok(Cmd::Dump(reply)) => {
                    let names = wm.live_session_names();
                    let _ = reply.send(names.into_iter().map(|n| { let d = wm.session_window_dump(&n); (n, d) }).collect());
                }
                Ok(Cmd::Stop) => return results,
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    });
    Receiver { tx, join: Some(join), path: ready_rx.recv().unwrap() }
}

impl Receiver {
    fn dump(&self) -> Dump {
        let (t, r) = mpsc::channel();
        self.tx.send(Cmd::Dump(t)).unwrap();
        r.recv_timeout(T).unwrap()
    }
    fn stop(mut self) -> Served {
        let _ = self.tx.send(Cmd::Stop);
        self.join.take().unwrap().join().unwrap()
    }
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

const COUNT_LOOP: &str = "i=0; while [ $i -lt 300 ]; do echo CNT $i; i=$((i+1)); sleep 0.01; done; echo FIN''ISHED; cat";

fn host_with_named_session(name: &str) -> WindowManager {
    let mut wm = WindowManager::new_host(24, 80).expect("sender host");
    wm.set_host_name("sender");
    wm.create_new_session(Some(name), false).expect("session");
    wm
}

#[test]
fn transfer_moves_a_running_session_without_loss_and_keeps_vt_state() {
    let dir = tmp_dir("ok");
    let recv = spawn_receiver(&dir, SocketKind::Accept);
    let mut wm = host_with_named_session("work");
    run_in_active(&mut wm, "echo HEL''LO-BEFORE; printf '\\033[31mred\\033[0m\\n'");
    run_in_active(&mut wm, COUNT_LOOP);
    pump_until(&mut wm, "40 counters", |w| counters(&active_dump(w).1).len() >= 40);
    let (pid, before) = active_dump(&mut wm);
    let pid = pid.expect("pid");
    assert!(before.windows(12).any(|w| w == b"HELLO-BEFORE"));

    let rep = transfer_sessions(&mut wm, &["work".into()], &recv.path, false, TransferOpts::default()).expect("transfer");
    assert_eq!(rep.pids, vec![pid]);
    assert!(alive(pid), "the shell must survive the transfer");
    assert_eq!(wm.live_session_names(), vec!["default".to_string()], "sender dropped the transferred session and moved on");

    // Receiver finishes the loop: every counter exactly once, in order, across the handover.
    let end = Instant::now() + T;
    let dump = loop {
        let d = recv.dump();
        let work = d.iter().find(|(n, _)| n == "work").cloned();
        if let Some((_, panes)) = work
            && String::from_utf8_lossy(&panes[0].2).contains("FINISHED")
        {
            break panes;
        }
        assert!(Instant::now() < end, "receiver never saw FINISHED");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(dump[0].1, Some(pid), "same child pid on the receiver");
    let seen = counters(&dump[0].2);
    assert_eq!(seen, (0..300).collect::<Vec<_>>(), "lost or duplicated output");
    assert!(dump[0].2.windows(12).any(|w| w == b"HELLO-BEFORE"), "earlier scrollback preserved");
    let results = recv.stop();
    assert!(matches!(results.as_slice(), [Ok(Some(_))]), "{results:?}");
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[test]
fn quiet_session_vt_replay_is_identical_after_transfer() {
    let dir = tmp_dir("vt");
    let recv = spawn_receiver(&dir, SocketKind::Accept);
    let mut wm = host_with_named_session("quiet");
    run_in_active(&mut wm, "for i in $(seq 0 99); do echo CNT $i; done; printf '\\033[1;32mbold-green\\033[0m\\nREA''DY'; cat");
    pump_until(&mut wm, "READY", |w| String::from_utf8_lossy(&active_dump(w).1).contains("READY"));
    // Wait until the terminal is quiet (stable for 400 ms): loaded machines deliver late.
    let mut stable_since = Instant::now();
    let mut last = text_lines(&active_dump(&mut wm).1);
    while stable_since.elapsed() < Duration::from_millis(400) {
        wm.poll_all_ptys();
        std::thread::sleep(Duration::from_millis(20));
        let now = text_lines(&active_dump(&mut wm).1);
        if now != last {
            last = now;
            stable_since = Instant::now();
        }
    }
    let (pid, before) = active_dump(&mut wm);
    transfer_sessions(&mut wm, &["quiet".into()], &recv.path, false, TransferOpts::default()).expect("transfer");
    let d = recv.dump();
    let (_, panes) = d.iter().find(|(n, _)| n == "quiet").expect("session on receiver");
    assert_eq!(panes[0].1, pid);
    let after = &panes[0].2;
    // The history/screen boundary may sit a few rows differently (the replay re-scrolls), but the
    // ordered text, the colours and the cursor-visible tail must be identical.
    let (ta, tb) = (text_lines(after), text_lines(&before));
    if ta != tb {
        let i = ta.iter().zip(&tb).position(|(a, b)| a != b).unwrap_or(ta.len().min(tb.len()));
        panic!("terminal text changed across the transfer at line {i} ({} vs {} lines)\nafter:  {:?}\nbefore: {:?}", ta.len(), tb.len(), ta.get(i.saturating_sub(1)..(i + 3).min(ta.len())), tb.get(i.saturating_sub(1)..(i + 3).min(tb.len())));
    }
    assert_eq!(counters(after), (0..100).collect::<Vec<_>>());
    assert!(after.windows(7).any(|w| w == b"\x1b[1;32m") || String::from_utf8_lossy(after).contains("bold-green"));
    recv.stop();
    if let Some(p) = pid {
        unsafe { libc::kill(p as i32, libc::SIGKILL) };
    }
}

fn expect_old_session_still_works(wm: &mut WindowManager, pid: u32) {
    assert!(alive(pid));
    pump_until(wm, "FINISHED on the old session", |w| String::from_utf8_lossy(&active_dump(w).1).contains("FINISHED"));
    let (_, replay) = active_dump(wm);
    assert_eq!(counters(&replay), (0..300).collect::<Vec<_>>(), "rollback lost or duplicated output");
    run_in_active(wm, "ALIVE-42"); // the foreground program is `cat`: it echoes what it is given
    pump_until(wm, "input after rollback", |w| String::from_utf8_lossy(&active_dump(w).1).contains("ALIVE-42"));
}

fn rollback_case(tag: &str, fake: impl FnOnce(std::os::unix::net::UnixStream) + Send + 'static) {
    let dir = tmp_dir(tag);
    let path = dir.join("fake.sock");
    let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let h = std::thread::spawn(move || fake(l.accept().unwrap().0));
    let mut wm = host_with_named_session("work");
    run_in_active(&mut wm, COUNT_LOOP);
    pump_until(&mut wm, "20 counters", |w| counters(&active_dump(w).1).len() >= 20);
    let pid = active_dump(&mut wm).0.unwrap();
    let opts = TransferOpts { ready_timeout: Duration::from_millis(700), adopt_timeout: Duration::from_millis(700) };
    let err = transfer_sessions(&mut wm, &["work".into()], &path, false, opts).expect_err("must fail");
    assert!(!err.is_empty());
    assert_eq!(wm.live_session_names()[0], "work", "session must stay active on the sender");
    expect_old_session_still_works(&mut wm, pid);
    h.join().unwrap();
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[test]
fn rollback_when_receiver_vanishes_after_offer() {
    rollback_case("eof", |mut s| {
        let _ = wire::recv_offer(&mut s);
        drop(s);
    });
}

#[test]
fn rollback_when_receiver_refuses() {
    rollback_case("refuse", |mut s| {
        let _ = wire::recv_offer(&mut s);
        wire::write_msg(&mut s, &Msg::Refuse { why: "no".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    });
}

#[test]
fn rollback_when_receiver_never_acks_after_ready() {
    rollback_case("noack", |mut s| {
        let _ = wire::recv_offer(&mut s);
        wire::write_msg(&mut s, &Msg::Ready).unwrap();
        let _ = wire::read_msg(&mut s); // Commit
        std::thread::sleep(Duration::from_millis(1500)); // never answers
    });
}

#[test]
fn rollback_when_receiver_reports_adoption_failure() {
    rollback_case("failed", |mut s| {
        let _ = wire::recv_offer(&mut s);
        wire::write_msg(&mut s, &Msg::Ready).unwrap();
        let _ = wire::read_msg(&mut s);
        wire::write_msg(&mut s, &Msg::Failed { why: "boom".into() }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    });
}

#[test]
fn receiver_refuses_a_name_collision_and_sender_keeps_its_session() {
    let dir = tmp_dir("collide");
    let recv = spawn_receiver(&dir, SocketKind::Accept);
    let mut wm = WindowManager::new_host(24, 80).unwrap();
    wm.set_host_name("sender");
    run_in_active(&mut wm, COUNT_LOOP);
    pump_until(&mut wm, "10 counters", |w| counters(&active_dump(w).1).len() >= 10);
    let pid = active_dump(&mut wm).0.unwrap();
    // Both hosts have a session called "default".
    let err = transfer_sessions(&mut wm, &["default".into()], &recv.path, false, TransferOpts::default()).expect_err("collision");
    assert!(err.contains("already exists"), "{err}");
    expect_old_session_still_works(&mut wm, pid);
    recv.stop();
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[test]
fn takeover_receiver_replaces_its_pristine_session_with_all_of_the_senders() {
    let dir = tmp_dir("take");
    let recv = spawn_receiver(&dir, SocketKind::Takeover);
    let mut wm = host_with_named_session("work");
    run_in_active(&mut wm, "echo IN-WORK; cat");
    pump_until(&mut wm, "IN-WORK", |w| String::from_utf8_lossy(&active_dump(w).1).contains("IN-WORK"));
    let names = wm.live_session_names();
    assert_eq!(names.len(), 2);
    let rep = transfer_sessions(&mut wm, &names, &recv.path, true, TransferOpts::default()).expect("takeover transfer");
    assert_eq!(rep.panes, 2);
    let d = recv.dump();
    let mut got: Vec<&str> = d.iter().map(|(n, _)| n.as_str()).collect();
    got.sort_unstable();
    assert_eq!(got, vec!["default", "work"]);
    assert_eq!(d[0].0, "work", "the sender's active session is active on the receiver");
    let results = recv.stop();
    assert!(matches!(results.as_slice(), [Ok(Some((_, true)))]), "{results:?}");
    for pid in rep.pids {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}
