//! SPIFFE URI SAN extraction from verified mTLS peer leaf certificates.
//!
//! Per the SPIFFE X.509-SVID specification, a valid workload leaf certificate
//! must contain **exactly one** `spiffe://` URI Subject Alternative Name.

use thiserror::Error;
use waddle_broker_core::identity::{IdentityError, SpiffeId};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpiffeCertError {
    #[error("failed to parse DER X.509 leaf certificate: {0}")]
    InvalidDerCertificate(String),
    #[error("certificate is missing a Subject Alternative Name extension")]
    MissingSanExtension,
    #[error("certificate contains no spiffe:// URI SAN")]
    MissingSpiffeUriSan,
    #[error("certificate contains multiple spiffe:// URI SANs ({0})")]
    MultipleSpiffeUriSans(usize),
    #[error("invalid SPIFFE ID in certificate SAN: {0}")]
    InvalidSpiffeId(#[from] IdentityError),
}

/// Extract the unique `spiffe://` URI SAN from a verified mTLS peer leaf
/// certificate (DER-encoded). Rejects certificates with 0 or >1 SPIFFE URIs.
pub fn extract_spiffe_id_from_der(leaf_cert_der: &[u8]) -> Result<SpiffeId, SpiffeCertError> {
    let (_, cert) = X509Certificate::from_der(leaf_cert_der)
        .map_err(|err| SpiffeCertError::InvalidDerCertificate(err.to_string()))?;

    let san = cert
        .subject_alternative_name()
        .map_err(|err| SpiffeCertError::InvalidDerCertificate(err.to_string()))?
        .ok_or(SpiffeCertError::MissingSanExtension)?;

    let mut spiffe_uris = Vec::new();
    for name in &san.value.general_names {
        if let GeneralName::URI(uri) = name {
            if uri.starts_with("spiffe://") {
                spiffe_uris.push(*uri);
            }
        }
    }

    match spiffe_uris.len() {
        0 => Err(SpiffeCertError::MissingSpiffeUriSan),
        1 => Ok(SpiffeId::parse(spiffe_uris[0])?),
        n => Err(SpiffeCertError::MultipleSpiffeUriSans(n)),
    }
}

/// Check whether a SPIFFE ID is authorized to claim or hold a lease for the
/// given `(cell_role, placement_mode)`.
///
/// - For `PlacementMode::DedicatedCell`: the SPIFFE ID path must end with
///   `/sa/<cell_role>` (e.g., `/sa/dedicated-acme`) and must NEVER be a
///   `shared-pool` ServiceAccount.
/// - For `PlacementMode::SharedPool`: the SPIFFE ID path must end with
///   `/sa/<cell_role>`.
pub fn spiffe_matches_cell_role(
    spiffe_id: &SpiffeId,
    cell_role: &waddle_broker_core::identity::CellRole,
    mode: waddle_broker_core::placement::PlacementMode,
) -> bool {
    let expected_suffix = format!("/sa/{}", cell_role.as_str());
    let path_matches = spiffe_id.as_str().ends_with(&expected_suffix);
    if !path_matches {
        return false;
    }
    match mode {
        waddle_broker_core::placement::PlacementMode::DedicatedCell => {
            !cell_role.as_str().starts_with("shared-pool")
                && !spiffe_id.as_str().contains("/sa/shared-pool")
        }
        waddle_broker_core::placement::PlacementMode::SharedPool => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_broker_core::identity::CellRole;
    use waddle_broker_core::placement::PlacementMode;

    fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if content.len() < 128 {
            out.push(content.len() as u8);
        } else if content.len() < 256 {
            out.push(0x81);
            out.push(content.len() as u8);
        } else {
            out.push(0x82);
            out.push(((content.len() >> 8) & 0xff) as u8);
            out.push((content.len() & 0xff) as u8);
        }
        out.extend_from_slice(content);
        out
    }

    fn build_minimal_x509_with_uris(uris: &[&str]) -> Vec<u8> {
        // Version [0] EXPLICIT INTEGER 2 (v3)
        let version = der_tlv(0xa0, &der_tlv(0x02, &[0x02]));
        // Serial number
        let serial = der_tlv(0x02, &[0x01]);
        // Signature algorithm: sha256WithRSAEncryption (1.2.840.113549.1.1.11)
        let alg_id = der_tlv(
            0x30,
            &[
                0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b, 0x05, 0x00,
            ],
        );
        // Issuer & Subject: empty SEQUENCE
        let empty_name = der_tlv(0x30, &[]);
        // Validity: 20250101000000Z - 20350101000000Z
        let t1 = der_tlv(0x17, b"250101000000Z");
        let t2 = der_tlv(0x17, b"350101000000Z");
        let mut validity_body = Vec::new();
        validity_body.extend_from_slice(&t1);
        validity_body.extend_from_slice(&t2);
        let validity = der_tlv(0x30, &validity_body);
        // SubjectPublicKeyInfo (Ed25519 OID 1.3.101.112 + dummy 32-byte key)
        let spki_alg = der_tlv(0x30, &[0x06, 0x03, 0x2b, 0x65, 0x70]);
        let mut bitstring_body = vec![0x00];
        bitstring_body.extend_from_slice(&[0x42; 32]);
        let spki_key = der_tlv(0x03, &bitstring_body);
        let mut spki_body = Vec::new();
        spki_body.extend_from_slice(&spki_alg);
        spki_body.extend_from_slice(&spki_key);
        let spki = der_tlv(0x30, &spki_body);

        let mut tbs_body = Vec::new();
        tbs_body.extend_from_slice(&version);
        tbs_body.extend_from_slice(&serial);
        tbs_body.extend_from_slice(&alg_id);
        tbs_body.extend_from_slice(&empty_name);
        tbs_body.extend_from_slice(&validity);
        tbs_body.extend_from_slice(&empty_name);
        tbs_body.extend_from_slice(&spki);

        if !uris.is_empty() {
            // SAN OID 2.5.29.17 = 55 1d 11
            let mut san_names = Vec::new();
            for uri in uris {
                // Context-specific tag 6 (uniformResourceIdentifier)
                san_names.extend_from_slice(&der_tlv(0x86, uri.as_bytes()));
            }
            let san_seq = der_tlv(0x30, &san_names);
            let san_octet = der_tlv(0x04, &san_seq);
            let mut ext_body = vec![0x06, 0x03, 0x55, 0x1d, 0x11];
            ext_body.extend_from_slice(&san_octet);
            let ext = der_tlv(0x30, &ext_body);
            let exts_seq = der_tlv(0x30, &ext);
            let exts_explicit = der_tlv(0xa3, &exts_seq);
            tbs_body.extend_from_slice(&exts_explicit);
        }

        let tbs = der_tlv(0x30, &tbs_body);
        let sig_val = der_tlv(0x03, &[0x00, 0xaa, 0xbb]);
        let mut cert_body = Vec::new();
        cert_body.extend_from_slice(&tbs);
        cert_body.extend_from_slice(&alg_id);
        cert_body.extend_from_slice(&sig_val);
        der_tlv(0x30, &cert_body)
    }

    #[test]
    fn extracts_single_spiffe_uri_and_rejects_zero_or_multiple() {
        let valid_der =
            build_minimal_x509_with_uris(&["spiffe://waddle.internal/ns/cells/sa/shared-pool"]);
        let spiffe = extract_spiffe_id_from_der(&valid_der).expect("single SPIFFE URI");
        assert_eq!(
            spiffe.as_str(),
            "spiffe://waddle.internal/ns/cells/sa/shared-pool"
        );

        let missing_san = build_minimal_x509_with_uris(&[]);
        assert_eq!(
            extract_spiffe_id_from_der(&missing_san),
            Err(SpiffeCertError::MissingSanExtension)
        );

        let non_spiffe = build_minimal_x509_with_uris(&["https://example.com/not-spiffe"]);
        assert_eq!(
            extract_spiffe_id_from_der(&non_spiffe),
            Err(SpiffeCertError::MissingSpiffeUriSan)
        );

        let multi = build_minimal_x509_with_uris(&[
            "spiffe://waddle.internal/ns/cells/sa/shared-pool",
            "spiffe://waddle.internal/ns/cells/sa/dedicated-acme",
        ]);
        assert_eq!(
            extract_spiffe_id_from_der(&multi),
            Err(SpiffeCertError::MultipleSpiffeUriSans(2))
        );
    }

    #[test]
    fn enforces_dedicated_cell_policy_against_shared_pool_sa() {
        let shared_sa =
            SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").expect("valid");
        let dedicated_sa =
            SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/dedicated-acme").expect("valid");
        let shared_role = CellRole::parse("shared-pool").expect("valid");
        let dedicated_role = CellRole::parse("dedicated-acme").expect("valid");

        assert!(spiffe_matches_cell_role(
            &shared_sa,
            &shared_role,
            PlacementMode::SharedPool
        ));
        assert!(!spiffe_matches_cell_role(
            &shared_sa,
            &dedicated_role,
            PlacementMode::DedicatedCell
        ));
        assert!(!spiffe_matches_cell_role(
            &shared_sa,
            &shared_role,
            PlacementMode::DedicatedCell
        ));
        assert!(spiffe_matches_cell_role(
            &dedicated_sa,
            &dedicated_role,
            PlacementMode::DedicatedCell
        ));
    }
}
