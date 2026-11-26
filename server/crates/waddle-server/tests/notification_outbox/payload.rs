//! XEP-0357 notification payload and summary form shapes.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use minidom::Element;
use waddle_server::notification_outbox::{
    build_xep0357_notification_payload, RichSummary, WADDLE_PUSH_CONTEXT_NS,
    XEP0357_SUMMARY_FORM_TYPE,
};
use waddle_xmpp::push::names::{DisplayName, PushNames, NS_PUSH_NAMES};
use waddle_xmpp::xep::NS_DATA_FORMS;

#[test]
fn xep0357_payload_uses_summary_form_and_waddle_context_only() {
    let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "bob@example.com",
        )
        .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "")
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
        .build();
    let payload = build_xep0357_notification_payload(3, &RichSummary::minimal(), &context, None);

    assert!(payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
    let summary = payload
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .expect("summary form");
    // XEP-0357 §4 example shows `<x xmlns='jabber:x:data'>` with
    // no `type` attribute — the form is a passively-encapsulated
    // summary, not a query response.
    assert_eq!(summary.attr("type"), None);
    assert!(summary.children().any(|field| {
        field.is("field", NS_DATA_FORMS)
            && field.attr("var") == Some("FORM_TYPE")
            && field.attr("type") == Some("hidden")
            && field.children().any(|value| {
                value.is("value", NS_DATA_FORMS) && value.text() == XEP0357_SUMMARY_FORM_TYPE
            })
    }));
    assert!(summary.children().any(|field| {
        field.is("field", NS_DATA_FORMS)
            && field.attr("var") == Some("message-count")
            && field
                .children()
                .any(|value| value.is("value", NS_DATA_FORMS) && value.text() == "3")
    }));
    assert!(!summary.children().any(|field| {
        matches!(
            field.attr("var"),
            Some("last-message-body" | "last-message-sender")
        )
    }));
    let context = payload
        .children()
        .find(|child| child.is("context", WADDLE_PUSH_CONTEXT_NS))
        .expect("waddle context");
    assert_eq!(context.attr("conversation"), Some("bob@example.com"));
    assert_eq!(context.attr("class"), Some("dm"));
}

#[test]
fn xep0357_summary_form_emits_rich_fields_when_opted_in() {
    let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "juliet@capulet.example",
        )
        .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "")
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
        .build();
    let rich = RichSummary {
        sender: Some("juliet@capulet.example/balcony".parse().expect("jid")),
        body: Some("Wherefore art thou, Romeo?".to_string()),
    };
    let payload = build_xep0357_notification_payload(1, &rich, &context, None);

    let summary = payload
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .expect("summary form");
    let field_value = |var: &str| -> Option<String> {
        summary
            .children()
            .find(|field| field.is("field", NS_DATA_FORMS) && field.attr("var") == Some(var))
            .and_then(|field| {
                field
                    .children()
                    .find(|value| value.is("value", NS_DATA_FORMS))
            })
            .map(|value| value.text())
    };
    assert_eq!(field_value("message-count").as_deref(), Some("1"));
    assert_eq!(
        field_value("last-message-sender").as_deref(),
        Some("juliet@capulet.example/balcony")
    );
    assert_eq!(
        field_value("last-message-body").as_deref(),
        Some("Wherefore art thou, Romeo?")
    );
}

#[test]
fn xep0357_summary_form_strips_body_but_keeps_sender_when_hint_stripped() {
    let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            "juliet@capulet.example",
        )
        .attr(minidom::rxml::xml_ncname!("thread").to_owned(), "")
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), "dm")
        .build();
    // Sender preserved, body stripped (XEP-0334 hint precedence).
    let rich = RichSummary {
        sender: Some("juliet@capulet.example/balcony".parse().expect("jid")),
        body: None,
    };
    let payload = build_xep0357_notification_payload(1, &rich, &context, None);
    let summary = payload
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .expect("summary form");
    assert!(summary
        .children()
        .any(|field| field.attr("var") == Some("last-message-sender")));
    assert!(!summary
        .children()
        .any(|field| field.attr("var") == Some("last-message-body")));
}

#[test]
fn xep0357_names_are_an_optional_sibling_and_preserve_summary_bytes() {
    assert_eq!(NS_PUSH_NAMES, "urn:waddle:push:names:0");
    let context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("class").to_owned(),
            "personal_mention",
        )
        .build();
    let rich = RichSummary {
        sender: Some("general@rooms.example.com/Wonderland".parse().expect("jid")),
        body: Some("hello".to_owned()),
    };
    let names = PushNames {
        sender: DisplayName::parse("Alice Liddell"),
        room: DisplayName::parse("General"),
    };
    let legacy = build_xep0357_notification_payload(3, &rich, &context, None);
    let named = build_xep0357_notification_payload(3, &rich, &context, Some(&names));
    assert_eq!(
        String::from(
            legacy
                .get_child("x", NS_DATA_FORMS)
                .expect("legacy summary")
        ),
        String::from(named.get_child("x", NS_DATA_FORMS).expect("named summary")),
    );
    assert_eq!(named.children().count(), 3);
    assert_eq!(
        named.get_child("context", WADDLE_PUSH_CONTEXT_NS),
        Some(&context)
    );
    let element = named
        .get_child("names", NS_PUSH_NAMES)
        .expect("names sibling");
    assert_eq!(element.children().count(), 2);
    assert_eq!(
        element
            .get_child("sender", NS_PUSH_NAMES)
            .expect("sender")
            .text(),
        "Alice Liddell"
    );
    assert_eq!(
        element
            .get_child("room", NS_PUSH_NAMES)
            .expect("room")
            .text(),
        "General"
    );

    let minimal =
        build_xep0357_notification_payload(3, &RichSummary::minimal(), &context, Some(&names));
    assert_eq!(minimal.children().count(), 2);
    let empty_names =
        build_xep0357_notification_payload(3, &rich, &context, Some(&PushNames::default()));
    assert_eq!(empty_names, legacy);
    let call_context = Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(minidom::rxml::xml_ncname!("class").to_owned(), "call")
        .build();
    let call = build_xep0357_notification_payload(3, &rich, &call_context, Some(&names));
    assert_eq!(call.children().count(), 2);
}
