//! XEP-0313 (1.1.3) for messages from remote accounts across S2S
//! (ADR-0020 D7; gate G18): a `chat` or `normal` message from a remote
//! sender with a resource is accepted through the ingress authority as the
//! recipient's half of a local direct message, so it is written once to the
//! recipient's archive, stamped by the recipient's bare JID, whether a live
//! resource takes it, a detached session holds it or the offline store
//! keeps it, and a query of the account's own archive returns it from the
//! remote sender. A stored message is a row that refers to that archived
//! copy, and the account's next session receives it with the archive's
//! XEP-0359 id. Nothing is archived under the remote sender's bare JID: its
//! archive is kept by its own server. A message carrying only a chat state
//! is not archived, as for a local sender. A XEP-0424 retraction from a
//! remote sender tombstones the archived message it names only when that
//! sender wrote it; one naming another sender's message is dropped, and
//! nothing answers its sender. A `<result/>` wrapper of an archive query in
//! a remote sender's message names the account's own archive and is
//! removed before delivery. Through the production `/ws` router, the real
//! S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use jid::Jid;
use minidom::Element;
use support::{
    federated, holds, jid, message, receive, receive_matching, receive_until, send, Federated, Pki,
    BOB, BOB_PHONE, MALLORY,
};
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::xep::xep0424::build_retract_element;
use waddle_xmpp::xep::NS_MAM;
use xmpp_parsers::chatstates::ChatState;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Message, MessageType};

const ALICE: &str = "alice@example.com";
const CAROL: &str = "carol@example.com";
const NS_FORWARD: &str = "urn:xmpp:forward:0";

/// A message from `from` to `to` that carries only a chat state
/// (XEP-0085).
fn chat_state(from: &str, to: &str, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(jid(to)));
    message.from = Some(jid(from));
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(ChatState::Composing.into());
    message
}

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
async fn a_remote_contacts_message_is_in_the_recipients_archive() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "archived-1",
            "keep this",
        ))
        .await;
    receive(&mut phone, "message", "archived-1").await;

    send(
        &mut phone,
        Iq::Set {
            from: None,
            to: None,
            id: "mam-1".to_owned(),
            payload: Element::builder("query", NS_MAM)
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q-1")
                .build(),
        },
    )
    .await;
    let result = receive_matching(&mut phone, |element| {
        element.name() == "message"
            && element
                .get_child("result", NS_MAM)
                .and_then(|result| result.get_child("forwarded", NS_FORWARD))
                .and_then(|forwarded| forwarded.get_child("message", "jabber:client"))
                .is_some_and(|archived| archived.attr("id") == Some("archived-1"))
    })
    .await;
    let archived = result
        .get_child("result", NS_MAM)
        .and_then(|result| result.get_child("forwarded", NS_FORWARD))
        .and_then(|forwarded| forwarded.get_child("message", "jabber:client"))
        .expect("the archived message");
    assert_eq!(archived.attr("from"), Some(BOB_PHONE));
    receive(&mut phone, "iq", "mam-1").await;
}

#[tokio::test]
async fn a_remote_message_stored_offline_is_archived_and_received_with_the_archives_id() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let alice = federated.account("alice").await;
    let mark = federated.peer_mark();

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "offline-1",
            "while you were away",
        ))
        .await;
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());

    // The recipient's archive stamps the message, and the stored row refers
    // to that copy instead of holding one of its own.
    let archived = support::wait_for_archived(&federated, ALICE, "offline-1").await;
    assert_eq!(archived.from, jid(BOB_PHONE));
    let rows = support::offline_rows(&federated, ALICE).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let PendingPayload::Archived(stamp) = &rows[0].payload else {
        panic!("the stored row refers to the archive: {rows:?}");
    };
    assert_eq!(stamp.by, Jid::from(alice));
    assert_eq!(stamp.id, archived.id);

    // The next session receives it carrying the id the archive gave it.
    let mut phone = federated.online("alice", "phone", 5).await;
    let delivered = receive(&mut phone, "message", "offline-1").await;
    assert_eq!(delivered.attr("from"), Some(BOB_PHONE));
    assert!(
        support::stanza_ids(delivered.children()).contains(&(ALICE.to_owned(), archived.id)),
        "{delivered:?}"
    );
}

