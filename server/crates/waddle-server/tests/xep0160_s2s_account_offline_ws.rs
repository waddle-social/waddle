//! XEP-0160 offline storage of messages from remote senders across S2S
//! (ADR-0020 D7; gate G18; RFC 6121 §8.5.2.2.1): a `chat` or `normal`
//! message no resource of the local account takes is stored for the
//! account's next session, from a contact and a stranger alike, and
//! nothing answers its sender. A stranger is held to bounds per sender,
//! sender domain and account; a message beyond them is dropped silently,
//! and a contact is not held to them. A detached XEP-0198 session hands
//! what it holds to the offline store when its resume window closes, so a
//! stranger's message to one, by full JID or bare JID, is held only within
//! the same bounds, and what the store would not keep (a chat state alone,
//! a `headline` other than an event notification the account asked for,
//! an `error`, directed presence) reaches it only from a contact. A stored message whose sender's domain is no longer admitted
//! (ADR-0020 D1) is deleted before the store is flushed, unless a flush
//! claimed it after it was read. Through the
//! production `/ws` router, the real S2S router and a loopback peer,
//! `home-b.test`.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use support::{contact, federated, holds, message, presence, Pki, BOB, BOB_PHONE, MALLORY};
use xmpp_parsers::chatstates::ChatState;
use xmpp_parsers::message::{Id, Message, MessageType};
use xmpp_parsers::presence::Type as PresenceType;

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
        .peer_sends(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "o1",
            "are you there",
        ))
        .await;
    federated
        .peer_sends(message(
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
            .peer_sends(message(MALLORY, ALICE, MessageType::Chat, &id, "hello"))
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
        .peer_sends(message(
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

/// A message from `from` to `to` that carries only a chat state
/// (XEP-0085), which the offline store does not keep.
fn chat_state(from: &str, to: &str, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(support::jid(to)));
    message.from = Some(support::jid(from));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(ChatState::Composing.into());
    message
}

#[tokio::test]
async fn a_strangers_messages_to_a_detached_full_jid_are_held_within_its_offline_bound() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let phone = federated.detached("alice", "phone", 5).await;
    let to = phone.to_string();
    let mark = federated.peer_mark();
    let bound = waddle_server::db::s2s_accounts::MAX_OFFLINE_PER_SENDER;

    // What the offline store would not keep never reaches the detached
    // session from a stranger, and uses none of its share.
    federated
        .peer_sends(chat_state(MALLORY, &to, "state"))
        .await;
    federated
        .peer_sends(message(MALLORY, &to, MessageType::Headline, "news", "hi"))
        .await;
    federated
        .peer_sends(message(MALLORY, &to, MessageType::Error, "oops", "hi"))
        .await;
    federated
        .peer_sends(presence(MALLORY, &to, PresenceType::None, "here"))
        .await;
    // One more than its share by full JID: the last is dropped.
    for index in 0..=bound {
        let id = ["held-", &index.to_string()].concat();
        federated
            .peer_sends(message(MALLORY, &to, MessageType::Chat, &id, "hello"))
            .await;
    }
    let barrier = federated.peer_barrier(MALLORY, ALICE).await;

    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    assert!(
        federated.is_detached(&phone).await,
        "{phone} is still detached, so nothing reached the offline store instead"
    );
    let held = federated.held_from(&phone, MALLORY).await;
    let ids: Vec<&str> = held.iter().filter_map(|stanza| stanza.attr("id")).collect();
    let expected: Vec<String> = (0..bound)
        .map(|index| ["held-", &index.to_string()].concat())
        .collect();
    assert_eq!(ids, expected, "the session holds the stranger's share only");
    assert_eq!(support::offline_count(&federated, ALICE).await, 0);

    // The share is spent for the account's offline store too.
    federated
        .peer_sends(message(MALLORY, ALICE, MessageType::Chat, "bare", "hello"))
        .await;
    federated.peer_barrier(MALLORY, ALICE).await;
    assert_eq!(federated.held_from(&phone, MALLORY).await.len(), bound);
    assert_eq!(support::offline_count(&federated, ALICE).await, 0);
}

#[tokio::test]
async fn a_contacts_stanzas_reach_a_detached_full_jid_beyond_a_strangers_bound() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    let phone = federated.detached("alice", "phone", 5).await;
    let to = phone.to_string();
    let bound = waddle_server::db::s2s_accounts::MAX_OFFLINE_PER_SENDER;

    federated
        .peer_sends(chat_state(BOB_PHONE, &to, "state"))
        .await;
    for index in 0..=bound {
        let id = ["contact-", &index.to_string()].concat();
        federated
            .peer_sends(message(BOB_PHONE, &to, MessageType::Chat, &id, "hello"))
            .await;
    }
    federated.peer_barrier(BOB_PHONE, ALICE).await;

    assert!(federated.is_detached(&phone).await);
    let held = federated.held_from(&phone, BOB_PHONE).await;
    assert_eq!(held.len(), bound + 2, "the chat state and every message");
    assert!(held
        .iter()
        .all(|stanza| stanza.attr("to") == Some(to.as_str())));
}

#[tokio::test]
async fn a_stored_message_whose_senders_domain_is_no_longer_admitted_is_never_delivered() {
    use waddle_xmpp::pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId};

    let pki = Pki::new();
    let federated = federated(&pki).await;
    let alice = federated.account("alice").await;
    // Mallory's message is stored the way the account side stores it.
    federated
        .peer_sends(message(MALLORY, ALICE, MessageType::Chat, "kept", "hi"))
        .await;
    federated.peer_barrier(MALLORY, ALICE).await;
    assert_eq!(support::offline_count(&federated, ALICE).await, 1);
    // A message stored while its sender's domain was admitted, which the
    // snapshot the replica runs no longer admits.
    let stale = message(
        "eve@unlisted.test/desk",
        ALICE,
        MessageType::Chat,
        "stale",
        "hi",
    );
    let inserted = federated
        .loopback
        .state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: alice,
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(stale)),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("the offline store is writable");
    assert_eq!(inserted, InsertOutcome::Inserted);
    assert_eq!(support::offline_count(&federated, ALICE).await, 2);

    let mut phone = federated.online("alice", "phone", 5).await;
    let frames = support::receive_messages(&mut phone, &["kept"]).await;
    let settled = federated.client_settle(&mut phone).await;

    assert!(!holds(&frames, "message", "stale"), "{frames:?}");
    assert!(!holds(&settled, "message", "stale"), "{settled:?}");
    // Neither row is left: one was delivered, the other deleted.
    assert!(
        support::routing::wait_for_async(support::ARRIVES, || async {
            (support::offline_count(&federated, ALICE).await == 0).then_some(())
        })
        .await
        .is_some(),
        "the offline store is empty"
    );
}

