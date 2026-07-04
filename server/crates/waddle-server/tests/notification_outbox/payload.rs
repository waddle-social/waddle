//! XEP-0357 notification payload and summary form shapes.
//!
//! Extracted from the former inline `mod tests` in `src/notification_outbox.rs`.

use minidom::Element;
use waddle_server::notification_outbox::*;
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
    let payload = build_xep0357_notification_payload(3, &RichSummary::minimal(), &context);

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
    let payload = build_xep0357_notification_payload(1, &rich, &context);

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
    let payload = build_xep0357_notification_payload(1, &rich, &context);
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
