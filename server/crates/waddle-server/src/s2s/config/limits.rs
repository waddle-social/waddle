//! The connection manager's resource model: global caps, per-peer limits,
//! rates and deadlines (ADR-0020 gate G7).
//!
//! Every value has a default and a ceiling, and configuration may set any
//! value from one up to its ceiling, never beyond, so a configuration file
//! cannot weaken the bounds recorded in
//! `server/docs/planning/s2s-connection-manager-2047.md`. Every rate and
//! most caps default to their ceilings, so those can only be lowered; the
//! unauthenticated sockets per source address and the negotiating streams
//! per peer default below their ceilings and can also be raised. Six
//! deadlines default below their ceilings and can also be raised: the
//! established-stream idle timeout, the maximum connection age, the
//! eviction idle time, the replacement interval and the two backoff
//! durations.
//!
//! The input rates also have a floor, [`MIN_INPUT_BYTES_PER_SECOND`], so a
//! stanza at the codec's ceiling can always arrive within
//! [`MAX_INCOMPLETE_ELEMENT_TIME`]. The loader also refuses limits that do
//! not fit together: established streams and unauthenticated sockets
//! together must fit the socket cap, so negotiation always has sockets;
//! the peer cap must stay below the inbound socket cap; and the global
//! input rate and burst must be at least the per-socket ones.

use std::time::Duration;

use waddle_xmpp::s2s::CodecLimits;

/// Largest S2S configuration file read, in bytes.
pub const MAX_CONFIG_FILE_BYTES: u64 = 64 * 1024;

/// Largest certificate, key or CRL file read, in bytes.
pub const MAX_MATERIAL_FILE_BYTES: u64 = 1024 * 1024;

/// Largest total of configuration and material bytes read for one snapshot.
pub const MAX_TOTAL_MATERIAL_BYTES: u64 = 16 * 1024 * 1024;

/// Most local domains one snapshot serves.
pub const MAX_LOCAL_DOMAINS: usize = 16;

/// Most entries in one admission list.
pub const MAX_LIST_ENTRIES: usize = 1024;

/// Most configured endpoints for one remote domain.
pub const MAX_ENDPOINTS_PER_DOMAIN: usize = 4;

/// Most domains with configured endpoints.
pub const MAX_ENDPOINT_DOMAINS: usize = 256;

/// Most private-CA profiles in one snapshot.
pub const MAX_PRIVATE_CA_PROFILES: usize = 32;

/// Most files one private-CA profile names, anchors, intermediates and CRLs
/// together.
pub const MAX_PROFILE_FILES: usize = 24;

/// The lowest configurable input rate, per socket and globally, in bytes
/// per second: a stanza at the codec's 512 KiB ceiling arrives in at most
/// 16 seconds at this rate, within [`MAX_INCOMPLETE_ELEMENT_TIME`].
pub const MIN_INPUT_BYTES_PER_SECOND: u32 = 32 * 1024;

/// The longest one first-level element on an established stream may stay
/// unfinished, however long the input rates held its reads back. The
/// incomplete-element deadline ([`Deadlines::incomplete_element`]) charges
/// the peer only for time the server was willing to read; this bounds the
/// whole time.
pub const MAX_INCOMPLETE_ELEMENT_TIME: Duration = Duration::from_secs(30);

/// A token bucket: `per_second` tokens accrue each second up to `burst`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub per_second: u32,
    pub burst: u32,
}

