//! XEP-0280: Message Carbons dedicated suite.

use jid::Jid;
use minidom::Element;
use waddle_xmpp::carbons::should_copy_message;
use waddle_xmpp::xep::{NS_CHATSTATES, NS_CHAT_MARKERS};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::receipts::Request;

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
    msg.payloads.push(Request.into());
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

// ---------------------------------------------------------------------
// #1106 / XEP-0280 §6.3 — the whole RFC 6121 §8.5.2.1.1 delivery set is
// the carbon exclusion set: "The receiving server MUST NOT send a
// forwarded copy to the client(s) the original <message/> stanza was
// addressed to, as these recipients receive the original <message/>
// stanza."
// ---------------------------------------------------------------------

#[tokio::test]
async fn xep0280_carbon_enumeration_excludes_whole_delivery_set() {
    use jid::FullJid;
    use waddle_xmpp::registry::ConnectionRegistry;

    let registry = ConnectionRegistry::new();
    let bare: jid::BareJid = "bob@localhost".parse().expect("bare jid");
    let web: FullJid = "bob@localhost/web".parse().expect("full jid");
    let phone: FullJid = "bob@localhost/phone".parse().expect("full jid");
    let laptop: FullJid = "bob@localhost/laptop".parse().expect("full jid");

    // All three resources online with carbons enabled; web + phone are
    // the same-priority delivery set that received the original stanza,
    // laptop is a lower-priority resource that only gets a carbon.
    let (web_tx, _web_rx) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(web.clone(), web_tx, true);
    let (phone_tx, _phone_rx) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(phone.clone(), phone_tx, true);
    let (laptop_tx, _laptop_rx) = tokio::sync::mpsc::channel(4);
    registry.register_with_carbons(laptop.clone(), laptop_tx, true);

    let delivery_set = vec![web.clone(), phone.clone()];
    let carbon_targets = registry.get_other_carbon_resources_for_user(&bare, &delivery_set);

    assert!(
        !carbon_targets.contains(&web),
        "web received the original and must not get a forwarded copy"
    );
    assert!(
        !carbon_targets.contains(&phone),
        "phone received the original and must not get a forwarded copy"
    );
    assert_eq!(
        carbon_targets,
        vec![laptop],
        "only the resource outside the delivery set gets the carbon"
    );
}

#[test]
fn xep0280_remote_carbon_intent_freezes_all_excluded_resources() {
    use waddle_xmpp::{ingress::IngressEffectIntent, protocol::CarbonKind};
    let intent = IngressEffectIntent::RelayCarbons {
        owner: "alice@example.com".parse().expect("owner"),
        exclude: vec![
            "alice@example.com/laptop".parse().expect("laptop"),
            "alice@example.com/phone".parse().expect("phone"),
        ],
        kind: CarbonKind::Received,
    };
    let decoded = intent
        .with_encoded_v1(IngressEffectIntent::decode_v1)
        .expect("encode frozen owner obligation")
        .expect("decode frozen owner obligation");
    assert_eq!(intent, decoded);
    assert_eq!(intent.authority_key(), decoded.authority_key());
    assert_eq!(intent.semantic_key(), decoded.semantic_key());
}
