//! Matrix row "Time": certificate and CRL validity is evaluated at the
//! supplied verification time, including not-yet-valid and expired
//! material, and a public end-entity certificate's validity period is held
//! to the ceiling in force when it was issued.

use std::time::Duration;

use rcgen::date_time_ymd;
use waddle_server::s2s::verify::limits::{max_public_leaf_validity_secs, DAY_SECS};
use waddle_server::s2s::verify::{TrustProfileKind, VerifyError};

use crate::harness::{
    attempt, attempt_at, private_inventory, private_profile, public_inventory, refused, verified,
    Fixture, HOME_A, HOME_B, PEER,
};
use crate::pki::{
    ca_params, leaf_params, unix, valid_between, Authority, Credential, Day, CA_FROM,
    CRL_NEXT_UPDATE, NOW, VALID_FROM, VALID_UNTIL,
};

#[test]
fn a_leaf_is_refused_before_and_after_its_validity_on_the_public_profile() {
    let fixture = Fixture::public();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let stream = |day: Day| {
        attempt_at(
            &fixture.inventory,
            (HOME_A, &home_a),
            (HOME_B, &home_b),
            day,
        )
    };

    let early = stream(Day(2030, 5, 31));
    assert!(matches!(
        refused(early.of_receiver()),
        VerifyError::Path(webpki::Error::CertNotValidYet { .. })
    ));

    for day in [VALID_FROM, NOW, Day(2030, 6, 30)] {
        let valid = stream(day);
        assert!(valid.handshake.completed(), "{day:?}");
        assert_eq!(
            verified(valid.of_initiator()).expires_at(),
            unix(VALID_UNTIL)
        );
    }

    let late = stream(Day(2030, 7, 2));
    assert!(matches!(
        refused(late.of_receiver()),
        VerifyError::Path(webpki::Error::CertExpired { .. })
    ));
}

#[test]
fn each_role_checks_the_validity_of_the_certificate_it_verifies() {
    let fixture = Fixture::public();
    let mut expired = leaf_params(&[HOME_A]);
    valid_between(&mut expired, Day(2030, 5, 1), Day(2030, 6, 10));

    let [as_receiver, as_initiator] =
        fixture.in_both_roles(HOME_A, |issuer| issuer.issue(expired.clone()));

    assert!(matches!(
        refused(as_receiver),
        VerifyError::Path(webpki::Error::CertExpired { .. })
    ));
    assert!(matches!(
        refused(as_initiator),
        VerifyError::Path(webpki::Error::CertExpired { .. })
    ));
}

#[test]
fn a_private_ca_profile_is_refused_outside_the_validity_of_its_crls() {
    let fixture = Fixture::private();
    // Valid well before and after the profile's CRLs, so only the CRLs
    // decide.
    let long_lived = |name: &str| {
        let mut params = leaf_params(&[name]);
        valid_between(&mut params, CA_FROM, Day(2030, 12, 1));
        fixture.issuer.issue(params)
    };
    let home_a = long_lived(HOME_A);
    let home_b = long_lived(HOME_B);
    let stream = |day: Day| {
        attempt_at(
            &fixture.inventory,
            (HOME_A, &home_a),
            (HOME_B, &home_b),
            day,
        )
    };

    let before_this_update = stream(Day(2030, 6, 11));
    assert_eq!(
        refused(before_this_update.of_receiver()),
        VerifyError::TrustNotYetValid
    );

    let current = stream(NOW);
    let peer = current
        .receiver_established()
        .expect("a completed handshake");
    assert_eq!(peer.profile(), TrustProfileKind::PrivateCa);
    // The evidence ends at the CRL's nextUpdate, long before the
    // certificate expires.
    assert_eq!(peer.expires_at(), unix(CRL_NEXT_UPDATE));
    assert!(peer.is_current(unix(Day(2030, 6, 18))));
    assert!(!peer.is_current(unix(CRL_NEXT_UPDATE)));

    // nextUpdate is the first instant at which the profile is stale.
    for day in [CRL_NEXT_UPDATE, Day(2030, 6, 20)] {
        let stale = stream(day);
        assert!(!stale.handshake.completed(), "{day:?}");
        assert_eq!(refused(stale.of_receiver()), VerifyError::TrustExpired);
    }
}

#[test]
fn a_private_ca_profile_is_refused_once_its_anchor_expires() {
    let mut params = ca_params("Short Lived Root");
    valid_between(&mut params, CA_FROM, Day(2030, 6, 17));
    let root = Authority::root_with(params);
    let public = Authority::root("Unrelated Public Root");
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&root], &[]),
        &[HOME_A, HOME_B],
    );
    let home_a = root.issue_for(&[HOME_A]);
    let home_b = root.issue_for(&[HOME_B]);

    let current = attempt(&inventory, (HOME_A, &home_a), (HOME_B, &home_b));
    // The anchor expires before the leaf and the CRL do.
    assert_eq!(
        current
            .receiver_established()
            .expect("a completed handshake")
            .expires_at(),
        unix(Day(2030, 6, 17))
    );

    let after = attempt_at(
        &inventory,
        (HOME_A, &home_a),
        (HOME_B, &home_b),
        Day(2030, 6, 18),
    );
    assert_eq!(refused(after.of_receiver()), VerifyError::TrustExpired);
}

