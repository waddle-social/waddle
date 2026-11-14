//! Matrix row "Root/inventory": an expired, not-yet-valid, non-CA or badly
//! self-signed anchor, a duplicate subject with a different key, an unknown
//! intermediate, a cycle, an unsupported algorithm and an invalid inventory
//! path each fail the construction of a private-CA profile.

use rcgen::{BasicConstraints, IsCa, KeyUsagePurpose};
use rustls::crypto::ring::default_provider;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, CertificateRevocationListDer};
use waddle_server::s2s::verify::limits::MAX_PROFILE_CA_CERTIFICATES;
use waddle_server::s2s::verify::{
    PrivateCaMaterial, PrivateCaProfile, TrustError, TrustErrorKind, TrustItem,
};

use crate::harness::settings;
use crate::pki::{
    ca_params, corrupt_signature, unix, unknown_critical_extension, valid_between, Authority, Day,
    CA_FROM, CA_UNTIL, CRL_NEXT_UPDATE, CRL_THIS_UPDATE, NOW,
};

fn load(material: &PrivateCaMaterial) -> Result<PrivateCaProfile, TrustError> {
    PrivateCaProfile::load(material, &settings(NOW).algorithms(), unix(NOW))
}

fn material(
    anchors: Vec<CertificateDer<'static>>,
    intermediates: Vec<CertificateDer<'static>>,
    crls: Vec<CertificateRevocationListDer<'static>>,
) -> PrivateCaMaterial {
    PrivateCaMaterial {
        anchors,
        intermediates,
        crls,
    }
}

/// Assert that the material fails to load with `kind` at `item`.
fn assert_refused(material: &PrivateCaMaterial, kind: TrustErrorKind, item: Option<TrustItem>) {
    let error = load(material).expect_err("invalid trust material");
    assert_eq!(error.kind(), kind, "{error}");
    assert_eq!(error.item(), item, "{error}");
}

#[test]
fn a_root_an_intermediate_and_their_crls_load() {
    let root = Authority::root("Private Test Root");
    let intermediate = root.intermediate("Private Intermediate");

    let profile = load(&material(
        vec![root.der()],
        vec![intermediate.der()],
        vec![intermediate.clean_crl(), root.clean_crl()],
    ))
    .expect("a valid private-CA profile");

    assert_eq!(profile.anchor_count(), 1);
    assert_eq!(profile.intermediate_count(), 1);
    assert_eq!(profile.crl_count(), 2);
    // The profile is current from the CRLs' thisUpdate to their nextUpdate,
    // which lies inside the CA certificates' validity.
    assert_eq!(profile.expires_at(), unix(CRL_NEXT_UPDATE));
    assert!(profile.is_current(unix(CRL_THIS_UPDATE)));
    assert!(profile.is_current(unix(NOW)));
    assert!(!profile.is_current(unix(CRL_NEXT_UPDATE)));
    assert!(!profile.is_current(unix(Day(2030, 6, 11))));
}

#[test]
fn material_without_a_trust_anchor_is_refused() {
    let root = Authority::root("Private Test Root");

    assert_refused(
        &material(Vec::new(), vec![root.der()], vec![root.clean_crl()]),
        TrustErrorKind::NoTrustAnchors,
        None,
    );
}

#[test]
fn an_expired_anchor_is_refused() {
    let mut params = ca_params("Expired Root");
    valid_between(&mut params, Day(2029, 1, 1), Day(2030, 6, 1));
    let root = Authority::root_with(params);

    assert_refused(
        &material(vec![root.der()], Vec::new(), vec![root.clean_crl()]),
        TrustErrorKind::Expired,
        Some(TrustItem::Anchor(0)),
    );
}

#[test]
fn an_anchor_that_is_not_valid_yet_is_refused() {
    let mut params = ca_params("Future Root");
    valid_between(&mut params, Day(2030, 7, 1), Day(2031, 7, 1));
    let root = Authority::root_with(params);

    assert_refused(
        &material(vec![root.der()], Vec::new(), vec![root.clean_crl()]),
        TrustErrorKind::NotYetValid,
        Some(TrustItem::Anchor(0)),
    );
}

#[test]
fn an_anchor_that_is_not_a_ca_is_refused() {
    for is_ca in [IsCa::NoCa, IsCa::ExplicitNoCa] {
        let mut params = ca_params("Not A CA");
        params.is_ca = is_ca;
        let root = Authority::root_with(params);

        assert_refused(
            &material(vec![root.der()], Vec::new(), vec![root.clean_crl()]),
            TrustErrorKind::NotCa,
            Some(TrustItem::Anchor(0)),
        );
    }
}

