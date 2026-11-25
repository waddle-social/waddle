//! XEP-0280 (1.0.1) for messages exchanged with remote accounts across S2S
//! (ADR-0020 D7; gate G18): a message from a remote sender is accepted for
//! one resource, the one most available for a message to the bare JID or
//! the one a full JID names, and copied as a `<received/>` carbon to the
//! account's other Carbons-enabled resources (§6), never to the resource
//! that receives the message itself; a message to a remote contact the
//! route takes is copied as a `<sent/>` carbon (§7). A `<sent/>` or
//! `<received/>` wrapper in a remote sender's message is a forgery of the
//! account's own traffic (§11) and is removed before delivery. Through the
//! production `/ws` router, the real S2S router and a loopback peer.
//!
//! Each negative case ends with a sentinel that is copied: the first carbon
//! the observing resource receives must be the sentinel's.
#![cfg(feature = "test-support")]

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_account_support/mod.rs"]
mod support;

use jid::{FullJid, Jid};
use minidom::Element;
use support::{
    federated, full_jid, jid, message, receive, receive_until, send, Federated, Pki, BOB, BOB_PHONE,
};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::carbons::CARBONS_NS;
use xmpp_parsers::carbons::{Enable, Received, Sent};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

const ALICE: &str = "alice@example.com";
const NS_FORWARD: &str = "urn:xmpp:forward:0";

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

/// Alice online as `phone` (priority 5) and as `laptop` (priority 1), both
/// with Carbons enabled.
async fn alice_with_carbons_on_both(federated: &Federated) -> (WsXmppClient, WsXmppClient) {
    let (mut phone, laptop) = alice(federated).await;
    let enabled = round_trip(&mut phone, Iq::from_set("phone-carbons", Enable)).await;
    assert!(matches!(enabled, Iq::Result { .. }), "{enabled:?}");
    (phone, laptop)
}

/// Whether `frame` is a carbon copy, sent or received, of the message with
/// `id`.
fn is_carbon_of(frame: &Element, id: &str) -> bool {
    ["received", "sent"].iter().any(|name| {
        frame
            .get_child(name, CARBONS_NS)
            .and_then(|carbon| carbon.get_child("forwarded", NS_FORWARD))
            .and_then(|forwarded| forwarded.get_child("message", "jabber:client"))
            .is_some_and(|copy| copy.attr("id") == Some(id))
    })
}

/// Whether `frame` is the message with `id` itself, not a carbon of it.
fn is_original(frame: &Element, id: &str) -> bool {
    frame.name() == "message"
        && frame.attr("id") == Some(id)
        && frame.get_child("received", CARBONS_NS).is_none()
        && frame.get_child("sent", CARBONS_NS).is_none()
}

/// The first carbon wrapped in `<name xmlns='urn:xmpp:carbons:2'/>` that
/// reaches `client`, checked for its addressing, and the message it
/// forwards.
async fn first_carbon(client: &mut WsXmppClient, name: &str) -> Message {
    let carbon = support::receive_matching(client, |element| {
        element.name() == "message" && element.get_child(name, CARBONS_NS).is_some()
    })
    .await;
    copied(carbon, &full_jid(client), name)
}

/// The message the carbon `carbon`, wrapped in
/// `<name xmlns='urn:xmpp:carbons:2'/>`, forwards to `observer`, checked
/// for its addressing.
fn copied(carbon: Element, observer: &FullJid, name: &str) -> Message {
    let carbon = Message::try_from(carbon).expect("a typed carbon");
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
        .peer_sends(message(BOB_PHONE, ALICE, MessageType::Chat, "in1", "hello"))
        .await;

    receive(&mut phone, "message", "in1").await;
    let copy = first_carbon(&mut laptop, "received").await;
    assert_eq!(id_of(&copy), Some("in1"));
    assert_eq!(copy.from, Some(jid(BOB_PHONE)));
}

#[tokio::test]
async fn a_remote_message_to_the_bare_jid_is_copied_to_the_other_resource_only() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice_with_carbons_on_both(&federated).await;

    federated
        .peer_sends(message(BOB_PHONE, ALICE, MessageType::Chat, "in3", "hello"))
        .await;

    // The phone, the most available resource, receives the message itself
    // and the laptop a received carbon of it.
    let mut on_phone = receive_until(&mut phone, |frame| is_original(frame, "in3")).await;
    let mut on_laptop = receive_until(&mut laptop, |frame| is_carbon_of(frame, "in3")).await;
    let carbon = on_laptop.pop().expect("the laptop's carbon");
    let copy = copied(carbon, &full_jid(&laptop), "received");
    assert_eq!(id_of(&copy), Some("in3"));
    assert_eq!(copy.from, Some(jid(BOB_PHONE)));
    assert_eq!(copy.to, Some(jid(ALICE)));

    // Neither receives anything else of it.
    federated.peer_barrier(BOB_PHONE, ALICE).await;
    on_phone.extend(federated.client_settle(&mut phone).await);
    on_laptop.extend(federated.client_settle(&mut laptop).await);
    assert!(
        !on_phone.iter().any(|frame| is_carbon_of(frame, "in3")),
        "{on_phone:?}"
    );
    assert!(
        !on_laptop.iter().any(|frame| is_original(frame, "in3")),
        "{on_laptop:?}"
    );
}

