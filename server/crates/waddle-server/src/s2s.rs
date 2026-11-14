//! Server-to-server transport for the federation profile of ADR-0020.
//!
//! - [`config`]: whether S2S runs, and the validated configuration snapshot
//!   it runs with. S2S is off by default.
//! - [`verify`]: the strict certificate verifier.
//! - [`conn`]: the connection manager: the inbound STARTTLS listener, the
//!   outbound dialer and their resource model.
//! - [`route`]: the router between S2S streams and local entities: what a
//!   remote sender may reach, egress queues per peer and IQ correlation.
//!
//! Nothing is advertised beyond what a remote asker of the server domain's
//! service discovery is answered.

pub mod config;
pub mod conn;
pub mod route;
pub mod verify;