/// Caps and rates. See the module documentation for how configuration may
/// change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Open S2S sockets in every stage and both directions.
    pub max_sockets: usize,
    /// Open inbound sockets.
    pub max_inbound_sockets: usize,
    /// Open outbound sockets.
    pub max_outbound_sockets: usize,
    /// Inbound sockets that have not completed authentication.
    pub max_unauthenticated: usize,
    /// Unauthenticated inbound sockets from one source address. The
    /// default is below the inbound handshake cap, so one source address
    /// cannot hold every inbound handshake.
    pub max_unauthenticated_per_address: usize,
    /// TLS handshakes in progress on inbound connections. Each direction
    /// has its own verifier workers of this size, so inbound handshakes
    /// cannot starve outbound ones or the reverse.
    pub max_inbound_handshakes: usize,
    /// TLS handshakes in progress on outbound connections.
    pub max_outbound_handshakes: usize,
    /// Established streams, both directions. The default leaves the
    /// unauthenticated sockets' worth of the socket cap for negotiation.
    /// In `open` mode a quarter of it is reserved for allowlisted and
    /// trusted-home peers.
    pub max_streams: usize,
    /// Remote domains with at least one established stream; below the
    /// inbound socket cap by default. In `open` mode a quarter of it is
    /// reserved for allowlisted and trusted-home peers.
    pub max_peers: usize,
    /// Streams negotiating with one remote domain at once, in each
    /// direction separately. An inbound connection counts only once TLS has
    /// verified its remote domain; before that it counts against the
    /// global and per-address unauthenticated caps alone.
    pub max_negotiating_per_peer: usize,
    /// New inbound connections, all sources together.
    pub accept_rate: RateLimit,
    /// New inbound connections from one source address. An IPv6 source is
    /// counted by its /64 prefix.
    pub accept_rate_per_address: RateLimit,
    /// Source addresses (IPv6 /64 prefixes) whose rate and socket counts
    /// are tracked; a new address is refused while the table is full.
    pub max_tracked_addresses: usize,
    /// Outbound attempts in progress at once, each for a distinct
    /// directional domain pair.
    pub max_dialing: usize,
    /// Remote domains whose outbound backoff is remembered. Only failing
    /// destinations have an entry; a domain without one is never refused.
    /// When the table is full, recording a new failure replaces the entry
    /// updated longest ago among those whose wait has passed, or else the
    /// entry updated longest ago.
    pub max_backoff_destinations: usize,
    /// Bytes read from one socket per second, with its burst. A peer over
    /// its rate is read more slowly; the connection is not closed for it,
    /// and the time its reads wait is not charged against its
    /// incomplete-element deadline.
    pub input_rate_per_socket: RateLimit,
    /// Bytes read from all S2S sockets together per second, with its burst.
    pub input_rate_global: RateLimit,
}

impl ResourceLimits {
    /// The defaults: every value at its ceiling except the unauthenticated
    /// sockets per source address and the negotiating streams per peer.
    pub const DEFAULT: Self = Self {
        max_unauthenticated_per_address: 2,
        max_negotiating_per_peer: 3,
        ..Self::CEILING
    };

    /// The ceilings.
    pub const CEILING: Self = Self {
        max_sockets: 32,
        max_inbound_sockets: 16,
        max_outbound_sockets: 16,
        max_unauthenticated: 8,
        max_unauthenticated_per_address: 4,
        max_inbound_handshakes: 4,
        max_outbound_handshakes: 2,
        max_streams: 24,
        max_peers: 12,
        max_negotiating_per_peer: 4,
        accept_rate: RateLimit {
            per_second: 8,
            burst: 8,
        },
        accept_rate_per_address: RateLimit {
            per_second: 2,
            burst: 4,
        },
        max_tracked_addresses: 256,
        max_dialing: 8,
        max_backoff_destinations: 256,
        input_rate_per_socket: RateLimit {
            per_second: 64 * 1024,
            burst: 128 * 1024,
        },
        input_rate_global: RateLimit {
            per_second: 512 * 1024,
            burst: 1024 * 1024,
        },
    };
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Deadlines. A negotiation deadline never restarts on partial input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// TCP connect of an outbound attempt, per endpoint.
    pub connect: Duration,
    /// Each negotiation stage: the initial header, the TLS handshake and
    /// each expected stream restart.
    pub stage: Duration,
    /// The SASL phase, from the post-TLS stream header; never renewed by
    /// replacement or aborted exchanges.
    pub sasl: Duration,
    /// Everything from TCP accept or connect to the established stream.
    pub negotiation: Duration,
    /// An unfinished first-level element on an established stream,
    /// counting only time the server was willing to read: time a read
    /// waits for the input rates is added back. The element may still take
    /// no longer than [`MAX_INCOMPLETE_ELEMENT_TIME`] in all.
    pub incomplete_element: Duration,
    /// An established stream with no traffic: nothing received, not even a
    /// whitespace keepalive, and nothing sent.
    pub idle: Duration,
    /// The longest an established stream lives, whatever its certificate
    /// allows.
    pub max_age: Duration,
    /// Writing one batch of output, and the terminal error and close.
    pub write: Duration,
    /// Draining streams at shutdown before the remaining tasks are aborted.
    pub drain: Duration,
    /// How long an established stream must have carried no element, in
    /// either direction, before it can be evicted for a new one. Whitespace
    /// keepalives do not count.
    pub eviction_idle: Duration,
    /// How long the stream of one directional pair is kept before a newer
    /// one may replace it. A new authenticated stream replaces the existing
    /// one, which closes with `conflict`, unless the existing stream was
    /// registered less than this long ago; the new stream is then refused
    /// with `conflict` instead, so two peers cannot make a pair flap.
    pub replacement_interval: Duration,
    /// Wait after the first failed outbound attempt to a domain.
    pub backoff_initial: Duration,
    /// Longest wait between outbound attempts to a domain.
    pub backoff_max: Duration,
}