#[tokio::test]
async fn a_remote_message_to_a_full_jid_is_copied_to_the_other_resource_only() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice_with_carbons_on_both(&federated).await;
    let to = full_jid(&laptop).to_string();

    federated
        .peer_sends(message(
            BOB_PHONE,
            &to,
            MessageType::Chat,
            "in4",
            "to the laptop",
        ))
        .await;

    // The laptop the message names receives it, though the phone is more
    // available, and the phone a received carbon of it.
    let mut on_laptop = receive_until(&mut laptop, |frame| is_original(frame, "in4")).await;
    let mut on_phone = receive_until(&mut phone, |frame| is_carbon_of(frame, "in4")).await;
    let carbon = on_phone.pop().expect("the phone's carbon");
    let copy = copied(carbon, &full_jid(&phone), "received");
    assert_eq!(id_of(&copy), Some("in4"));
    assert_eq!(copy.from, Some(jid(BOB_PHONE)));
    assert_eq!(copy.to, Some(jid(&to)));

    federated.peer_barrier(BOB_PHONE, ALICE).await;
    on_laptop.extend(federated.client_settle(&mut laptop).await);
    on_phone.extend(federated.client_settle(&mut phone).await);
    assert!(
        !on_laptop.iter().any(|frame| is_carbon_of(frame, "in4")),
        "{on_laptop:?}"
    );
    assert!(
        !on_phone.iter().any(|frame| is_original(frame, "in4")),
        "{on_phone:?}"
    );
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
    federated.peer_sends(private).await;
    receive(&mut phone, "message", "in2").await;
    federated
        .peer_sends(message(
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

/// A message the router refuses, which the sender answers with an error.
fn refused(to: &str, id: &str) -> Message {
    let mut pin = outgoing(to, id);
    pin.payloads.push(
        Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
    );
    pin
}

/// XEP-0280 section 6.1: "it is of type 'error' and it was sent in response
/// to a MESSAGE that was eligible for carbons delivery". The refused
/// message reaches the other resource as a `<sent/>` carbon and the error
/// as a `<received/>` carbon, in that order.
#[tokio::test]
async fn an_error_for_a_message_the_route_refuses_is_copied() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice(&federated).await;

    send(&mut phone, refused(BOB, "out2")).await;
    receive(&mut phone, "message", "out2").await;

    let sent = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&sent), Some("out2"));
    assert_eq!(sent.type_, MessageType::Chat);
    let error = first_carbon(&mut laptop, "received").await;
    assert_eq!(id_of(&error), Some("out2"));
    assert_eq!(error.type_, MessageType::Error);
    assert_eq!(error.to, Some(Jid::from(full_jid(&phone))));
}

#[tokio::test]
async fn an_error_for_a_private_message_the_route_refuses_is_not_copied() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, mut laptop) = alice(&federated).await;

    let mut private = refused(BOB, "out3");
    private
        .payloads
        .push(Element::builder("private", CARBONS_NS).build());
    send(&mut phone, private).await;
    receive(&mut phone, "message", "out3").await;
    send(&mut phone, outgoing(BOB, "out3-sentinel")).await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("out3-sentinel"));
}

#[tokio::test]
async fn carbon_wrappers_in_a_remote_message_are_removed_before_delivery() {
    let pki = Pki::new();
    let federated = federated(&pki).await;
    let (mut phone, _laptop) = alice(&federated).await;

    let mut forged = message(BOB_PHONE, ALICE, MessageType::Chat, "forged", "hello");
    for wrapper in ["sent", "received"] {
        forged.payloads.push(
            Element::builder(wrapper, CARBONS_NS)
                .append(
                    Element::builder("forwarded", "urn:xmpp:forward:0")
                        .append(Element::from(outgoing("mallory@home-b.test", "inner")))
                        .build(),
                )
                .build(),
        );
    }
    federated.peer_sends(forged).await;

    let delivered = receive(&mut phone, "message", "forged").await;
    assert!(
        delivered.get_child("sent", CARBONS_NS).is_none(),
        "{delivered:?}"
    );
    assert!(
        delivered.get_child("received", CARBONS_NS).is_none(),
        "{delivered:?}"
    );
}
