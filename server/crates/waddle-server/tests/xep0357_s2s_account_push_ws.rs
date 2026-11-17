//! XEP-0357 (0.4.1) for direct messages from remote accounts across S2S
//! (ADR-0020 D7; gate G18): a remote sender's `chat` message to an account
//! with no resource is accepted through the ingress authority as the
//! recipient's half of a local direct message, so it is archived, stored as
//! a row that refers to the archived copy, and raises the push notification
//! candidate a local sender's message to the same account raises: keyed by
//! the conversation with the sender's bare JID and the archived copy, with
//! the same class and the same summary. Nothing answers the remote sender.
//! Through the production `/ws` router, the real S2S router and a loopback
//! peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use jid::Jid;
use support::{federated, jid, message, send, Pki, BOB, BOB_PHONE};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

const ALICE: &str = "alice@example.com";
const CAROL: &str = "carol@example.com";

#[tokio::test]
async fn a_remote_message_stored_offline_refers_to_the_archive_and_raises_a_local_dms_push_candidate(
) {
    use waddle_xmpp::pending_delivery::PendingPayload;

    let pki = Pki::new();
    let federated = federated(&pki).await;
    let alice = federated.account("alice").await;
    federated.account("carol").await;
    let mut carol = federated.online("carol", "desk", 0).await;
    let mark = federated.peer_mark();

    federated
        .peer_sends(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "push-remote",
            "are you there",
        ))
        .await;
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;
    assert!(federated.peer_received_besides(mark, &barrier).is_empty());
    // The same message from a local account, which the remote one is
    // compared with.
    let mut local = Message::chat(Some(jid(ALICE)));
    local.id = Some(Id("push-local".to_owned()));
    local.bodies.insert(Lang::new(), "are you there".to_owned());
    send(&mut carol, local).await;

    // Both stored rows refer to Alice's archive.
    let remote = support::wait_for_archived(&federated, ALICE, "push-remote").await;
    let local = support::wait_for_archived(&federated, ALICE, "push-local").await;
    assert_eq!(remote.from, jid(BOB_PHONE));
    let rows = support::routing::wait_for_async(support::ARRIVES, || async {
        let rows = support::offline_rows(&federated, ALICE).await;
        (rows.len() == 2).then_some(rows)
    })
    .await
    .expect("both messages are stored for Alice's next session");
    let mut stored: Vec<String> = Vec::new();
    for row in &rows {
        let PendingPayload::Archived(stamp) = &row.payload else {
            panic!("a stored row refers to the archive: {rows:?}");
        };
        assert_eq!(stamp.by, Jid::from(alice.clone()));
        stored.push(stamp.id.clone());
    }
    stored.sort();
    let mut archived = vec![remote.id.clone(), local.id.clone()];
    archived.sort();
    assert_eq!(stored, archived);

    // Each raises the push notification candidate of a direct message to
    // an account with no resource, keyed by its conversation and its
    // archived copy.
    let candidates = support::routing::wait_for_async(support::ARRIVES, || async {
        let candidates: Vec<_> = federated
            .loopback
            .state
            .deps
            .protocol
            .notification_outbox
            .pending_candidates(1_000)
            .await
            .expect("the notification candidates are readable")
            .into_iter()
            .filter(|candidate| candidate.recipient_bare_jid() == &alice)
            .collect();
        (candidates.len() == 2).then_some(candidates)
    })
    .await
    .expect("both messages raise a notification candidate for Alice");
    let conversation = |peer: &str| {
        let peer: jid::BareJid = peer.parse().expect("a peer JID");
        candidates
            .iter()
            .find(|candidate| candidate.conversation_jid() == &peer)
            .unwrap_or_else(|| panic!("a candidate for the conversation with {peer}"))
    };
    let from_bob = conversation(BOB);
    let from_carol = conversation(CAROL);
    assert_eq!(from_bob.sender_jid(), &jid(BOB_PHONE));
    assert_eq!(
        from_bob.archive_stanza_id(),
        &StanzaId::new(remote.id.clone(), Jid::from(alice.clone()))
    );
    assert_eq!(
        from_carol.archive_stanza_id(),
        &StanzaId::new(local.id.clone(), Jid::from(alice.clone()))
    );
    assert_eq!(from_bob.class(), from_carol.class());
    assert_eq!(from_bob.last_message_body(), from_carol.last_message_body());
}
