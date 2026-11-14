//! Matrix row "Resource/cancellation in the verifier", first half:
//! oversized DER, name and extension inventories are refused at the
//! ceilings, and the worst permitted path search ends in a rejection.

use rcgen::{GeneralSubtree, NameConstraints};
use rustls::server::danger::ClientCertVerifier;
use waddle_server::s2s::verify::limits::{
    MAX_CERTIFICATE_DER_BYTES, MAX_CERTIFICATE_EXTENSIONS, MAX_CHAIN_CERTIFICATES,
    MAX_CHAIN_DER_BYTES, MAX_NAME_CONSTRAINT_SUBTREES, MAX_SUBJECT_ALT_NAMES,
};
use waddle_server::s2s::verify::{
    PrivateCaMaterial, PrivateCaProfile, TrustErrorKind, VerifyError,
};

use crate::harness::{
    attempt, public_inventory, refused, settings, verified, verifier, Fixture, HOME_A, PEER,
};
use crate::pki::{ca_params, leaf_params, padding_extension, unix, Authority, Credential, NOW};

#[test]
fn a_chain_with_more_certificates_than_the_ceiling_is_refused() {
    for fixture in Fixture::both() {
        // The leaf and one certificate fewer than the ceiling are
        // accepted for reading; one more is not.
        let extras = |count: usize| -> Vec<_> {
            (0..count)
                .map(|index| Authority::root(&format!("Extra {index}")).der())
                .collect()
        };
        let at_ceiling = extras(MAX_CHAIN_CERTIFICATES - 1);
        let beyond_ceiling = extras(MAX_CHAIN_CERTIFICATES);

        for accepted in fixture.in_both_roles(HOME_A, |issuer| {
            issuer.issue_for(&[HOME_A]).presenting(&at_ceiling)
        }) {
            assert_eq!(verified(accepted).profile(), fixture.kind);
        }
        for failure in fixture.in_both_roles(HOME_A, |issuer| {
            issuer.issue_for(&[HOME_A]).presenting(&beyond_ceiling)
        }) {
            assert_eq!(refused(failure), VerifyError::ChainTooLong);
        }
    }
}

/// A CA certificate of about `bytes` bytes.
fn padded_certificate(name: &str, bytes: usize) -> rustls::pki_types::CertificateDer<'static> {
    let mut params = ca_params(name);
    params.custom_extensions.push(padding_extension(1, bytes));
    Authority::root_with(params).der()
}

#[test]
fn a_chain_with_more_bytes_than_the_ceiling_is_refused() {
    // rustls itself refuses a certificate message this large, so the
    // verifier is called directly, as rustls would call it.
    let fixture = Fixture::public();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let padded: Vec<_> = (0..5)
        .map(|index| padded_certificate(&format!("Padded {index}"), 13 * 1024))
        .collect();
    let total: usize = home_a.leaf().len() + padded.iter().map(|der| der.len()).sum::<usize>();
    assert!(padded
        .iter()
        .all(|der| der.len() <= MAX_CERTIFICATE_DER_BYTES));
    assert!(total > MAX_CHAIN_DER_BYTES);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);

    let result = receiving.verify_client_cert(home_a.leaf(), &padded, unix(NOW));

    assert!(result.is_err());
    assert_eq!(
        refused(receiving.verification()),
        VerifyError::ChainTooLarge
    );
}

#[test]
fn a_certificate_larger_than_the_ceiling_is_refused() {
    for fixture in Fixture::both() {
        let failures = fixture.in_both_roles(HOME_A, |issuer| {
            let mut params = leaf_params(&[HOME_A]);
            params
                .custom_extensions
                .push(padding_extension(1, MAX_CERTIFICATE_DER_BYTES));
            issuer.issue(params)
        });

        for failure in failures {
            assert_eq!(refused(failure), VerifyError::CertificateTooLarge);
        }
    }
}

#[test]
fn an_oversized_certificate_among_the_intermediates_is_refused() {
    let fixture = Fixture::public();
    let oversized = padded_certificate("Oversized", MAX_CERTIFICATE_DER_BYTES);

    let failures = fixture.in_both_roles(HOME_A, |issuer| {
        issuer
            .issue_for(&[HOME_A])
            .presenting(std::slice::from_ref(&oversized))
    });

    for failure in failures {
        assert_eq!(refused(failure), VerifyError::CertificateTooLarge);
    }
}

fn leaf_with_extensions(issuer: &Authority, extra: usize) -> Credential {
    let mut params = leaf_params(&[HOME_A]);
    for arc in 0..extra {
        params.custom_extensions.push(padding_extension(
            u64::try_from(arc).expect("a small arc"),
            1,
        ));
    }
    issuer.issue(params)
}

#[test]
fn a_certificate_with_more_extensions_than_the_ceiling_is_refused() {
    for fixture in Fixture::both() {
        // The leaf profile itself uses four extensions: authority key
        // identifier, subject alternative name, key usage and extended key
        // usage.
        let at_ceiling = MAX_CERTIFICATE_EXTENSIONS - 4;
        for accepted in
            fixture.in_both_roles(HOME_A, |issuer| leaf_with_extensions(issuer, at_ceiling))
        {
            assert_eq!(verified(accepted).profile(), fixture.kind);
        }
        for failure in fixture.in_both_roles(HOME_A, |issuer| {
            leaf_with_extensions(issuer, at_ceiling + 1)
        }) {
            assert_eq!(refused(failure), VerifyError::TooManyExtensions);
        }
    }
}

