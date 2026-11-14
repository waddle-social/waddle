//! Matrix row "Constraints": DNS name constraints are enforced, a CA or
//! path with an SRVName/otherName constraint is refused outright, SRV-IDs and
//! XmppAddrs are not matched under a name-constrained path, and no retry
//! drops a constraint or promotes an intermediate to a root.

use rcgen::{GeneralSubtree, NameConstraints};
use waddle_server::s2s::verify::{
    MatchedIdentifier, PrivateCaMaterial, PrivateCaProfile, TrustErrorKind, TrustItem, VerifyError,
};

use crate::harness::{
    attempt, private_inventory, private_profile, public_inventory, refused, settings, verified,
    HOME_A, HOME_B, PEER,
};
use crate::pki::{
    ca_params, leaf_params, other_name_constraint, san_extension, san_srv, san_xmpp_addr, unix,
    Authority, Credential, NOW,
};

/// A CA under `root` that may issue only for `permitted` and never for
/// `excluded`.
fn constrained_intermediate(root: &Authority, permitted: &[&str], excluded: &[&str]) -> Authority {
    let subtrees = |names: &[&str]| {
        names
            .iter()
            .map(|name| GeneralSubtree::DnsName((*name).to_owned()))
            .collect()
    };
    let mut params = ca_params("Constrained Intermediate");
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: subtrees(permitted),
        excluded_subtrees: subtrees(excluded),
    });
    root.intermediate_with(params)
}

/// A leaf whose subjectAltName holds exactly `entry`.
fn leaf_naming(issuer: &Authority, entry: Vec<u8>) -> Credential {
    let mut params = leaf_params(&[]);
    params.custom_extensions.push(san_extension(&[entry]));
    issuer.issue(params)
}

fn srv_only_leaf(issuer: &Authority, srv_name: &str) -> Credential {
    leaf_naming(issuer, san_srv(srv_name))
}

fn xmpp_addr_only_leaf(issuer: &Authority, addr: &str) -> Credential {
    leaf_naming(issuer, san_xmpp_addr(addr))
}

#[test]
fn a_permitted_dns_subtree_is_enforced_in_both_roles() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let intermediate = constrained_intermediate(&root, &[HOME_A], &[]);
    let peer = root.issue_for(&[PEER]);
    let inside = intermediate
        .issue_for(&[HOME_A])
        .presenting(&[intermediate.der()]);
    let outside = intermediate
        .issue_for(&[HOME_B])
        .presenting(&[intermediate.der()]);

    let stream = attempt(&inventory, (HOME_A, &inside), (PEER, &peer));
    assert_eq!(
        verified(stream.of_initiator()).matched_identifier(),
        MatchedIdentifier::DnsId
    );
    let stream = attempt(&inventory, (PEER, &peer), (HOME_A, &inside));
    assert_eq!(
        verified(stream.of_receiver()).matched_identifier(),
        MatchedIdentifier::DnsId
    );

    let stream = attempt(&inventory, (HOME_B, &outside), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::NameConstraintViolation)
    );
    let stream = attempt(&inventory, (PEER, &peer), (HOME_B, &outside));
    assert_eq!(
        refused(stream.of_receiver()),
        VerifyError::Path(webpki::Error::NameConstraintViolation)
    );
}

#[test]
fn an_excluded_dns_subtree_is_enforced() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let intermediate = constrained_intermediate(&root, &[], &[HOME_B]);
    let peer = root.issue_for(&[PEER]);
    let allowed = intermediate
        .issue_for(&[HOME_A])
        .presenting(&[intermediate.der()]);
    let excluded = intermediate
        .issue_for(&[HOME_B])
        .presenting(&[intermediate.der()]);

    let stream = attempt(&inventory, (HOME_A, &allowed), (PEER, &peer));
    assert!(stream.handshake.completed());

    let stream = attempt(&inventory, (HOME_B, &excluded), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::NameConstraintViolation)
    );
}

