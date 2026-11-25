//! XEP-0085: Chat State Notifications dedicated client suite.

use minidom::Element;
use waddle_xmpp_client::caps::{
    build_client_caps_disco_info_response, client_caps_features, client_caps_node_ver,
};
use waddle_xmpp_client::discovery::DISCO_INFO_NS;
use waddle_xmpp_client::messaging::{
    build_chat_state_message, build_outbound_message, parse_chat_state_payload, SendMessageOptions,
    NS_CHAT_STATES, NS_CLIENT,
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
    assert!(!message.attr("id").expect("delivery id").is_empty());
    assert!(message
        .children()
        .any(|child| child.is("composing", NS_CHAT_STATES)));
    assert_eq!(message.children().count(), 1, "§5.6 permits no extra child");
}

#[test]
fn xep0085_thread_is_the_only_optional_standalone_child() {
    let thread = waddle_xmpp_client::xep::thread::ThreadRef {
        id: "thread-1".to_owned(),
        parent: None,
    };
    let message =
        build_chat_state_message("room@muc.example", "composing", "groupchat", Some(&thread))
            .expect("threaded chat state builds");

    assert_eq!(message.children().count(), 2);
    assert!(message.get_child("composing", NS_CHAT_STATES).is_some());
    assert!(message.get_child("thread", NS_CLIENT).is_some());
}

fn chat_state_children(stanza: &Element) -> Vec<String> {
    stanza
        .children()
        .filter(|child| child.ns() == NS_CHAT_STATES)
        .map(|child| child.name().to_owned())
        .collect()
}

#[test]
fn xep0085_content_messages_carry_exactly_one_active_notification() {
    let mut options = SendMessageOptions::default();
    options.chat_state_active = true;
    for message_type in ["chat", "groupchat"] {
        let (_, stanza) =
            build_outbound_message("juliet@example.com", message_type, "hello", &options)
                .expect("content message builds");
        assert_eq!(chat_state_children(&stanza), vec!["active".to_owned()]);
        assert!(stanza.get_child("body", NS_CLIENT).is_some());
    }
}

#[test]
fn xep0085_content_messages_omit_active_when_chat_states_disabled() {
    let (_, stanza) = build_outbound_message(
        "juliet@example.com",
        "chat",
        "hello",
        &SendMessageOptions::default(),
    )
    .expect("content message builds");
    assert!(chat_state_children(&stanza).is_empty());
}
