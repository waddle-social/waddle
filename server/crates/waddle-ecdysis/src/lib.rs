//! # waddle-ecdysis
//!
//! Inherited listeners and graceful shutdown support for Waddle.
//!
//! A launcher may pass listening sockets using `LISTEN_FDS` and
//! `LISTEN_FD_NAMES`. Capture those descriptors before starting runtime threads,
//! then convert them to Tokio listeners inside the runtime. The server drains
//! connections on shutdown; it does not spawn or re-exec a replacement process.
//!
//! ## Signal Conventions
//!
//! - `SIGTERM` — Graceful shutdown (drain connections, then exit)
//! - `SIGQUIT` — Drain and exit with restart intent; replacement is supervisor-managed
//!
//! ## Environment Variables
//!
//! - `LISTEN_FDS` — Number of inherited file descriptors (starting at fd 3)
//! - `LISTEN_FD_NAMES` — Colon-separated names for each inherited fd
//! - `WADDLE_DRAIN_TIMEOUT_SECS` — Drain timeout in seconds (default: 30)
//!
//! ## Platform
//!
//! This crate requires Unix (Linux / macOS). It will not compile on other platforms.

#[cfg(not(unix))]
compile_error!("waddle-ecdysis requires a Unix platform (Linux or macOS)");

mod listener;
mod shutdown;

pub use listener::{InheritedListeners, ListenerSet};
pub use shutdown::{ConnectionGuard, GracefulShutdown, ShutdownHandle, ShutdownSignal};
