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

/// How a stream's remote domain was authenticated (ADR-0020 D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthenticationStrength {
    /// TLS certificate validation followed by SASL EXTERNAL.
    Pkix,
    /// XEP-0220 Server Dialback. Not produced by this release: dialback is
    /// disabled, and the value is reserved for the dialback slice.
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

/// An S2S stream whose TLS verification, SASL EXTERNAL and authenticated
/// restart completed.
///
/// Only the connection manager constructs one, from the verifier's
/// evidence for the completed handshake and the negotiated domain pair. It
/// is a domain capability: it names exactly one directional pair of
/// domains, never a local session, account, grant or delivery receipt, and
/// authenticating a domain does not authorize its subdomains or any other
/// domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPeerStream {
    connection: u64,
    direction: StreamDirection,
    local: CanonicalDomain,
    remote: CanonicalDomain,
    strength: AuthenticationStrength,
    profile: TrustProfileKind,
    identifier: MatchedIdentifier,
    tls: TlsParameters,
    generation: Uuid,
    certificate_sha256: [u8; 32],
    expires_at: UnixTime,
    established_at: Instant,
}

impl VerifiedPeerStream {
    pub(super) fn pkix(
        connection: u64,
        direction: StreamDirection,
        local: CanonicalDomain,
        peer: &VerifiedPeer,
        tls: TlsParameters,
        generation: Uuid,
    ) -> Self {
        Self {
            connection,
            direction,
            local,
            remote: peer.domain().clone(),
            strength: AuthenticationStrength::Pkix,
            profile: peer.profile(),
            identifier: peer.matched_identifier(),
            tls,
            generation,
            certificate_sha256: *peer.certificate_sha256(),
            expires_at: peer.expires_at(),
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

    /// The trust profile the remote certificate was verified under.
    pub fn trust_profile(&self) -> TrustProfileKind {
        self.profile
    }

    /// Which presented identifier proved the remote domain.
    pub fn matched_identifier(&self) -> MatchedIdentifier {
        self.identifier
    }

    /// The TLS version and cipher suite of the connection.
    pub fn tls(&self) -> TlsParameters {
        self.tls
    }

    /// The configuration snapshot the stream was admitted under.
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    /// The SHA-256 digest of the remote end-entity certificate.
    pub fn certificate_sha256(&self) -> &[u8; 32] {
        &self.certificate_sha256
    }

    /// When the certificate evidence stops holding; the stream ends no
    /// later.
    pub fn expires_at(&self) -> UnixTime {
        self.expires_at
    }

    /// When the stream was established.
    pub fn established_at(&self) -> Instant {
        self.established_at
    }
}
