//! The per-connection rustls verifier.
//!
//! One [`PeerVerifier`] serves one connection. It owns the connection's
//! reference identity and the trust profile selected for it, implements the
//! rustls verifier traits of both TLS roles with the same policy core, and
//! keeps what the verification established so the connection owner can read
//! it once the handshake completes.
//!
//! The evidence a connection owner acts on is
//! [`PeerVerifier::established`]: it requires the handshake to have
//! completed on the connection and the connection's peer certificate to be
//! the one this verifier verified. [`Verification::Verified`] alone is
//! recorded once the chain and the peer's handshake signature have
//! verified, which is before rustls has checked the peer's Finished
//! message.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{CommonState, DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};
use waddle_xmpp::identity::CanonicalDomain;

use super::error::{TrustError, VerifyError};
use super::identity::ReferenceIdentity;
use super::path::{verify_chain, CertificateRole, VerifiedPeer};
use super::trust::{TrustInventory, TrustProfile, TrustProfileKind};

/// What a [`PeerVerifier`] has established so far.
#[derive(Debug, Clone, PartialEq)]
pub enum Verification {
    /// Nothing is established yet: no certificate has been presented, or
    /// its chain verified and the peer has not yet signed the handshake
    /// with its key.
    Pending,
    /// The peer's certificate chain was verified for the reference identity
    /// and the peer's handshake signature verified under its key. This is
    /// authentication evidence only for a connection whose handshake
    /// completed with this certificate, which [`PeerVerifier::established`]
    /// checks.
    Verified(VerifiedPeer),
    /// A presented certificate chain, or the handshake signature made for
    /// it, was refused. Under ADR-0020 D2 this is an active certificate
    /// failure: the peer is never offered a weaker way to authenticate on
    /// this connection.
    Failed(VerifyError),
}

/// The internal state of a [`PeerVerifier`].
#[derive(Debug)]
enum State {
    Pending,
    /// The chain verified; the handshake signature has not been checked.
    ChainVerified(VerifiedPeer),
    Verified(VerifiedPeer),
    Failed(VerifyError),
}

/// Verifies the certificate of one connection's peer.
///
/// As a [`ServerCertVerifier`] it verifies the receiving server on a stream
/// this deployment initiates; as a [`ClientCertVerifier`] it verifies the
/// initiating server on a stream this deployment receives, and requires a
/// certificate. In both roles the reference identity is the stream's source
/// domain, fixed when the verifier is built: the TLS server name, the socket
/// address and the certificate's own names never replace it.
pub struct PeerVerifier {
    reference: ReferenceIdentity,
    profile: TrustProfile,
    algorithms: WebPkiSupportedAlgorithms,
    /// Set by the first certificate presented. Every later one belongs to
    /// another connection and is refused without touching `state`.
    used: AtomicBool,
    state: Mutex<State>,
}

impl PeerVerifier {
    /// A verifier for the peer that must prove `domain`.
    ///
    /// The trust profile is the one `inventory` holds for exactly that
    /// domain, selected here and never changed afterwards. `algorithms` are
    /// the signature algorithms of the TLS crypto provider.
    pub fn new(
        inventory: &TrustInventory,
        domain: CanonicalDomain,
        algorithms: WebPkiSupportedAlgorithms,
    ) -> Result<Self, TrustError> {
        let profile = inventory.profile_for(&domain);
        Ok(Self {
            reference: ReferenceIdentity::new(domain)?,
            profile,
            algorithms,
            used: AtomicBool::new(false),
            state: Mutex::new(State::Pending),
        })
    }

    /// The identity the peer must prove.
    pub fn reference(&self) -> &ReferenceIdentity {
        &self.reference
    }

    /// The kind of trust profile selected for the reference identity.
    pub fn profile_kind(&self) -> TrustProfileKind {
        self.profile.kind()
    }

    /// What the verifier has established.
    pub fn verification(&self) -> Verification {
        match &*self.state() {
            State::Pending | State::ChainVerified(_) => Verification::Pending,
            State::Verified(peer) => Verification::Verified(peer.clone()),
            State::Failed(failure) => Verification::Failed(failure.clone()),
        }
    }

