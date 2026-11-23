//! XEP-0045 §10.2.1: a room tells its occupants that its configuration
//! changed with a groupchat message whose `muc#user` payload carries a
//! status code; 104 covers changes that are not privacy-related, so
//! occupants can read the room's configuration again.

use minidom::Element;
use waddle_xmpp_client::messaging::{parse, InboundMessage, MessagingEvent, MucStatus, NS_CLIENT};
use waddle_xmpp_core::mam::MUC_USER_NS;

fn notification(codes: &[&str]) -> Element {
    let mut x = Element::builder("x", MUC_USER_NS);
    for code in codes {
        x = x.append(
            Element::builder("status", MUC_USER_NS)
                .attr(minidom::rxml::xml_ncname!("code").to_owned(), *code)
                .build(),
        );
    }
    Element::builder("message", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "coven@chat.shakespeare.lit",
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "crone1@shakespeare.lit/desktop",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .append(x.build())
        .build()
}

fn parsed(element: &Element) -> InboundMessage {
    match parse(element).expect("message parses") {
        MessagingEvent::Message(message) => *message,
        _ => panic!("expected message"),
    }
}

#[test]
fn a_configuration_change_notice_carries_status_104() {
    let message = parsed(&notification(&["104"]));
    assert_eq!(message.muc_status, vec![MucStatus::ConfigurationChanged]);
    assert!(message.body.is_none());
}

#[test]
fn privacy_related_codes_are_kept_as_they_came() {
    // §10.2.1 Example 191: logging now enabled.
    let message = parsed(&notification(&["170"]));
    assert_eq!(message.muc_status, vec![MucStatus::Other(170)]);
}

#[test]
fn an_ordinary_room_message_carries_no_status() {
    let element = Element::builder("message", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "coven@chat.shakespeare.lit/firstwitch",
        )
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .append(Element::builder("body", NS_CLIENT).append("Thrice").build())
        .build();
    assert!(parsed(&element).muc_status.is_empty());
}
