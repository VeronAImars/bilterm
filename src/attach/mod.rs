//! Attach protocol wire layer (P2 of `docs/plans/session-host-p2-attach.md`, task T-A1).
//!
//! Pure library: frame codec, limits and message types. No sockets, no listener, no
//! `WindowManager` coupling; later tasks (T-A2..T-A7) build on these types.

pub mod frame;
pub mod limits;
pub mod msg;

#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod msg_tests;
