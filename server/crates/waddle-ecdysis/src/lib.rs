//! # waddle-ecdysis
//!
//! Graceful restart support for Waddle, implementing the
//! [Cloudflare Ecdysis pattern](https://blog.cloudflare.com/ecdysis-rust-graceful-restarts/).
//!
//! ## Overview
//!
//! Ecdysis enables zero-downtime restarts by:
//! 1. Passing listening sockets from the old process to the new process via fd inheritance
//! 2. The new process immediately accepts connections on the inherited sockets
//! 3. The old process gracefully drains in-flight connections
//! 4. After drain (or timeout), the old process exits
//!
//! ## Signal Conventions
//!
//! - `SIGTERM` — Graceful shutdown (drain connections, then exit)
//! - `SIGQUIT` — Graceful restart (spawn new process with fd inheritance, then drain and exit)
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
mod restart;
mod shutdown;

pub use listener::ListenerSet;
pub use restart::{restart, RestartError};
pub use shutdown::{ConnectionGuard, GracefulShutdown, ShutdownSignal};
