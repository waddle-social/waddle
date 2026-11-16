//! XEP-0160 offline storage of messages from remote senders across S2S
//! (ADR-0020 D7; gate G18; RFC 6121 §8.5.2.2.1): a `chat` or `normal`
//! message no resource of the local account takes is stored for the
//! account's next session, from a contact and a stranger alike, and
//! nothing answers its sender. A stranger is held to bounds per sender,
//! parent domain and account; a message beyond them is dropped silently,
//! and a contact is not held to them. Through the production `/ws` router,
//! the real S2S router and a loopback peer, `home-b.test`.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use support::{contact, federated, holds, message, Pki, BOB, BOB_PHONE, MALLORY};
use xmpp_parsers::message::MessageType;

const ALICE: &str = "alice@example.com";

#[tokio::test]
async fn a_message_to_an_account_with_no_resource_is_stored_unanswered_from_contact_and_stranger() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;
    let mark = federated.peer_mark();

    // Bob may see Alice's presence and Mallory may not; neither is told
    // anything (RFC 6121 §8.5.2.2.1: stored for later delivery).
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "o1",
            "are you there",
        ))
        .await;
    federated
        .from_peer(message(
            MALLORY,
            ALICE,
            MessageType::Normal,
            "o2",
            "are you there",
        ))
        .await;
    let mut barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;
    barrier.extend(federated.peer_barrier(MALLORY, ALICE).await);
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    assert_eq!(support::offline_count(&federated, ALICE).await, 2);

    // Alice's next session receives both.
    let mut phone = federated.online("alice", "phone", 5).await;
    let frames = support::receive_messages(&mut phone, &["o1", "o2"]).await;
    assert!(holds(&frames, "message", "o1"), "{frames:?}");
}

#[tokio::test]
async fn a_strangers_messages_beyond_its_offline_bound_are_dropped_unanswered() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mark = federated.peer_mark();
    let bound = waddle_server::db::s2s_accounts::MAX_OFFLINE_PER_SENDER;

    for index in 0..=bound {
        let id = ["flood-", &index.to_string()].concat();
        federated
            .from_peer(message(MALLORY, ALICE, MessageType::Chat, &id, "hello"))
            .await;
    }
    let barrier = federated.peer_barrier(MALLORY, ALICE).await;
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    assert_eq!(
        support::offline_count(&federated, ALICE).await,
        u32::try_from(bound).expect("a small bound")
    );

    // A contact is not held to a stranger's bound.
    contact(&federated, ALICE, BOB, "from").await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "contact",
            "hi",
        ))
        .await;
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert_eq!(
        support::offline_count(&federated, ALICE).await,
        u32::try_from(bound + 1).expect("a small bound")
    );
}