    /// The evidence for `connection`, the connection this verifier was
    /// configured for, once its TLS handshake has completed.
    ///
    /// It is `Some` only when the handshake is no longer in progress, which
    /// rustls reaches only after it has verified the peer's Finished
    /// message, when the chain and the handshake signature verified, and
    /// when the first certificate the connection received from its peer is
    /// the end-entity certificate this verifier verified. A connection that
    /// failed, is still handshaking, or completed with another certificate
    /// yields `None`.
    pub fn established(&self, connection: &CommonState) -> Option<VerifiedPeer> {
        if connection.is_handshaking() {
            return None;
        }
        let presented = connection.peer_certificates()?.first()?;
        match &*self.state() {
            State::Verified(peer) if peer.certificate_sha256() == &digest(presented) => {
                Some(peer.clone())
            }
            _ => None,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // The guarded value is replaced whole, so a poisoned lock still
        // holds a consistent state.
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn fail(&self, failure: VerifyError) -> rustls::Error {
        *self.state() = State::Failed(failure.clone());
        failure.into_rustls()
    }

    /// Claim the verifier for the connection presenting a certificate now.
    ///
    /// A second certificate means a second connection, whose evidence could
    /// not be told apart from the first. It is refused, and the first
    /// connection's state is left as it is.
    fn claim(&self) -> Result<(), rustls::Error> {
        if self.used.swap(true, Ordering::SeqCst) {
            return Err(VerifyError::VerifierReused.into_rustls());
        }
        Ok(())
    }

    fn verify(
        &self,
        role: CertificateRole,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<(), rustls::Error> {
        match verify_chain(
            &self.reference,
            &self.profile,
            role,
            end_entity,
            intermediates,
            now,
            &self.algorithms,
        ) {
            Ok(peer) => {
                *self.state() = State::ChainVerified(peer);
                Ok(())
            }
            Err(failure) => Err(self.fail(failure)),
        }
    }

    /// Record the outcome of the peer's handshake signature over `cert`.
    ///
    /// rustls asks for the signature only after the certificate verified on
    /// the same connection, so `cert` is the end-entity certificate whose
    /// chain verified. A signature over any other certificate, or one asked
    /// for in any other state, is refused without changing the state, which
    /// may belong to an earlier connection.
    fn verify_signature(
        &self,
        cert: &CertificateDer<'_>,
        verify: impl FnOnce() -> Result<HandshakeSignatureValid, rustls::Error>,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let mut state = self.state();
        let peer = match &*state {
            State::ChainVerified(peer) if peer.certificate_sha256() == &digest(cert) => {
                peer.clone()
            }
            _ => return Err(VerifyError::HandshakeSignature.into_rustls()),
        };
        match verify() {
            Ok(valid) => {
                *state = State::Verified(peer);
                Ok(valid)
            }
            Err(error) => {
                *state = State::Failed(VerifyError::HandshakeSignature);
                Err(error)
            }
        }
    }
}

fn digest(certificate: &CertificateDer<'_>) -> [u8; 32] {
    Sha256::digest(certificate.as_ref()).into()
}

impl fmt::Debug for PeerVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerVerifier")
            .field("profile", &self.profile.kind())
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for PeerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        // Stapled OCSP responses are not requested and not consulted; see
        // the public-chain revocation decision in the verifier design.
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.claim()?;
        // The connection must have been opened for the reference identity.
        // The server name is checked, never used as an identity.
        if !self.reference.is_server_name(server_name) {
            return Err(self.fail(VerifyError::ReferenceMismatch));
        }
        self.verify(CertificateRole::TlsServer, end_entity, intermediates, now)
            .map(|()| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(cert, || {
            verify_tls12_signature(message, cert, dss, &self.algorithms)
        })
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(cert, || {
            verify_tls13_signature(message, cert, dss, &self.algorithms)
        })
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for PeerVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        // No CA names are disclosed before the peer is authenticated.
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.claim()?;
        self.verify(CertificateRole::TlsClient, end_entity, intermediates, now)
            .map(|()| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(cert, || {
            verify_tls12_signature(message, cert, dss, &self.algorithms)
        })
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(cert, || {
            verify_tls13_signature(message, cert, dss, &self.algorithms)
        })
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
