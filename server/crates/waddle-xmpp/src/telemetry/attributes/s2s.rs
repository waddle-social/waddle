//! Attributes for server-to-server connection metrics. Each is a closed
//! enum; none can carry a domain, a JID, an address or a stream id.

use super::sealed;
use super::MetricAttribute;

/// `direction` — which side opened the S2S connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sDirection {
    /// A peer connected to this server's `s2s` listener.
    Inbound,
    /// This server dialled the peer.
    Outbound,
}

impl S2sDirection {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 2] = [Self::Inbound, Self::Outbound];
}

impl sealed::Sealed for S2sDirection {}
impl MetricAttribute for S2sDirection {
    fn key(&self) -> &'static str {
        "direction"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

/// `outcome` — how an S2S connection ended, or that it was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sOutcome {
    /// TLS, SASL EXTERNAL and the authenticated restart completed.
    Established,
    /// A connection limit refused the socket or the stream.
    Capacity,
    /// Peer admission (ADR-0020 D1) refused the domain.
    Admission,
    /// The peer's certificate was refused: an active certificate failure.
    Certificate,
    /// The TLS handshake failed for another reason.
    Tls,
    /// The stream failed during or after negotiation with a stream error
    /// this server sent.
    Protocol,
    /// The peer sent a stream error.
    PeerError,
    /// The peer sent `see-other-host`; the redirect was not followed.
    Redirected,
    /// The peer closed the stream or the connection.
    PeerClosed,
    /// A deadline passed.
    Timeout,
    /// The certificate, its trust material or the connection age expired.
    Expired,
    /// The stream was evicted to make room for another.
    Evicted,
    /// Another stream already carries the directional domain pair.
    Conflict,
    /// The server is shutting down.
    Shutdown,
    /// Reading from or writing to the socket failed.
    Io,
}

impl S2sOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 15] = [
        Self::Established,
        Self::Capacity,
        Self::Admission,
        Self::Certificate,
        Self::Tls,
        Self::Protocol,
        Self::PeerError,
        Self::Redirected,
        Self::PeerClosed,
        Self::Timeout,
        Self::Expired,
        Self::Evicted,
        Self::Conflict,
        Self::Shutdown,
        Self::Io,
    ];
}

impl sealed::Sealed for S2sOutcome {}
impl MetricAttribute for S2sOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Established => "established",
            Self::Capacity => "capacity",
            Self::Admission => "admission",
            Self::Certificate => "certificate",
            Self::Tls => "tls",
            Self::Protocol => "protocol",
            Self::PeerError => "peer_error",
            Self::Redirected => "redirected",
            Self::PeerClosed => "peer_closed",
            Self::Timeout => "timeout",
            Self::Expired => "expired",
            Self::Evicted => "evicted",
            Self::Conflict => "conflict",
            Self::Shutdown => "shutdown",
            Self::Io => "io",
        }
    }
}

/// `resource` — the S2S connection limit that refused a socket or stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sResource {
    /// Open S2S sockets, all stages and both directions.
    Sockets,
    /// Inbound sockets that have not authenticated yet.
    Unauthenticated,
    /// Unauthenticated inbound sockets from one source address (an IPv6
    /// /64 prefix), or the table of tracked source addresses.
    SourceAddress,
    /// The inbound accept rate, globally or for one source address.
    AcceptRate,
    /// Concurrent TLS handshakes.
    Handshakes,
    /// Established streams.
    Streams,
    /// Remote domains with an established stream.
    Peers,
    /// Streams negotiating with one remote domain in one direction; an
    /// inbound stream counts once TLS has verified the domain.
    PerPeer,
}

impl S2sResource {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 8] = [
        Self::Sockets,
        Self::Unauthenticated,
        Self::SourceAddress,
        Self::AcceptRate,
        Self::Handshakes,
        Self::Streams,
        Self::Peers,
        Self::PerPeer,
    ];
}

impl sealed::Sealed for S2sResource {}
impl MetricAttribute for S2sResource {
    fn key(&self) -> &'static str {
        "resource"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Sockets => "sockets",
            Self::Unauthenticated => "unauthenticated",
            Self::SourceAddress => "source_address",
            Self::AcceptRate => "accept_rate",
            Self::Handshakes => "handshakes",
            Self::Streams => "streams",
            Self::Peers => "peers",
            Self::PerPeer => "per_peer",
        }
    }
}

/// `transport` — how an outbound S2S connection reached TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sTransport {
    /// STARTTLS on the plaintext stream.
    StartTls,
    /// XEP-0368 direct TLS.
    DirectTls,
}

impl S2sTransport {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 2] = [Self::StartTls, Self::DirectTls];
}

impl sealed::Sealed for S2sTransport {}
impl MetricAttribute for S2sTransport {
    fn key(&self) -> &'static str {
        "transport"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::StartTls => "starttls",
            Self::DirectTls => "direct_tls",
        }
    }
}
