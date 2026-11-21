//! Matrix row "Trust profile": a public-CA chain fails for a domain on a
//! private-CA profile, a private-CA chain fails for a domain on the
//! public-WebPKI profile, and no second profile is tried after a rejection.

use std::sync::Arc;

use waddle_server::s2s::verify::{
    PeerVerifier, PublicRoots, TrustErrorKind, TrustInventory, TrustProfileKind, VerifyError,
};
use waddle_xmpp::identity::CanonicalDomain;

use crate::harness::{
    attempt, domain, private_inventory, private_profile, refused, settings, verified, verifier,
    DEEP_A, HOME_A, HOME_B, MUC_A,
};
use crate::pki::{unix, Authority, CA_UNTIL, CRL_NEXT_UPDATE, NOW};

/// [`HOME_A`] uses a private-CA profile and every other domain the
/// substituted public roots.
struct Mixed {
    public: Authority,
    private: Authority,
    inventory: TrustInventory,
}

fn mixed() -> Mixed {
    let public = Authority::root("Substituted Public Root");
    let private = Authority::root("Private Test Root");
    let inventory = private_inventory(&[&public], private_profile(&[&private], &[]), &[HOME_A]);
    Mixed {
        public,
        private,
        inventory,
    }
}

#[test]
fn each_domain_is_verified_under_its_own_profile() {
    let mixed = mixed();
    let home_a = mixed.private.issue_for(&[HOME_A]);
    let home_b = mixed.public.issue_for(&[HOME_B]);

    let stream = attempt(&mixed.inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert!(stream.handshake.completed());
    assert_eq!(
        verified(stream.of_initiator()).profile(),
        TrustProfileKind::PrivateCa
    );
    assert_eq!(
        verified(stream.of_receiver()).profile(),
        TrustProfileKind::PublicWebPki
    );
}

#[test]
fn a_public_chain_fails_for_a_private_ca_domain_in_both_roles() {
    let mixed = mixed();
    // The certificate would verify under the public profile, which the
    // same inventory holds; it is never consulted for home-a.test.
    let public_a = mixed.public.issue_for(&[HOME_A, "alias.test"]);
    let home_b = mixed.public.issue_for(&[HOME_B]);

    let as_initiator = attempt(&mixed.inventory, (HOME_A, &public_a), (HOME_B, &home_b));
    assert!(!as_initiator.handshake.completed());
    assert_eq!(
        as_initiator.receiving.profile_kind(),
        TrustProfileKind::PrivateCa
    );
    assert_eq!(
        refused(as_initiator.of_initiator()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );

    let as_receiver = attempt(&mixed.inventory, (HOME_B, &home_b), (HOME_A, &public_a));
    assert!(!as_receiver.handshake.completed());
    assert_eq!(
        refused(as_receiver.of_receiver()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );

    // The same certificate is accepted for its other name, which is on the
    // public profile.
    let as_alias = attempt(
        &mixed.inventory,
        ("alias.test", &public_a),
        (HOME_B, &home_b),
    );
    assert!(as_alias.handshake.completed());
    assert_eq!(
        verified(as_alias.of_initiator()).profile(),
        TrustProfileKind::PublicWebPki
    );
}

#[test]
fn a_private_chain_fails_for_a_public_domain_in_both_roles() {
    let mixed = mixed();
    let home_a = mixed.private.issue_for(&[HOME_A]);
    // Issued by the private CA, which the inventory trusts, but only for
    // home-a.test.
    let private_b = mixed.private.issue_for(&[HOME_B, HOME_A]);

    let as_receiver = attempt(&mixed.inventory, (HOME_A, &home_a), (HOME_B, &private_b));
    assert!(!as_receiver.handshake.completed());
    assert_eq!(
        as_receiver.initiating.profile_kind(),
        TrustProfileKind::PublicWebPki
    );
    assert_eq!(
        refused(as_receiver.of_receiver()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );

    let home_a_peer = mixed.private.issue_for(&[HOME_A]);
    let as_initiator = attempt(
        &mixed.inventory,
        (HOME_B, &private_b),
        (HOME_A, &home_a_peer),
    );
    assert!(!as_initiator.handshake.completed());
    assert_eq!(
        refused(as_initiator.of_initiator()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );

    // The same certificate is accepted for home-a.test.
    let as_home_a = attempt(
        &mixed.inventory,
        (HOME_A, &private_b),
        (HOME_A, &home_a_peer),
    );
    assert_eq!(
        verified(as_home_a.of_initiator()).profile(),
        TrustProfileKind::PrivateCa
    );
}

#[test]
fn a_private_ca_profile_covers_exactly_the_configured_domain() {
    let mixed = mixed();

    assert_eq!(
        mixed.inventory.profile_for(&domain(HOME_A)).kind(),
        TrustProfileKind::PrivateCa
    );
    // A component of the configured domain is not covered.
    assert_eq!(
        mixed.inventory.profile_for(&domain(MUC_A)).kind(),
        TrustProfileKind::PublicWebPki
    );
    assert_eq!(
        mixed.inventory.profile_for(&domain(HOME_B)).kind(),
        TrustProfileKind::PublicWebPki
    );
    // The U-label and A-label spellings of a domain are one domain with
    // one profile.
    assert_eq!(
        CanonicalDomain::parse_exact("bücher.test").expect("a U-label domain"),
        domain("xn--bcher-kva.test")
    );
}

#[test]
fn a_domain_cannot_be_given_two_profiles_or_a_profile_it_cannot_use() {
    let public = Authority::root("Substituted Public Root");
    let private = Authority::root("Private Test Root");
    let profile = Arc::new(private_profile(&[&private], &[]));
    let mut inventory =
        TrustInventory::new(PublicRoots::from_certificates(&[public.der()]).expect("roots"));

    inventory
        .configure_private_ca(domain(HOME_A), Arc::clone(&profile))
        .expect("the first profile of a domain");
    let duplicate = inventory
        .configure_private_ca(domain(HOME_A), Arc::clone(&profile))
        .expect_err("a second profile for one domain");
    assert_eq!(duplicate.kind(), TrustErrorKind::DuplicateDomain);

    // An IP literal, a numeric IPv4 form and a single-label name cannot
    // name a federation peer.
    for name in ["[::1]", "192.0.2.1", "0x7f.1", "localhost"] {
        let refused = inventory
            .configure_private_ca(domain(name), Arc::clone(&profile))
            .expect_err("a domain that names no peer");
        assert_eq!(refused.kind(), TrustErrorKind::NotAPeerDomain);
        let no_verifier = PeerVerifier::new(&inventory, domain(name), settings(NOW).algorithms())
            .expect_err("a reference identity that names no peer");
        assert_eq!(no_verifier.kind(), TrustErrorKind::NotAPeerDomain);
    }
}

#[test]
fn the_inventory_summary_counts_material_without_naming_it() {
    let public = Authority::root("Substituted Public Root");
    let private = Authority::root("Private Test Root");
    let intermediate = private.intermediate("Private Intermediate");
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&private], &[&intermediate]),
        &[HOME_A, HOME_B],
    );

    let summary = inventory.summary([]);

    assert_eq!(summary.public_roots, 1);
    assert_eq!(summary.private_domains, 2);
    assert_eq!(summary.private_profiles, 1);
    assert_eq!(summary.private_ca_certificates, 2);
    assert_eq!(summary.private_crls, 2);
    // The CRLs expire before the CA certificates do.
    assert_eq!(summary.earliest_private_expiry, Some(unix(CRL_NEXT_UPDATE)));
    assert!(unix(CRL_NEXT_UPDATE) < unix(CA_UNTIL));
    assert_eq!(summary.public_subdomains_of_private, 0);
    summary.log();
}

#[test]
fn the_summary_counts_peer_subdomains_left_on_the_public_profile() {
    let mixed = mixed();
    // home-a.test has a private-CA profile; its component domains do not.
    let peers = [
        domain(HOME_A),
        domain(MUC_A),
        domain(DEEP_A),
        domain(MUC_A),
        domain(HOME_B),
        domain("muc.home-b.test"),
        domain("xhome-a.test"),
    ];

    let summary = mixed.inventory.summary(&peers);

    // muc.home-a.test and a.muc.home-a.test, each counted once.
    assert_eq!(summary.public_subdomains_of_private, 2);
    assert_eq!(summary.private_domains, 1);
    // The count is for the log only; the profiles stay exact.
    assert_eq!(
        mixed.inventory.profile_for(&domain(MUC_A)).kind(),
        TrustProfileKind::PublicWebPki
    );
    summary.log();

    // Once the component domain is configured, it is no longer counted.
    let mut inventory = private_inventory(
        &[&mixed.public],
        private_profile(&[&mixed.private], &[]),
        &[HOME_A, MUC_A],
    );
    assert_eq!(inventory.summary(&peers).public_subdomains_of_private, 1);
    inventory
        .configure_private_ca(
            domain(DEEP_A),
            Arc::new(private_profile(&[&mixed.private], &[])),
        )
        .expect("a third private-CA domain");
    assert_eq!(inventory.summary(&peers).public_subdomains_of_private, 0);
}

#[test]
fn the_public_root_store_is_the_compiled_in_mozilla_set_or_a_substitute() {
    let mozilla = PublicRoots::mozilla();
    assert!(!mozilla.is_empty());
    assert_eq!(mozilla.len(), webpki_roots::TLS_SERVER_ROOTS.len());

    let empty = PublicRoots::from_certificates(&[]).expect_err("no roots");
    assert_eq!(empty.kind(), TrustErrorKind::NoTrustAnchors);

    // A domain without a private-CA profile is verified against the
    // substituted roots only: the compiled-in set is not consulted.
    let substitute = Authority::root("Substituted Public Root");
    let inventory =
        TrustInventory::new(PublicRoots::from_certificates(&[substitute.der()]).expect("roots"));
    let pending = verifier(&inventory, HOME_A, NOW);
    assert_eq!(pending.profile_kind(), TrustProfileKind::PublicWebPki);
}
