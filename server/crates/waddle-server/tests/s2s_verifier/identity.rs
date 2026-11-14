//! Matrix row "DNS/service identity". Positive exact-name matches are
//! recorded separately from negative ones: a certificate authenticates only
//! the reference identity of the stream it is presented on, whatever other
//! names it carries.

use rcgen::{CertificateParams, DnType};
use waddle_server::s2s::verify::{MatchedIdentifier, Verification, VerifyError};
use waddle_xmpp::identity::CanonicalDomain;

use crate::harness::{
    attempt, domain, refused, verified, Fixture, HOME_A, HOME_B, IDN_A_LABEL, IDN_U_LABEL, MUC_A,
    PEER,
};
use crate::pki::{
    leaf_params, san_dns, san_extension, san_other_name, san_srv, san_xmpp_addr, tlv, Authority,
    Credential, OID_SRV_NAME, OID_XMPP_ADDR,
};

const IA5_STRING: u8 = 0x16;
const UTF8_STRING: u8 = 0x0c;

/// Issues the certificate a case presents.
type Issue = fn(&Authority) -> Credential;

/// A leaf whose subjectAltName holds exactly `entries`.
fn leaf_with_san(issuer: &Authority, entries: &[Vec<u8>]) -> Credential {
    let mut params = leaf_params(&[]);
    params.custom_extensions.push(san_extension(entries));
    issuer.issue(params)
}

/// Assert that `reference` is authenticated in both roles through
/// `expected` by the certificate `issue` returns, in both trust profiles.
fn assert_matches(
    reference: &str,
    expected: MatchedIdentifier,
    issue: impl Fn(&Authority) -> Credential,
) {
    for fixture in Fixture::both() {
        for verification in fixture.in_both_roles(reference, &issue) {
            let peer = verified(verification);
            assert_eq!(peer.matched_identifier(), expected, "{:?}", fixture.kind);
            assert_eq!(peer.domain(), &domain(reference), "{:?}", fixture.kind);
        }
    }
}

/// Assert that the certificate `issue` returns is refused with `expected`
/// for `reference` in both roles and both trust profiles.
fn assert_refused(
    reference: &str,
    expected: VerifyError,
    issue: impl Fn(&Authority) -> Credential,
) {
    for fixture in Fixture::both() {
        for verification in fixture.in_both_roles(reference, &issue) {
            assert_eq!(refused(verification), expected, "{:?}", fixture.kind);
        }
    }
}

// Positive matches.

#[test]
fn an_exact_dns_id_matches() {
    assert_matches(HOME_A, MatchedIdentifier::DnsId, |issuer| {
        issuer.issue_for(&[HOME_A])
    });
}

#[test]
fn a_dns_id_matches_without_regard_to_ascii_case() {
    assert_matches(HOME_A, MatchedIdentifier::DnsId, |issuer| {
        leaf_with_san(issuer, &[san_dns("Home-A.Test")])
    });
}

#[test]
fn a_wildcard_dns_id_matches_exactly_one_left_most_label() {
    assert_matches(MUC_A, MatchedIdentifier::DnsId, |issuer| {
        issuer.issue_for(&["*.home-a.test"])
    });
}

#[test]
fn a_certificate_with_several_names_matches_the_one_that_is_the_reference() {
    assert_matches(HOME_A, MatchedIdentifier::DnsId, |issuer| {
        issuer.issue_for(&[HOME_B, "alias.test", HOME_A])
    });
}

#[test]
fn an_srv_id_alone_matches() {
    assert_matches(HOME_A, MatchedIdentifier::SrvId, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-server.home-a.test")])
    });
}

#[test]
fn an_srv_id_matches_without_regard_to_ascii_case() {
    assert_matches(HOME_A, MatchedIdentifier::SrvId, |issuer| {
        leaf_with_san(issuer, &[san_srv("_XMPP-Server.Home-A.Test")])
    });
}

#[test]
fn an_xmpp_addr_alone_matches() {
    assert_matches(HOME_A, MatchedIdentifier::XmppAddr, |issuer| {
        leaf_with_san(issuer, &[san_xmpp_addr(HOME_A)])
    });
}

#[test]
fn a_dns_id_is_preferred_when_several_identifier_types_match() {
    assert_matches(HOME_A, MatchedIdentifier::DnsId, |issuer| {
        leaf_with_san(
            issuer,
            &[
                san_xmpp_addr(HOME_A),
                san_srv("_xmpp-server.home-a.test"),
                san_dns(HOME_A),
            ],
        )
    });
}

