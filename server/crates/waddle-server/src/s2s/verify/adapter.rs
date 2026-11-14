//! The per-connection rustls verifier.
//!
//! One [`PeerVerifier`] serves one connection. It owns the connection's
//! reference identity and the trust profile selected for it, implements the
//! rustls verifier traits of both TLS roles with the same policy core, and
//! keeps what the verification established so the connection owner can read
//! it once the handshake completes.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use waddle_xmpp::identity::CanonicalDomain;

use super::error::{TrustError, VerifyError};
use super::identity::ReferenceIdentity;
use super::path::{verify_chain, CertificateRole, VerifiedPeer};
use super::trust::{TrustInventory, TrustProfile, TrustProfileKind};

/// What a [`PeerVerifier`] has established so far.
#[derive(Debug, Clone, PartialEq)]
pub enum Verification {
    /// No certificate has been presented to the verifier.
    Pending,
    /// The peer's certificate chain was verified for the reference
    /// identity. This becomes authentication evidence only when the TLS
    /// handshake that carried it also completes, which is when rustls has
    /// verified the peer's handshake signature.
    Verified(VerifiedPeer),
    /// A presented certificate chain was refused. Under ADR-0020 D2 this is
    /// an active certificate failure: the peer is never offered a weaker
    /// way to authenticate on this connection.
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
    used: AtomicBool,
    verification: Mutex<Verification>,
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
            verification: Mutex::new(Verification::Pending),
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
        self.state().clone()
    }

    fn state(&self) -> MutexGuard<'_, Verification> {
        // The guarded value is replaced whole, so a poisoned lock still
        // holds a consistent state.
        match self.verification.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn fail(&self, failure: VerifyError) -> rustls::Error {
        *self.state() = Verification::Failed(failure.clone());
        failure.into_rustls()
    }

    fn verify(
        &self,
        role: CertificateRole,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<(), rustls::Error> {
        // A second certificate means a second connection, whose evidence
        // could not be told apart from the first.
        if self.used.swap(true, Ordering::SeqCst) {
            return Err(self.fail(VerifyError::VerifierReused));
        }
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
                *self.state() = Verification::Verified(peer);
                Ok(())
            }
            Err(failure) => Err(self.fail(failure)),
        }
    }
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
        // The connection must have been opened for the reference identity.
        // The server name is checked, never used as an identity.
        if !self.reference.is_server_name(server_name) {
            self.used.store(true, Ordering::SeqCst);
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
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
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
        self.verify(CertificateRole::TlsClient, end_entity, intermediates, now)
            .map(|()| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
