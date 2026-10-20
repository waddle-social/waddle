use minidom::Element;
use waddle_xmpp::xep::xep0428::{build_fallback_element, FallbackIndication};
use waddle_xmpp::xep::xep0461::{build_reply_element, ReplyReference};
use xmpp_parsers::message::Lang;

use super::*;

fn reply(body: &str) -> Message {
    let mut message = Message::new(None::<jid::Jid>);
    message.bodies.insert(Lang::new(), body.to_owned());
    message.payloads.push(build_reply_element(
        &ReplyReference::new("parent").with_to("author@example.org".parse().expect("JID")),
    ));
    message
}

fn explicit_fallback(start: Option<&str>, end: Option<&str>) -> Element {
    let mut body = Element::builder("body", NS_FALLBACK);
    if let Some(start) = start {
        body = body.attr(minidom::rxml::xml_ncname!("start").to_owned(), start);
    }
    if let Some(end) = end {
        body = body.attr(minidom::rxml::xml_ncname!("end").to_owned(), end);
    }
    Element::builder("fallback", NS_FALLBACK)
        .attr(minidom::rxml::xml_ncname!("for").to_owned(), NS_REPLY)
        .append(body.build())
        .build()
}

fn extracted(message: &Message) -> Option<DisplayText> {
    observation_body(
        message,
        message.get_best_body(vec![]).expect("fixture body").1,
    )
}

#[test]
fn structured_reply_observes_only_the_authors_new_text() {
    let quote = "> hostile quoted message\n\n";
    let mut message = reply(&[quote, "should I receive a notification?"].concat());
    message
        .payloads
        .push(build_fallback_element(&FallbackIndication::for_range(
            NS_REPLY,
            0,
            quote.chars().count(),
        )));
    assert_eq!(
        extracted(&message).expect("authored text").as_str(),
        "should I receive a notification?"
    );
    assert!(message
        .get_best_body(vec![])
        .expect("body")
        .1
        .starts_with(quote));
}

#[test]
fn ranges_count_unicode_code_points_without_normalization() {
    let quote = "> 🙂 e\u{301} & quoted\n\n";
    let authored = "👩\u{200d}💻 reply e\u{301}";
    let mut message = reply(&[quote, authored].concat());
    message
        .payloads
        .push(build_fallback_element(&FallbackIndication::for_range(
            NS_REPLY,
            0,
            quote.chars().count(),
        )));
    assert_eq!(extracted(&message).expect("authored").as_str(), authored);
}

#[test]
fn offsets_count_decoded_xml_characters() {
    let element: Element = "<message xmlns='jabber:client'><body>&gt; 🙂 &amp;\n\nanswer</body><reply xmlns='urn:xmpp:reply:0' id='parent' to='author@example.org'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='7'/></fallback></message>"
        .parse()
        .expect("XML");
    let message = Message::try_from(element).expect("message");
    assert_eq!(extracted(&message).expect("authored").as_str(), "answer");
}

#[test]
fn malformed_and_out_of_bounds_ranges_retain_the_entire_body() {
    let body = "> quote\n\nvisible text";
    for (start, end) in [
        (Some("5"), Some("2")),
        (Some("-1"), Some("2")),
        (Some(""), Some("")),
        (Some("no"), Some("4")),
        (Some(" 0"), Some("9")),
        (Some("0"), Some("9 ")),
        (Some("0"), Some("4294967296")),
        (Some("0"), Some("999")),
        (Some("0"), None),
        (None, Some("2")),
        (Some("4294967296"), Some("4294967297")),
    ] {
        let mut message = reply(body);
        message.payloads.push(explicit_fallback(start, end));
        assert_eq!(extracted(&message).expect("raw").as_str(), body);
    }
}

#[test]
fn unsupported_whole_childless_and_multiple_ranges_retain_raw_body() {
    let body = "> quote\n\nvisible text";
    for fallback in [
        build_fallback_element(&FallbackIndication::whole_body(NS_REPLY)),
        Element::builder("fallback", NS_FALLBACK)
            .attr(minidom::rxml::xml_ncname!("for").to_owned(), NS_REPLY)
            .build(),
        build_fallback_element(&FallbackIndication::for_ranges(
            NS_REPLY,
            [
                FallbackRange { start: 0, end: 3 },
                FallbackRange { start: 2, end: 9 },
            ],
        )),
        build_fallback_element(&FallbackIndication::whole_subject(NS_REPLY)),
    ] {
        let mut message = reply(body);
        message.payloads.push(fallback);
        assert_eq!(extracted(&message).expect("raw").as_str(), body);
    }
}

#[test]
fn duplicate_reply_fallbacks_retain_raw_body() {
    let body = "> quote\n\nvisible text";
    let mut message = reply(body);
    message
        .payloads
        .push(explicit_fallback(Some("0"), Some("9")));
    message
        .payloads
        .push(explicit_fallback(Some("9"), Some("10")));
    assert_eq!(extracted(&message).expect("raw").as_str(), body);
}

#[test]
fn unknown_or_unscoped_fallbacks_and_plain_quotes_are_not_removed() {
    let body = "> quote\n\nvisible text";
    for fallback in [
        build_fallback_element(&FallbackIndication::for_range("urn:example:unknown", 0, 9)),
        build_fallback_element(&FallbackIndication::whole_message()),
        Element::builder("fallback", "urn:example:unknown")
            .attr(minidom::rxml::xml_ncname!("for").to_owned(), NS_REPLY)
            .build(),
    ] {
        let mut message = reply(body);
        message.payloads.push(fallback);
        assert_eq!(extracted(&message).expect("raw").as_str(), body);
    }
    assert_eq!(extracted(&reply(body)).expect("raw").as_str(), body);
}

#[test]
fn absent_malformed_or_duplicate_reply_marker_retains_raw_body() {
    let body = "> quote\n\nvisible text";
    let mut message = reply(body);
    message.payloads.clear();
    message
        .payloads
        .push(explicit_fallback(Some("0"), Some("9")));
    assert_eq!(extracted(&message).expect("raw").as_str(), body);
    message
        .payloads
        .push(Element::builder("reply", NS_REPLY).build());
    assert_eq!(extracted(&message).expect("raw").as_str(), body);
    message.payloads.push(build_reply_element(
        &ReplyReference::new("parent").with_to("author@example.org".parse().expect("JID")),
    ));
    assert_eq!(extracted(&message).expect("raw").as_str(), body);
}

#[test]
fn zero_length_range_keeps_body_and_full_explicit_range_has_no_authored_text() {
    let body = "🙂 quote";
    let mut message = reply(body);
    message
        .payloads
        .push(build_fallback_element(&FallbackIndication::for_range(
            NS_REPLY, 1, 1,
        )));
    assert_eq!(extracted(&message).expect("raw").as_str(), body);
    message.payloads.pop();
    message
        .payloads
        .push(build_fallback_element(&FallbackIndication::for_range(
            NS_REPLY,
            0,
            body.chars().count(),
        )));
    assert!(extracted(&message).is_none());
}

#[test]
fn reply_without_client_parseable_author_retains_raw_body() {
    let body = "> quote\n\nvisible text";
    for author in [None, Some(" ")] {
        let mut message = reply(body);
        message.payloads.clear();
        let mut marker = Element::builder("reply", NS_REPLY)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "parent");
        if let Some(author) = author {
            marker = marker.attr(minidom::rxml::xml_ncname!("to").to_owned(), author);
        }
        message.payloads.push(marker.build());
        message
            .payloads
            .push(explicit_fallback(Some("0"), Some("9")));
        assert_eq!(extracted(&message).expect("raw").as_str(), body);
    }
}
