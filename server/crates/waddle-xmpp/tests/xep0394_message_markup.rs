//! XEP-0394: Message Markup dedicated suite.
//!
//! The module builds markup metadata payloads (`urn:xmpp:markup:0`)
//! that annotate spans of the plain body without altering it. These
//! tests pin the wire shape and the empty-input contract.

use minidom::Element;
use waddle_xmpp::xep::{
    build_message_markup_element, Xep0394MarkupKind as MarkupKind, Xep0394MarkupSpan as MarkupSpan,
    NS_MESSAGE_MARKUP,
};

fn reparse(elem: &Element) -> Element {
    String::from(elem)
        .parse::<Element>()
        .expect("serialized markup is well-formed XML")
}

#[test]
fn xep0394_namespace_is_exact() {
    assert_eq!(NS_MESSAGE_MARKUP, "urn:xmpp:markup:0");
}

#[test]
fn xep0394_blockquote_span_wire_shape() {
    let elem = build_message_markup_element(&[MarkupSpan {
        kind: MarkupKind::Blockquote,
        start: 0,
        end: 8,
    }])
    .expect("non-empty spans build an element");

    assert!(elem.is("markup", NS_MESSAGE_MARKUP));
    let quote = elem
        .get_child("bquote", NS_MESSAGE_MARKUP)
        .expect("bquote child");
    assert_eq!(quote.attr("start"), Some("0"));
    assert_eq!(quote.attr("end"), Some("8"));
    assert_eq!(quote.children().count(), 0);
    assert!(quote.text().is_empty());
}

#[test]
fn xep0394_empty_span_list_builds_no_element() {
    // A <markup/> with no children carries no information; the builder
    // must omit it entirely rather than emit an empty wrapper.
    assert!(build_message_markup_element(&[]).is_none());
}

#[test]
fn xep0394_multiple_spans_preserve_order_after_wire_round_trip() {
    let spans = [
        MarkupSpan {
            kind: MarkupKind::Blockquote,
            start: 0,
            end: 8,
        },
        MarkupSpan {
            kind: MarkupKind::Blockquote,
            start: 10,
            end: 25,
        },
    ];
    let elem = reparse(&build_message_markup_element(&spans).expect("builds"));

    let children: Vec<_> = elem.children().collect();
    assert_eq!(children.len(), 2);
    for (child, span) in children.iter().zip(spans.iter()) {
        assert_eq!(child.name(), "bquote");
        assert_eq!(child.ns(), NS_MESSAGE_MARKUP);
        assert_eq!(child.attr("start"), Some(span.start.to_string().as_str()));
        assert_eq!(child.attr("end"), Some(span.end.to_string().as_str()));
    }
}

#[test]
fn xep0394_markup_attaches_to_message_without_touching_body() {
    // XEP-0394's core promise: the body remains the single textual
    // source of truth and the markup rides alongside it.
    let markup = build_message_markup_element(&[MarkupSpan {
        kind: MarkupKind::Blockquote,
        start: 0,
        end: 4,
    }])
    .expect("builds");

    let message = Element::builder("message", "jabber:client")
        .append(
            Element::builder("body", "jabber:client")
                .append("look")
                .build(),
        )
        .append(markup)
        .build();
    let reparsed = reparse(&message);

    let body = reparsed
        .get_child("body", "jabber:client")
        .expect("body child");
    assert_eq!(body.text(), "look");

    let found = reparsed
        .get_child("markup", NS_MESSAGE_MARKUP)
        .expect("markup child survives");
    assert!(found.get_child("bquote", NS_MESSAGE_MARKUP).is_some());
}

#[test]
fn xep0394_span_offsets_are_unsigned_wire_integers() {
    let elem = build_message_markup_element(&[MarkupSpan {
        kind: MarkupKind::Blockquote,
        start: u32::MAX - 1,
        end: u32::MAX,
    }])
    .expect("builds");
    let quote = elem
        .get_child("bquote", NS_MESSAGE_MARKUP)
        .expect("bquote child");
    assert_eq!(quote.attr("start"), Some("4294967294"));
    assert_eq!(quote.attr("end"), Some("4294967295"));
}