#[tokio::test]
async fn the_purge_of_an_unadmitted_senders_message_leaves_a_row_a_flush_claimed() {
    use waddle_server::pending_delivery::DatabasePendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::{
        PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
    };

    // The purge reads the account's rows and deletes those of a domain no
    // longer admitted; a flush may claim one between the read and the
    // delete, and that row is then the flush's to deliver or release.
    let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
        .await
        .expect("pending storage");
    let account: jid::BareJid = ALICE.parse().expect("an account JID");
    let stored = |id: &str| PendingRow {
        id: PendingRowId::fresh(),
        recipient: account.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(message(
            MALLORY,
            ALICE,
            MessageType::Chat,
            id,
            "are you there",
        ))),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let claimed = stored("claimed");
    let claimed_id = claimed.id.clone();
    storage.insert(claimed).await.expect("the row is stored");
    let session = SmSessionId::new("flushing-session");
    let taken = storage
        .claim_for_session(&account, &session)
        .await
        .expect("the flush claims the row");
    assert_eq!(taken.len(), 1);
    assert_eq!(
        storage
            .delete_unclaimed_row(&claimed_id)
            .await
            .expect("the purge runs"),
        0,
        "the purge deleted a row a flush had claimed"
    );
    assert_eq!(storage.list(&account).await.expect("rows").len(), 1);

    // A row nobody claimed is deleted.
    let unclaimed = stored("unclaimed");
    let unclaimed_id = unclaimed.id.clone();
    storage.insert(unclaimed).await.expect("the row is stored");
    assert_eq!(
        storage
            .delete_unclaimed_row(&unclaimed_id)
            .await
            .expect("the purge runs"),
        1
    );
    let left: Vec<PendingRowId> = storage
        .list(&account)
        .await
        .expect("rows")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(left, vec![claimed_id]);
}
