#[path = "broker_test_support/cred_iq.rs"]
mod cred_iq;

use cred_iq::CredIqTestHarness;
use waddle_broker::DestructionLedgerStore;
use waddle_server::credential_iq::stanza_to_xml;
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::waddle_credential::CredentialResponse;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

const SENTINEL: &str = "sk-waddle-sm-replay-sentinel-77889900";

#[tokio::test]
async fn xep0198_sm_replay_is_idempotent_with_opaque_results_and_secret_free_sm_unacked() {
    let (harness, ledger) = CredIqTestHarness::new("k1").await;
    let sealed_xml = harness.seal_member_payload("sm-replay-nonce-1", SENTINEL.as_bytes());
    let ctx = harness.context("sm-chal");

    let mut sm = StreamManagementState::new();
    sm.enable("sm-stream-1".to_owned(), true, Some(300));

    let mut first_cred_id = None;
    for attempt in 0..3 {
        let payload =
            harness.build_enroll_element("sm-replay-nonce-1", sealed_xml.clone(), "sm-chal");
        let iq = harness.build_enroll_iq(&format!("sm-enr-{attempt}"), payload);
        let reply = harness.handler.handle_iq(&iq, &ctx).await;
        let reply_xml = stanza_to_xml(&Stanza::Iq(Box::new(reply.clone())));
        let _ = sm.record_outbound(reply_xml.clone(), SmEvictionPath::DirectOutbound);

        assert!(!reply_xml.contains(SENTINEL));
        assert!(!reply_xml.contains(&sealed_xml.ciphertext_b64));
        let Iq::Result {
            payload: Some(el), ..
        } = reply
        else {
            panic!("expected opaque result on attempt {attempt}");
        };
        let CredentialResponse::CredentialReceipt(rec) =
            CredentialResponse::try_from(&el).expect("receipt")
        else {
            panic!("expected CredentialReceipt");
        };
        match &first_cred_id {
            None => first_cred_id = Some(rec.credential_id),
            Some(expected) => assert_eq!(&rec.credential_id, expected),
        }
    }

    // Only 1 credential row created across 3 XEP-0198 replays.
    let stored = ledger
        .list_envelopes_for_tenant(&harness.tenant_id)
        .await
        .expect("list");
    assert_eq!(stored.len(), 1);

    // Every stanza in sm_unacked is opaque and secret-free.
    assert_eq!(sm.unacked_count(), 3);
    for entry in sm.stanzas_to_resend(0) {
        assert!(!entry.stanza_xml.contains(SENTINEL));
        assert!(!entry.stanza_xml.contains(&sealed_xml.ciphertext_b64));
    }
}
