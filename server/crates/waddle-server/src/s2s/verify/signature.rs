//! Signature checks on configured certificates and CRLs.
//!
//! Startup validation verifies signatures with the same algorithms the TLS
//! crypto provider uses for certificate chains, through webpki, so the set
//! accepted at startup is exactly the set a handshake can verify: no
//! algorithm is accepted in one place and not the other.

use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::CertificateDer;
use webpki::EndEntityCert;

use super::der::signed_object;

/// Why a signature on configured material was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SignatureFailure {
    /// The signed object or the signer certificate could not be read.
    Malformed,
    /// No provider algorithm is both named by the signed object and usable
    /// with the signer's key.
    UnsupportedAlgorithm,
    /// The signature does not verify under the signer's key.
    Invalid,
}

/// Verify the signature on `signed_der`, a certificate or CRL, with the
/// public key of `signer`.
pub(super) fn verify_signed_by(
    signer: &CertificateDer<'_>,
    signed_der: &[u8],
    algorithms: &WebPkiSupportedAlgorithms,
) -> Result<(), SignatureFailure> {
    let object = signed_object(signed_der).map_err(|_| SignatureFailure::Malformed)?;
    // The signer is only used as a public key here; whether it may issue is
    // decided by the caller.
    let signer = EndEntityCert::try_from(signer).map_err(|_| SignatureFailure::Malformed)?;
    for algorithm in algorithms
        .all
        .iter()
        .filter(|algorithm| algorithm.signature_alg_id().as_ref() == object.algorithm)
    {
        match signer.verify_signature(*algorithm, object.signed, object.signature) {
            Ok(()) => return Ok(()),
            Err(webpki::Error::InvalidSignatureForPublicKey) => {
                return Err(SignatureFailure::Invalid)
            }
            // The algorithm does not fit the signer's key type; another
            // with the same identifier may.
            Err(_) => {}
        }
    }
    Err(SignatureFailure::UnsupportedAlgorithm)
}