#[tokio::test]
async fn a_remote_message_held_by_a_detached_session_is_archived_once() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let phone = federated.detached("alice", "phone", 5).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "held-1",
            "for when you are back",
        ))
        .await;
    federated.peer_barrier(BOB_PHONE, ALICE).await;

    let held = federated.held_from(&phone, BOB_PHONE).await;
    let ids: Vec<&str> = held.iter().filter_map(|stanza| stanza.attr("id")).collect();
    assert_eq!(ids, vec!["held-1"], "{held:?}");
    let archived = support::wait_for_archived(&federated, ALICE, "held-1").await;
    assert_eq!(archived.from, jid(BOB_PHONE));
    assert_eq!(
        support::archived_copies(&federated, ALICE, "held-1").await,
        1
    );
    // The session holds the copy the recipient pass stamped.
    assert!(
        support::stanza_ids(held[0].children()).contains(&(ALICE.to_owned(), archived.id)),
        "{held:?}"
    );
    assert_eq!(support::offline_count(&federated, ALICE).await, 0);
}

#[tokio::test]
async fn a_remote_chat_state_alone_is_not_archived() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    federated.account("carol").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    // To a live resource the chat state is delivered and not archived; the
    // message after it, on the same sender's queue, is archived.
    federated
        .from_peer(chat_state(BOB_PHONE, ALICE, "state-online"))
        .await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "after-state",
            "and now some words",
        ))
        .await;
    let frames = receive_until(&mut phone, |frame| {
        frame.name() == "message" && frame.attr("id") == Some("after-state")
    })
    .await;
    assert!(holds(&frames, "message", "state-online"), "{frames:?}");
    support::wait_for_archived(&federated, ALICE, "after-state").await;
    assert!(support::archived(&federated, ALICE, "state-online")
        .await
        .is_none());

    // To an account with no resource it is neither archived nor stored.
    federated
        .from_peer(chat_state(BOB_PHONE, CAROL, "state-offline"))
        .await;
    federated.peer_barrier(BOB_PHONE, CAROL).await;
    assert!(support::archived(&federated, CAROL, "state-offline")
        .await
        .is_none());
    assert_eq!(support::offline_count(&federated, CAROL).await, 0);
}

#[tokio::test]
async fn nothing_is_archived_under_the_remote_senders_bare_jid() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    federated.account("carol").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "to-online",
            "hello",
        ))
        .await;
    federated
        .from_peer(message(
            BOB_PHONE,
            CAROL,
            MessageType::Chat,
            "to-offline",
            "hello",
        ))
        .await;
    receive(&mut phone, "message", "to-online").await;
    federated.peer_barrier(BOB_PHONE, CAROL).await;

    // Each recipient's archive holds its copy; the sender's archive is its
    // own server's.
    support::wait_for_archived(&federated, ALICE, "to-online").await;
    support::wait_for_archived(&federated, CAROL, "to-offline").await;
    let senders = support::archive_messages(&federated, BOB).await;
    assert!(senders.is_empty(), "{senders:?}");
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

#[tokio::test]
async fn an_archive_result_wrapper_in_a_remote_message_is_removed_before_delivery() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut forged = message(BOB_PHONE, ALICE, MessageType::Chat, "forged-result", "hi");
    forged.payloads.push(
        Element::builder("result", NS_MAM)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "q-forged")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "forged-archive-id",
            )
            .append(Element::builder("forwarded", NS_FORWARD).build())
            .build(),
    );
    federated.from_peer(forged).await;

    let delivered = receive(&mut phone, "message", "forged-result").await;
    assert!(
        delivered.get_child("result", NS_MAM).is_none(),
        "{delivered:?}"
    );
}
