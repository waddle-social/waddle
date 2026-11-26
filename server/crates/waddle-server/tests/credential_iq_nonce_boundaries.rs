#[path = "broker_test_support/cred_iq.rs"]
mod cred_iq;

use cred_iq::CredIqTestHarness;
use waddle_server::credential_iq::stanza_to_xml;
use waddle_xmpp::waddle_credential::{CredentialResponse, SealedPayloadXml};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

#[tokio::test]
async fn duplicate_nonce_is_idempotent_while_mutated_ciphertext_fails_closed_with_conflict() {
    let (harness, _ledger) = CredIqTestHarness::new("k1").await;
    let sealed_a = harness.seal_member_payload("shared-nonce-42", b"sk-secret-alpha");
    let sealed_b = harness.seal_member_payload("shared-nonce-42", b"sk-secret-beta-mutated");
    let ctx = harness.context("nonce-chal");

    let make_iq = |id: &str, sealed: SealedPayloadXml| {
        let payload = harness.build_enroll_element("shared-nonce-42", sealed, "nonce-chal");
        harness.build_enroll_iq(id, payload)
    };

    // First enrollment succeeds.
    let r1 = harness
        .handler
        .handle_iq(&make_iq("n-1", sealed_a.clone()), &ctx)
        .await;
    let Iq::Result {
        payload: Some(el1), ..
    } = r1
    else {
        panic!("expected result");
    };
    let CredentialResponse::CredentialReceipt(rec1) =
        CredentialResponse::try_from(&el1).expect("rec1")
    else {
        panic!("expected receipt");
    };

    // Identical replay with same nonce + same ciphertext returns identical receipt.
    let r2 = harness
        .handler
        .handle_iq(&make_iq("n-2", sealed_a), &ctx)
        .await;
    let Iq::Result {
        payload: Some(el2), ..
    } = r2
    else {
        panic!("expected idempotent result");
    };
    let CredentialResponse::CredentialReceipt(rec2) =
        CredentialResponse::try_from(&el2).expect("rec2")
    else {
        panic!("expected receipt");
    };
    assert_eq!(rec1.credential_id, rec2.credential_id);
    assert_eq!(rec1.version, rec2.version);

    // Same nonce + mutated ciphertext MUST fail closed with <conflict/> and payload: None.
    let r3 = harness
        .handler
        .handle_iq(&make_iq("n-3", sealed_b.clone()), &ctx)
        .await;
    let r3_xml = stanza_to_xml(&Stanza::Iq(Box::new(r3.clone())));
    assert!(!r3_xml.contains(&sealed_b.ciphertext_b64));
    match r3 {
        Iq::Error {
            payload: None,
            error,
            ..
        } => {
            assert_eq!(error.defined_condition, DefinedCondition::Conflict);
        }
        other => panic!("expected opaque Conflict error, got {other:?}"),
    }
}
