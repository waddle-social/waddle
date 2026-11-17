//! XEP-0308 (1.2.1) for direct messages from remote accounts across S2S
//! (ADR-0020 D7; gate G18): a remote sender's correction is accepted
//! through the ingress authority as the recipient's half of a local direct
//! message. A correction may replace only a message its sender wrote
//! (Business Rules: in a direct conversation the bare JIDs must match); a
//! local sender's own server checks that, and for a remote sender
//! this deployment checks it against the recipient's archive. A correction
//! that names a message another sender wrote, the local account's own
//! among them, is not delivered or archived, and nothing answers its
//! sender. Through the production `/ws` router, the real S2S router and a
//! loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use support::{
    federated, full_jid, holds, message, receive, send, Federated, Pki, BOB_PHONE, MALLORY,
};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::xep::xep0308::build_replace_element;
use xmpp_parsers::message::{Message, MessageType};

const ALICE: &str = "alice@example.com";
const CAROL: &str = "carol@example.com";

/// A XEP-0308 correction from `from` to Alice, with the id `id`, of the
/// message with the id `replaces`.
fn correction(from: &str, id: &str, replaces: &str) -> Message {
    let mut message = message(from, ALICE, MessageType::Chat, id, "corrected");
    message.payloads.push(build_replace_element(replaces));
    message
}

/// Alice, online, has written `id` to Carol: her archive holds it as hers.
async fn alices_message(federated: &Federated, id: &str) -> WsXmppClient {
    federated.account("alice").await;
    federated.account("carol").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let from = full_jid(&phone).to_string();
    send(
        &mut phone,
        message(&from, CAROL, MessageType::Chat, id, "alice wrote this"),
    )
    .await;
    support::wait_for_archived(federated, ALICE, id).await;
    phone
}

#[tokio::test]
async fn a_remote_correction_of_the_local_accounts_message_is_dropped_unanswered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let mut phone = alices_message(&federated, "alices-message").await;
    let before = support::wait_for_archived(&federated, ALICE, "alices-message").await;
    let mark = federated.peer_mark();

    federated
        .from_peer(correction(BOB_PHONE, "bob-corrects", "alices-message"))
        .await;
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;

    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    let seen = federated.client_settle(&mut phone).await;
    assert!(!holds(&seen, "message", "bob-corrects"), "{seen:?}");
    assert!(support::archived(&federated, ALICE, "bob-corrects")
        .await
        .is_none());
    let after = support::archived(&federated, ALICE, "alices-message")
        .await
        .expect("Alice's message is still archived");
    assert_eq!(after.id, before.id);
    assert_eq!(after.body.as_deref(), Some("alice wrote this"));
}

#[tokio::test]
async fn a_remote_correction_of_another_remote_senders_message_is_dropped() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "bobs-message",
            "bob wrote this",
        ))
        .await;
    receive(&mut phone, "message", "bobs-message").await;
    support::wait_for_archived(&federated, ALICE, "bobs-message").await;

    federated
        .from_peer(correction(MALLORY, "mallory-corrects", "bobs-message"))
        .await;
    federated.peer_barrier(MALLORY, ALICE).await;

    let seen = federated.client_settle(&mut phone).await;
    assert!(!holds(&seen, "message", "mallory-corrects"), "{seen:?}");
    assert!(support::archived(&federated, ALICE, "mallory-corrects")
        .await
        .is_none());
}

#[tokio::test]
async fn a_remote_correction_of_the_senders_own_message_is_delivered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "bobs-typo",
            "teh",
        ))
        .await;
    receive(&mut phone, "message", "bobs-typo").await;
    support::wait_for_archived(&federated, ALICE, "bobs-typo").await;

    federated
        .from_peer(correction(BOB_PHONE, "bob-fixes", "bobs-typo"))
        .await;

    receive(&mut phone, "message", "bob-fixes").await;
    support::wait_for_archived(&federated, ALICE, "bob-fixes").await;
}
