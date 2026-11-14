//! The S2S connection manager (ADR-0020 D1, D2 and D8; gate G7): the
//! configuration snapshot and its load failures, peer admission, two
//! in-process deployments on loopback establishing STARTTLS and SASL
//! EXTERNAL streams in both directions, refusals before TLS, redirects,
//! the resource model's caps, eviction, replacement and deadlines, and the
//! shutdown join.
//!
//! Every certificate comes from a test CA configured as each peer's
//! private-CA profile, and the clock that certificates and CRLs are checked
//! against is fixed; sockets are real loopback TCP. No PostgreSQL, SpiceDB
//! or second process is involved.

#[path = "s2s_connection_manager/support.rs"]
mod support;

#[path = "s2s_connection_manager/config.rs"]
mod config;

#[path = "s2s_connection_manager/streams.rs"]
mod streams;

#[path = "s2s_connection_manager/limits.rs"]
mod limits;
