//! The S2S configuration file as written by the operator. Every table
//! refuses unknown keys, so a misspelt setting fails to load instead of
//! being ignored.

use std::path::PathBuf;

use serde::Deserialize;

/// The whole file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    /// A UUID naming this snapshot. It is not secret and grants nothing.
    pub(super) generation: String,
    /// The inbound STARTTLS listener's address. Without it the process
    /// accepts no S2S connection.
    #[serde(default)]
    pub(super) listen: Option<String>,
    /// The domains this process authenticates as and accepts streams for.
    /// The first is the default domain of an error-carrying response header.
    pub(super) local_domains: Vec<String>,
    #[serde(default)]
    pub(super) admission: AdmissionDocument,
    #[serde(default)]
    pub(super) credentials: Vec<CredentialDocument>,
    #[serde(default)]
    pub(super) private_ca: Vec<PrivateCaDocument>,
    #[serde(default)]
    pub(super) endpoints: Vec<EndpointDocument>,
    #[serde(default)]
    pub(super) limits: LimitsDocument,
}

/// ADR-0020 D1 and D8: the admission mode and lists.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdmissionDocument {
    #[serde(default)]
    pub(super) mode: Option<ModeDocument>,
    #[serde(default)]
    pub(super) allow: Vec<String>,
    #[serde(default)]
    pub(super) deny: Vec<String>,
    #[serde(default)]
    pub(super) dialback: Vec<String>,
    #[serde(default)]
    pub(super) trusted_homes: Vec<String>,
    /// Trusted homes whose streams count as trusted-home streams only when
    /// their certificate verified under a private-CA profile.
    #[serde(default)]
    pub(super) require_private_ca: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ModeDocument {
    Closed,
    Allowlist,
    Open,
}

/// The certificate chain and private key one local domain presents.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CredentialDocument {
    pub(super) domain: String,
    pub(super) certificate_chain: PathBuf,
    pub(super) private_key: PathBuf,
}

/// A private-CA trust profile and the remote domains that use it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateCaDocument {
    pub(super) domains: Vec<String>,
    pub(super) anchors: Vec<PathBuf>,
    #[serde(default)]
    pub(super) intermediates: Vec<PathBuf>,
    pub(super) crls: Vec<PathBuf>,
}

/// An explicitly configured endpoint for one remote domain (RFC 6120
/// §3.2.3). It selects a socket only; the remote domain stays the reference
/// identity.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EndpointDocument {
    pub(super) domain: String,
    /// Reached with STARTTLS. XEP-0368 direct TLS endpoints arrive with
    /// discovery.
    pub(super) address: String,
}

/// Lowered caps and deadlines; see [`super::ResourceLimits`] and
/// [`super::Deadlines`]. Durations are in milliseconds.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LimitsDocument {
    pub(super) max_sockets: Option<usize>,
    pub(super) max_inbound_sockets: Option<usize>,
    pub(super) max_outbound_sockets: Option<usize>,
    pub(super) max_unauthenticated: Option<usize>,
    pub(super) max_unauthenticated_per_address: Option<usize>,
    pub(super) max_inbound_handshakes: Option<usize>,
    pub(super) max_outbound_handshakes: Option<usize>,
    pub(super) max_dialing: Option<usize>,
    pub(super) max_backoff_destinations: Option<usize>,
    pub(super) max_streams: Option<usize>,
    pub(super) max_peers: Option<usize>,
    pub(super) max_negotiating_per_peer: Option<usize>,
    pub(super) accept_per_second: Option<u32>,
    pub(super) accept_burst: Option<u32>,
    pub(super) accept_per_address_per_second: Option<u32>,
    pub(super) accept_per_address_burst: Option<u32>,
    pub(super) connect_timeout_ms: Option<u64>,
    pub(super) stage_timeout_ms: Option<u64>,
    pub(super) sasl_timeout_ms: Option<u64>,
    pub(super) negotiation_timeout_ms: Option<u64>,
    pub(super) incomplete_element_timeout_ms: Option<u64>,
    pub(super) idle_timeout_ms: Option<u64>,
    pub(super) max_age_ms: Option<u64>,
    pub(super) write_timeout_ms: Option<u64>,
    pub(super) drain_timeout_ms: Option<u64>,
    pub(super) eviction_idle_ms: Option<u64>,
    pub(super) replacement_interval_ms: Option<u64>,
    pub(super) backoff_initial_ms: Option<u64>,
    pub(super) backoff_max_ms: Option<u64>,
}
