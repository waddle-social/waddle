//! RFC 6120 §8.3: errors carry outcomes, never executable echoed payloads.
mod rejection_support;

use minidom::Element;
use rejection_support::{rejection, runtime};
use waddle_xmpp_client::{messaging, ClientEvent};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

#[test]
fn message_error_is_only_a_typed_rejection() {
    let stanza = rejection("chat@example.com");
    assert!(messaging::parse(&stanza).is_none());
    assert!(messaging::parse_call_event(&stanza).is_none());
    let events = runtime(None).handle_app_stanza(&stanza);
    let [ClientEvent::MessageRejected(error)] = events.as_slice() else {
        panic!("expected only rejection, got {events:?}")
    };
    assert_eq!(error.stanza_id.as_str(), "rejected");
    assert_eq!(error.from, "chat@example.com".parse::<jid::Jid>().unwrap());
    assert_eq!(error.to, Some("alice@example.com/web".parse().unwrap()));
    assert_eq!(error.error.type_, ErrorType::Cancel);
    assert_eq!(
        error.error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
}

#[test]
fn malformed_errors_are_consumed_without_content_or_rejection() {
    let valid = rejection("chat@example.com");
    let mut no_error = valid.clone();
    no_error.remove_child("error", "jabber:client");
    let mut duplicate_error = valid.clone();
    duplicate_error.append_child(valid.get_child("error", "jabber:client").unwrap().clone());
    let mut invalid_type = no_error.clone();
    invalid_type.append_child(
        Element::builder("error", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "invalid")
            .append(
                Element::builder("service-unavailable", "urn:ietf:params:xml:ns:xmpp-stanzas")
                    .build(),
            )
            .build(),
    );
    let mut no_condition = no_error.clone();
    no_condition.append_child(
        Element::builder("error", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "cancel")
            .build(),
    );
    let invalid_id: Element = "<message xmlns='jabber:client' type='error' from='chat@example.com'><body>echo</body><error type='cancel'><service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></message>".parse().unwrap();
    for stanza in [
        no_error,
        duplicate_error,
        invalid_type,
        no_condition,
        invalid_id,
        rejection("bad jid"),
        "<message xmlns='jabber:client' type='error'/>"
            .parse()
            .unwrap(),
    ] {
        assert!(runtime(None).handle_app_stanza(&stanza).is_empty());
        assert!(messaging::parse(&stanza).is_none());
        assert!(messaging::parse_call_event(&stanza).is_none());
    }
}

#[test]
fn missing_sender_on_direct_error_means_own_account_not_server() {
    let stanza: Element = "<message xmlns='jabber:client' type='error' id='own-account'><error type='cancel'><service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></message>".parse().unwrap();
    let events = runtime(None).handle_app_stanza(&stanza);
    let [ClientEvent::MessageRejected(error)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(error.from, "alice@example.com".parse::<jid::Jid>().unwrap());
}
