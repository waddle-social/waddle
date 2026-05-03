//! XEP-0334: Message Processing Hints dedicated suite.

use jid::Jid;
use minidom::Element;
use waddle_xmpp::carbons::should_copy_message;
use waddle_xmpp::xep::{should_skip_carbons, Hint, NS_HINTS};
use xmpp_parsers::message::{Body, Message, MessageType};

fn chat_message(to: &str) -> Message {
    let to: Jid = to.parse().expect("valid jid");
    let mut msg = Message::new(Some(to));
    msg.type_ = MessageType::Chat;
    msg.bodies.insert(String::new(), Body("secret".to_string()));
    msg.payloads
        .push(Element::builder(Hint::NoCopy.element_name(), NS_HINTS).build());
    msg
}

#[test]
fn xep0334_no_copy_suppresses_carbons_for_full_jid_targets() {
    let msg = chat_message("peer@localhost/phone");
    assert!(should_skip_carbons(&msg));
    assert!(!should_copy_message(&msg));
}

#[test]
fn xep0334_no_copy_does_not_override_bare_jid_routing() {
    let msg = chat_message("peer@localhost");
    assert!(!should_skip_carbons(&msg));
    assert!(should_copy_message(&msg));
}
