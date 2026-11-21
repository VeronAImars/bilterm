//! File-descriptor passing over `AF_UNIX` sockets with `SCM_RIGHTS` (`H1.3`, unix only).
//!
//! Plain `libc` `sendmsg`/`recvmsg`; the payload must be non-empty because ancillary data only
//! travels with at least one byte of ordinary data. Received descriptors get `FD_CLOEXEC`.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// Most descriptors one message may carry.
pub const MAX_FDS: usize = 16;

fn cmsg_buf() -> Vec<u64> {
    // SAFETY: CMSG_SPACE is a pure size computation.
    let n = unsafe { libc::CMSG_SPACE((MAX_FDS * std::mem::size_of::<RawFd>()) as u32) } as usize;
    vec![0u64; n.div_ceil(8)]
}

/// Sends `payload` plus `fds` on the connected unix socket `sock`. The descriptors stay open in
/// the sender: closing them is the caller's decision, after the peer acknowledged (macOS drops
/// queued PTY output when the last copy closes early).
pub fn send_fds(sock: RawFd, payload: &[u8], fds: &[RawFd]) -> io::Result<usize> {
    if payload.is_empty() || fds.is_empty() || fds.len() > MAX_FDS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "need payload and 1..=16 fds"));
    }
    let mut iov = libc::iovec { iov_base: payload.as_ptr() as *mut _, iov_len: payload.len() };
    let mut ctl = cmsg_buf();
    let bytes = std::mem::size_of_val(fds);
    // SAFETY: msghdr is plain data; all pointers below outlive the sendmsg call.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ctl.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(bytes as u32) as _;
        let c = libc::CMSG_FIRSTHDR(&raw const msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(bytes as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr().cast::<u8>(), libc::CMSG_DATA(c), bytes);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = 0;
        loop {
            let n = libc::sendmsg(sock, &raw const msg, flags);
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

/// Receives up to `buf.len()` payload bytes and any passed descriptors. Descriptors are owned
/// and `FD_CLOEXEC` on return; truncated ancillary data is an error (nothing leaks).
pub fn recv_fds(sock: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut ctl = cmsg_buf();
    let mut out = Vec::new();
    // SAFETY: as in `send_fds`; received descriptors are wrapped in OwnedFd immediately.
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ctl.as_mut_ptr().cast();
        msg.msg_controllen = (ctl.len() * 8) as _;
        let n = loop {
            let n = libc::recvmsg(sock, &raw mut msg, 0);
            if n >= 0 {
                break n as usize;
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        };
        let mut c = libc::CMSG_FIRSTHDR(&raw const msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(c).cast::<RawFd>();
                let len = ((*c).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                for i in 0..len / std::mem::size_of::<RawFd>() {
                    let fd = data.add(i).read_unaligned();
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    out.push(OwnedFd::from_raw_fd(fd));
                }
            }
            c = libc::CMSG_NXTHDR(&raw const msg, c);
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(io::Error::other("ancillary data truncated"));
        }
        n
    };
    Ok((n, out))
}
