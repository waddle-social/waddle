//! XEP-0394: Message Markup dedicated client suite.
//!
//! XEP-0394: "Spans MUST NOT overlap with each other." and "Entities MUST
//! silently ignore elements and attributes (arbitrarly deep) in <markup/>
//! which they do not understand". The client therefore emits one `<span/>`
//! per range with one child per style, and on receive matches markup
//! elements by namespace and name, collecting every recognised style.

use minidom::Element;
use waddle_xmpp_client::messaging::{
    build_outbound_message, parse, MarkupSpanData, MarkupSpanType, MessagingEvent,
    SendMessageOptions, NS_CLIENT,
};

const NS_MARKUP: &str = "urn:xmpp:markup:0";
const NS_WADDLE_MARKUP: &str = "urn:waddle:markup:0";
const NS_FOREIGN: &str = "urn:example:other:0";

fn data(kind: &str, start: u32, end: u32) -> MarkupSpanData {
    MarkupSpanData {
        span_type: kind.to_owned(),
        start,
        end,
        uri: None,
    }
}

fn inbound(xml: &str) -> Vec<(MarkupSpanType, usize, usize, Option<String>)> {
    let el: Element = xml.parse().expect("valid xml");
    match parse(&el) {
        Some(MessagingEvent::Message(m)) => m
            .markup_spans
            .iter()
            .map(|s| (s.span_type.clone(), s.start, s.end, s.uri.clone()))
            .collect(),
        other => panic!("expected inbound message, got {other:?}"),
    }
}

fn message_with(markup: &str) -> String {
    format!(
        "<message xmlns='jabber:client' type='chat' from='a@example.com/r' to='b@example.com' id='m1'>\
         <body>hello world</body>{markup}</message>"
    )
}

#[test]
fn xep0394_combined_inline_styles_are_one_span_with_children() {
    let mut options = SendMessageOptions::default();
    options.markup_spans = vec![
        data("bold", 0, 5),
        data("italic", 0, 5),
        data("code", 6, 11),
    ];
    let (_, stanza) =
        build_outbound_message("b@example.com", "chat", "hello world", &options).expect("builds");
    assert!(stanza.get_child("body", NS_CLIENT).is_some());
    let markup = stanza.get_child("markup", NS_MARKUP).expect("markup");
    let spans: Vec<&Element> = markup.children().collect();
    assert_eq!(spans.len(), 2, "one span per range, no overlap");
    let names: Vec<&str> = spans[0].children().map(|c| c.name()).collect();
    assert_eq!(names, ["strong", "emphasis"]);
    assert!(spans[0].children().all(|c| c.ns() == NS_MARKUP));
    assert_eq!(spans[0].attr("start"), Some("0"));
    assert_eq!(spans[0].attr("end"), Some("5"));
    assert_eq!(spans[1].children().count(), 1);
}

#[test]
fn xep0394_combined_styles_round_trip() {
    let mut options = SendMessageOptions::default();
    options.markup_spans = vec![
        data("bold", 0, 5),
        data("italic", 0, 5),
        data("strikethrough", 0, 5),
    ];
    let (_, stanza) =
        build_outbound_message("b@example.com", "chat", "hello world", &options).expect("builds");
    let parsed = inbound(&String::from(&stanza));
    assert_eq!(
        parsed,
        vec![
            (MarkupSpanType::Bold, 0, 5, None),
            (MarkupSpanType::Italic, 0, 5, None),
            (MarkupSpanType::Strikethrough, 0, 5, None),
        ]
    );
}

#[test]
fn xep0394_inbound_span_with_several_children_keeps_every_style() {
    let parsed = inbound(&message_with(
        "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='5'><strong/><emphasis/><code/></span></markup>",
    ));
    assert_eq!(
        parsed,
        vec![
            (MarkupSpanType::Bold, 0, 5, None),
            (MarkupSpanType::Italic, 0, 5, None),
            (MarkupSpanType::Code, 0, 5, None),
        ]
    );
}

#[test]
fn xep0394_foreign_namespace_elements_are_ignored() {
    let parsed = inbound(&message_with(&format!(
        "<markup xmlns='urn:xmpp:markup:0'>\
         <span xmlns='{NS_FOREIGN}' start='0' end='5'><strong xmlns='{NS_FOREIGN}'/></span>\
         <bcode xmlns='{NS_FOREIGN}' start='0' end='5'/>\
         <bquote xmlns='{NS_FOREIGN}' start='0' end='5'/>\
         <span start='6' end='11'><strong xmlns='{NS_FOREIGN}'/><emphasis/></span>\
         </markup>"
    )));
    assert_eq!(parsed, vec![(MarkupSpanType::Italic, 6, 11, None)]);
}

#[test]
fn xep0394_uri_on_official_span_is_an_unknown_attribute() {
    let parsed = inbound(&message_with(
        "<markup xmlns='urn:xmpp:markup:0'>\
         <span start='0' end='5' uri='https://example.com'><strong/></span>\
         <span start='6' end='11' uri='https://example.com'/></markup>",
    ));
    assert_eq!(parsed, vec![(MarkupSpanType::Bold, 0, 5, None)]);
}

#[test]
fn xep0394_waddle_link_span_requires_waddle_namespace() {
    let parsed = inbound(&message_with(&format!(
        "<markup xmlns='urn:xmpp:markup:0'>\
         <span xmlns='{NS_WADDLE_MARKUP}' start='6' end='11' uri='https://example.com'/>\
         <span xmlns='{NS_FOREIGN}' start='0' end='5' uri='https://evil.example'/>\
         </markup>"
    )));
    assert_eq!(
        parsed,
        vec![(
            MarkupSpanType::Link,
            6,
            11,
            Some("https://example.com".to_owned())
        )]
    );
}

#[test]
fn xep0394_block_elements_in_official_namespace_still_parse() {
    let parsed = inbound(&message_with(
        "<markup xmlns='urn:xmpp:markup:0'><bcode start='0' end='5'/><bquote start='6' end='11'/></markup>",
    ));
    assert_eq!(
        parsed,
        vec![
            (MarkupSpanType::CodeBlock, 0, 5, None),
            (MarkupSpanType::Blockquote, 6, 11, None),
        ]
    );
}
