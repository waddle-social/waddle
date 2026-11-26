#[path = "broker_test_support/cred_iq.rs"]
mod cred_iq;

use cred_iq::CredIqTestHarness;
use jid::{DomainPart, FullJid, Jid};
use waddle_server::credential_iq::{
    bounce_iq_error_xml_with_payload, bounce_outgoing_block_frames, bounce_undeliverable_iq_reply,
    bounce_unroutable_iq_reply, stanza_to_xml,
};
use waddle_server::destination::UnroutableReason;
use waddle_xmpp::stream_management::StreamManagementState;
use waddle_xmpp::telemetry::attributes::SmEvictionPath;
use waddle_xmpp::waddle_credential::NS_WADDLE_CREDENTIAL;
use waddle_xmpp::Stanza;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const PAIRWISE_SENTINEL: &str = "sk-waddle-pairwise-sealed-sentinel-f0e1d2c3";

#[tokio::test]
async fn pairwise_sealed_enroll_and_all_four_bounce_strip_paths() {
    let (harness, _ledger) = CredIqTestHarness::new("pw-k1").await;
    let sealed_payload = harness.seal_member_payload("pw-nonce-1", PAIRWISE_SENTINEL.as_bytes());
    let ct_b64 = sealed_payload.ciphertext_b64.clone();
    let enroll_payload = harness.build_enroll_element("pw-nonce-1", sealed_payload, "pw-chal");
    let iq = harness.build_enroll_iq("pw-enr-1", enroll_payload.clone());

    // 1. Happy-path enrollment succeeds and returns opaque receipt.
    let ctx = harness.context("pw-chal");
    let reply = harness.handler.handle_iq(&iq, &ctx).await;
    let reply_xml = stanza_to_xml(&Stanza::Iq(Box::new(reply)));
    assert!(!reply_xml.contains(PAIRWISE_SENTINEL));
    assert!(!reply_xml.contains(&ct_b64));

    // 2. If the same sealed IQ is misrouted through ANY of the 4 bounce paths,
    // neither the ciphertext nor the credential namespace is echoed back.
    let mut sm = StreamManagementState::new();
    sm.enable("pw-sm".to_owned(), true, Some(300));

    let b1 = stanza_to_xml(
        &bounce_undeliverable_iq_reply(&Stanza::Iq(Box::new(iq.clone()))).expect("b1"),
    );
    let sender: Jid = "alice@example.com/web".parse().expect("sender");
    let home = DomainPart::new("example.com").expect("home");
    let b2 = stanza_to_xml(&Stanza::Iq(Box::new(
        bounce_unroutable_iq_reply(&iq, &sender, &home, UnroutableReason::FederationDisabled)
            .expect("b2"),
    )));
    let sender_full: FullJid = "alice@example.com/web".parse().expect("full");
    let target: Jid = "example.com".parse().expect("target");
    let b3 = bounce_outgoing_block_frames(&iq, &sender_full, &target)
        .into_iter()
        .next()
        .expect("b3");
    let b4 = bounce_iq_error_xml_with_payload(
        "pw-b4",
        Some("example.com"),
        Some("alice@example.com/web"),
        enroll_payload,
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "en",
            "unavailable",
        ),
    );

    for xml in [&b1, &b2, &b3, &b4] {
        assert!(!xml.contains(PAIRWISE_SENTINEL));
        assert!(!xml.contains(&ct_b64));
        assert!(!xml.contains(NS_WADDLE_CREDENTIAL));
        let _ = sm.record_outbound(xml.clone(), SmEvictionPath::DirectOutbound);
    }

    for entry in sm.stanzas_to_resend(0) {
        assert!(!entry.stanza_xml.contains(PAIRWISE_SENTINEL));
        assert!(!entry.stanza_xml.contains(&ct_b64));
    }
}
