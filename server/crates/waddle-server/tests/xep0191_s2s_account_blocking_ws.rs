//! XEP-0191 (1.3) across S2S (ADR-0020 D7; gate G18): a local account's
//! blocking list applies to remote senders (§3.3: nothing is delivered from
//! a blocked JID, presence is never answered, and here a message is not
//! answered either, so that a blocked sender learns no more than a stranger
//! about the account), and to the account's own messages and presence for
//! remote addresses (`not-acceptable` with `<blocked/>`, nothing routed);
//! blocking a remote contact sends it unavailable presence through the
//! route, and removing a blocked contact sends it nothing. A recorded
//! subscription request from a sender the account blocks is not presented
//! again: blocking forgets it, and so does a denial even when the denial
//! itself is refused as blocked. Through the production `/ws` router, the
//! real S2S router and a loopback peer.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    block, client_settle, contact, error_condition, federated, get, holds, is_presence, jid,
    message, pending_request, ping, presence, receive, receive_until, send, Pki, BOB, BOB_PHONE,
    PEER,
};
use waddle_ws_test_support::iq::round_trip;
use xmpp_parsers::blocking::Block;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ALICE: &str = "alice@example.com";
const ALICE_PHONE: &str = "alice@example.com/phone";
const NS_BLOCKING_ERRORS: &str = waddle_xmpp::protocol::handlers::errors::NS_BLOCKING_ERRORS;
/// Another account at the peer, never blocked: the address of a client's
/// barrier on the pair.
const CAROL: &str = "carol@home-b.test";

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
    let mark = federated.peer_mark();

    let mut chat = Message::chat(Some(jid(BOB)));
    chat.id = Some(Id("to-blocked".to_owned()));
    chat.bodies.insert(Lang::new(), "hi".to_owned());
    send(&mut phone, chat).await;

    let refusal = receive(&mut phone, "message", "to-blocked").await;
    assert_eq!(error_condition(&refusal).as_deref(), Some("not-acceptable"));
    assert!(has_blocked(&refusal));
    let barrier = federated.client_barrier(&mut phone, CAROL).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(received.is_empty(), "{received:?}");
}

#[tokio::test]
async fn a_subscription_request_to_a_blocked_remote_address_changes_no_roster() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    block(&federated, ALICE, PEER).await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mark = federated.peer_mark();

    let mut request = Presence::new(PresenceType::Subscribe);
    request.to = Some(jid(BOB));
    request.id = Some("sub-blocked".to_owned());
    send(&mut phone, request).await;

    let refusal = receive(&mut phone, "presence", "sub-blocked").await;
    assert_eq!(error_condition(&refusal).as_deref(), Some("not-acceptable"));
    assert!(has_blocked(&refusal));
    // The whole peer domain is blocked, so no stanza of the client's can
    // serve as the barrier. A stanza routed for the request would have
    // been queued before the client was answered; a ping on the pair,
    // queued after it, is the barrier.
    let barrier = federated.peer_ping_barrier(CAROL).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(received.is_empty(), "{received:?}");
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
    let mark = federated.peer_mark();

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
    let barrier = federated.peer_barrier(BOB_PHONE, ALICE).await;

    // The refused request is the only answer, as for any stranger.
    let received = federated.peer_received_besides(mark, &barrier);
    assert_eq!(received.len(), 1, "{received:?}");
    assert_eq!(received[0].attr("id"), Some("b4"));
    assert_eq!(
        error_condition(&received[0]).as_deref(),
        Some("service-unavailable")
    );
    let seen = client_settle(&mut phone).await;
    assert!(!holds(&seen, "message", "b1"));
    assert!(!holds(&seen, "presence", "b3"));
    assert!(!holds(&seen, "iq", "b4"));
    assert_eq!(support::offline_count(&federated, ALICE).await, 0);
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

