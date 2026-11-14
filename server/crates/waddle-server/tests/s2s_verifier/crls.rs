//! Matrix row "CRLs": an empty or missing bundle, an unknown issuer, a bad
//! signature, missing, future, expired or reversed timestamps, missing
//! chain coverage, a revoked leaf or intermediate, duplicate clean and
//! revoking CRLs in both orders, delta, indirect and distribution-point
//! variants and unsupported critical extensions each fail closed; valid
//! complete chain coverage passes.

use rustls::pki_types::{CertificateDer, CertificateRevocationListDer};
use waddle_server::s2s::verify::limits::{MAX_CRL_ENTRIES, MAX_PROFILE_CRLS};
use waddle_server::s2s::verify::{
    PrivateCaMaterial, PrivateCaProfile, TrustError, TrustErrorKind, TrustItem, VerifyError,
};

use crate::harness::{
    attempt, private_inventory, refused, settings, verified, HOME_A, HOME_B, PEER,
};
use crate::pki::{
    corrupt_crl_signature, delta_crl_indicator, indirect_distribution_point, leaf_params,
    oversized_extension, unix, unknown_critical_crl_extension,
    user_certificates_distribution_point, valid_between, Authority, CrlSpec, Day, NOW, VALID_FROM,
    VALID_UNTIL,
};

fn load(material: &PrivateCaMaterial) -> Result<PrivateCaProfile, TrustError> {
    PrivateCaProfile::load(material, &settings(NOW).algorithms(), unix(NOW))
}

/// A profile with `root` as its only CA and `crls` as its bundle.
fn with_crls(
    root: &Authority,
    crls: Vec<CertificateRevocationListDer<'static>>,
) -> PrivateCaMaterial {
    PrivateCaMaterial {
        anchors: vec![root.der()],
        intermediates: Vec::new(),
        crls,
    }
}

fn assert_refused(material: &PrivateCaMaterial, kind: TrustErrorKind, item: Option<TrustItem>) {
    let error = load(material).expect_err("an invalid CRL bundle");
    assert_eq!(error.kind(), kind, "{error}");
    assert_eq!(error.item(), item, "{error}");
}

/// Assert that the root's CRL described by `spec` is refused with `kind`.
fn assert_crl_refused(spec: &CrlSpec, kind: TrustErrorKind) {
    let root = Authority::root("Private Test Root");
    assert_refused(
        &with_crls(&root, vec![root.crl(spec)]),
        kind,
        Some(TrustItem::Crl(0)),
    );
}

#[test]
fn an_empty_bundle_is_refused() {
    let root = Authority::root("Private Test Root");

    assert_refused(&with_crls(&root, Vec::new()), TrustErrorKind::NoCrls, None);
}

#[test]
fn a_ca_without_a_crl_is_refused() {
    let root = Authority::root("Private Test Root");
    let intermediate = root.intermediate("Private Intermediate");

    // Only the root's CRL: certificates the intermediate issues would have
    // no revocation status.
    assert_refused(
        &PrivateCaMaterial {
            anchors: vec![root.der()],
            intermediates: vec![intermediate.der()],
            crls: vec![root.clean_crl()],
        },
        TrustErrorKind::MissingCrl,
        Some(TrustItem::Intermediate(0)),
    );
    // Only the intermediate's CRL: the intermediate itself would have none.
    assert_refused(
        &PrivateCaMaterial {
            anchors: vec![root.der()],
            intermediates: vec![intermediate.der()],
            crls: vec![intermediate.clean_crl()],
        },
        TrustErrorKind::MissingCrl,
        Some(TrustItem::Anchor(0)),
    );
}

#[test]
fn a_crl_of_an_unconfigured_issuer_is_refused() {
    let root = Authority::root("Private Test Root");
    let stranger = Authority::root("Unconfigured Root");

    assert_refused(
        &with_crls(&root, vec![root.clean_crl(), stranger.clean_crl()]),
        TrustErrorKind::CrlUnknownIssuer,
        Some(TrustItem::Crl(1)),
    );
}

