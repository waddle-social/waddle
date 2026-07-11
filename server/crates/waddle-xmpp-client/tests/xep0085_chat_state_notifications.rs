//! XEP-0085: Chat State Notifications dedicated client suite.

use minidom::Element;
use waddle_xmpp_client::{
    caps::{build_client_caps_disco_info_response, client_caps_features, client_caps_node_ver},
    discovery::DISCO_INFO_NS,
    messaging::{build_chat_state_message, parse_chat_state_payload, NS_CHAT_STATES, NS_CLIENT},
};

#[test]
fn xep0085_client_advertises_chatstates_exactly_once() {
    assert_eq!(
        client_caps_features()
            .into_iter()
            .filter(|feature| *feature == NS_CHAT_STATES)
            .count(),
        1
    );
}

#[test]
fn xep0085_current_caps_node_disco_includes_chatstates_exactly_once() {
    let request = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .append(
            Element::builder("query", DISCO_INFO_NS)
                .attr(
                    minidom::rxml::xml_ncname!("node").to_owned(),
                    client_caps_node_ver(),
                )
                .build(),
        )
        .build();
    let response = build_client_caps_disco_info_response(&request, None)
        .expect("the current caps node is served");
    let query = response
        .get_child("query", DISCO_INFO_NS)
        .expect("disco query is present");

    assert_eq!(
        query
            .children()
            .filter(|child| {
                child.is("feature", DISCO_INFO_NS) && child.attr("var") == Some(NS_CHAT_STATES)
            })
            .count(),
        1
    );
}

#[test]
fn xep0085_client_builds_and_parses_a_chat_state_notification() {
    let message = build_chat_state_message("juliet@example.com", "composing", "chat", None)
        .expect("valid chat state builds");
    let parsed = parse_chat_state_payload(&message).expect("chat state parses");

    assert_eq!(parsed.state, "composing");
    assert!(message
        .children()
        .any(|child| child.is("composing", NS_CHAT_STATES)));
}
