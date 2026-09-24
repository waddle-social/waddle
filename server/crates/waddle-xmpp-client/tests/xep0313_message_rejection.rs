//! Historical errors must never act as live rejections or incoming content.
mod rejection_support;

use minidom::Element;
use rejection_support::{rejection, runtime};
use waddle_xmpp_client::mam;

#[test]
fn archived_error_is_not_a_message_call_or_live_rejection() {
    let result = Element::builder("message", "jabber:client")
        .append(
            Element::builder("result", "urn:xmpp:mam:2")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "archive-id")
                .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), "query-id")
                .append(
                    Element::builder("forwarded", "urn:xmpp:forward:0")
                        .append(rejection("chat@example.com"))
                        .build(),
                )
                .build(),
        )
        // Even attacker-supplied effects on the result wrapper must not leak.
        .append(
            Element::builder("body", "jabber:client")
                .append("wrapper body")
                .build(),
        )
        .build();
    assert!(mam::parse_mam_result(&result).is_none());
    assert!(runtime(None).handle_app_stanza(&result).is_empty());
}
