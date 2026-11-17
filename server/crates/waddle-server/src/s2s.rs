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
//! - [`route`]: the router between S2S streams and local entities: what a
//!   remote sender may reach, egress queues per peer and IQ correlation.
//! - `remote_content`: what is removed from a message another server sent
//!   before a local client, archive or store sees it.
//!
//! Nothing is advertised beyond what a remote asker of the server domain's
//! service discovery is answered.

pub mod config;
pub mod conn;
pub mod dialback;
pub mod discovery;
pub(crate) mod remote_content;
pub mod route;
pub mod verify;
