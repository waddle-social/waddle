//! XEP-0184: Message Delivery Receipts dedicated suite.
//!
//! #1247 conformance: XEP-0184 §7 assigns ack generation to the
//! **receiving entity** — the recipient's client — and an entity that
//! does not support (or has disabled) receipts "MUST NOT return a
//! receipt". A server fabricating `<received/>` on the recipient's
//! behalf produces dup receipts with conformant clients, false
//! "delivered" confirmations, and a presence oracle. The server's
//! whole role is to
//! route `<request/>`-carrying content messages and client-generated
//! `<received/>` acks verbatim, which this suite pins:
//!
//! 1. the recipient pass never synthesizes a receipt;
//! 2. the `<request/>` payload survives recipient-pass processing on
//!    the delivered wire copy (so the receiving client can ack);
//! 3. a client-generated ack routes through the sender pass to the
//!    requester untouched;
//! 4. the server does not advertise `urn:xmpp:receipts` (the disco
//!    feature announces ack *generation*, which the server no longer
//!    performs).

use jid::{BareJid, FullJid, Jid};
use std::sync::Arc;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::protocol::{
    handlers::register_default_message_handlers, FixedIdGenerator, InboundEvent, InboundFrame,
    OutboundEvent, StanzaDispatcher, XmppStateMachine, HEADLESS_RECIPIENT_RESOURCE,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::receipts::{Received, Request};

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn has_receipt_request(message: &Message) -> bool {
    message
        .payloads
        .iter()
        .any(|payload| Request::try_from(payload.clone()).is_ok())
}

fn received_id(message: &Message) -> Option<String> {
    message
        .payloads
        .iter()
        .find_map(|payload| Received::try_from(payload.clone()).ok())
        .map(|receipt| receipt.id)
}

fn has_receipt_received(message: &Message) -> bool {
    received_id(message).is_some()
}

fn build_dispatcher() -> StanzaDispatcher {
    let mut dispatcher = StanzaDispatcher::new();
    register_default_message_handlers(&mut dispatcher);
    dispatcher
}

fn ready_machine(full_jid: &FullJid) -> XmppStateMachine {
    let mut machine = XmppStateMachine::with_id_gen(
        "example.com",
        build_dispatcher(),
        Arc::new(FixedIdGenerator("receipt-stamp".to_string())),
    );
    machine.transition_to_ready(full_jid.clone(), false);
    machine
}

fn direct_message(
    from: &FullJid,
    to: &BareJid,
    message_type: MessageType,
    id: Option<&str>,
) -> Message {
    let mut message = Message::new(Some(Jid::from(to.clone())));
    message.from = Some(Jid::from(from.clone()));
    message.type_ = message_type;
    message.id = id.map(|s| xmpp_parsers::message::Id(s.to_string()));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());
    message.payloads.push(Request.into());
    message
}

/// Any routed message carrying a `<received/>` ack in `events`.
fn synthesized_receipt(events: &[OutboundEvent]) -> Option<&Message> {
    events.iter().find_map(|event| match event {
        OutboundEvent::RouteToConnection { stanza, .. } => match stanza.as_ref() {
            Stanza::Message(message) if has_receipt_received(message) => Some(message),
            _ => None,
        },
        _ => None,
    })
}

#[test]
fn xep0184_server_does_not_advertise_receipts() {
    // The XEP-0184 disco feature announces "I will generate acks" —
    // the receiving client's job, not the server's (#1247).
    let features = server_features();
    assert!(!features.contains(&Feature::new(xmpp_parsers::ns::RECEIPTS)));
}

#[test]
fn xep0184_recipient_pass_does_not_fabricate_a_receipt() {
    let bob = full("bob@example.com/desk");
    let alice = full("alice@example.com/web");
    let mut machine = ready_machine(&bob);

    let message = direct_message(
        &alice,
        &bare("bob@example.com"),
        MessageType::Chat,
        Some("msg-1"),
    );
    let events = machine.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        message,
    ))));

    assert!(
        synthesized_receipt(&events).is_none(),
        "server must not generate <received/> on the recipient's behalf"
    );
}

#[test]
fn xep0184_recipient_pass_delivers_request_payload_verbatim() {
    // The receiving client can only honor XEP-0184 if the server
    // forwards the `<request/>` untouched on the delivered copy.
    let bob = full("bob@example.com/desk");
    let alice = full("alice@example.com/web");
    let mut machine = ready_machine(&bob);

    let message = direct_message(
        &alice,
        &bare("bob@example.com"),
        MessageType::Chat,
        Some("msg-1"),
    );
    let events = machine.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        message,
    ))));

    let delivered = events
        .iter()
        .find_map(|event| match event {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Message(message) => Some(message),
                _ => None,
            },
            _ => None,
        })
        .expect("recipient pass should deliver the original stanza to the wire");
    assert!(
        has_receipt_request(delivered),
        "the <request/> payload must survive recipient-pass processing"
    );
    assert_eq!(
        delivered.id.as_ref().map(|id| id.0.as_str()),
        Some("msg-1"),
        "the wire message id the ack will reference must be preserved"
    );
}

#[test]
fn xep0184_client_generated_ack_routes_to_the_requester_verbatim() {
    // Bob's client acks msg-1; the server's sender pass routes the ack
    // to Alice untouched — it neither strips nor duplicates it.
    let bob = full("bob@example.com/desk");
    let mut machine = ready_machine(&bob);

    let mut ack = Message::new(Some(Jid::from(full("alice@example.com/web"))));
    ack.from = Some(Jid::from(bob.clone()));
    ack.type_ = MessageType::Chat;
    ack.payloads.push(
        Received {
            id: "msg-1".to_string(),
        }
        .into(),
    );

    let events = machine.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(ack),
    ))));

    let routed: Vec<&Message> = events
        .iter()
        .filter_map(|event| match event {
            OutboundEvent::RouteToConnection { jid, stanza } => match stanza.as_ref() {
                Stanza::Message(message) if has_receipt_received(message) => {
                    assert_eq!(jid.to_string(), "alice@example.com/web");
                    Some(message)
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(routed.len(), 1, "exactly one ack routed, no duplicates");
    assert_eq!(received_id(routed[0]), Some("msg-1".to_string()));
    assert!(
        !has_receipt_request(routed[0]),
        "XEP-0184 §4: an ack message must not itself request a receipt"
    );
}

#[test]
fn xep0184_groupchat_receipt_requests_do_not_produce_server_acks() {
    let bob = full("bob@example.com/desk");
    let alice = full("alice@example.com/web");
    let mut machine = ready_machine(&bob);

    let message = direct_message(
        &alice,
        &bare("bob@example.com"),
        MessageType::Groupchat,
        Some("group-1"),
    );
    let events = machine.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        message,
    ))));

    assert!(synthesized_receipt(&events).is_none());
}

#[test]
fn xep0184_headless_resource_constant_matches_offline_pass_guard() {
    assert_eq!(HEADLESS_RECIPIENT_RESOURCE, "offline-recipient-pass");
}
