//! XEP-0280: Message Carbons dedicated suite.

use jid::Jid;
use minidom::Element;
use waddle_xmpp::carbons::should_copy_message;
use waddle_xmpp::xep::{NS_CHATSTATES, NS_CHAT_MARKERS, NS_RECEIPTS};
use xmpp_parsers::message::{Message, MessageType};

fn message_of_type(message_type: MessageType) -> Message {
    let to: Jid = "peer@localhost".parse().expect("valid jid");
    let mut msg = Message::new(Some(to));
    msg.type_ = message_type;
    msg
}

#[test]
fn xep0280_bodyless_chat_message_is_eligible_for_carbons() {
    let msg = message_of_type(MessageType::Chat);
    assert!(should_copy_message(&msg));
}

#[test]
fn xep0280_normal_message_with_body_is_eligible_for_carbons() {
    let mut msg = message_of_type(MessageType::Normal);
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());
    assert!(should_copy_message(&msg));
}

#[test]
fn xep0280_normal_message_without_body_or_im_payload_is_not_eligible() {
    let msg = message_of_type(MessageType::Normal);
    assert!(!should_copy_message(&msg));
}

#[test]
fn xep0280_bodyless_chat_state_message_is_eligible_for_carbons() {
    let mut msg = message_of_type(MessageType::Normal);
    msg.payloads
        .push(Element::builder("composing", NS_CHATSTATES).build());
    assert!(should_copy_message(&msg));
}

#[test]
fn xep0280_bodyless_receipt_request_is_eligible_for_carbons() {
    let mut msg = message_of_type(MessageType::Normal);
    msg.payloads
        .push(Element::builder("request", NS_RECEIPTS).build());
    assert!(should_copy_message(&msg));
}

#[test]
fn xep0280_bodyless_chat_marker_is_eligible_for_carbons() {
    let mut msg = message_of_type(MessageType::Normal);
    msg.payloads.push(
        Element::builder("displayed", NS_CHAT_MARKERS)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "msg-1")
            .build(),
    );
    assert!(should_copy_message(&msg));
}
