//! XEP-0184: Message Delivery Receipts dedicated suite.

use jid::{BareJid, FullJid, Jid};
use std::sync::Arc;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::protocol::{
    handlers::register_default_message_handlers, FixedIdGenerator, InboundEvent, OutboundEvent,
    StanzaDispatcher, XmppStateMachine, HEADLESS_RECIPIENT_RESOURCE,
};
use waddle_xmpp::xep::{build_receipt_request_element, extract_received_id, has_receipt_request};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Body, Message, MessageType};

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
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
    message.id = id.map(str::to_string);
    message
        .bodies
        .insert(String::new(), Body("hello".to_string()));
    message.payloads.push(build_receipt_request_element());
    message
}

fn receipt_route(events: &[OutboundEvent]) -> Option<(usize, &Jid, &Message)> {
    events
        .iter()
        .enumerate()
        .find_map(|(idx, event)| match event {
            OutboundEvent::RouteToConnection { jid, stanza } => match stanza.as_ref() {
                Stanza::Message(message) => Some((idx, jid, message)),
                _ => None,
            },
            _ => None,
        })
}

#[test]
fn xep0184_server_root_advertises_receipts_support() {
    let features = server_features();
    assert!(features.contains(&Feature::receipts()));
}

#[test]
fn xep0184_recipient_pass_emits_receipt_after_original_delivery() {
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

    let send_idx = events
        .iter()
        .enumerate()
        .find_map(|(idx, event)| matches!(event, OutboundEvent::SendStanza(_)).then_some(idx))
        .expect("recipient pass should deliver original stanza to the wire");
    let (receipt_idx, receipt_jid, receipt) =
        receipt_route(&events).expect("recipient pass should emit a receipt route");

    assert!(
        send_idx < receipt_idx,
        "receipt route must be emitted after the original SendStanza event"
    );
    assert_eq!(*receipt_jid, Jid::from(alice.clone()));
    assert_eq!(receipt.to, Some(Jid::from(alice.clone())));
    assert_eq!(receipt.from, Some(Jid::from(bob.clone())));
    assert_eq!(receipt.type_, MessageType::Chat);
    assert!(
        receipt.bodies.is_empty(),
        "ack must not include body content"
    );
    assert!(
        !has_receipt_request(receipt),
        "ack must not request another receipt"
    );
    assert_eq!(
        receipt.payloads.len(),
        1,
        "ack should contain only the receipt payload"
    );
    assert_eq!(extract_received_id(receipt), Some("msg-1".to_string()));
}

#[test]
fn xep0184_groupchat_messages_do_not_trigger_delivery_receipts() {
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

    assert!(
        receipt_route(&events).is_none(),
        "groupchat receipt requests are not eligible for automatic XEP-0184 acks"
    );
}

#[test]
fn xep0184_messages_without_ids_do_not_trigger_delivery_receipts() {
    let bob = full("bob@example.com/desk");
    let alice = full("alice@example.com/web");
    let mut machine = ready_machine(&bob);

    let message = direct_message(&alice, &bare("bob@example.com"), MessageType::Normal, None);
    let events = machine.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        message,
    ))));

    assert!(
        receipt_route(&events).is_none(),
        "messages missing the required content-message id must not receive an ack"
    );
}

#[test]
fn xep0184_headless_resource_constant_matches_offline_pass_guard() {
    assert_eq!(HEADLESS_RECIPIENT_RESOURCE, "offline-recipient-pass");
}
