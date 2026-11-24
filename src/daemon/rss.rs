//! Resident-memory sampler for `daemon list` (P4 / C1 & C4). Read-only; never signals anything.
//!
//! Linux: `/proc/<pid>/statm` (resident pages x page size). macOS: `proc_pid_rusage`
//! `ri_phys_footprint` (what Activity Monitor and jetsam use; see
//! `docs/plans/session-host-p4-limits.md` section 1). Windows: `K32GetProcessMemoryInfo`
//! (`WorkingSetSize`). Other platforms report `None`.

/// Resident bytes of this process, or `None` when the platform cannot say.
pub fn current_rss_bytes() -> Option<u64> {
    rss_bytes_of(std::process::id())
}

/// Resident bytes of `pid`, or `None` (unsupported platform, process gone, not permitted).
pub fn rss_bytes_of(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
        parse_statm_resident_pages(&statm).map(|pages| pages.saturating_mul(page_size()))
    }
    #[cfg(target_os = "macos")]
    {
        macos_footprint(pid)
    }
    #[cfg(target_os = "windows")]
    {
        windows_footprint(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// Second field of `/proc/<pid>/statm` (resident pages).
pub fn parse_statm_resident_pages(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn page_size() -> u64 {
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 { n as u64 } else { 4096 }
}

#[cfg(target_os = "macos")]
fn macos_footprint(pid: u32) -> Option<u64> {
    // RUSAGE_INFO_V4 == 4 (sys/resource.h); the struct is the libc crate's `rusage_info_v4`.
    const RUSAGE_INFO_V4: libc::c_int = 4;
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // C signature takes `rusage_info_t *` (void **) but the buffer it fills is the struct itself,
    // so pass the struct's address cast to that type (not the address of a pointer variable).
    let rc = unsafe { libc::proc_pid_rusage(pid as libc::c_int, RUSAGE_INFO_V4, &mut info as *mut libc::rusage_info_v4 as *mut libc::rusage_info_t) };
    (rc == 0).then_some(info.ri_phys_footprint)
}

#[cfg(target_os = "windows")]
fn windows_footprint(pid: u32) -> Option<u64> {
    use std::ffi::c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_READ: u32 = 0x0010;
    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> *mut c_void;
        fn K32GetProcessMemoryInfo(process: *mut c_void, ppsmem_counters: *mut ProcessMemoryCountersEx, cb: u32) -> i32;
        fn CloseHandle(h_object: *mut c_void) -> i32;
    }
    // SAFETY: `OpenProcess` opens a read-only process query handle for `pid` (returning null if not
    // found/permitted); `K32GetProcessMemoryInfo` writes into stack `ProcessMemoryCountersEx` with
    // exact byte size `cb`; `CloseHandle` always closes the returned handle before returning.
    unsafe {
        let mut h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if h.is_null() {
            h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        }
        if h.is_null() {
            return None;
        }
        let mut pmc = ProcessMemoryCountersEx {
            cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
            ..Default::default()
        };
        let ok = K32GetProcessMemoryInfo(h, &mut pmc, pmc.cb) != 0;
        let _ = CloseHandle(h);
        ok.then_some(pmc.working_set_size as u64)
    }
}

/// Human form for table cells: `812K`, `12.3M`, `1.5G`, `-` when unknown.
pub fn format_bytes(bytes: Option<u64>) -> String {
    let Some(b) = bytes else { return "-".to_string() };
    const K: f64 = 1024.0;
    let f = b as f64;
    if f < K {
        format!("{b}B")
    } else if f < K * K {
        format!("{:.0}K", f / K)
    } else if f < K * K * K {
        format!("{:.1}M", f / (K * K))
    } else {
        format!("{:.1}G", f / (K * K * K))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statm_parse_and_format() {
        assert_eq!(parse_statm_resident_pages("1000 250 100 5 0 400 0\n"), Some(250));
        assert_eq!(parse_statm_resident_pages("garbage"), None);
        assert_eq!(parse_statm_resident_pages(""), None);
        assert_eq!(format_bytes(None), "-");
        assert_eq!(format_bytes(Some(512)), "512B");
        assert_eq!(format_bytes(Some(2048)), "2K");
        assert_eq!(format_bytes(Some(12 * 1024 * 1024 + 300 * 1024)), "12.3M");
        assert_eq!(format_bytes(Some(3 * 1024 * 1024 * 1024 / 2)), "1.5G");
    }

    #[test]
    fn pid_zero_is_never_sampled() {
        assert_eq!(rss_bytes_of(0), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn own_rss_is_plausible() {
        let rss = current_rss_bytes().expect("own rss");
        assert!(rss > 100 * 1024 && rss < 64 * 1024 * 1024 * 1024, "{rss}");
    }
}
