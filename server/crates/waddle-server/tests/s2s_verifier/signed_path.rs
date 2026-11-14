//! Matrix row "Signed path and TLS ownership": mutual TLS succeeds in both
//! roles, and a wrong CA, a missing certificate, an invalid certificate
//! signature and a forged CertificateVerify each fail the handshake.

use std::sync::Arc;

use rustls::version::{TLS12, TLS13};
use rustls::{CertificateError, SupportedProtocolVersion};
use sha2::{Digest, Sha256};
use waddle_server::s2s::verify::{
    receiving_config, CertificateRole, MatchedIdentifier, TrustProfileKind, Verification,
    VerifyError, TLS_PROTOCOL_VERSIONS,
};
use waddle_xmpp::s2s::TlsPeer;

use crate::harness::{
    anonymous_client_config, attempt, connect, domain, raw_client_config, raw_server_config,
    refused, settings, verified, verifier, Fixture, HOME_A, HOME_B,
};
use crate::pki::{corrupt_signature, unix, Authority, NOW, VALID_UNTIL};

#[test]
fn mutual_tls_authenticates_each_domain_in_its_role() {
    for fixture in Fixture::both() {
        let home_a = fixture.issuer.issue_for(&[HOME_A]);
        let home_b = fixture.issuer.issue_for(&[HOME_B]);

        let stream = attempt(&fixture.inventory, (HOME_A, &home_a), (HOME_B, &home_b));

        assert!(
            stream.handshake.completed(),
            "{:?} profile: client {:?}, server {:?}",
            fixture.kind,
            stream.handshake.client_error,
            stream.handshake.server_error,
        );
        let receiver = verified(stream.of_receiver());
        assert_eq!(receiver.domain(), &domain(HOME_B));
        assert_eq!(receiver.role(), CertificateRole::TlsServer);
        assert_eq!(receiver.profile(), fixture.kind);
        assert_eq!(receiver.matched_identifier(), MatchedIdentifier::DnsId);
        assert_eq!(
            receiver.tls_peer(),
            TlsPeer::Verified(domain(HOME_B).into_domain_part())
        );
        let digest: [u8; 32] = Sha256::digest(home_b.leaf().as_ref()).into();
        assert_eq!(receiver.certificate_sha256(), &digest);
        assert_eq!(receiver.expires_at(), unix(VALID_UNTIL));
        assert!(receiver.is_current(unix(NOW)));

        let initiator = verified(stream.of_initiator());
        assert_eq!(initiator.domain(), &domain(HOME_A));
        assert_eq!(initiator.role(), CertificateRole::TlsClient);
        assert_eq!(initiator.profile(), fixture.kind);
        let digest: [u8; 32] = Sha256::digest(home_a.leaf().as_ref()).into();
        assert_eq!(initiator.certificate_sha256(), &digest);
    }
}

#[test]
fn a_verifier_establishes_nothing_before_a_handshake() {
    let fixture = Fixture::private();
    let pending = verifier(&fixture.inventory, HOME_A, NOW);

    assert_eq!(pending.verification(), Verification::Pending);
    assert_eq!(pending.profile_kind(), TrustProfileKind::PrivateCa);
    assert_eq!(pending.reference().domain(), &domain(HOME_A));
}

#[test]
fn a_certificate_from_an_untrusted_ca_fails_in_both_roles() {
    for fixture in Fixture::both() {
        let outsider = Authority::root("Untrusted Root");

        for failure in fixture.in_both_roles(HOME_A, |_| outsider.issue_for(&[HOME_A])) {
            assert_eq!(
                refused(failure),
                VerifyError::Path(webpki::Error::UnknownIssuer),
                "{:?} profile",
                fixture.kind,
            );
        }
    }
}

#[test]
fn a_rejected_certificate_fails_the_handshake_of_the_endpoint_that_verified_it() {
    let fixture = Fixture::private();
    let outsider = Authority::root("Untrusted Root");
    let good_a = fixture.issuer.issue_for(&[HOME_A]);
    let good_b = fixture.issuer.issue_for(&[HOME_B]);
    let rogue_a = outsider.issue_for(&[HOME_A]);
    let rogue_b = outsider.issue_for(&[HOME_B]);

    let rogue_receiver = attempt(&fixture.inventory, (HOME_A, &good_a), (HOME_B, &rogue_b));
    assert!(!rogue_receiver.handshake.completed());
    assert_eq!(
        rogue_receiver.handshake.client_error,
        Some(rustls::Error::InvalidCertificate(
            CertificateError::UnknownIssuer
        )),
    );

    let rogue_initiator = attempt(&fixture.inventory, (HOME_A, &rogue_a), (HOME_B, &good_b));
    assert!(!rogue_initiator.handshake.completed());
    assert_eq!(
        rogue_initiator.handshake.server_error,
        Some(rustls::Error::InvalidCertificate(
            CertificateError::UnknownIssuer
        )),
    );
    // The initiating endpoint verified the receiving server before its own
    // certificate was refused; the failed handshake still authenticates
    // nobody.
    assert!(rogue_initiator.handshake.client_error.is_some());
}

