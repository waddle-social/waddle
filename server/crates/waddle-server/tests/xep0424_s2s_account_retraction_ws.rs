//! XEP-0424 (0.5.0) for direct messages from remote accounts across S2S
//! (ADR-0020 D7; gate G18): a remote sender's retraction is accepted
//! through the ingress authority as the recipient's half of a local direct
//! message, so the recipient's archive replaces the message it names with
//! a tombstone. A retraction is processed only when it and the original
//! message come from the same bare JID (Business Rules); a local sender's
//! own server checks that, and for a remote sender this deployment checks
//! it against the recipient's archive. A retraction that
//! names another sender's message changes nothing in the archive, is not
//! delivered, and nothing answers its sender. Through the production `/ws`
//! router, the real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use support::{
    federated, full_jid, holds, message, receive, send, Federated, Pki, BOB_PHONE, MALLORY,
};
use waddle_xmpp::xep::xep0424::build_retract_element;
use xmpp_parsers::message::{Message, MessageType};

const ALICE: &str = "alice@example.com";

/// A XEP-0424 retraction from `from` to `to` of the message with the wire
/// id `retracts`.
fn retraction(from: &str, to: &str, id: &str, retracts: &str) -> Message {
    let mut message = message(from, to, MessageType::Chat, id, "retracted a message");
    message.payloads.push(build_retract_element(retracts));
    message
}

/// Whether `archive` holds the message with the wire id `id` as a
/// XEP-0424 tombstone.
async fn tombstoned(federated: &Federated, archive: &str, id: &str) -> bool {
    support::archived(federated, archive, id)
        .await
        .and_then(|archived| archived.rich)
        .is_some_and(|rich| rich.is_tombstoned())
}

#[tokio::test]
async fn a_remote_retraction_of_another_senders_message_is_dropped_unanswered() {
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
            "keep this",
        ))
        .await;
    receive(&mut phone, "message", "bobs-message").await;
    let before = support::wait_for_archived(&federated, ALICE, "bobs-message").await;
    let mark = federated.peer_mark();

    // Mallory names Bob's message.
    federated
        .from_peer(retraction(
            MALLORY,
            ALICE,
            "mallory-retracts",
            "bobs-message",
        ))
        .await;
    let barrier = federated.peer_barrier(MALLORY, ALICE).await;

    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    let seen = federated.client_settle(&mut phone).await;
    assert!(!holds(&seen, "message", "mallory-retracts"), "{seen:?}");
    let after = support::archived(&federated, ALICE, "bobs-message")
        .await
        .expect("Bob's message is still archived");
    assert!(!after.rich.as_ref().is_some_and(|rich| rich.is_tombstoned()));
    assert_eq!(after.id, before.id);
    assert_eq!(after.body.as_deref(), Some("keep this"));
    assert_eq!(after.stanza_xml, before.stanza_xml);
}

#[tokio::test]
async fn a_remote_retraction_of_the_local_accounts_message_is_dropped_unanswered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    federated.account("carol").await;
    let mut phone = federated.online("alice", "phone", 5).await;
    // Alice's own message, to another local account, is in her archive.
    let from = full_jid(&phone).to_string();
    send(
        &mut phone,
        message(
            &from,
            "carol@example.com",
            MessageType::Chat,
            "alices-message",
            "alice wrote this",
        ),
    )
    .await;
    let before = support::wait_for_archived(&federated, ALICE, "alices-message").await;
    let mark = federated.peer_mark();

    // Bob names Alice's message by its id.
    federated
        .from_peer(retraction(
            BOB_PHONE,
            ALICE,
            "bob-retracts-alices",
            "alices-message",
        ))
        .await;
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;

    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    let seen = federated.client_settle(&mut phone).await;
    assert!(!holds(&seen, "message", "bob-retracts-alices"), "{seen:?}");
    assert!(!tombstoned(&federated, ALICE, "alices-message").await);
    let after = support::archived(&federated, ALICE, "alices-message")
        .await
        .expect("Alice's message is still archived");
    assert_eq!(after.id, before.id);
    assert_eq!(after.body.as_deref(), Some("alice wrote this"));
}

#[tokio::test]
async fn a_remote_retraction_of_the_senders_own_message_tombstones_it() {
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
    let mark = federated.peer_mark();

    federated
        .from_peer(retraction(BOB_PHONE, ALICE, "bob-retracts", "bobs-typo"))
        .await;
    receive(&mut phone, "message", "bob-retracts").await;
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;

    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    assert!(
        support::routing::wait_for_async(support::ARRIVES, || async {
            tombstoned(&federated, ALICE, "bobs-typo")
                .await
                .then_some(())
        })
        .await
        .is_some(),
        "Bob's own message is tombstoned in Alice's archive"
    );
    let archived = support::archived(&federated, ALICE, "bobs-typo")
        .await
        .expect("the tombstone keeps the archived message's place");
    assert_eq!(archived.body, None);
}
