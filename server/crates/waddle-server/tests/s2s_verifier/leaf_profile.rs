//! Matrix row "Roles/profile": a missing or wrong extended key usage, a
//! missing digitalSignature usage, a CA certificate used as a leaf, invalid
//! issuer usages, an unknown critical extension and trailing DER each fail,
//! and a serverAuth-only leaf authenticates in both roles. A clientAuth-only
//! leaf or issuer authenticates an initiating server on a private-CA profile
//! only.

use rcgen::{BasicConstraints, ExtendedKeyUsagePurpose, IsCa, KeyUsagePurpose};
use rustls::pki_types::CertificateDer;
use waddle_server::s2s::verify::{
    CertificateRole, PrivateCaMaterial, PrivateCaProfile, TrustErrorKind, TrustItem,
    TrustProfileKind, VerifyError,
};

use crate::harness::{
    attempt, private_inventory, private_profile, public_inventory, refused, settings, verified,
    Fixture, HOME_A, HOME_B, PEER,
};
use crate::pki::{
    ca_params, leaf_params, unix, unknown_critical_extension, Authority, Credential, NOW,
};

#[test]
fn a_server_auth_only_leaf_authenticates_in_both_roles_in_every_profile() {
    for fixture in Fixture::both() {
        let [as_receiver, as_initiator] = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            issuer.issue(params)
        });

        assert_eq!(verified(as_receiver).role(), CertificateRole::TlsServer);
        assert_eq!(verified(as_initiator).role(), CertificateRole::TlsClient);
    }
}

fn client_auth_only_leaf(issuer: &Authority) -> Credential {
    let mut params = leaf_params(&[HOME_A]);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    issuer.issue(params)
}

#[test]
fn a_client_auth_only_leaf_is_refused_in_both_roles_on_the_public_profile() {
    let [as_receiver, as_initiator] =
        Fixture::public().in_both_roles(HOME_A, client_auth_only_leaf);

    assert_eq!(refused(as_receiver), VerifyError::ExtendedKeyUsage);
    assert_eq!(refused(as_initiator), VerifyError::ExtendedKeyUsage);
}

#[test]
fn a_client_auth_only_leaf_authenticates_an_initiating_server_on_a_private_ca_profile() {
    let fixture = Fixture::private();
    let peer = fixture.issuer.issue_for(&[PEER]);

    let as_receiver = attempt(
        &fixture.inventory,
        (PEER, &peer),
        (HOME_A, &client_auth_only_leaf(&fixture.issuer)),
    );
    assert_eq!(
        refused(as_receiver.of_receiver()),
        VerifyError::ExtendedKeyUsage
    );

    let as_initiator = attempt(
        &fixture.inventory,
        (HOME_A, &client_auth_only_leaf(&fixture.issuer)),
        (PEER, &peer),
    );
    let initiator = as_initiator
        .initiator_established()
        .expect("a completed handshake");
    assert_eq!(initiator.role(), CertificateRole::TlsClient);
    assert_eq!(initiator.profile(), TrustProfileKind::PrivateCa);
}

#[test]
fn a_leaf_without_an_extended_key_usage_extension_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params.extended_key_usages = Vec::new();
            issuer.issue(params)
        });

        for failure in failures {
            assert_eq!(refused(failure), VerifyError::MissingExtendedKeyUsage);
        }
    }
}

#[test]
fn a_leaf_with_an_unrelated_extended_key_usage_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params.extended_key_usages = vec![
                ExtendedKeyUsagePurpose::EmailProtection,
                ExtendedKeyUsagePurpose::Any,
            ];
            issuer.issue(params)
        });

        for failure in failures {
            assert_eq!(refused(failure), VerifyError::ExtendedKeyUsage);
        }
    }
}

#[test]
fn a_leaf_that_may_not_sign_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params.key_usages = vec![KeyUsagePurpose::KeyEncipherment];
            issuer.issue(params)
        });

        for failure in failures {
            assert_eq!(refused(failure), VerifyError::KeyUsage);
        }
    }
}

#[test]
fn a_leaf_without_a_key_usage_extension_is_refused_on_the_private_ca_profile_only() {
    let without_key_usage = |issuer: &Authority| {
        let mut params = leaf_params(&[HOME_A]);
        params.key_usages = Vec::new();
        issuer.issue(params)
    };

    for failure in Fixture::private().in_both_roles(HOME_A, without_key_usage) {
        assert_eq!(refused(failure), VerifyError::MissingKeyUsage);
    }
    for accepted in Fixture::public().in_both_roles(HOME_A, without_key_usage) {
        assert_eq!(verified(accepted).profile(), TrustProfileKind::PublicWebPki);
    }
}