#[test]
fn a_crl_with_a_bad_signature_is_refused() {
    let root = Authority::root("Private Test Root");

    assert_refused(
        &with_crls(&root, vec![corrupt_crl_signature(&root.clean_crl())]),
        TrustErrorKind::CrlBadSignature,
        Some(TrustItem::Crl(0)),
    );
}

#[test]
fn a_crl_signed_by_another_key_under_the_issuers_name_is_refused() {
    let root = Authority::root("Private Test Root");
    let impostor = Authority::root("Private Test Root");

    // The impostor's CRL names its own key.
    assert_refused(
        &with_crls(&root, vec![impostor.clean_crl()]),
        TrustErrorKind::KeyIdentifierMismatch,
        Some(TrustItem::Crl(0)),
    );

    // Claiming the configured CA's key identifier does not help: the
    // signature is checked with the configured CA's key.
    let honest = load(&with_crls(&root, vec![root.clean_crl()]));
    assert!(honest.is_ok());
    let forged = CrlSpec {
        authority_key_identifier: Some(key_identifier_of(&root)),
        ..CrlSpec::current()
    };
    assert_refused(
        &with_crls(&root, vec![impostor.crl(&forged)]),
        TrustErrorKind::CrlBadSignature,
        Some(TrustItem::Crl(0)),
    );
}

/// The subject key identifier `rcgen` gave `authority`: the authority key
/// identifier of the certificates it issues.
fn key_identifier_of(authority: &Authority) -> Vec<u8> {
    authority.certificate.key_identifier()
}

#[test]
fn a_crl_without_next_update_is_refused() {
    assert_crl_refused(
        &CrlSpec {
            next_update: None,
            ..CrlSpec::current()
        },
        TrustErrorKind::CrlMissingNextUpdate,
    );
}

#[test]
fn a_crl_from_the_future_is_refused() {
    assert_crl_refused(
        &CrlSpec {
            this_update: Day(2030, 6, 20),
            ..CrlSpec::current()
        },
        TrustErrorKind::CrlNotYetValid,
    );
}

#[test]
fn an_expired_crl_is_refused() {
    assert_crl_refused(
        &CrlSpec {
            this_update: Day(2030, 5, 1),
            next_update: Some(Day(2030, 6, 10)),
            ..CrlSpec::current()
        },
        TrustErrorKind::CrlExpired,
    );
    // nextUpdate is the first instant at which the CRL is stale.
    assert_crl_refused(
        &CrlSpec {
            next_update: Some(NOW),
            ..CrlSpec::current()
        },
        TrustErrorKind::CrlExpired,
    );
}

#[test]
fn a_crl_whose_timestamps_are_reversed_is_refused() {
    assert_crl_refused(
        &CrlSpec {
            this_update: Day(2030, 6, 14),
            next_update: Some(Day(2030, 6, 10)),
            ..CrlSpec::current()
        },
        TrustErrorKind::CrlTimestampsReversed,
    );
    assert_crl_refused(
        &CrlSpec {
            this_update: Day(2030, 6, 10),
            next_update: Some(Day(2030, 6, 10)),
            ..CrlSpec::current()
        },
        TrustErrorKind::CrlTimestampsReversed,
    );
}

#[test]
fn a_delta_crl_is_refused() {
    assert_crl_refused(
        &CrlSpec::current().with_extension(delta_crl_indicator()),
        TrustErrorKind::DeltaCrl,
    );
}

#[test]
fn an_indirect_crl_is_refused() {
    assert_crl_refused(
        &CrlSpec::current().with_extension(indirect_distribution_point()),
        TrustErrorKind::IndirectCrl,
    );
}

#[test]
fn a_crl_limited_by_an_issuing_distribution_point_is_refused() {
    assert_crl_refused(
        &CrlSpec::current().with_extension(user_certificates_distribution_point()),
        TrustErrorKind::CrlDistributionPoint,
    );
}

