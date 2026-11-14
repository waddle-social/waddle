//! Server-to-server transport for the federation profile of ADR-0020.
//!
//! Only the certificate verifier exists so far. Nothing in this module opens
//! a listener, dials a peer or is reachable from the running server.

pub mod verify;
