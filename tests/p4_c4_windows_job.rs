//! P4 / C4 live Windows Job Object checks: the memory limit stops a hog and the guard kills its
//! process tree on drop. Windows only.
#![cfg(windows)]

use bilterm::daemon::limits::WinJobGuard;
use std::os::windows::io::AsRawHandle;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;

fn powershell(script: &str) -> std::process::Child {
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn powershell")
}

#[test]
fn job_memory_limit_stops_a_hog_and_records_peak() {
    let max = 256 * MIB;
    let mut child = powershell("$l = New-Object System.Collections.ArrayList; while ($true) { [void]$l.Add((New-Object byte[] 16777216)); Start-Sleep -Milliseconds 5 }");
    let guard = WinJobGuard::create_and_assign(child.as_raw_handle(), max).expect("job");
    let end = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("hog was not stopped by the job memory limit");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(!status.success(), "hog must not exit cleanly: {status:?}");
    let peak = guard.peak_memory_bytes().expect("peak");
    assert!(peak <= max + 64 * MIB, "peak {peak} far above limit {max}");
    assert!(peak >= max / 10 * 5, "peak {peak} suspiciously low for a {max} limit");
}

#[test]
fn dropping_the_guard_kills_the_assigned_process() {
    let mut child = powershell("Start-Sleep -Seconds 120");
    let guard = WinJobGuard::create_and_assign(child.as_raw_handle(), 1 << 30).expect("job");
    assert!(child.try_wait().unwrap().is_none());
    drop(guard);
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    panic!("KILL_ON_JOB_CLOSE did not terminate the child");
}