#[test]
fn a_crl_with_an_unknown_critical_extension_is_refused() {
    assert_crl_refused(
        &CrlSpec::current().with_extension(unknown_critical_crl_extension()),
        TrustErrorKind::CrlCriticalExtension,
    );
}

#[test]
fn a_malformed_crl_is_refused() {
    let root = Authority::root("Private Test Root");
    let mut trailing = root.clean_crl().to_vec();
    trailing.push(0x00);
    let truncated = root.clean_crl()[..64].to_vec();

    for der in [trailing, truncated, Vec::new()] {
        assert_refused(
            &with_crls(&root, vec![CertificateRevocationListDer::from(der)]),
            TrustErrorKind::MalformedCrl,
            Some(TrustItem::Crl(0)),
        );
    }
}

#[test]
fn clean_and_revoking_crls_of_one_issuer_are_refused_in_either_order() {
    let root = Authority::root("Private Test Root");
    let home_a = root.issue_for(&[HOME_A]);
    let clean = root.clean_crl();
    let revoking = root.crl(&CrlSpec::revoking(&[home_a.leaf()]));

    for crls in [
        vec![clean.clone(), revoking.clone()],
        vec![revoking.clone(), clean.clone()],
        vec![clean.clone(), clean.clone()],
    ] {
        assert_refused(
            &with_crls(&root, crls),
            TrustErrorKind::DuplicateCrl,
            Some(TrustItem::Crl(1)),
        );
    }
}

#[test]
fn a_revoked_leaf_is_refused_in_both_roles() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let home_a = root.issue_for(&[HOME_A]);
    let peer = root.issue_for(&[PEER]);
    let profile = load(&with_crls(
        &root,
        vec![root.crl(&CrlSpec::revoking(&[home_a.leaf()]))],
    ))
    .expect("a profile whose CRL revokes a leaf");
    let inventory = private_inventory(&[&public], profile, &[HOME_A, PEER]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert!(!stream.handshake.completed());
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::CertRevoked)
    );
    // The other certificate of the same issuer is not revoked.
    assert_eq!(verified(stream.of_receiver()).domain().as_str(), PEER);

    let stream = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
    assert!(!stream.handshake.completed());
    assert_eq!(
        refused(stream.of_receiver()),
        VerifyError::Path(webpki::Error::CertRevoked)
    );
}

#[test]
fn a_leaf_revoked_under_an_intermediate_is_refused() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let intermediate = root.intermediate("Private Intermediate");
    let home_a = intermediate.issue_for(&[HOME_A]);
    let home_b = intermediate.issue_for(&[HOME_B]);
    let profile = load(&PrivateCaMaterial {
        anchors: vec![root.der()],
        intermediates: vec![intermediate.der()],
        crls: vec![
            root.clean_crl(),
            intermediate.crl(&CrlSpec::revoking(&[home_a.leaf()])),
        ],
    })
    .expect("a profile whose intermediate revokes a leaf");
    let inventory = private_inventory(&[&public], profile, &[HOME_A, HOME_B]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::CertRevoked)
    );
    assert_eq!(verified(stream.of_receiver()).domain().as_str(), HOME_B);
}

#[test]
fn a_revoked_intermediate_is_refused_at_startup() {
    let root = Authority::root("Private Test Root");
    let intermediate = root.intermediate("Revoked Intermediate");
    let intermediate_der: CertificateDer<'static> = intermediate.der();

    assert_refused(
        &PrivateCaMaterial {
            anchors: vec![root.der()],
            intermediates: vec![intermediate.der()],
            crls: vec![
                root.crl(&CrlSpec::revoking(&[&intermediate_der])),
                intermediate.clean_crl(),
            ],
        },
        TrustErrorKind::RevokedIntermediate,
        Some(TrustItem::Intermediate(0)),
    );
}