#[test]
fn an_initiating_server_without_a_certificate_fails_the_handshake() {
    let fixture = Fixture::private();
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let server_config = receiving_config(
        &settings(NOW),
        home_b.certified_key(),
        Arc::clone(&receiving),
    )
    .expect("a receiving configuration");

    let handshake = connect(
        anonymous_client_config(Arc::clone(&initiating)),
        server_config,
        initiating.reference().server_name(),
    );

    assert!(!handshake.completed());
    assert_eq!(
        handshake.server_error,
        Some(rustls::Error::NoCertificatesPresented)
    );
    assert_eq!(receiving.verification(), Verification::Pending);
}

#[test]
fn a_certificate_with_an_invalid_signature_fails_in_both_roles() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let credential = issuer.issue_for(&[HOME_A]);
            let tampered = corrupt_signature(credential.leaf());
            credential.with_leaf(tampered)
        });

        for failure in failures {
            assert_eq!(
                refused(failure),
                VerifyError::Path(webpki::Error::InvalidSignatureForPublicKey),
                "{:?} profile",
                fixture.kind,
            );
        }
    }
}

/// A peer presents a valid certificate chain that belongs to someone else
/// and signs the handshake with its own key.
fn forged_certificate_verify(versions: &'static [&'static SupportedProtocolVersion]) {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let thief = fixture.issuer.issue_for(&["thief.test"]);
    let bad_signature = Some(rustls::Error::InvalidCertificate(
        CertificateError::BadSignature,
    ));

    // The initiating server presents home-a.test's chain without its key.
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let handshake = connect(
        raw_client_config(NOW, versions, Arc::clone(&initiating), &home_a, &thief),
        receiving_config(
            &settings(NOW),
            home_b.certified_key(),
            Arc::clone(&receiving),
        )
        .expect("a receiving configuration"),
        initiating.reference().server_name(),
    );
    assert!(!handshake.completed());
    assert_eq!(handshake.server_error, bad_signature);
    // The chain itself is valid, which is why chain verification alone is
    // never authentication evidence.
    assert!(matches!(
        receiving.verification(),
        Verification::Verified(_)
    ));

    // The receiving server presents home-b.test's chain without its key.
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let handshake = connect(
        raw_client_config(NOW, versions, Arc::clone(&initiating), &home_a, &home_a),
        raw_server_config(NOW, versions, Arc::clone(&receiving), &home_b, &thief),
        initiating.reference().server_name(),
    );
    assert!(!handshake.completed());
    assert_eq!(handshake.client_error, bad_signature);
}

#[test]
fn a_forged_tls13_certificate_verify_fails_in_both_roles() {
    static TLS13_ONLY: &[&SupportedProtocolVersion] = &[&TLS13];
    forged_certificate_verify(TLS13_ONLY);
}

#[test]
fn a_forged_tls12_handshake_signature_fails_in_both_roles() {
    static TLS12_ONLY: &[&SupportedProtocolVersion] = &[&TLS12];
    forged_certificate_verify(TLS12_ONLY);
}

#[test]
fn a_verifier_refuses_a_second_connection() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let first = attempt(&fixture.inventory, (HOME_A, &home_a), (HOME_B, &home_b));
    assert!(first.handshake.completed());

    // A second connection that reuses the first connection's verifier.
    let second_initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let handshake = connect(
        raw_client_config(
            NOW,
            TLS_PROTOCOL_VERSIONS,
            Arc::clone(&second_initiating),
            &home_a,
            &home_a,
        ),
        raw_server_config(
            NOW,
            TLS_PROTOCOL_VERSIONS,
            Arc::clone(&first.receiving),
            &home_b,
            &home_b,
        ),
        second_initiating.reference().server_name(),
    );

    assert!(!handshake.completed());
    assert_eq!(
        refused(first.receiving.verification()),
        VerifyError::VerifierReused
    );
}

#[test]
fn a_connection_opened_for_another_name_is_refused() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B, "elsewhere.test"]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let elsewhere = verifier(&fixture.inventory, "elsewhere.test", NOW);

    // The verifier's reference identity is home-b.test, but the connection
    // is opened with another server name.
    let handshake = connect(
        raw_client_config(
            NOW,
            TLS_PROTOCOL_VERSIONS,
            Arc::clone(&initiating),
            &home_a,
            &home_a,
        ),
        raw_server_config(
            NOW,
            TLS_PROTOCOL_VERSIONS,
            Arc::clone(&receiving),
            &home_b,
            &home_b,
        ),
        elsewhere.reference().server_name(),
    );

    assert!(!handshake.completed());
    assert_eq!(
        refused(initiating.verification()),
        VerifyError::ReferenceMismatch
    );
}
