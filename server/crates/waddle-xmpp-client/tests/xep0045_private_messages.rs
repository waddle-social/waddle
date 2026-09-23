//! XEP-0045 §7.5: preserve private-message evidence before room discovery.

use minidom::Element;
use waddle_xmpp_client::messaging::{parse, InboundMessage, MessagingEvent, NS_CLIENT};
use waddle_xmpp_core::mam::MUC_USER_NS;

fn parsed_message(message_type: Option<&str>, marker_ns: Option<&str>) -> InboundMessage {
    let mut builder = Element::builder("message", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "room@conference.example/nick/phone",
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "alice@example/web",
        );
    if let Some(message_type) = message_type {
        builder = builder.attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type);
    }
    if let Some(namespace) = marker_ns {
        builder = builder.append(Element::builder("x", namespace).build());
    }
    match parse(&builder.build()).expect("message parses") {
        MessagingEvent::Message(message) => *message,
        _ => panic!("expected message"),
    }
}

#[test]
fn private_message_marker_survives_chat_and_normal_parsing() {
    for message_type in [Some("chat"), Some("normal"), None] {
        let message = parsed_message(message_type, Some(MUC_USER_NS));
        assert!(message.muc_pm);
        assert_eq!(
            message.from.as_deref(),
            Some("room@conference.example/nick/phone")
        );
    }
}

#[test]
fn groupchat_and_other_namespaces_are_not_private_message_markers() {
    assert!(!parsed_message(Some("groupchat"), Some(MUC_USER_NS)).muc_pm);
    assert!(!parsed_message(Some("error"), Some(MUC_USER_NS)).muc_pm);
    assert!(!parsed_message(Some("chat"), Some(NS_CLIENT)).muc_pm);
    assert!(!parsed_message(Some("chat"), None).muc_pm);
}