#[test]
fn an_anchor_with_a_bad_self_signature_is_refused() {
    let root = Authority::root("Private Test Root");

    assert_refused(
        &material(
            vec![corrupt_signature(&root.der())],
            Vec::new(),
            vec![root.clean_crl()],
        ),
        TrustErrorKind::BadSignature,
        Some(TrustItem::Anchor(0)),
    );
}

#[test]
fn an_anchor_that_is_not_self_issued_is_refused() {
    let root = Authority::root("Private Test Root");
    let intermediate = root.intermediate("Private Intermediate");

    assert_refused(
        &material(
            vec![root.der(), intermediate.der()],
            Vec::new(),
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::AnchorNotSelfIssued,
        Some(TrustItem::Anchor(1)),
    );
}

#[test]
fn two_cas_with_one_subject_and_different_keys_are_refused() {
    let root = Authority::root("Twin Root");
    let twin = Authority::root("Twin Root");

    assert_refused(
        &material(
            vec![root.der(), twin.der()],
            Vec::new(),
            vec![root.clean_crl()],
        ),
        TrustErrorKind::DuplicateSubject,
        Some(TrustItem::Anchor(1)),
    );

    // The same holds for a rotated intermediate that keeps its name.
    let old = root.intermediate("Rotated Intermediate");
    let new = root.intermediate("Rotated Intermediate");
    assert_refused(
        &material(
            vec![root.der()],
            vec![old.der(), new.der()],
            vec![root.clean_crl()],
        ),
        TrustErrorKind::DuplicateSubject,
        Some(TrustItem::Intermediate(1)),
    );
}

#[test]
fn an_intermediate_whose_issuer_is_not_configured_is_refused() {
    let root = Authority::root("Private Test Root");
    let stranger = Authority::root("Unconfigured Root");
    let intermediate = stranger.intermediate("Orphan Intermediate");

    assert_refused(
        &material(
            vec![root.der()],
            vec![intermediate.der()],
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::UnknownIssuer,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn intermediates_that_only_issue_each_other_are_refused() {
    let root = Authority::root("Private Test Root");
    let first = Authority::root("Cycle First");
    let second = Authority::root("Cycle Second");
    // Each is certified by the other and neither by the root.
    let first_by_second = second.cross_sign(&first);
    let second_by_first = first.cross_sign(&second);

    assert_refused(
        &material(
            vec![root.der()],
            vec![first_by_second, second_by_first],
            vec![root.clean_crl()],
        ),
        TrustErrorKind::IssuerCycle,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn a_self_signed_certificate_configured_as_an_intermediate_is_refused() {
    let root = Authority::root("Private Test Root");
    let other = Authority::root("Second Root");

    assert_refused(
        &material(
            vec![root.der()],
            vec![other.der()],
            vec![root.clean_crl(), other.clean_crl()],
        ),
        TrustErrorKind::IssuerCycle,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn material_signed_with_an_algorithm_the_provider_does_not_verify_is_refused() {
    // The provider's algorithms without ECDSA: the test CAs sign with
    // ECDSA P-256, which this set cannot verify.
    const ECDSA_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    let provider = default_provider().signature_verification_algorithms;
    let without_ecdsa: Vec<_> = provider
        .all
        .iter()
        .copied()
        .filter(|algorithm| !algorithm.public_key_alg_id().starts_with(ECDSA_PUBLIC_KEY))
        .collect();
    assert!(!without_ecdsa.is_empty());
    assert!(without_ecdsa.len() < provider.all.len());
    let restricted = WebPkiSupportedAlgorithms {
        all: Box::leak(without_ecdsa.into_boxed_slice()),
        mapping: &[],
    };
    let root = Authority::root("Private Test Root");
    let material = material(vec![root.der()], Vec::new(), vec![root.clean_crl()]);

    let error = PrivateCaProfile::load(&material, &restricted, unix(NOW))
        .expect_err("an algorithm outside the provider's set");

    assert_eq!(error.kind(), TrustErrorKind::UnsupportedSignatureAlgorithm);
    assert_eq!(error.item(), Some(TrustItem::Anchor(0)));
    // The same material loads with the provider's own set.
    load(&material).expect("the provider verifies ECDSA P-256");
}

#[test]
fn an_intermediate_with_a_bad_signature_is_refused() {
    let root = Authority::root("Private Test Root");
    let intermediate = root.intermediate("Private Intermediate");

    assert_refused(
        &material(
            vec![root.der()],
            vec![corrupt_signature(&intermediate.der())],
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::BadSignature,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn an_intermediate_signed_by_another_key_than_its_issuers_is_refused() {
    let root = Authority::root("Private Test Root");
    // A different CA with the configured root's name issues the
    // intermediate.
    let impostor = Authority::root("Private Test Root");
    let intermediate = impostor.intermediate("Private Intermediate");

    assert_refused(
        &material(
            vec![root.der()],
            vec![intermediate.der()],
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::KeyIdentifierMismatch,
        Some(TrustItem::Intermediate(0)),
    );

    // Without an authority key identifier to compare, the signature check
    // refuses it.
    let mut params = ca_params("Private Intermediate");
    params.use_authority_key_identifier_extension = false;
    let intermediate = impostor.intermediate_with(params);
    assert_refused(
        &material(
            vec![root.der()],
            vec![intermediate.der()],
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::BadSignature,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn an_intermediate_beyond_a_path_length_constraint_is_refused() {
    let mut params = ca_params("Private Test Root");
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    let root = Authority::root_with(params);
    let intermediate = root.intermediate("Private Intermediate");

    assert_refused(
        &material(
            vec![root.der()],
            vec![intermediate.der()],
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::PathLength,
        Some(TrustItem::Intermediate(0)),
    );

    // One level is allowed by a constraint of one; a second is not.
    let mut params = ca_params("Private Test Root");
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(1));
    let root = Authority::root_with(params);
    let first = root.intermediate("First Intermediate");
    let second = first.intermediate("Second Intermediate");
    load(&material(
        vec![root.der()],
        vec![first.der()],
        vec![root.clean_crl(), first.clean_crl()],
    ))
    .expect("one intermediate under a path length of one");
    assert_refused(
        &material(
            vec![root.der()],
            vec![first.der(), second.der()],
            vec![root.clean_crl(), first.clean_crl(), second.clean_crl()],
        ),
        TrustErrorKind::PathLength,
        Some(TrustItem::Intermediate(1)),
    );
}

#[test]
fn an_expired_intermediate_is_refused() {
    let root = Authority::root("Private Test Root");
    let mut params = ca_params("Expired Intermediate");
    valid_between(&mut params, CA_FROM, Day(2030, 6, 10));
    let intermediate = root.intermediate_with(params);

    assert_refused(
        &material(
            vec![root.der()],
            vec![intermediate.der()],
            vec![root.clean_crl(), intermediate.clean_crl()],
        ),
        TrustErrorKind::Expired,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn a_ca_with_an_unknown_critical_extension_is_refused() {
    let mut params = ca_params("Private Test Root");
    params.custom_extensions.push(unknown_critical_extension());
    let root = Authority::root_with(params);

    assert_refused(
        &material(vec![root.der()], Vec::new(), vec![root.clean_crl()]),
        TrustErrorKind::UnsupportedCriticalExtension,
        Some(TrustItem::Anchor(0)),
    );
}

#[test]
fn a_ca_that_may_not_sign_crls_is_refused() {
    let mut params = ca_params("Private Test Root");
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let root = Authority::root_with(params);

    assert_refused(
        &material(vec![root.der()], Vec::new(), vec![root.clean_crl()]),
        TrustErrorKind::CrlSignUsage,
        Some(TrustItem::Anchor(0)),
    );
}

#[test]
fn a_malformed_ca_certificate_is_refused() {
    let root = Authority::root("Private Test Root");
    let mut trailing = root.der().to_vec();
    trailing.push(0x00);
    let truncated = root.der()[..root.der().len() - 1].to_vec();

    for der in [trailing, truncated, Vec::new()] {
        assert_refused(
            &material(
                vec![CertificateDer::from(der)],
                Vec::new(),
                vec![root.clean_crl()],
            ),
            TrustErrorKind::MalformedCertificate,
            Some(TrustItem::Anchor(0)),
        );
    }
}

#[test]
fn more_ca_certificates_than_the_ceiling_are_refused() {
    let roots: Vec<Authority> = (0..=MAX_PROFILE_CA_CERTIFICATES)
        .map(|index| Authority::root(&format!("Root {index}")))
        .collect();
    let crls = roots.iter().map(|root| root.clean_crl()).collect();

    assert_refused(
        &material(
            roots.iter().map(|root| root.der()).collect(),
            Vec::new(),
            crls,
        ),
        TrustErrorKind::TooManyCaCertificates,
        None,
    );
}

#[test]
fn the_ca_validity_window_is_part_of_the_profile() {
    // The anchor expires before the CRLs do, so it bounds the profile.
    let mut params = ca_params("Short Lived Root");
    valid_between(&mut params, CA_FROM, Day(2030, 6, 17));
    let root = Authority::root_with(params);

    let profile = load(&material(
        vec![root.der()],
        Vec::new(),
        vec![root.clean_crl()],
    ))
    .expect("a profile that is valid today");

    assert_eq!(profile.expires_at(), unix(Day(2030, 6, 17)));
    assert!(profile.expires_at() < unix(CRL_NEXT_UPDATE));
    assert!(profile.expires_at() < unix(CA_UNTIL));
    assert!(profile.is_current(unix(NOW)));
    assert!(!profile.is_current(unix(Day(2030, 6, 17))));
}
