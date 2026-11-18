//! Attach protocol wire layer (P2 of `docs/plans/session-host-p2-attach.md`, task T-A1).
//!
//! Wire layer (T-A1: `frame`, `limits`, `msg`) plus the T-A2 listener: `listener` (sibling
//! socket, accept loop), `conn` (handshake + control loop), `registry`, `handshake`, `host`
//! (wiring). No screen data yet; later tasks (T-A3..T-A7) build on these.

pub mod conn;
pub mod frame;
pub mod handshake;
pub mod host;
pub mod limits;
pub mod listener;
pub mod msg;
pub mod registry;
pub mod view;
pub mod size_owner;

#[cfg(test)]
mod frame_tests;
#[cfg(test)]
mod conn_tests;
#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod msg_tests;
#[cfg(test)]
mod size_owner_tests;
