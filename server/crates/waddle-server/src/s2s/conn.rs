//! The S2S connection manager (ADR-0020 D1 and D2; gates G4 and G7).
//!
//! - Inbound: a STARTTLS-only listener. The initial, plaintext stream
//!   header must carry `from`; it fixes the remote domain, which admission
//!   checks and which selects the trust profile and reference identity
//!   before the TLS handshake starts. There is no inbound direct TLS.
//! - Outbound: [`S2sHandle::connect`] dials the endpoints an
//!   [`EndpointResolver`] returns, with STARTTLS or XEP-0368 direct TLS as
//!   each endpoint says, and the remote domain as the TLS server name and
//!   Server Name Indication. A DNS-derived address is checked against the
//!   blocked address classes and the operator's blocked prefixes on the
//!   address connected to. A
//!   `see-other-host` redirect ends the attempt and is never followed.
//! - Both directions authenticate with certificate verification and SASL
//!   EXTERNAL, or, where configured (ADR-0020 D2), XEP-0220 Server Dialback
//!   over TLS: outbound as the Initiating Server, inbound as the Receiving
//!   Server for listed domains with a verification connection to the
//!   peer's Authoritative Server, and as this deployment's Authoritative
//!   Server. Only then does a stream become a [`VerifiedPeerStream`], which
//!   carries its authentication strength.
//! - Global caps on sockets, unauthenticated sockets, handshakes, streams
//!   and peers, per-address and per-peer limits, accept, dial and input
//!   rates, a separate budget for dialback verification connections,
//!   eviction of streams that carry no traffic, outbound backoff,
//!   per-stage deadlines and a bounded shutdown join, as recorded in
//!   `server/docs/planning/s2s-connection-manager-2047.md`.
//!
//! An established inbound stream hands each stanza to a [`StanzaSink`];
//! [`RefuseAllStanzas`] refuses every one, and the S2S router
//! (`crate::s2s::route`) is the sink when S2S is enabled. An established
//! outbound stream writes the stanzas [`S2sHandle::transmit`] gives it, one
//! at a time.

mod admission;
mod capacity;
mod endpoints;
mod outbox;
mod peer_stream;
mod registry;
mod service;
mod session;
mod sink;
mod startup;
mod telemetry;
mod transport;
mod verification;

pub use endpoints::{
    ConfiguredEndpoints, Endpoint, EndpointOrigin, EndpointResolver, Endpoints, ResolveError,
};
pub use outbox::TransmitError;
pub use peer_stream::{AuthenticationStrength, StreamDirection, TlsParameters, VerifiedPeerStream};
pub use service::{ConnectError, S2sHandle, S2sService, ServiceOptions, ShutdownReport};
pub use sink::{RefuseAllStanzas, SinkVerdict, StanzaSink};
pub use startup::{
    check_inherited_listener, check_inherited_listener_in, load_config, load_enabled_config,
    load_enabled_config_from, StartupError, S2S_LISTENER_NAME,
};
