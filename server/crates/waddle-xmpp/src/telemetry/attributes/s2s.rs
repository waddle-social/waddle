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
    /// Server Dialback did not authenticate the pair: the key was refused
    /// or could not be verified.
    Dialback,
    /// A dialback verification connection received its answer and closed.
    Verified,
}

impl S2sOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 17] = [
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
        Self::Dialback,
        Self::Verified,
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
            Self::Dialback => "dialback",
            Self::Verified => "verified",
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
    /// The global rate of new outbound connections.
    DialRate,
}

impl S2sResource {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 9] = [
        Self::Sockets,
        Self::Unauthenticated,
        Self::SourceAddress,
        Self::AcceptRate,
        Self::Handshakes,
        Self::Streams,
        Self::Peers,
        Self::PerPeer,
        Self::DialRate,
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
            Self::DialRate => "dial_rate",
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

/// `strength` — how an established S2S stream's domain pair authenticated
/// (ADR-0020 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sStrength {
    /// TLS certificate verification and SASL EXTERNAL.
    Pkix,
    /// XEP-0220 Server Dialback over TLS.
    Dialback,
}

impl S2sStrength {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 2] = [Self::Pkix, Self::Dialback];
}

impl sealed::Sealed for S2sStrength {}
impl MetricAttribute for S2sStrength {
    fn key(&self) -> &'static str {
        "strength"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Pkix => "pkix",
            Self::Dialback => "dialback",
        }
    }
}

/// `role` — the XEP-0220 role in which this server took part in a dialback
/// exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sDialbackRole {
    /// Sent a key for its own domain on an outbound stream.
    Initiating,
    /// Verified a key an inbound peer sent, with that peer's Authoritative
    /// Server.
    Receiving,
    /// Answered a verification request about a key it issued.
    Authoritative,
}

impl S2sDialbackRole {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 3] = [Self::Initiating, Self::Receiving, Self::Authoritative];
}

impl sealed::Sealed for S2sDialbackRole {}
impl MetricAttribute for S2sDialbackRole {
    fn key(&self) -> &'static str {
        "role"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Initiating => "initiating",
            Self::Receiving => "receiving",
            Self::Authoritative => "authoritative",
        }
    }
}

/// `result` — what a dialback exchange concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sDialbackResult {
    Valid,
    Invalid,
    /// The key could not be verified, or the exchange failed.
    Error,
}

impl S2sDialbackResult {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 3] = [Self::Valid, Self::Invalid, Self::Error];
}

impl sealed::Sealed for S2sDialbackResult {}
impl MetricAttribute for S2sDialbackResult {
    fn key(&self) -> &'static str {
        "result"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Error => "error",
        }
    }
}

/// `source` — where an outbound connection's endpoints came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S2sDiscovery {
    /// Explicitly configured endpoints.
    Configured,
    /// `_xmpps-server` and `_xmpp-server` SRV records.
    Srv,
    /// A/AAAA records of the domain on port 5269, after both SRV lookups
    /// answered that no record exists.
    Fallback,
    /// DNS says the service is not offered, or nothing is configured.
    NoService,
    /// A lookup failed; not the absence of records.
    LookupFailed,
    /// Every address DNS returned is in a blocked class.
    Blocked,
}

impl S2sDiscovery {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 6] = [
        Self::Configured,
        Self::Srv,
        Self::Fallback,
        Self::NoService,
        Self::LookupFailed,
        Self::Blocked,
    ];
}

impl sealed::Sealed for S2sDiscovery {}
impl MetricAttribute for S2sDiscovery {
    fn key(&self) -> &'static str {
        "source"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Srv => "srv",
            Self::Fallback => "fallback",
            Self::NoService => "no_service",
            Self::LookupFailed => "lookup_failed",
            Self::Blocked => "blocked",
        }
    }
}
