//! XEP-0191 (1.3) across S2S (ADR-0020 D7; gate G18): a local account's
//! blocking list applies to remote senders (§3.3: nothing is delivered from
//! a blocked JID, presence is never answered, and here a message is not
//! answered either, so that a blocked sender learns no more than a stranger
//! about the account), and to the account's own messages and presence for
//! remote addresses (`not-acceptable` with `<blocked/>`, nothing routed);
//! blocking a remote contact sends it unavailable presence through the
//! route. Through the production `/ws` router, the real S2S router and a
//! loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    block, contact, error_condition, federated, get, is_presence, jid, message, ping, presence,
    receive, send, Pki, BOB, BOB_PHONE, PEER,
};
use waddle_ws_test_support::iq::round_trip;
use xmpp_parsers::blocking::Block;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ALICE: &str = "alice@example.com";
const ALICE_PHONE: &str = "alice@example.com/phone";
const NS_BLOCKING_ERRORS: &str = waddle_xmpp::protocol::handlers::errors::NS_BLOCKING_ERRORS;

fn has_blocked(stanza: &Element) -> bool {
    stanza
        .children()
        .find(|child| child.name() == "error")
        .is_some_and(|error| error.has_child("blocked", NS_BLOCKING_ERRORS))
}

#[tokio::test]
async fn a_message_to_a_blocked_remote_address_is_refused_and_not_routed() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    block(&federated, ALICE, BOB).await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut chat = Message::chat(Some(jid(BOB)));
    chat.id = Some(Id("to-blocked".to_owned()));
    chat.bodies.insert(Lang::new(), "hi".to_owned());
    send(&mut phone, chat).await;

    let refusal = receive(&mut phone, "message", "to-blocked").await;
    assert_eq!(error_condition(&refusal).as_deref(), Some("not-acceptable"));
    assert!(has_blocked(&refusal));
    assert!(federated
        .peer_received_after_quiet("to-blocked")
        .await
        .is_empty());
}

#[tokio::test]
async fn a_subscription_request_to_a_blocked_remote_address_changes_no_roster() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    block(&federated, ALICE, PEER).await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut request = Presence::new(PresenceType::Subscribe);
    request.to = Some(jid(BOB));
    request.id = Some("sub-blocked".to_owned());
    send(&mut phone, request).await;

    let refusal = receive(&mut phone, "presence", "sub-blocked").await;
    assert_eq!(error_condition(&refusal).as_deref(), Some("not-acceptable"));
    assert!(has_blocked(&refusal));
    assert!(federated
        .peer_received_after_quiet("sub-blocked")
        .await
        .is_empty());
    assert_eq!(support::roster_item(&federated, ALICE, BOB).await, None);
}

#[tokio::test]
async fn nothing_from_a_blocked_remote_sender_is_delivered_or_answered_beyond_a_stranger() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    block(&federated, ALICE, BOB).await;
    let mut phone = federated.online("alice", "phone", 5).await;

    federated
        .from_peer(message(BOB_PHONE, ALICE, MessageType::Chat, "b1", "hello"))
        .await;
    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Probe, "b2"))
        .await;
    federated
        .from_peer(presence(BOB_PHONE, ALICE, PresenceType::None, "b3"))
        .await;
    federated
        .from_peer(get(BOB_PHONE, ALICE_PHONE, "b4", ping()))
        .await;

    let refusal = federated.peer_receives("b4").await;
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(federated.peer_received_after_quiet("b1").await.is_empty());
    assert!(federated
        .peer_received(|stanza| stanza.name() == "presence" && stanza.attr("to") == Some(BOB))
        .is_empty());
    assert!(!support::arrives_quietly(&mut phone, "message", "b1").await);
    assert!(!support::arrives_quietly(&mut phone, "presence", "b3").await);
    assert!(!support::arrives_quietly(&mut phone, "iq", "b4").await);
}

#[tokio::test]
async fn blocking_a_remote_contact_sends_it_unavailable_through_the_route() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "from").await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let reply = round_trip(
        &mut phone,
        Iq::from_set(
            "block-bob",
            Block {
                items: vec![jid(BOB)],
            },
        ),
    )
    .await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");

    support::wait_for(support::ARRIVES, || {
        federated
            .peer_received(|stanza| {
                is_presence(stanza, Some("unavailable"))
                    && stanza.attr("from") == Some(ALICE_PHONE)
                    && stanza.attr("to") == Some(BOB)
            })
            .into_iter()
            .next()
    })
    .await
    .expect("the blocked contact receives unavailable");
}