#[tokio::test]
async fn removing_a_blocked_remote_contact_sends_it_nothing() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    contact(&federated, ALICE, BOB, "both").await;
    block(&federated, ALICE, BOB).await;
    let mut phone = federated.online("alice", "phone", 5).await;
    let mark = federated.peer_mark();

    let remove = Iq::from_set(
        "remove-blocked-bob",
        xmpp_parsers::roster::Roster {
            ver: None,
            items: vec![xmpp_parsers::roster::Item {
                jid: BOB.parse().expect("a bare JID"),
                name: None,
                subscription: xmpp_parsers::roster::Subscription::Remove,
                ask: Default::default(),
                groups: Vec::new(),
            }],
        },
    );
    let reply = round_trip(&mut phone, remove).await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");

    let barrier = federated.client_barrier(&mut phone, CAROL).await;
    let received = federated.peer_received_besides(mark, &[barrier]);
    assert!(received.is_empty(), "{received:?}");
}

/// Bob asks Alice, offline, for a subscription: the request is recorded.
async fn recorded_request(federated: &support::Federated) {
    federated
        .from_peer(presence(BOB, ALICE, PresenceType::Subscribe, "ask"))
        .await;
    federated.peer_barrier(BOB, ALICE).await;
    assert!(pending_request(federated, ALICE, BOB).await);
}

/// Carol, never blocked, asks Alice for a subscription after Bob did. A new
/// session receives the recorded requests oldest first, so Carol's arrives
/// after Bob's would.
async fn later_request(federated: &support::Federated, id: &str) {
    federated
        .from_peer(presence(CAROL, ALICE, PresenceType::Subscribe, id))
        .await;
    federated.peer_barrier(CAROL, ALICE).await;
    assert!(pending_request(federated, ALICE, CAROL).await);
}

/// Every stanza a new session receives up to Carol's request `id`.
async fn presented_until(
    client: &mut waddle_ws_test_support::WsXmppClient,
    id: &str,
) -> Vec<Element> {
    receive_until(client, |frame| {
        frame.name() == "presence" && frame.attr("id") == Some(id)
    })
    .await
}

#[tokio::test]
async fn a_recorded_request_from_a_sender_the_account_then_blocks_is_not_presented_again() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    recorded_request(&federated).await;
    later_request(&federated, "carol-ask").await;
    // Blocked by a write of the list alone, as another replica's block
    // would look here.
    block(&federated, ALICE, BOB).await;

    let mut phone = federated.online("alice", "phone", 5).await;
    let presented = presented_until(&mut phone, "carol-ask").await;
    assert!(!holds(&presented, "presence", "ask"), "{presented:?}");
}

#[tokio::test]
async fn blocking_a_sender_forgets_its_recorded_request() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    recorded_request(&federated).await;
    let mut phone = federated.online("alice", "phone", 5).await;
    receive(&mut phone, "presence", "ask").await;

    let reply = round_trip(
        &mut phone,
        Iq::from_set(
            "block-asker",
            Block {
                items: vec![jid(BOB)],
            },
        ),
    )
    .await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");
    assert!(!pending_request(&federated, ALICE, BOB).await);

    // Unblocked again, the request does not come back.
    let reply = round_trip(
        &mut phone,
        Iq::from_set(
            "unblock-asker",
            xmpp_parsers::blocking::Unblock {
                items: vec![jid(BOB)],
            },
        ),
    )
    .await;
    assert!(matches!(reply, Iq::Result { .. }), "{reply:?}");
    later_request(&federated, "carol-ask").await;
    let mut laptop = federated.online("alice", "laptop", 5).await;
    let presented = presented_until(&mut laptop, "carol-ask").await;
    assert!(!holds(&presented, "presence", "ask"), "{presented:?}");
}

#[tokio::test]
async fn denying_a_blocked_senders_request_forgets_it_though_the_denial_is_refused() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    federated.account("alice").await;
    recorded_request(&federated).await;
    block(&federated, ALICE, BOB).await;
    let mut phone = federated.online("alice", "phone", 5).await;

    let mut denial = Presence::new(PresenceType::Unsubscribed);
    denial.to = Some(jid(BOB));
    denial.id = Some("deny".to_owned());
    send(&mut phone, denial).await;
    let refusal = receive_until(&mut phone, |frame| {
        frame.name() == "presence" && frame.attr("id") == Some("deny")
    })
    .await;
    let refusal = refusal.last().expect("the denial is answered");
    assert_eq!(error_condition(refusal).as_deref(), Some("not-acceptable"));
    assert!(has_blocked(refusal));
    assert!(!pending_request(&federated, ALICE, BOB).await);
}
