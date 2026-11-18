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
    /// Whether peers are limited to `.test` domains and loopback addresses
    /// (`test`, the default) or may be any DNS domain and address
    /// (`public`); ADR-0020 gate G6.
    #[serde(default)]
    pub(super) peer_scope: Option<PeerScopeDocument>,
    #[serde(default)]
    pub(super) discovery: DiscoveryDocument,
    #[serde(default)]
    pub(super) dialback: DialbackDocument,
    #[serde(default)]
    pub(super) host_policy: HostPolicyDocument,
}

/// Host policy for the grants of remote principals (ADR-0020 D2 and D5).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostPolicyDocument {
    /// Honour a remote principal's grants only when the stream that carried
    /// its request has `pkix` strength, member-level grants included. Off
    /// by default: a member-level grant is honoured on a dialback stream
    /// too, while privileged grants and room moderation always need `pkix`
    /// (ADR-0020 D8).
    #[serde(default)]
    pub(super) require_pkix: bool,
    /// Create new rooms with their `federated` setting on, so accounts at
    /// admitted servers reach them unless an owner turns it off. Off by
    /// default: a room is opened to other servers by its owner. Rooms that
    /// exist already keep their setting whatever this says.
    #[serde(default)]
    pub(super) federate_new_rooms: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PeerScopeDocument {
    Test,
    Public,
}

/// ADR-0020 D3: DNS discovery of remote domains without configured
/// endpoints. Off by default, and only with `peer_scope = "public"`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DiscoveryDocument {
    #[serde(default)]
    pub(super) enabled: bool,
    /// Address blocks, in CIDR notation, refused for DNS-derived addresses
    /// in addition to the built-in classes: the network's own NAT64
    /// (RFC 6052) and 6rd (RFC 5969) prefixes, which embed IPv4 addresses
    /// an address alone does not reveal, or any other block.
    #[serde(default)]
    pub(super) blocked_prefixes: Vec<String>,
}

/// ADR-0020 D2: Server Dialback (XEP-0220). Every role is off by default.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DialbackDocument {
    /// The Initiating and Authoritative Server roles: authenticate this
    /// deployment by dialback to peers that do not offer SASL EXTERNAL,
    /// and answer their verification requests. Requires `secret_file`.
    #[serde(default)]
    pub(super) outbound: bool,
    /// The Receiving Server role: accept dialback from domains on
    /// `admission.dialback` that are also admitted.
    #[serde(default)]
    pub(super) inbound: bool,
    /// The deployment's dialback secret (XEP-0185), shared by every
    /// replica of this deployment and by no other deployment.
    #[serde(default)]
    pub(super) secret_file: Option<PathBuf>,
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
    pub(super) address: String,
    /// `starttls` (the default) or `direct_tls` (XEP-0368).
    #[serde(default)]
    pub(super) transport: Option<EndpointTransportDocument>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum EndpointTransportDocument {
    Starttls,
    DirectTls,
}

/// Caps, rates and deadlines within their ceilings; see
/// [`super::ResourceLimits`] and [`super::Deadlines`]. Rates are in
/// connections or bytes per second, durations in milliseconds.
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
    /// New outbound connections per second across every remote domain.
    pub(super) dial_per_second: Option<u32>,
    pub(super) dial_burst: Option<u32>,
    /// Dialback verification connections in progress, and new ones per
    /// second.
    pub(super) max_verifications: Option<usize>,
    pub(super) verification_per_second: Option<u32>,
    pub(super) verification_burst: Option<u32>,
    pub(super) max_backoff_destinations: Option<usize>,
    pub(super) max_streams: Option<usize>,
    pub(super) max_peers: Option<usize>,
    pub(super) max_negotiating_per_peer: Option<usize>,
    /// Established inbound streams for one directional domain pair.
    pub(super) max_inbound_streams_per_peer: Option<usize>,
    /// Directional domain pairs the router keeps an outbound queue for.
    pub(super) max_pair_queues: Option<usize>,
    pub(super) accept_per_second: Option<u32>,
    pub(super) accept_burst: Option<u32>,
    pub(super) accept_per_address_per_second: Option<u32>,
    pub(super) accept_per_address_burst: Option<u32>,
    /// Bytes read from all S2S sockets together per second.
    pub(super) input_per_second: Option<u32>,
    pub(super) input_burst: Option<u32>,
    /// Bytes read from one socket per second.
    pub(super) input_per_socket_per_second: Option<u32>,
    pub(super) input_per_socket_burst: Option<u32>,
    pub(super) connect_timeout_ms: Option<u64>,
    pub(super) stage_timeout_ms: Option<u64>,
    pub(super) sasl_timeout_ms: Option<u64>,
    pub(super) dialback_timeout_ms: Option<u64>,
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