#[test]
fn u_label_and_a_label_references_match_the_same_names() {
    let u_label = CanonicalDomain::parse_exact(IDN_U_LABEL).expect("a U-label domain");
    assert_eq!(u_label, domain(IDN_A_LABEL));
    assert_eq!(u_label.as_str(), IDN_A_LABEL);

    let certificates: [(MatchedIdentifier, Issue); 4] = [
        (MatchedIdentifier::DnsId, |issuer| {
            issuer.issue_for(&[IDN_A_LABEL])
        }),
        (MatchedIdentifier::SrvId, |issuer| {
            leaf_with_san(issuer, &[san_srv("_xmpp-server.xn--bcher-kva.test")])
        }),
        // An XmppAddr is a UTF8String and may spell the domain either way.
        (MatchedIdentifier::XmppAddr, |issuer| {
            leaf_with_san(issuer, &[san_xmpp_addr(IDN_U_LABEL)])
        }),
        (MatchedIdentifier::XmppAddr, |issuer| {
            leaf_with_san(issuer, &[san_xmpp_addr(IDN_A_LABEL)])
        }),
    ];
    for (expected, issue) in certificates {
        // The harness canonicalizes the reference, so both spellings name
        // the same stream.
        for reference in [IDN_U_LABEL, IDN_A_LABEL] {
            let fixture = Fixture::private();
            for verification in fixture.in_both_roles(reference, issue) {
                let peer = verified(verification);
                assert_eq!(peer.matched_identifier(), expected);
                assert_eq!(peer.domain().as_str(), IDN_A_LABEL);
            }
        }
    }
}

// Negative matches.

#[test]
fn an_account_domain_certificate_does_not_authenticate_a_component_domain() {
    assert_refused(MUC_A, VerifyError::IdentityMismatch, |issuer| {
        issuer.issue_for(&[HOME_A])
    });
}

#[test]
fn a_component_domain_certificate_does_not_authenticate_its_account_domain() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        issuer.issue_for(&[MUC_A])
    });
}

#[test]
fn an_alias_in_the_certificate_never_becomes_the_authenticated_domain() {
    let fixture = Fixture::private();
    let peer = fixture.issuer.issue_for(&[PEER]);
    let with_alias = fixture.issuer.issue_for(&[HOME_A, HOME_B]);

    // Presented for home-a.test, the certificate authenticates home-a.test
    // and nothing else.
    let stream = attempt(&fixture.inventory, (HOME_A, &with_alias), (PEER, &peer));
    assert_eq!(verified(stream.of_initiator()).domain(), &domain(HOME_A));

    // A certificate that lacks the stream's source domain is refused even
    // though the inventory serves every name it carries.
    let other = fixture.issuer.issue_for(&[HOME_B, PEER]);
    let stream = attempt(&fixture.inventory, (HOME_A, &other), (PEER, &peer));
    assert_eq!(
        refused(stream.of_initiator()),
        VerifyError::IdentityMismatch
    );
}

#[test]
fn a_common_name_is_never_an_identifier() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        let mut params: CertificateParams = leaf_params(&[]);
        params.distinguished_name.push(DnType::CommonName, HOME_A);
        issuer.issue(params)
    });
}

#[test]
fn a_certificate_naming_only_the_srv_target_host_is_refused() {
    // SRV discovery may point home-a.test at this host; DNS never supplies
    // a reference identity.
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        issuer.issue_for(&["xmpp1.hosting.test"])
    });
}

#[test]
fn an_srv_id_for_another_service_is_refused() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-client.home-a.test")])
    });
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpps-server.home-a.test")])
    });
}

#[test]
fn an_srv_id_with_a_protocol_label_is_refused() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-server._tcp.home-a.test")])
    });
}

#[test]
fn an_srv_id_for_another_domain_is_refused() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-server.home-b.test")])
    });
    // Neither a parent nor a child of the reference domain matches.
    assert_refused(MUC_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-server.home-a.test")])
    });
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-server.muc.home-a.test")])
    });
}

#[test]
fn a_wildcard_does_not_match_its_parent_or_more_than_one_label() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        issuer.issue_for(&["*.home-a.test"])
    });
    assert_refused(MUC_A, VerifyError::IdentityMismatch, |issuer| {
        issuer.issue_for(&["*.test"])
    });
}

#[test]
fn a_wildcard_that_is_not_the_whole_left_most_label_is_ignored() {
    assert_refused(MUC_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_dns("m*.home-a.test"), san_dns("muc.*.test")])
    });
}