#[test]
fn a_ca_certificate_presented_as_a_leaf_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            issuer.issue(params)
        });

        for failure in failures {
            assert_eq!(refused(failure), VerifyError::LeafIsCa);
        }
    }
}

#[test]
fn a_leaf_with_an_unknown_critical_extension_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params.custom_extensions.push(unknown_critical_extension());
            issuer.issue(params)
        });

        for failure in failures {
            assert_eq!(
                refused(failure),
                VerifyError::Path(webpki::Error::UnsupportedCriticalExtension)
            );
        }
    }
}

#[test]
fn a_leaf_followed_by_trailing_bytes_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let credential = issuer.issue_for(&[HOME_A]);
            let mut der = credential.leaf().to_vec();
            der.push(0x00);
            credential.with_leaf(CertificateDer::from(der))
        });

        for failure in failures {
            assert_eq!(refused(failure), VerifyError::MalformedCertificate);
        }
    }
}

/// An intermediate CA that meets the issuer profile except for
/// `key_usages`.
fn intermediate_with_usages(root: &Authority, key_usages: Vec<KeyUsagePurpose>) -> Authority {
    let mut params = ca_params("Misissued Intermediate");
    params.key_usages = key_usages;
    root.intermediate_with(params)
}

/// An intermediate CA that meets the issuer profile and restricts its
/// extended key usage to `usages`.
fn intermediate_with_extended_usages(
    root: &Authority,
    usages: Vec<ExtendedKeyUsagePurpose>,
) -> Authority {
    let mut params = ca_params("Restricted Intermediate");
    params.extended_key_usages = usages;
    root.intermediate_with(params)
}

#[test]
fn a_public_path_through_an_intermediate_without_server_auth_is_refused_in_both_roles() {
    let restrictions = [
        vec![ExtendedKeyUsagePurpose::ClientAuth],
        vec![ExtendedKeyUsagePurpose::Any],
        vec![
            ExtendedKeyUsagePurpose::EmailProtection,
            ExtendedKeyUsagePurpose::Any,
        ],
    ];
    for usages in restrictions {
        let root = Authority::root("Substituted Public Root");
        let inventory = public_inventory(&[&root]);
        let intermediate = intermediate_with_extended_usages(&root, usages);
        let peer = root.issue_for(&[PEER]);
        let home_a = intermediate
            .issue_for(&[HOME_A])
            .presenting(&[intermediate.der()]);

        let as_receiver = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
        assert!(matches!(
            refused(as_receiver.of_receiver()),
            VerifyError::Path(webpki::Error::RequiredEkuNotFoundContext(_))
        ));
        let as_initiator = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
        assert!(matches!(
            refused(as_initiator.of_initiator()),
            VerifyError::Path(webpki::Error::RequiredEkuNotFoundContext(_))
        ));
    }
}

#[test]
fn a_public_path_through_a_server_auth_intermediate_is_accepted_in_both_roles() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let intermediate =
        intermediate_with_extended_usages(&root, vec![ExtendedKeyUsagePurpose::ServerAuth]);
    let home_a = intermediate
        .issue_for(&[HOME_A])
        .presenting(&[intermediate.der()]);
    let home_b = intermediate
        .issue_for(&[HOME_B])
        .presenting(&[intermediate.der()]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert!(stream.receiver_established().is_some());
    assert!(stream.initiator_established().is_some());
}

#[test]
fn a_client_auth_only_private_intermediate_issues_for_initiating_servers_only() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let intermediate =
        intermediate_with_extended_usages(&root, vec![ExtendedKeyUsagePurpose::ClientAuth]);
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&root], &[&intermediate]),
        &[HOME_A, PEER],
    );
    let peer = root.issue_for(&[PEER]);
    let home_a = intermediate.issue_for(&[HOME_A]);

    let as_initiator = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert_eq!(
        as_initiator
            .initiator_established()
            .expect("a completed handshake")
            .role(),
        CertificateRole::TlsClient
    );
    let as_receiver = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
    assert!(matches!(
        refused(as_receiver.of_receiver()),
        VerifyError::Path(webpki::Error::RequiredEkuNotFoundContext(_))
    ));
}