/// SRV-IDs and XmppAddrs are matched on a private-CA profile only, so that
/// is where a name-constrained path must keep them from matching.
#[test]
fn an_srv_id_or_xmpp_addr_is_not_matched_under_a_name_constrained_path() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let intermediate = constrained_intermediate(&root, &[HOME_A], &[]);
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&root], &[&intermediate]),
        &[HOME_A, HOME_B, PEER],
    );
    let peer = root.issue_for(&[PEER]);

    // The issuer is limited to home-a.test by a dNSName constraint, which
    // does not cover the otherName form: an SRV-ID or XmppAddr for any
    // domain passes webpki's constraint check. It must not authenticate
    // home-b.test.
    let escaping = [
        srv_only_leaf(&intermediate, "_xmpp-server.home-b.test"),
        xmpp_addr_only_leaf(&intermediate, HOME_B),
    ];
    for leaf in &escaping {
        let stream = attempt(&inventory, (HOME_B, leaf), (PEER, &peer));
        assert_eq!(
            refused(stream.of_initiator()),
            VerifyError::IdentityMismatch
        );
        let stream = attempt(&inventory, (PEER, &peer), (HOME_B, leaf));
        assert_eq!(refused(stream.of_receiver()), VerifyError::IdentityMismatch);
    }

    // Nor is either honoured inside the permitted subtree: the verifier
    // does not implement SRVName constraints at all.
    let inside = [
        srv_only_leaf(&intermediate, "_xmpp-server.home-a.test"),
        xmpp_addr_only_leaf(&intermediate, HOME_A),
    ];
    for leaf in &inside {
        let stream = attempt(&inventory, (HOME_A, leaf), (PEER, &peer));
        assert_eq!(
            refused(stream.of_initiator()),
            VerifyError::IdentityMismatch
        );
    }

    // The same identifiers under the unconstrained root match.
    let unconstrained = [
        (
            srv_only_leaf(&root, "_xmpp-server.home-a.test"),
            MatchedIdentifier::SrvId,
        ),
        (
            xmpp_addr_only_leaf(&root, HOME_A),
            MatchedIdentifier::XmppAddr,
        ),
    ];
    for (leaf, expected) in &unconstrained {
        let stream = attempt(&inventory, (HOME_A, leaf), (PEER, &peer));
        assert_eq!(
            stream
                .initiator_established()
                .expect("a completed handshake")
                .matched_identifier(),
            *expected
        );
    }
}

#[test]
fn a_name_constrained_private_ca_anchor_keeps_srv_ids_and_xmpp_addrs_from_matching() {
    let mut params = ca_params("Constrained Private Root");
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::DnsName(HOME_A.to_owned()),
            GeneralSubtree::DnsName(PEER.to_owned()),
        ],
        excluded_subtrees: Vec::new(),
    });
    let root = Authority::root_with(params);
    let public = Authority::root("Unrelated Public Root");
    let inventory = private_inventory(&[&public], private_profile(&[&root], &[]), &[HOME_A, PEER]);
    let peer = root.issue_for(&[PEER]);

    let stream = attempt(
        &inventory,
        (HOME_A, &root.issue_for(&[HOME_A])),
        (PEER, &peer),
    );
    assert!(stream.initiator_established().is_some());

    for leaf in [
        srv_only_leaf(&root, "_xmpp-server.home-a.test"),
        xmpp_addr_only_leaf(&root, HOME_A),
    ] {
        let stream = attempt(&inventory, (HOME_A, &leaf), (PEER, &peer));
        assert_eq!(
            refused(stream.of_initiator()),
            VerifyError::IdentityMismatch
        );
    }
}

#[test]
fn a_name_constrained_trust_anchor_constrains_the_path() {
    let mut params = ca_params("Constrained Public Root");
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: vec![
            GeneralSubtree::DnsName(HOME_A.to_owned()),
            GeneralSubtree::DnsName(PEER.to_owned()),
        ],
        excluded_subtrees: Vec::new(),
    });
    let root = Authority::root_with(params);
    let inventory = public_inventory(&[&root]);
    let peer = root.issue_for(&[PEER]);

    let inside = root.issue_for(&[HOME_A]);
    let stream = attempt(&inventory, (HOME_A, &inside), (PEER, &peer));
    assert!(stream.handshake.completed());

    let outside = root.issue_for(&[HOME_B]);
    let stream = attempt(&inventory, (HOME_B, &outside), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::NameConstraintViolation)
    );

    let srv_only = srv_only_leaf(&root, "_xmpp-server.home-a.test");
    let stream = attempt(&inventory, (HOME_A, &srv_only), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::IdentityMismatch
    );
}

#[test]
fn a_public_path_with_an_other_name_constraint_is_refused_despite_a_matching_dns_id() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let mut params = ca_params("SRVName Constrained Intermediate");
    params
        .custom_extensions
        .push(other_name_constraint("_xmpp-server.home-a.test"));
    let intermediate = root.intermediate_with(params);
    let peer = root.issue_for(&[PEER]);
    let home_a = intermediate
        .issue_for(&[HOME_A])
        .presenting(&[intermediate.der()]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::UnsupportedNameConstraint
    );
    let stream = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
    assert_eq!(
        refused(stream.of_receiver()),
        VerifyError::UnsupportedNameConstraint
    );
}

