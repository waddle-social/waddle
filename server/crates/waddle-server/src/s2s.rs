//! Server-to-server transport for the federation profile of ADR-0020.
//!
//! - [`config`]: whether S2S runs, and the validated configuration snapshot
//!   it runs with. S2S is off by default.
//! - [`verify`]: the strict certificate verifier.
//! - [`conn`]: the connection manager: the inbound STARTTLS listener, the
//!   outbound dialer and their resource model.
//! - [`discovery`]: DNS discovery of remote endpoints (XEP-0368, RFC 6120
//!   §3.2) with the blocked address classes. Off by default, and only in
//!   the public peer scope.
//! - [`dialback`]: XEP-0185 dialback keys for the Server Dialback roles.
//!   Off by default.
//!
//! No stanza from an S2S stream is routed, and nothing is advertised.

pub mod config;
pub mod conn;
pub mod dialback;
pub mod discovery;
pub mod verify;