#[test]
fn a_private_ca_anchor_whose_extended_key_usage_lacks_server_auth_is_refused_at_startup() {
    let refused_usages = [
        vec![ExtendedKeyUsagePurpose::ClientAuth],
        vec![ExtendedKeyUsagePurpose::Any],
        vec![ExtendedKeyUsagePurpose::EmailProtection],
    ];
    for usages in refused_usages {
        let mut params = ca_params("Restricted Private Root");
        params.extended_key_usages = usages;
        let root = Authority::root_with(params);
        let material = PrivateCaMaterial {
            anchors: vec![root.der()],
            intermediates: Vec::new(),
            crls: vec![root.clean_crl()],
        };

        let error = PrivateCaProfile::load(&material, &settings(NOW).algorithms(), unix(NOW))
            .expect_err("an anchor that does not admit both roles");

        assert_eq!(error.kind(), TrustErrorKind::AnchorExtendedKeyUsage);
        assert_eq!(error.item(), Some(TrustItem::Anchor(0)));
    }

    // An anchor whose extended key usage includes serverAuth admits both
    // roles, and so does one without the extension.
    let mut params = ca_params("Server Auth Private Root");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let root = Authority::root_with(params);
    let public = Authority::root("Unrelated Public Root");
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&root], &[]),
        &[HOME_A, HOME_B],
    );
    let stream = attempt(
        &inventory,
        (HOME_A, &root.issue_for(&[HOME_A])),
        (HOME_B, &root.issue_for(&[HOME_B])),
    );
    assert!(stream.receiver_established().is_some());
    assert!(stream.initiator_established().is_some());
}

#[test]
fn a_public_path_through_an_issuer_that_may_not_sign_certificates_is_refused() {
    let usages = [
        // keyUsage present without keyCertSign.
        vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::CrlSign],
        // No keyUsage extension at all.
        Vec::new(),
    ];
    for key_usages in usages {
        let root = Authority::root("Substituted Public Root");
        let inventory = public_inventory(&[&root]);
        let intermediate = intermediate_with_usages(&root, key_usages);
        let peer = root.issue_for(&[PEER]);
        let home_a = intermediate
            .issue_for(&[HOME_A])
            .presenting(&[intermediate.der()]);

        let as_receiver = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
        assert_eq!(refused(as_receiver.of_receiver()), VerifyError::IssuerUsage);
        let as_initiator = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
        assert_eq!(
            refused(as_initiator.of_initiator()),
            VerifyError::IssuerUsage
        );
    }
}

#[test]
fn a_public_path_through_a_conforming_intermediate_is_accepted() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let intermediate = root.intermediate("Public Intermediate");
    let home_a = intermediate
        .issue_for(&[HOME_A])
        .presenting(&[intermediate.der()]);
    let home_b = intermediate
        .issue_for(&[HOME_B])
        .presenting(&[intermediate.der()]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert!(stream.handshake.completed());
    assert_eq!(
        verified(stream.of_receiver()).role(),
        CertificateRole::TlsServer
    );
    assert_eq!(
        verified(stream.of_initiator()).role(),
        CertificateRole::TlsClient
    );
}

#[test]
fn a_private_ca_profile_refuses_an_issuer_that_may_not_sign_at_startup() {
    let cases = [
        (
            vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::CrlSign],
            TrustErrorKind::CertSignUsage,
        ),
        (
            vec![
                KeyUsagePurpose::DigitalSignature,
                KeyUsagePurpose::KeyCertSign,
            ],
            TrustErrorKind::CrlSignUsage,
        ),
        (Vec::new(), TrustErrorKind::MissingKeyUsage),
    ];
    for (key_usages, expected) in cases {
        let root = Authority::root("Private Test Root");
        let intermediate = intermediate_with_usages(&root, key_usages);
        let material = PrivateCaMaterial {
            anchors: vec![root.der()],
            intermediates: vec![intermediate.der()],
            crls: vec![root.clean_crl()],
        };

        let error = PrivateCaProfile::load(&material, &settings(NOW).algorithms(), unix(NOW))
            .expect_err("an issuer outside the profile");

        assert_eq!(error.kind(), expected);
        assert_eq!(error.item(), Some(TrustItem::Intermediate(0)));
    }
}

#[test]
fn a_private_ca_profile_builds_paths_from_its_own_intermediates() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let intermediate = root.intermediate("Private Intermediate");
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&root], &[&intermediate]),
        &[HOME_A, HOME_B],
    );
    // One endpoint sends only its leaf, the other also sends the
    // intermediate; the profile supplies the issuer either way.
    let home_a = intermediate.issue_for(&[HOME_A]);
    let home_b = intermediate
        .issue_for(&[HOME_B])
        .presenting(&[intermediate.der()]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert!(stream.handshake.completed());
    assert_eq!(
        verified(stream.of_receiver()).profile(),
        TrustProfileKind::PrivateCa
    );
    assert_eq!(
        verified(stream.of_initiator()).profile(),
        TrustProfileKind::PrivateCa
    );
}