#[test]
fn a_public_path_ends_with_its_earliest_expiring_issuer() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let mut params = ca_params("Short Lived Intermediate");
    valid_between(&mut params, CA_FROM, Day(2030, 6, 20));
    let intermediate = root.intermediate_with(params);
    let home_a = intermediate
        .issue_for(&[HOME_A])
        .presenting(&[intermediate.der()]);
    let peer = root.issue_for(&[PEER]);

    let current = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert!(current.handshake.completed());
    assert_eq!(
        verified(current.of_initiator()).expires_at(),
        unix(Day(2030, 6, 20))
    );
    assert_eq!(
        verified(current.of_receiver()).expires_at(),
        unix(VALID_UNTIL)
    );

    let after = attempt_at(
        &inventory,
        (HOME_A, &home_a),
        (PEER, &peer),
        Day(2030, 6, 21),
    );
    assert!(matches!(
        refused(after.of_initiator()),
        VerifyError::Path(webpki::Error::CertExpired { .. })
    ));
}

/// A credential for `name` whose validity starts at `from` and lasts
/// `days` days.
fn leaf_valid_for(issuer: &Authority, name: &str, from: Day, days: u64) -> Credential {
    let mut params = leaf_params(&[name]);
    params.not_before = date_time_ymd(from.0, from.1, from.2);
    params.not_after = params.not_before + Duration::from_secs(days * 86_400);
    issuer.issue(params)
}

#[test]
fn the_public_leaf_validity_ceiling_steps_down_on_the_ballot_dates() {
    let at = |day: Day| i64::try_from(unix(day).as_secs()).expect("a test day");
    let cases = [
        (at(Day(2026, 3, 15)) - 1, 398),
        (at(Day(2026, 3, 15)), 200),
        (at(Day(2027, 3, 15)) - 1, 200),
        (at(Day(2027, 3, 15)), 100),
        (at(Day(2029, 3, 15)) - 1, 100),
        (at(Day(2029, 3, 15)), 47),
        (at(Day(2035, 1, 1)), 47),
        (i64::MIN, 398),
    ];
    for (not_before, days) in cases {
        assert_eq!(
            max_public_leaf_validity_secs(not_before),
            days * DAY_SECS,
            "{not_before}"
        );
    }
}

#[test]
fn a_public_leaf_is_held_to_the_validity_ceiling_of_its_not_before() {
    let mut params = ca_params("Long Lived Public Root");
    valid_between(&mut params, Day(2025, 1, 1), Day(2032, 1, 1));
    let root = Authority::root_with(params);
    let inventory = public_inventory(&[&root]);
    // notBefore, the longest validity it allows in days, and a day on which
    // a certificate issued then is valid.
    let steps = [
        (Day(2026, 3, 14), 398, Day(2026, 4, 14)),
        (Day(2026, 3, 15), 200, Day(2026, 4, 15)),
        (Day(2027, 3, 14), 200, Day(2027, 4, 14)),
        (Day(2027, 3, 15), 100, Day(2027, 4, 15)),
        (Day(2029, 3, 14), 100, Day(2029, 4, 14)),
        (Day(2029, 3, 15), 47, Day(2029, 4, 1)),
        (Day(2030, 6, 1), 47, NOW),
    ];
    for (from, days, day) in steps {
        // Within every step, so only the certificate under test varies.
        let peer = leaf_valid_for(&root, PEER, from, 40);

        let within = leaf_valid_for(&root, HOME_A, from, days);
        let as_receiver = attempt_at(&inventory, (PEER, &peer), (HOME_A, &within), day);
        assert!(as_receiver.receiver_established().is_some(), "{from:?}");
        let as_initiator = attempt_at(&inventory, (HOME_A, &within), (PEER, &peer), day);
        assert!(as_initiator.initiator_established().is_some(), "{from:?}");

        let beyond = leaf_valid_for(&root, HOME_A, from, days + 1);
        let as_receiver = attempt_at(&inventory, (PEER, &peer), (HOME_A, &beyond), day);
        assert_eq!(
            refused(as_receiver.of_receiver()),
            VerifyError::ValidityTooLong,
            "{from:?}"
        );
        let as_initiator = attempt_at(&inventory, (HOME_A, &beyond), (PEER, &peer), day);
        assert_eq!(
            refused(as_initiator.of_initiator()),
            VerifyError::ValidityTooLong,
            "{from:?}"
        );
    }
}

#[test]
fn a_private_ca_sets_its_own_leaf_validity() {
    // A private CA sets its own issuance policy and is checked for
    // revocation.
    let long_lived = |issuer: &Authority| {
        let mut params = leaf_params(&[HOME_A]);
        valid_between(&mut params, Day(2029, 6, 14), Day(2030, 7, 18));
        issuer.issue(params)
    };
    for accepted in Fixture::private().established_in_both_roles(HOME_A, long_lived) {
        assert_eq!(accepted.profile(), TrustProfileKind::PrivateCa);
    }
    for failure in Fixture::public().in_both_roles(HOME_A, long_lived) {
        assert_eq!(refused(failure), VerifyError::ValidityTooLong);
    }
}
