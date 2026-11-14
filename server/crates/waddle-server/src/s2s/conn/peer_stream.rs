//! The authenticated stream capability handed to the rest of the server.

use std::time::Instant;

use rustls::pki_types::UnixTime;
use rustls::{CipherSuite, ProtocolVersion};
use uuid::Uuid;
use waddle_xmpp::identity::CanonicalDomain;

use crate::s2s::verify::{MatchedIdentifier, TrustProfileKind, VerifiedPeer};

/// Which side opened the connection carrying a stream. Streams are
/// directional: an inbound stream carries stanzas from the remote domain
/// to a local one, an outbound stream from a local domain to the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamDirection {
    Inbound,
    Outbound,
}

/// How a stream's domain pair was authenticated (ADR-0020 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthenticationStrength {
    /// TLS certificate validation followed by SASL EXTERNAL.
    Pkix,
    /// XEP-0220 Server Dialback over TLS: inbound, the remote domain proved
    /// itself by dialback; outbound, this deployment proved itself to the
    /// remote domain by dialback after verifying the remote's certificate.
    /// A pair authenticated by dialback is `dialback` even when a
    /// certificate on the stream or on the verification connection
    /// validated.
    Dialback,
}

impl AuthenticationStrength {
    /// A stable code for logs.
    pub fn code(self) -> &'static str {
        match self {
            Self::Pkix => "pkix",
            Self::Dialback => "dialback",
        }
    }
}

/// The TLS version and cipher suite a stream's connection negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlsParameters {
    pub version: Option<ProtocolVersion>,
    pub cipher_suite: Option<CipherSuite>,
}

/// What the remote certificate of a stream's connection proved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerCertificate {
    profile: TrustProfileKind,
    identifier: MatchedIdentifier,
    sha256: [u8; 32],
    expires_at: UnixTime,
}

/// An S2S stream whose negotiation completed over TLS: certificate
/// verification, SASL EXTERNAL and the authenticated restart, or Server
/// Dialback.
///
/// Only the connection manager constructs one, from the verifier's
/// evidence for the completed handshake, where the peer presented a
/// certificate, and the negotiated domain pair. It is a domain capability:
/// it names exactly one directional pair of domains, never a local
/// session, account, grant or delivery receipt, and authenticating a
/// domain does not authorize its subdomains or any other domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPeerStream {
    connection: u64,
    direction: StreamDirection,
    local: CanonicalDomain,
    remote: CanonicalDomain,
    strength: AuthenticationStrength,
    certificate: Option<PeerCertificate>,
    tls: TlsParameters,
    generation: Uuid,
    established_at: Instant,
}

/// What a new [`VerifiedPeerStream`] is built from.
pub(super) struct PeerStreamParts<'a> {
    pub(super) connection: u64,
    pub(super) direction: StreamDirection,
    pub(super) local: CanonicalDomain,
    pub(super) remote: CanonicalDomain,
    pub(super) strength: AuthenticationStrength,
    /// The verifier's evidence for the remote certificate, when one was
    /// presented and verified for `remote`.
    pub(super) peer: Option<&'a VerifiedPeer>,
    pub(super) tls: TlsParameters,
    pub(super) generation: Uuid,
}

impl VerifiedPeerStream {
    pub(super) fn new(parts: PeerStreamParts<'_>) -> Self {
        Self {
            connection: parts.connection,
            direction: parts.direction,
            local: parts.local,
            remote: parts.remote,
            strength: parts.strength,
            certificate: parts.peer.map(|peer| PeerCertificate {
                profile: peer.profile(),
                identifier: peer.matched_identifier(),
                sha256: *peer.certificate_sha256(),
                expires_at: peer.expires_at(),
            }),
            tls: parts.tls,
            generation: parts.generation,
            established_at: Instant::now(),
        }
    }

    /// The process-local identifier of the connection carrying the stream.
    pub fn connection(&self) -> u64 {
        self.connection
    }

    pub fn direction(&self) -> StreamDirection {
        self.direction
    }

    /// The local domain of the pair.
    pub fn local(&self) -> &CanonicalDomain {
        &self.local
    }

    /// The authenticated remote domain of the pair.
    pub fn remote(&self) -> &CanonicalDomain {
        &self.remote
    }

    /// The domain stanzas on this stream come from.
    pub fn source(&self) -> &CanonicalDomain {
        match self.direction {
            StreamDirection::Inbound => &self.remote,
            StreamDirection::Outbound => &self.local,
        }
    }

    /// The domain stanzas on this stream are for.
    pub fn destination(&self) -> &CanonicalDomain {
        match self.direction {
            StreamDirection::Inbound => &self.local,
            StreamDirection::Outbound => &self.remote,
        }
    }

    /// How the remote domain authenticated. It travels with every stanza
    /// from this stream to the protected-effect boundary.
    pub fn strength(&self) -> AuthenticationStrength {
        self.strength
    }

    /// The trust profile the remote certificate was verified under; `None`
    /// on an inbound dialback stream whose peer presented no certificate.
    pub fn trust_profile(&self) -> Option<TrustProfileKind> {
        self.certificate
            .as_ref()
            .map(|certificate| certificate.profile)
    }

    /// Which presented identifier proved the remote domain; `None` without
    /// a certificate.
    pub fn matched_identifier(&self) -> Option<MatchedIdentifier> {
        self.certificate
            .as_ref()
            .map(|certificate| certificate.identifier)
    }

    /// The TLS version and cipher suite of the connection.
    pub fn tls(&self) -> TlsParameters {
        self.tls
    }

    /// The configuration snapshot the stream was admitted under.
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    /// The SHA-256 digest of the remote end-entity certificate; `None`
    /// without a certificate.
    pub fn certificate_sha256(&self) -> Option<&[u8; 32]> {
        self.certificate
            .as_ref()
            .map(|certificate| &certificate.sha256)
    }

    /// When the certificate evidence stops holding; the stream ends no
    /// later. `None` without a certificate, when only the maximum
    /// connection age bounds the stream.
    pub fn expires_at(&self) -> Option<UnixTime> {
        self.certificate
            .as_ref()
            .map(|certificate| certificate.expires_at)
    }

    /// When the stream was established.
    pub fn established_at(&self) -> Instant {
        self.established_at
    }
}
