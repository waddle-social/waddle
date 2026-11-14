//! ADR-0020 D7 until account traffic across S2S is reviewed (gate G18):
//! remote traffic to a local account is refused as if the account did not
//! exist (RFC 6121 §8.5.1). Messages and IQ requests receive
//! `service-unavailable`, presence of every type is ignored, and no error
//! answers an error, a response or anything else the account never asked
//! for. Every local account destination here is one no reviewed handler
//! serves.
//!
//! `home-a.test` runs the S2S router; `home-b.test` sends through its own
//! connection manager and keeps what it receives. No PostgreSQL, SpiceDB or
//! second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use minidom::Element;
use support::{
    error_condition, jid, routed_and_capturing, send, wait_for, ARRIVES, HOME_A, HOME_B, QUIET,
};
use waddle_server::s2s::route::RouterLimits;
use waddle_xmpp::xep::xep0199::NS_PING;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const BOB: &str = "bob@home-b.test/phone";
const ALICE: &str = "alice@home-a.test";
const ALICE_PHONE: &str = "alice@home-a.test/phone";

fn message(to: &str, id: &str, type_: MessageType) -> Message {
    let mut message = Message::new_with_type(type_, Some(jid(to)));
    message.from = Some(jid(BOB));
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang(String::new()), "hi".to_owned());
    message
}

fn presence(to: &str, id: &str, type_: PresenceType) -> Presence {
    let mut presence = Presence::new(type_);
    presence.from = Some(jid(BOB));
    presence.to = Some(jid(to));
    presence.id = Some(id.to_owned());
    presence
}

fn ping(to: &str, id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(BOB)),
        to: Some(jid(to)),
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

/// The refusal `home-b.test` receives for the stanza `id`: from the
/// account it was addressed to, to its sender.
async fn refusal(captured: &support::CapturingSink, id: &str, from: &str) -> Element {
    let reply = wait_for(ARRIVES, || captured.with_id(id).into_iter().next())
        .await
        .expect("the stanza is refused");
    assert_eq!(reply.attr("type"), Some("error"));
    assert_eq!(reply.attr("from"), Some(from));
    assert_eq!(reply.attr("to"), Some(BOB));
    assert_eq!(
        error_condition(&reply).as_deref(),
        Some("service-unavailable")
    );
    reply
}

#[tokio::test]
async fn messages_to_a_local_account_are_refused_with_service_unavailable() {
    let pki = support::Pki::new();
    let (_a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    for (to, id, type_) in [
        (ALICE, "m1", MessageType::Chat),
        (ALICE_PHONE, "m2", MessageType::Chat),
        (ALICE, "m3", MessageType::Normal),
        (ALICE, "m4", MessageType::Headline),
        (ALICE, "m5", MessageType::Groupchat),
    ] {
        send(&b, HOME_B, HOME_A, message(to, id, type_)).await;
        let reply = refusal(&captured, id, to).await;
        assert_eq!(reply.name(), "message");
        // The body is not echoed back.
        assert!(reply.children().all(|child| child.name() != "body"));
    }
}

#[tokio::test]
async fn iq_requests_to_a_local_account_are_refused_with_service_unavailable() {
    let pki = support::Pki::new();
    let (_a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    send(&b, HOME_B, HOME_A, ping(ALICE, "i1")).await;
    refusal(&captured, "i1", ALICE).await;
    send(&b, HOME_B, HOME_A, ping(ALICE_PHONE, "i2")).await;
    let reply = refusal(&captured, "i2", ALICE_PHONE).await;
    assert_eq!(reply.name(), "iq");
    let set = Iq::Set {
        from: Some(jid(BOB)),
        to: Some(jid(ALICE)),
        id: "i3".into(),
        payload: Element::builder("query", "jabber:iq:roster").build(),
    };
    send(&b, HOME_B, HOME_A, set).await;
    refusal(&captured, "i3", ALICE).await;
}

#[tokio::test]
async fn presence_of_every_type_to_a_local_account_is_ignored() {
    let pki = support::Pki::new();
    let (_a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    let cases = [
        (ALICE_PHONE, "p1", PresenceType::None),
        (ALICE, "p2", PresenceType::Subscribe),
        (ALICE, "p3", PresenceType::Subscribed),
        (ALICE, "p4", PresenceType::Unsubscribe),
        (ALICE, "p5", PresenceType::Unsubscribed),
        (ALICE, "p6", PresenceType::Probe),
        (ALICE_PHONE, "p7", PresenceType::Unavailable),
    ];
    for (to, id, type_) in cases {
        send(&b, HOME_B, HOME_A, presence(to, id, type_)).await;
    }
    // A message sent after them is answered; nothing answered the presence.
    send(
        &b,
        HOME_B,
        HOME_A,
        message(ALICE, "sentinel", MessageType::Chat),
    )
    .await;
    refusal(&captured, "sentinel", ALICE).await;
    tokio::time::sleep(QUIET).await;
    for id in ["p1", "p2", "p3", "p4", "p5", "p6", "p7"] {
        assert!(captured.with_id(id).is_empty(), "presence {id} is ignored");
    }
}

#[tokio::test]
async fn errors_and_uncorrelated_responses_to_a_local_account_are_never_answered() {
    let pki = support::Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    let mut error_message = message(ALICE, "e1", MessageType::Error);
    error_message.payloads.push(Element::from(StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "en",
        "No such item.",
    )));
    send(&b, HOME_B, HOME_A, error_message).await;
    send(
        &b,
        HOME_B,
        HOME_A,
        Iq::Result {
            from: Some(jid(BOB)),
            to: Some(jid(ALICE_PHONE)),
            id: "e2".into(),
            payload: None,
        },
    )
    .await;
    send(
        &b,
        HOME_B,
        HOME_A,
        Iq::Error {
            from: Some(jid(BOB)),
            to: Some(jid(ALICE)),
            id: "e3".into(),
            payload: None,
            error: StanzaError::new(
                ErrorType::Cancel,
                DefinedCondition::ServiceUnavailable,
                "en",
                "No.",
            ),
        },
    )
    .await;
    send(
        &b,
        HOME_B,
        HOME_A,
        message(ALICE, "sentinel", MessageType::Chat),
    )
    .await;
    refusal(&captured, "sentinel", ALICE).await;
    tokio::time::sleep(QUIET).await;
    for id in ["e1", "e2", "e3"] {
        assert!(captured.with_id(id).is_empty(), "{id} is not answered");
    }
    assert!(
        a.delivery.delivered().is_empty(),
        "nothing reached a local resource"
    );
}
