//! The inbox of a local account for direct messages from remote accounts
//! across S2S (ADR-0020 D7; gate G18; `urn:xmpp:inbox:1`): a `chat` or
//! `normal` message from a remote sender with a resource is accepted
//! through the ingress authority as the recipient's half of a local direct
//! message, so it is projected into the recipient's inbox, with the remote
//! sender's bare JID as the conversation's peer, the recipient archive's id
//! as its last message and one more unread message, whether a live
//! resource takes it or the offline store keeps it. The sender's inbox and
//! activity are its own server's: no inbox or notification activity row is
//! kept here for the remote account. Through the production `/ws` router,
//! the real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use support::{federated, message, receive, Federated, Pki, BOB, BOB_PHONE};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use xmpp_parsers::message::MessageType;

const ALICE: &str = "alice@example.com";

/// The inbox rows `owner` holds.
async fn inbox(federated: &Federated, owner: &str) -> Vec<InboxEntry> {
    let owner: jid::BareJid = owner.parse().expect("an owner JID");
    federated
        .loopback
        .state
        .deps
        .protocol
        .inbox_storage
        .list(&owner)
        .await
        .expect("the inbox is readable")
}

/// Wait until `owner`'s inbox holds the conversation with `peer` and its
/// last message is the one the archive of `owner` holds as `last`; the
/// row.
async fn wait_for_conversation(
    federated: &Federated,
    owner: &str,
    peer: &str,
    last: &str,
) -> InboxEntry {
    let peer: jid::BareJid = peer.parse().expect("a peer JID");
    let archived = support::wait_for_archived(federated, owner, last).await;
    support::routing::wait_for_async(support::ARRIVES, || async {
        inbox(federated, owner)
            .await
            .into_iter()
            .find(|entry| entry.partner == peer && entry.last_stanza_id == archived.id)
    })
    .await
    .unwrap_or_else(|| panic!("{owner}'s inbox holds the conversation with {peer}"))
}

/// How many notification activity rows `owner` holds.
async fn activity_rows(federated: &Federated, owner: &str) -> i64 {
    let rows = federated
        .loopback
        .state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(waddle_server::db::actor::DbQuery {
            sql: "SELECT COUNT(*) FROM notification_activity WHERE owner_bare_jid = ?".to_owned(),
            params: vec![owner.to_owned().into()],
        })
        .await
        .expect("the notification activity is readable");
    match rows.first().and_then(|row| row.first()) {
        Some(waddle_server::db::Value::Integer(count)) => *count,
        other => panic!("a row count: {other:?}"),
    }
}

/// Nothing of the remote sender's own conversation is kept here.
async fn assert_nothing_kept_for_the_sender(federated: &Federated) {
    let senders = inbox(federated, BOB).await;
    assert!(senders.is_empty(), "{senders:?}");
    assert_eq!(activity_rows(federated, BOB).await, 0);
}

#[tokio::test]
async fn a_remote_message_to_an_online_account_projects_the_recipients_inbox() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .peer_sends(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "inbox-online",
            "hello alice",
        ))
        .await;
    receive(&mut phone, "message", "inbox-online").await;

    let entry = wait_for_conversation(&federated, ALICE, BOB, "inbox-online").await;
    assert_eq!(entry.kind, ConversationKind::Direct);
    assert_eq!(entry.unread, 1, "{entry:?}");
    assert_eq!(entry.thread_id, None, "{entry:?}");
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert_nothing_kept_for_the_sender(&federated).await;
}

#[tokio::test]
async fn a_remote_message_to_an_offline_account_projects_the_recipients_inbox() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;

    for id in ["inbox-offline-1", "inbox-offline-2"] {
        federated
            .peer_sends(message(BOB_PHONE, ALICE, MessageType::Chat, id, "hello"))
            .await;
    }
    federated.peer_barrier(BOB_PHONE, ALICE).await;

    let entry = wait_for_conversation(&federated, ALICE, BOB, "inbox-offline-2").await;
    assert_eq!(entry.kind, ConversationKind::Direct);
    assert_eq!(entry.unread, 2, "{entry:?}");
    assert_eq!(support::offline_count(&federated, ALICE).await, 2);
    assert_nothing_kept_for_the_sender(&federated).await;
}
