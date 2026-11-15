//! Operator configuration of the S2S transport (ADR-0020 D1, D2 and D8;
//! gate G7).
//!
//! S2S is off by default ([`S2sActivation`]). When it is on, one immutable
//! [`S2sConfig`] snapshot is loaded and validated before any S2S socket
//! exists: the local domains and their credentials, peer admission
//! ([`AdmissionPolicy`]), per-domain trust profiles for the verifier,
//! configured endpoints, and the resource model ([`ResourceLimits`] and
//! [`Deadlines`]). A configuration that is contradictory or incomplete fails
//! to load and S2S does not start. A change takes effect when the process
//! restarts with a new snapshot; there is no reload.
//!
//! Configured endpoints are reached with STARTTLS or, when configured so,
//! XEP-0368 direct TLS. DNS discovery ([`S2sConfig::discovery_enabled`])
//! and every Server Dialback role ([`DialbackSettings`]) are off by
//! default; discovery loads only with [`PeerScope::Public`].
//!
//! Under the default [`PeerScope::Test`] every configured domain must be
//! within `.test`, except denylist entries, which only refuse, and the
//! listener and every endpoint must be loopback addresses.
//! [`PeerScope::Public`] lifts that restriction (ADR-0020 gate G6).

mod admission;
mod document;
mod error;
mod limits;
mod load;
mod snapshot;

pub use admission::{
    AdmissionDecision, AdmissionMode, AdmissionPolicy, AdmissionRefusal, PeerScope,
};
pub use error::{ConfigError, ConfigSection};
pub use limits::{
    Deadlines, RateLimit, ResourceLimits, MAX_BLOCKED_PREFIXES, MAX_CONFIG_FILE_BYTES,
    MAX_ENDPOINTS_PER_DOMAIN, MAX_INCOMPLETE_ELEMENT_TIME, MAX_MATERIAL_FILE_BYTES,
    MAX_TOTAL_MATERIAL_BYTES, MIN_INPUT_BYTES_PER_SECOND, NO_ENDPOINT_WAIT, STREAM_CODEC_LIMITS,
};
pub use load::{S2sActivation, CONFIG_FILE_VARIABLE, ENABLED_VARIABLE};
pub use snapshot::{ConfiguredEndpoint, DialbackSettings, S2sConfig, MIN_DIALBACK_SECRET_BYTES};
