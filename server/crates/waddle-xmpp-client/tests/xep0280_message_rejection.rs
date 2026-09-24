//! XEP-0280 §6.1 and §11: received errors are eligible, envelope trust is mandatory.
mod rejection_support;

use minidom::Element;
use rejection_support::{rejection, runtime};
use waddle_xmpp_client::ClientEvent;

fn carbon(direction: &str, from: &str) -> Element {
    Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from)
        .append(
            Element::builder(direction, "urn:xmpp:carbons:2")
                .append(
                    Element::builder("forwarded", "urn:xmpp:forward:0")
                        .append(rejection("chat@example.com"))
                        .build(),
                )
                .build(),
        )
        .build()
}

#[test]
fn received_carbon_error_is_a_rejection_without_echoed_effects() {
    let events = runtime(None).handle_app_stanza(&carbon("received", "alice@example.com"));
    let [ClientEvent::MessageRejected(error)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(error.from, "chat@example.com".parse::<jid::Jid>().unwrap());
    assert_eq!(error.stanza_id.as_str(), "rejected");
}

#[test]
fn sent_and_forged_carbon_errors_do_not_reject_our_outbound_messages() {
    for (direction, from) in [
        ("sent", "alice@example.com"),
        ("received", "mallory@example.com"),
        ("received", "alice@example.com/other"),
    ] {
        assert!(runtime(None)
            .handle_app_stanza(&carbon(direction, from))
            .is_empty());
    }
}
