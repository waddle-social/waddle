//! XEP-0280 (1.0.1) for messages exchanged with remote accounts across S2S
//! (ADR-0020 D7; gate G18): a message from a remote contact delivered to
//! one resource is copied as a `<received/>` carbon to the account's other
//! Carbons-enabled resources (§6), and a message to a remote contact the
//! route takes is copied as a `<sent/>` carbon (§7). Through the production
//! `/ws` router, the real S2S router and a loopback peer.
//!
//! Each negative case ends with a sentinel that is copied: the first carbon
//! the observing resource receives must be the sentinel's.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use jid::Jid;
use minidom::Element;
use support::{federated, full_jid, jid, message, receive, send, Federated, Pki, BOB, BOB_PHONE};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::carbons::CARBONS_NS;
use xmpp_parsers::carbons::{Enable, Received, Sent};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

const ALICE: &str = "alice@example.com";

/// Alice online as `phone` (priority 5) and as `laptop` (priority 1) with
/// Carbons enabled on the laptop.
async fn alice(federated: &Federated) -> (WsXmppClient, WsXmppClient) {
    federated.account("alice").await;
    let phone = federated.online("alice", "phone", 5).await;
    let mut laptop = federated.online("alice", "laptop", 1).await;
    let enabled = round_trip(&mut laptop, Iq::from_set("laptop-carbons", Enable)).await;
    assert!(matches!(enabled, Iq::Result { .. }), "{enabled:?}");
    (phone, laptop)
}

/// The first carbon wrapped in `<name xmlns='urn:xmpp:carbons:2'/>` that
/// reaches `client`, checked for its addressing, and the message it
/// forwards.
async fn first_carbon(client: &mut WsXmppClient, name: &str) -> Message {
    let carbon = support::receive_matching(client, |element| {
        element.name() == "message" && element.get_child(name, CARBONS_NS).is_some()
    })
    .await;
    let carbon = Message::try_from(carbon).expect("a typed carbon");
    let observer = full_jid(client);
    assert_eq!(carbon.from, Some(Jid::from(observer.to_bare())));
    let forwarded = carbon.payloads.into_iter().find_map(|payload| {
        if name == "sent" {
            Sent::try_from(payload).ok().map(|sent| sent.forwarded)
        } else {
            Received::try_from(payload)
                .ok()
                .map(|received| received.forwarded)
        }
    });
    forwarded.expect("a typed carbon").message
}

fn id_of(message: &Message) -> Option<&str> {
    message.id.as_ref().map(|id| id.0.as_str())
}

fn outgoing(to: &str, id: &str) -> Message {
    let mut message = Message::chat(Some(jid(to)));
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), "from alice".to_owned());
    message
}

#[tokio::test]
async fn a_message_from_a_remote_contact_is_copied_as_a_received_carbon() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice(&federated).await;

    federated
        .from_peer(message(BOB_PHONE, ALICE, MessageType::Chat, "in1", "hello"))
        .await;

    receive(&mut phone, "message", "in1").await;
    let copy = first_carbon(&mut laptop, "received").await;
    assert_eq!(id_of(&copy), Some("in1"));
    assert_eq!(copy.from, Some(jid(BOB_PHONE)));
}

#[tokio::test]
async fn a_remote_message_marked_private_is_not_copied() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice(&federated).await;

    let mut private = message(BOB_PHONE, ALICE, MessageType::Chat, "in2", "only here");
    private
        .payloads
        .push(Element::builder("private", CARBONS_NS).build());
    federated.from_peer(private).await;
    receive(&mut phone, "message", "in2").await;
    federated
        .from_peer(message(
            BOB_PHONE,
            ALICE,
            MessageType::Chat,
            "in2-sentinel",
            "copied",
        ))
        .await;

    let copy = first_carbon(&mut laptop, "received").await;
    assert_eq!(id_of(&copy), Some("in2-sentinel"));
}

#[tokio::test]
async fn a_message_to_a_remote_contact_is_copied_as_a_sent_carbon() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice(&federated).await;

    send(&mut phone, outgoing(BOB, "out1")).await;

    federated.peer_receives("out1").await;
    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("out1"));
    assert_eq!(copy.from, Some(Jid::from(full_jid(&phone))));
    assert_eq!(copy.to, Some(jid(BOB)));
}

#[tokio::test]
async fn a_message_the_route_refuses_is_not_copied() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice(&federated).await;

    let mut pin = outgoing(BOB, "out2");
    pin.payloads.push(
        Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
    );
    send(&mut phone, pin).await;
    receive(&mut phone, "message", "out2").await;
    send(&mut phone, outgoing(BOB, "out2-sentinel")).await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("out2-sentinel"));
}