#[test]
fn a_public_root_with_an_other_name_constraint_is_refused() {
    let mut params = ca_params("SRVName Constrained Root");
    params
        .custom_extensions
        .push(other_name_constraint("_xmpp-server.home-a.test"));
    let root = Authority::root_with(params);
    // The other endpoint's certificate chains to a second, unconstrained
    // root. Were it issued under the constrained root, the initiating
    // endpoint would refuse the receiving server's certificate and abort
    // the handshake before the receiving endpoint saw the initiating
    // server's certificate at all.
    let unconstrained = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root, &unconstrained]);
    let peer = unconstrained.issue_for(&[PEER]);
    let home_a = root.issue_for(&[HOME_A]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert!(!stream.handshake.completed());
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::UnsupportedNameConstraint
    );
    assert!(stream.initiator_established().is_none());

    let stream = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
    assert!(!stream.handshake.completed());
    assert_eq!(
        refused(stream.of_receiver()),
        VerifyError::UnsupportedNameConstraint
    );

    // The same chain verifies when the root carries no such constraint.
    let plain = Authority::root("Plain Public Root");
    let inventory = public_inventory(&[&plain, &unconstrained]);
    let home_a = plain.issue_for(&[HOME_A]);
    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert!(stream.initiator_established().is_some());
}

#[test]
fn a_private_ca_profile_with_an_other_name_constraint_fails_at_startup() {
    let cases: [(bool, TrustItem); 2] = [
        (true, TrustItem::Anchor(0)),
        (false, TrustItem::Intermediate(0)),
    ];
    for (on_anchor, item) in cases {
        let mut constrained = ca_params("SRVName Constrained CA");
        constrained
            .custom_extensions
            .push(other_name_constraint("_xmpp-server.home-a.test"));
        let (root, intermediate) = if on_anchor {
            let root = Authority::root_with(constrained);
            let intermediate = root.intermediate("Private Intermediate");
            (root, intermediate)
        } else {
            let root = Authority::root("Private Test Root");
            let intermediate = root.intermediate_with(constrained);
            (root, intermediate)
        };
        let material = PrivateCaMaterial {
            anchors: vec![root.der()],
            intermediates: vec![intermediate.der()],
            crls: vec![root.clean_crl(), intermediate.clean_crl()],
        };

        let error = PrivateCaProfile::load(&material, &settings(NOW).algorithms(), unix(NOW))
            .expect_err("an otherName constraint");

        assert_eq!(error.kind(), TrustErrorKind::UnsupportedNameConstraint);
        assert_eq!(error.item(), Some(item));
    }
}

#[test]
fn a_private_ca_profile_enforces_the_dns_constraints_of_its_intermediates() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let intermediate = constrained_intermediate(&root, &[HOME_A, PEER], &[]);
    let inventory = private_inventory(
        &[&public],
        private_profile(&[&root], &[&intermediate]),
        &[HOME_A, HOME_B, PEER],
    );
    let peer = intermediate.issue_for(&[PEER]);

    let inside = intermediate.issue_for(&[HOME_A]);
    let stream = attempt(&inventory, (HOME_A, &inside), (PEER, &peer));
    assert!(stream.handshake.completed());

    let outside = intermediate.issue_for(&[HOME_B]);
    let stream = attempt(&inventory, (HOME_B, &outside), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::NameConstraintViolation)
    );
}

#[test]
fn an_intermediate_the_peer_sends_never_becomes_a_trust_anchor() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let peer = root.issue_for(&[PEER]);
    // A complete, internally valid chain to a root the verifier does not
    // trust, sent in full.
    let rogue_root = Authority::root("Rogue Root");
    let rogue_intermediate = rogue_root.intermediate("Rogue Intermediate");
    let rogue = rogue_intermediate
        .issue_for(&[HOME_A])
        .presenting(&[rogue_intermediate.der(), rogue_root.der()]);

    let stream = attempt(&inventory, (HOME_A, &rogue), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );
    let stream = attempt(&inventory, (PEER, &peer), (HOME_A, &rogue));
    assert_eq!(
        refused(stream.of_receiver()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );
}

#[test]
fn a_private_ca_profile_ignores_intermediates_the_peer_sends() {
    let root = Authority::root("Private Test Root");
    let public = Authority::root("Unrelated Public Root");
    let inventory = private_inventory(&[&public], private_profile(&[&root], &[]), &[HOME_A, PEER]);
    let peer = root.issue_for(&[PEER]);
    // Issued by the trusted root, but not part of the profile, so the
    // profile holds no CRL for the certificates it issues.
    let unlisted = root.intermediate("Unlisted Intermediate");
    let home_a = unlisted.issue_for(&[HOME_A]).presenting(&[unlisted.der()]);

    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::Path(webpki::Error::UnknownIssuer)
    );

    // The same chain verifies where the peer's intermediates are used: on
    // the public profile with the same root.
    let as_public = public_inventory(&[&root]);
    let stream = attempt(&as_public, (HOME_A, &home_a), (PEER, &peer));
    assert!(stream.handshake.completed());
}