fn leaf_with_names(issuer: &Authority, count: usize) -> Credential {
    let mut names = vec![HOME_A.to_owned()];
    names.extend((1..count).map(|index| format!("n{index}.home-a.test")));
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    issuer.issue_for(&names)
}

#[test]
fn a_certificate_with_more_names_than_the_ceiling_is_refused() {
    for fixture in Fixture::both() {
        for accepted in fixture.in_both_roles(HOME_A, |issuer| {
            leaf_with_names(issuer, MAX_SUBJECT_ALT_NAMES)
        }) {
            assert_eq!(verified(accepted).profile(), fixture.kind);
        }
        for failure in fixture.in_both_roles(HOME_A, |issuer| {
            leaf_with_names(issuer, MAX_SUBJECT_ALT_NAMES + 1)
        }) {
            assert_eq!(refused(failure), VerifyError::TooManyNames);
        }
    }
}

/// A CA under `root` whose nameConstraints permit [`HOME_A`] and enough
/// further names to make `count` subtrees.
fn intermediate_with_subtrees(root: &Authority, count: usize) -> Authority {
    let mut permitted = vec![GeneralSubtree::DnsName(HOME_A.to_owned())];
    permitted.extend((1..count).map(|index| GeneralSubtree::DnsName(format!("d{index}.test"))));
    let mut params = ca_params("Heavily Constrained Intermediate");
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: permitted,
        excluded_subtrees: Vec::new(),
    });
    root.intermediate_with(params)
}

#[test]
fn a_certificate_with_more_name_constraint_subtrees_than_the_ceiling_is_refused() {
    let root = Authority::root("Substituted Public Root");
    let inventory = public_inventory(&[&root]);
    let peer = root.issue_for(&[PEER]);

    let at_ceiling = intermediate_with_subtrees(&root, MAX_NAME_CONSTRAINT_SUBTREES);
    let home_a = at_ceiling
        .issue_for(&[HOME_A])
        .presenting(&[at_ceiling.der()]);
    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert!(stream.handshake.completed());

    let beyond_ceiling = intermediate_with_subtrees(&root, MAX_NAME_CONSTRAINT_SUBTREES + 1);
    let home_a = beyond_ceiling
        .issue_for(&[HOME_A])
        .presenting(&[beyond_ceiling.der()]);
    let stream = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert_eq!(refused(stream.of_initiator()), VerifyError::TooManyNames);

    // The same CA is refused as part of a private-CA profile.
    let material = PrivateCaMaterial {
        anchors: vec![root.der()],
        intermediates: vec![beyond_ceiling.der()],
        crls: vec![root.clean_crl(), beyond_ceiling.clean_crl()],
    };
    let error = PrivateCaProfile::load(&material, &settings(NOW).algorithms(), unix(NOW))
        .expect_err("too many name constraint subtrees");
    assert_eq!(error.kind(), TrustErrorKind::TooManyNames);
}

#[test]
fn an_oversized_ca_certificate_is_refused_at_startup() {
    let mut params = ca_params("Oversized Root");
    params
        .custom_extensions
        .push(padding_extension(1, MAX_CERTIFICATE_DER_BYTES));
    let root = Authority::root_with(params);
    let material = PrivateCaMaterial {
        anchors: vec![root.der()],
        intermediates: Vec::new(),
        crls: vec![root.clean_crl()],
    };

    let error = PrivateCaProfile::load(&material, &settings(NOW).algorithms(), unix(NOW))
        .expect_err("an oversized CA certificate");

    assert_eq!(error.kind(), TrustErrorKind::CertificateTooLarge);
}

/// The worst path search the chain ceiling permits: every certificate the
/// peer sends has the trust anchor's name, so each is a candidate issuer of
/// every other, and none chains to the anchor's key. webpki explores the
/// orderings until its signature budget is spent and then gives up; the
/// verification ends with a rejection instead of running on.
#[test]
fn the_worst_permitted_path_search_ends_in_a_rejection() {
    const NAME: &str = "Ambiguous CA";
    let anchor = Authority::root(NAME);
    let inventory = public_inventory(&[&anchor]);
    let peer = anchor.issue_for(&[PEER]);
    let lookalikes: Vec<Authority> = (0..MAX_CHAIN_CERTIFICATES - 1)
        .map(|_| Authority::root(NAME))
        .collect();
    // Each lookalike is certified by the next one, in a ring.
    let ring: Vec<_> = (0..lookalikes.len())
        .map(|index| lookalikes[(index + 1) % lookalikes.len()].cross_sign(&lookalikes[index]))
        .collect();
    let home_a = lookalikes[0].issue_for(&[HOME_A]).presenting(&ring);
    assert_eq!(home_a.chain.len(), MAX_CHAIN_CERTIFICATES);

    let as_initiator = attempt(&inventory, (HOME_A, &home_a), (PEER, &peer));
    assert!(!as_initiator.handshake.completed());
    assert_eq!(
        refused(as_initiator.of_initiator()),
        VerifyError::Path(webpki::Error::MaximumSignatureChecksExceeded)
    );

    let as_receiver = attempt(&inventory, (PEER, &peer), (HOME_A, &home_a));
    assert!(!as_receiver.handshake.completed());
    assert_eq!(
        refused(as_receiver.of_receiver()),
        VerifyError::Path(webpki::Error::MaximumSignatureChecksExceeded)
    );
}