impl Deadlines {
    /// The defaults.
    pub const DEFAULT: Self = Self {
        connect: Duration::from_secs(5),
        stage: Duration::from_secs(5),
        sasl: Duration::from_secs(5),
        negotiation: Duration::from_secs(20),
        incomplete_element: Duration::from_secs(5),
        idle: Duration::from_secs(300),
        max_age: Duration::from_secs(3600),
        write: Duration::from_secs(1),
        drain: Duration::from_secs(5),
        eviction_idle: Duration::from_secs(30),
        replacement_interval: Duration::from_secs(30),
        backoff_initial: Duration::from_secs(1),
        backoff_max: Duration::from_secs(300),
    };

    /// The ceilings. `idle`, `max_age`, `eviction_idle`,
    /// `replacement_interval`, `backoff_initial` and `backoff_max` exceed
    /// their defaults, so an operator may raise them that far; every other
    /// deadline's ceiling is its default.
    pub const CEILING: Self = Self {
        connect: Duration::from_secs(5),
        stage: Duration::from_secs(5),
        sasl: Duration::from_secs(5),
        negotiation: Duration::from_secs(20),
        incomplete_element: Duration::from_secs(5),
        idle: Duration::from_secs(3600),
        max_age: Duration::from_secs(24 * 3600),
        write: Duration::from_secs(1),
        drain: Duration::from_secs(5),
        eviction_idle: Duration::from_secs(3600),
        replacement_interval: Duration::from_secs(3600),
        backoff_initial: Duration::from_secs(60),
        backoff_max: Duration::from_secs(3600),
    };
}

impl Default for Deadlines {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The S2S codec's ceilings (ADR-0020 gate G7).
///
/// Before SASL succeeds the decoder runs in its pre-authentication mode, so
/// no first-level element exceeds `max_control_element_bytes` (8 KiB) and
/// nothing but negotiation elements is decoded. The larger values apply
/// only to an authenticated stream. They are sized for the largest stanzas
/// federation is expected to carry: a `disco#items` result listing about
/// 2,000 rooms and a MUC affiliation list of about 2,000 members. Each such
/// item is one element with two or three attributes and usually under
/// 150 bytes, so 2,000 of them are about 300 KiB of markup, 2,000 elements
/// and under 300 KiB of attributes.
///
/// Resident decoder memory per authenticated connection, by the codec's
/// own accounting (`CodecLimits` documentation): the 768 KiB input
/// buffer, the 1.5 MiB tree-string ceiling (up to twice that in capacity),
/// and node structure of about 130 bytes per element plus about 1.1 KiB per
/// element with attributes, about 4.9 MiB at 4,096 elements. The worst case
/// is therefore about 9 MiB for a peer that keeps one maximal stanza
/// unfinished, which it can do for at most [`MAX_INCOMPLETE_ELEMENT_TIME`],
/// and about 220 MiB across the 24 established streams the stream cap
/// allows. This is an analysis, not a measurement; the verification phase
/// measures it.
pub const STREAM_CODEC_LIMITS: CodecLimits = CodecLimits {
    max_header_bytes: 8 * 1024,
    max_stanza_bytes: 512 * 1024,
    max_control_element_bytes: 8 * 1024,
    max_depth: 32,
    max_attributes_per_element: 32,
    max_attribute_bytes_per_stanza: 384 * 1024,
    max_elements_per_stanza: 4096,
    // A stanza at the ceiling plus one 16 KiB read, with room to spare:
    // the decoder accepts a push of at most `max_buffered_bytes -
    // max_stanza_bytes`.
    max_buffered_bytes: 768 * 1024,
    max_token_bytes: 8 * 1024,
    max_namespace_bytes: 1024,
    max_tree_bytes_per_stanza: 1536 * 1024,
};
