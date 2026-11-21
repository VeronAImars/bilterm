//! H1 prerequisites for live handover: pollable reader, `release()`, fd export/adopt.
#![cfg(unix)]

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use bilterm::pty::{PtyConfig, PtyEvent, PtySession};

const T: Duration = Duration::from_secs(10);

fn sh(script: &str) -> PtySession {
    let cfg = PtyConfig {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        rows: 24,
        cols: 80,
        ..PtyConfig::default()
    };
    PtySession::spawn(cfg).expect("spawn")
}

fn collect(s: &mut PtySession, out: &mut Vec<u8>, until: impl Fn(&[u8]) -> bool) {
    let end = Instant::now() + T;
    while !until(out) {
        assert!(Instant::now() < end, "timeout; got {:?}", String::from_utf8_lossy(out));
        for ev in s.drain_events() {
            if let PtyEvent::Output(b) = ev {
                out.extend_from_slice(&b);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn push(out: &mut Vec<u8>, evs: Vec<PtyEvent>) {
    for ev in evs {
        if let PtyEvent::Output(b) = ev {
            out.extend_from_slice(&b);
        }
    }
}

/// Reads whatever is currently readable on `fd` within `wait` (used to inspect the PTY directly).
fn raw_read(fd: i32, wait: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let end = Instant::now() + wait;
    while Instant::now() < end {
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: valid pollfd.
        if unsafe { libc::poll(&mut p, 1, 20) } > 0 {
            let mut b = [0u8; 4096];
            // SAFETY: valid buffer.
            let n = unsafe { libc::read(fd, b.as_mut_ptr().cast(), b.len()) };
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&b[..n as usize]);
        }
    }
    out
}

fn counters(bytes: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("CNT ").and_then(|n| n.trim().parse().ok()))
        .collect()
}

#[test]
fn stopped_reader_does_not_consume_pty_bytes() {
    let mut s = sh("cat");
    s.write_all(b"before\n").unwrap();
    let mut got = Vec::new();
    collect(&mut s, &mut got, |b| String::from_utf8_lossy(b).matches("before").count() >= 2);

    let pending = s.quiesce_reader(T).expect("quiesce");
    push(&mut got, pending);

    let fd = s.master_raw_fd().unwrap();
    s.write_all(b"after\n").unwrap();
    assert!(s.drain_events().is_empty(), "nothing may be queued while quiesced");
    // The bytes are still in the kernel buffer: a direct read sees the echo and cat output.
    let direct = raw_read(fd, Duration::from_secs(2));
    assert!(String::from_utf8_lossy(&direct).matches("after").count() >= 1, "{direct:?}");
    assert!(s.drain_events().is_empty());

    // Resume: the reader works again and sees new output.
    s.resume_reader().unwrap();
    s.write_all(b"again\n").unwrap();
    let mut more = Vec::new();
    collect(&mut s, &mut more, |b| String::from_utf8_lossy(b).contains("again"));
}

#[test]
fn shell_survives_release_and_adopt_without_byte_loss() {
    const N: u32 = 250;
    let mut a = sh(&format!(
        "i=0; while [ $i -lt {N} ]; do echo CNT $i; i=$((i+1)); sleep 0.01; done; echo DONE; sleep 30"
    ));
    let pid = a.child_pid().unwrap();
    let mut stream = Vec::new();
    collect(&mut a, &mut stream, |b| counters(b).len() >= 40);

    let mut released = a.release(T).expect("release");
    assert_eq!(released.meta().pid, pid);
    push(&mut stream, released.take_pending());

    let (tx, rx) = UnixStream::pair().unwrap();
    released.export(tx.as_raw_fd(), b"H").unwrap();
    // Old side still holds its copies and the (unkilled) child before the ack.
    assert_eq!(unsafe { libc::fcntl(released.raw_fd(), libc::F_GETFD) } & !libc::FD_CLOEXEC, 0);
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0);

    let mut buf = [0u8; 8];
    let (n, mut b) = PtySession::adopt_from_socket(rx.as_raw_fd(), released.meta(), &mut buf).unwrap();
    assert_eq!(&buf[..n], b"H");
    assert_eq!(b.child_pid(), Some(pid));
    released.ack();
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0, "ack must not kill the shell");
    drop(a); // a released session's Drop must not kill the child either
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0, "dropping the old session killed the shell");

    collect(&mut b, &mut stream, |s| String::from_utf8_lossy(s).contains("DONE"));
    let c = counters(&stream);
    let want: Vec<u32> = (0..N).collect();
    assert_eq!(c, want, "gap or duplicate across handover");

    // Adopted session is fully usable: resize dedup seeded, resize reaches the tty.
    assert_eq!(b.size(), (24, 80));
    assert!(!b.resize(24, 80).unwrap());
    assert!(b.resize(30, 100).unwrap());
    assert_eq!(b.exit_code(Duration::from_millis(1)), None);
    // Kill via the master hang-up on drop.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[test]
fn release_refuses_a_second_release() {
    let mut s = sh("cat");
    let _r = s.release(T).unwrap();
    assert!(s.release(T).is_err());
}

#[test]
fn fdpass_round_trips_a_descriptor_and_rejects_empty_payload() {
    let (a, b) = UnixStream::pair().unwrap();
    let (r, w): (OwnedFd, OwnedFd) = {
        let mut p = [0; 2];
        assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
        unsafe { (std::os::fd::FromRawFd::from_raw_fd(p[0]), std::os::fd::FromRawFd::from_raw_fd(p[1])) }
    };
    assert!(bilterm::pty::fdpass::send_fds(a.as_raw_fd(), b"", &[w.as_raw_fd()]).is_err());
    bilterm::pty::fdpass::send_fds(a.as_raw_fd(), b"x", &[w.as_raw_fd()]).unwrap();
    let mut buf = [0u8; 4];
    let (n, fds) = bilterm::pty::fdpass::recv_fds(b.as_raw_fd(), &mut buf).unwrap();
    assert_eq!((n, fds.len()), (1, 1));
    unsafe { libc::write(fds[0].as_raw_fd(), b"hi".as_ptr().cast(), 2) };
    let got = raw_read(r.as_raw_fd(), Duration::from_secs(1));
    assert_eq!(got, b"hi");
}
