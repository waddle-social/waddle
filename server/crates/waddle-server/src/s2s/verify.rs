//! Strict certificate verification for server-to-server streams.
//!
//! This is the verifier of ADR-0020 decision D2, as designed in
//! `server/docs/planning/s2s-verifier-2047.md`, which also records the
//! standards decisions behind it. It provides:
//!
//! - [`TrustInventory`]: the trust material of a process, validated at
//!   startup. Every remote domain has one trust profile: a
//!   [`PrivateCaProfile`] where the operator configured one, with complete
//!   and current CRLs, and otherwise [`PublicRoots`].
//! - [`PeerVerifier`]: the per-connection rustls verifier for both TLS
//!   roles. It verifies the peer's chain under the profile of the stream's
//!   source domain and matches DNS-ID, SRV-ID and XmppAddr identifiers
//!   against that domain in the identity profile's canonical form.
//! - [`initiating_config`] and [`receiving_config`]: the TLS profile, with
//!   resumption, tickets and early data off.
//! - [`VerifierWorkers`]: bounded workers on which a connection processes
//!   its handshake, so verification never runs on the async runtime.
//!
//! The module owns no socket, reads no file and is not used by the running
//! server. A [`VerifiedPeer`] is evidence about a certificate, not an
//! authorization: admission, SASL EXTERNAL and everything after them belong
//! to the connection manager.

mod adapter;
mod crl;
mod der;
mod error;
mod facts;
mod identity;
mod inventory;
pub mod limits;
mod path;
mod signature;
mod tls;
mod trust;
mod worker;

pub use adapter::{PeerVerifier, Verification};
pub use error::{
    TlsConfigError, TrustError, TrustErrorKind, TrustItem, VerifyError, WorkerError, WorkerLost,
};
pub use identity::{MatchedIdentifier, ReferenceIdentity};
pub use inventory::{PrivateCaMaterial, PrivateCaProfile};
pub use path::{CertificateRole, VerifiedPeer};
pub use tls::{
    initiating_config, receiving_config, OutboundTransport, TlsSettings, DIRECT_TLS_ALPN,
    TLS_PROTOCOL_VERSIONS,
};
pub use trust::{
    PublicRoots, TrustInventory, TrustInventorySummary, TrustProfile, TrustProfileKind,
};
pub use worker::{Completion, VerifierWorkers};