#[test]
fn a_wildcard_is_not_honoured_in_an_srv_id_or_an_xmpp_addr() {
    assert_refused(MUC_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_srv("_xmpp-server.*.home-a.test")])
    });
    assert_refused(MUC_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_xmpp_addr("*.home-a.test")])
    });
}

#[test]
fn an_xmpp_addr_with_a_localpart_or_resource_does_not_name_a_server() {
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_xmpp_addr("admin@home-a.test")])
    });
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_xmpp_addr("home-a.test/server")])
    });
    assert_refused(HOME_A, VerifyError::IdentityMismatch, |issuer| {
        leaf_with_san(issuer, &[san_xmpp_addr("home-a.test.")])
    });
}

#[test]
fn a_malformed_srv_id_fails_the_certificate_even_beside_a_matching_dns_id() {
    let malformed: [fn() -> Vec<u8>; 5] = [
        // The value is a UTF8String, not an IA5String.
        || san_other_name(OID_SRV_NAME, &tlv(UTF8_STRING, b"_xmpp-server.home-a.test")),
        // Bytes follow the string inside the explicit tag.
        || {
            let mut value = tlv(IA5_STRING, b"_xmpp-server.home-a.test");
            value.extend_from_slice(&[0x05, 0x00]);
            san_other_name(OID_SRV_NAME, &value)
        },
        // The IA5String holds a byte outside ASCII.
        || {
            san_other_name(
                OID_SRV_NAME,
                &tlv(IA5_STRING, b"_xmpp-server.home-\xc3\xa4.test"),
            )
        },
        // The name has no service label.
        || san_srv("home-a.test"),
        // The service label is empty.
        || san_srv("_.home-a.test"),
    ];
    for entry in malformed {
        assert_refused(HOME_A, VerifyError::MalformedIdentifier, |issuer| {
            leaf_with_san(issuer, &[san_dns(HOME_A), entry()])
        });
    }
}

#[test]
fn a_malformed_xmpp_addr_fails_the_certificate_even_beside_a_matching_dns_id() {
    let malformed: [fn() -> Vec<u8>; 3] = [
        // The value is an IA5String, not a UTF8String.
        || san_other_name(OID_XMPP_ADDR, &tlv(IA5_STRING, HOME_A.as_bytes())),
        // The UTF8String is not UTF-8.
        || san_other_name(OID_XMPP_ADDR, &tlv(UTF8_STRING, b"home-\xff.test")),
        // The value is not wrapped in the explicit tag.
        || {
            tlv(
                0xa0,
                &[
                    tlv(0x06, OID_XMPP_ADDR),
                    tlv(UTF8_STRING, HOME_A.as_bytes()),
                ]
                .concat(),
            )
        },
    ];
    for entry in malformed {
        assert_refused(HOME_A, VerifyError::MalformedIdentifier, |issuer| {
            leaf_with_san(issuer, &[san_dns(HOME_A), entry()])
        });
    }
}

#[test]
fn a_repeated_xmpp_identifier_fails_the_certificate() {
    assert_refused(HOME_A, VerifyError::DuplicateIdentifier, |issuer| {
        leaf_with_san(
            issuer,
            &[
                san_dns(HOME_A),
                san_srv("_xmpp-server.home-a.test"),
                san_srv("_xmpp-server.home-a.test"),
            ],
        )
    });
    assert_refused(HOME_A, VerifyError::DuplicateIdentifier, |issuer| {
        leaf_with_san(
            issuer,
            &[
                san_dns(HOME_A),
                san_xmpp_addr(HOME_A),
                san_xmpp_addr(HOME_A),
            ],
        )
    });
}

#[test]
fn distinct_srv_ids_for_several_domains_are_not_duplicates() {
    assert_matches(HOME_A, MatchedIdentifier::SrvId, |issuer| {
        leaf_with_san(
            issuer,
            &[
                san_srv("_xmpp-server.home-b.test"),
                san_srv("_xmpp-client.home-a.test"),
                san_srv("_xmpp-server.home-a.test"),
            ],
        )
    });
}

#[test]
fn a_refused_identity_reports_no_verified_peer() {
    let fixture = Fixture::private();
    let [as_receiver, as_initiator] =
        fixture.in_both_roles(HOME_A, |issuer| issuer.issue_for(&[HOME_B]));

    assert!(matches!(
        as_receiver,
        Verification::Failed(VerifyError::IdentityMismatch)
    ));
    assert!(matches!(
        as_initiator,
        Verification::Failed(VerifyError::IdentityMismatch)
    ));
}
