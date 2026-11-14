//! The S2S connection manager (ADR-0020 D1 and D2; gates G4 and G7).
//!
//! - Inbound: a STARTTLS-only listener. The initial, plaintext stream
//!   header must carry `from`; it fixes the remote domain, which admission
//!   checks and which selects the trust profile and reference identity
//!   before the TLS handshake starts. There is no inbound direct TLS.
//! - Outbound: [`S2sHandle::connect`] dials the endpoints an
//!   [`EndpointResolver`] returns, with STARTTLS only and the remote domain
//!   as the TLS server name; XEP-0368 direct TLS arrives with discovery. A
//!   `see-other-host` redirect ends the attempt and is never followed.
//! - Both directions authenticate with certificate verification and SASL
//!   EXTERNAL. Only then does a stream become a [`VerifiedPeerStream`],
//!   which carries its authentication strength.
//! - Global caps on sockets, unauthenticated sockets, handshakes, streams
//!   and peers, per-address and per-peer limits, accept and input rates,
//!   eviction of streams that carry no traffic, outbound backoff,
//!   per-stage deadlines and a bounded shutdown join, as recorded in
//!   `server/docs/planning/s2s-connection-manager-2047.md`.
//!
//! No stanza is routed: an established inbound stream hands each stanza
//! to a [`StanzaSink`], and the sink of this release refuses all of them.

mod admission;
mod capacity;
mod endpoints;
mod peer_stream;
mod registry;
mod service;
mod session;
mod sink;
mod startup;
mod telemetry;
mod transport;

pub use endpoints::{
    ConfiguredEndpoints, Endpoint, EndpointOrigin, EndpointResolver, Endpoints, ResolveError,
};
pub use peer_stream::{AuthenticationStrength, StreamDirection, TlsParameters, VerifiedPeerStream};
pub use service::{ConnectError, S2sHandle, S2sService, ServiceOptions, ShutdownReport};
pub use sink::{RefuseAllStanzas, SinkVerdict, StanzaSink};
pub use startup::{
    check_inherited_listener, load_config, load_enabled_config, load_enabled_config_from,
    StartupError, S2S_LISTENER_NAME,
};