#[test]
fn complete_chain_coverage_passes_in_both_roles() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let intermediate = root.intermediate("Private Intermediate");
    let revoked_elsewhere = intermediate.issue_for(&["retired.test"]);
    let profile = load(&PrivateCaMaterial {
        anchors: vec![root.der()],
        intermediates: vec![intermediate.der()],
        crls: vec![
            intermediate.crl(&CrlSpec::revoking(&[revoked_elsewhere.leaf()])),
            root.clean_crl(),
        ],
    })
    .expect("complete CRL coverage");
    assert_eq!(profile.crl_count(), 2);
    assert_eq!(profile.expires_at(), unix(VALID_UNTIL));
    let inventory = private_inventory(&[&public], profile, &[HOME_A, HOME_B]);
    let home_a = intermediate.issue_for(&[HOME_A]);
    let mut long_lived = leaf_params(&[HOME_B]);
    valid_between(&mut long_lived, VALID_FROM, Day(2030, 8, 1));
    let home_b = intermediate.issue(long_lived);

    let stream = attempt(&inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert!(stream.handshake.completed());
    let receiver = verified(stream.of_receiver());
    assert_eq!(receiver.domain().as_str(), HOME_B);
    // The evidence ends when the CRLs do, which is before this certificate
    // expires.
    assert_eq!(receiver.expires_at(), unix(VALID_UNTIL));
    assert_eq!(verified(stream.of_initiator()).domain().as_str(), HOME_A);
}

#[test]
fn more_crls_than_the_ceiling_are_refused() {
    let root = Authority::root("Private Test Root");
    let crls = vec![root.clean_crl(); MAX_PROFILE_CRLS + 1];

    assert_refused(&with_crls(&root, crls), TrustErrorKind::TooManyCrls, None);
}

#[test]
fn a_crl_with_more_entries_than_the_ceiling_is_refused() {
    let root = Authority::root("Private Test Root");
    // Distinct two-byte serial numbers with a non-zero, positive first
    // byte.
    let serials = |count: usize| -> Vec<Vec<u8>> {
        (0..count)
            .map(|index| {
                let [high, low] = u16::try_from(index).expect("a small index").to_be_bytes();
                vec![high + 1, low]
            })
            .collect()
    };
    let at_ceiling = CrlSpec {
        revoked: serials(MAX_CRL_ENTRIES),
        ..CrlSpec::current()
    };
    let beyond_ceiling = CrlSpec {
        revoked: serials(MAX_CRL_ENTRIES + 1),
        ..CrlSpec::current()
    };

    load(&with_crls(&root, vec![root.crl(&at_ceiling)])).expect("a CRL at the entry ceiling");
    assert_refused(
        &with_crls(&root, vec![root.crl(&beyond_ceiling)]),
        TrustErrorKind::TooManyCrlEntries,
        Some(TrustItem::Crl(0)),
    );
}

#[test]
fn a_crl_larger_than_the_ceiling_is_refused() {
    let root = Authority::root("Private Test Root");
    let oversized = CrlSpec::current().with_extension(oversized_extension());

    assert_refused(
        &with_crls(&root, vec![root.crl(&oversized)]),
        TrustErrorKind::CrlTooLarge,
        Some(TrustItem::Crl(0)),
    );
}

#[test]
fn crl_validity_is_evaluated_at_the_supplied_time() {
    let root = Authority::root("Private Test Root");
    let material = with_crls(&root, vec![root.clean_crl()]);
    let algorithms = settings(NOW).algorithms();
    let load_at = |day: Day| PrivateCaProfile::load(&material, &algorithms, unix(day));

    assert!(load_at(VALID_FROM).is_ok());
    assert!(load_at(NOW).is_ok());
    assert_eq!(
        load_at(Day(2030, 5, 31))
            .expect_err("before thisUpdate")
            .kind(),
        TrustErrorKind::CrlNotYetValid
    );
    assert_eq!(
        load_at(VALID_UNTIL).expect_err("at nextUpdate").kind(),
        TrustErrorKind::CrlExpired
    );
    assert_eq!(
        load_at(Day(2030, 8, 1))
            .expect_err("after nextUpdate")
            .kind(),
        TrustErrorKind::CrlExpired
    );
}
