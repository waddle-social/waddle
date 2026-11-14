//! Matrix row "Time": certificate and CRL validity is evaluated at the
//! supplied verification time, including not-yet-valid and expired
//! material.

use waddle_server::s2s::verify::{TrustProfileKind, VerifyError};

use crate::harness::{
    attempt, attempt_at, private_inventory, private_profile, public_inventory, refused, verified,
    Fixture, HOME_A, HOME_B, PEER,
};
use crate::pki::{
    ca_params, leaf_params, unix, valid_between, Authority, Day, CA_FROM, NOW, VALID_FROM,
    VALID_UNTIL,
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

    let before_this_update = stream(Day(2030, 5, 31));
    assert_eq!(
        refused(before_this_update.of_receiver()),
        VerifyError::TrustNotYetValid
    );

    let current = stream(NOW);
    assert!(current.handshake.completed());
    let peer = verified(current.of_receiver());
    assert_eq!(peer.profile(), TrustProfileKind::PrivateCa);
    // The evidence ends at the CRL's nextUpdate, long before the
    // certificate expires.
    assert_eq!(peer.expires_at(), unix(VALID_UNTIL));
    assert!(peer.is_current(unix(Day(2030, 6, 30))));
    assert!(!peer.is_current(unix(VALID_UNTIL)));

    // nextUpdate is the first instant at which the profile is stale.
    for day in [VALID_UNTIL, Day(2030, 7, 2)] {
        let stale = stream(day);
        assert!(!stale.handshake.completed(), "{day:?}");
        assert_eq!(refused(stale.of_receiver()), VerifyError::TrustExpired);
    }
}

#[test]
fn a_private_ca_profile_is_refused_once_its_anchor_expires() {
    let mut params = ca_params("Short Lived Root");
    valid_between(&mut params, CA_FROM, Day(2030, 6, 20));
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
    assert!(current.handshake.completed());
    // The anchor expires before the leaf and the CRL do.
    assert_eq!(
        verified(current.of_receiver()).expires_at(),
        unix(Day(2030, 6, 20))
    );

    let after = attempt_at(
        &inventory,
        (HOME_A, &home_a),
        (HOME_B, &home_b),
        Day(2030, 6, 21),
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

#[test]
fn a_public_leaf_valid_for_longer_than_398_days_is_refused() {
    let within = |issuer: &Authority| {
        let mut params = leaf_params(&[HOME_A]);
        valid_between(&mut params, Day(2029, 6, 14), Day(2030, 7, 17));
        issuer.issue(params)
    };
    let beyond = |issuer: &Authority| {
        let mut params = leaf_params(&[HOME_A]);
        valid_between(&mut params, Day(2029, 6, 14), Day(2030, 7, 18));
        issuer.issue(params)
    };

    let public = Fixture::public();
    for accepted in public.in_both_roles(HOME_A, within) {
        assert_eq!(verified(accepted).profile(), TrustProfileKind::PublicWebPki);
    }
    for failure in public.in_both_roles(HOME_A, beyond) {
        assert_eq!(refused(failure), VerifyError::ValidityTooLong);
    }
    // A private CA sets its own issuance policy and is checked for
    // revocation.
    for accepted in Fixture::private().in_both_roles(HOME_A, beyond) {
        assert_eq!(verified(accepted).profile(), TrustProfileKind::PrivateCa);
    }
}
