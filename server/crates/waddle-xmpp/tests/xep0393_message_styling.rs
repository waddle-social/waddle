//! XEP-0393: Message Styling dedicated suite.
//!
//! Exercises the styling grammar on spec-example bodies, the
//! `<unstyled/>` stanza-level opt-out (including namespace
//! exactness), and the HTML/plain-text renderers.

use minidom::Element;
use waddle_xmpp::xep::{
    add_unstyled, blocks_to_html, blocks_to_plain, build_unstyled_element, has_unstyled,
    is_unstyled_element, parse_blocks, parse_message_body, parse_spans, strip_unstyled, Block,
    Span, StyledBody, StylingCarrier, NS_STYLING,
};
use xmpp_parsers::message::Message;

fn message_from(xml: &str) -> Message {
    Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message")
}

#[test]
fn xep0393_namespace_is_exact() {
    assert_eq!(NS_STYLING, "urn:xmpp:styling:0");
    let elem = build_unstyled_element();
    assert_eq!(elem.name(), "unstyled");
    assert_eq!(elem.ns(), NS_STYLING);
    assert!(is_unstyled_element(&elem));
}

#[test]
fn xep0393_spec_bold_example() {
    // XEP-0393 §5.1.1 example: "The full title is *Twelfth Night, or What
    // You Will* but *Twelfth Night* is commonly used."
    let spans = parse_spans("The full title is *Twelfth Night, or What You Will*.");
    assert_eq!(
        spans,
        vec![
            Span::Plain("The full title is ".to_owned()),
            Span::Bold(vec![Span::Plain(
                "Twelfth Night, or What You Will".to_owned()
            )]),
            Span::Plain(".".to_owned()),
        ]
    );
}

#[test]
fn xep0393_nested_styling_inside_bold() {
    let spans = parse_spans("*bold and _italic_ inside*");
    let Span::Bold(inner) = &spans[0] else {
        panic!("expected bold span, got {spans:?}");
    };
    assert!(inner
        .iter()
        .any(|s| matches!(s, Span::Italic(i) if i == &vec![Span::Plain("italic".to_owned())])));
}

#[test]
fn xep0393_inline_code_suppresses_nested_formatting() {
    // §5.1.4: preformatted text spans keep their content literal.
    let spans = parse_spans("`*not bold*`");
    assert_eq!(spans, vec![Span::InlineCode("*not bold*".to_owned())]);
}

#[test]
fn xep0393_mid_word_directives_are_plain_text() {
    // §4: a styling directive must not appear mid-word.
    let spans = parse_spans("snake_case_name stays plain");
    assert_eq!(
        spans,
        vec![Span::Plain("snake_case_name stays plain".to_owned())]
    );
}

#[test]
fn xep0393_opening_directive_followed_by_whitespace_is_plain() {
    let spans = parse_spans("2 * 3 * 4 = 24");
    assert_eq!(spans, vec![Span::Plain("2 * 3 * 4 = 24".to_owned())]);
}

#[test]
fn xep0393_code_block_and_quote_block_structure() {
    let body = "> quoted wisdom\nplain paragraph\n```\nlet x = 1;\n```";
    let blocks = parse_blocks(body);
    assert_eq!(blocks.len(), 3);
    assert!(matches!(&blocks[0], Block::BlockQuote(inner)
            if inner == &vec![Block::Paragraph(vec![Span::Plain("quoted wisdom".to_owned())])]));
    assert!(matches!(&blocks[1], Block::Paragraph(_)));
    assert!(matches!(&blocks[2], Block::CodeBlock(code) if code == "let x = 1;"));
}

#[test]
fn xep0393_unstyled_message_returns_body_as_single_plain_block() {
    // §6: <unstyled/> disables styling for the whole stanza.
    let msg = message_from(
        "<message xmlns='jabber:client' type='chat'>\
         <body>*not actually bold*</body>\
         <unstyled xmlns='urn:xmpp:styling:0'/>\
         </message>",
    );
    assert!(msg.styling_disabled());
    let blocks = msg.styled_body_blocks().expect("body present");
    assert_eq!(
        blocks,
        vec![Block::Paragraph(vec![Span::Plain(
            "*not actually bold*".to_owned()
        )])]
    );
}

#[test]
fn xep0393_unstyled_in_wrong_namespace_does_not_disable_styling() {
    let msg = message_from(
        "<message xmlns='jabber:client' type='chat'>\
         <body>*bold*</body>\
         <unstyled xmlns='urn:xmpp:styling:1'/>\
         </message>",
    );
    assert!(!has_unstyled(&msg));
    let blocks = parse_message_body(&msg).expect("body present");
    assert_eq!(
        blocks,
        vec![Block::Paragraph(vec![Span::Bold(vec![Span::Plain(
            "bold".to_owned()
        )])])]
    );
}

#[test]
fn xep0393_add_and_strip_unstyled_are_idempotent() {
    let mut msg =
        message_from("<message xmlns='jabber:client' type='chat'><body>hi</body></message>");
    add_unstyled(&mut msg);
    add_unstyled(&mut msg);
    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| is_unstyled_element(e))
            .count(),
        1,
        "add_unstyled must not duplicate"
    );

    strip_unstyled(&mut msg);
    assert!(!has_unstyled(&msg));
}

#[test]
fn xep0393_message_without_body_yields_no_blocks() {
    let msg = message_from("<message xmlns='jabber:client' type='chat'/>");
    assert!(msg.styled_body_blocks().is_none());
}

#[test]
fn xep0393_plain_text_extraction_strips_directives() {
    let body = "*bold* and `code`";
    assert_eq!(body.plain_text(), "bold and code");
    assert_eq!(blocks_to_plain(&body.styled_blocks()), "bold and code");
}

#[test]
fn xep0393_html_rendering_escapes_untrusted_content() {
    let blocks = parse_blocks("*<script>alert(1)</script>*");
    let html = blocks_to_html(&blocks);
    assert!(
        !html.contains("<script>"),
        "raw HTML must be escaped: {html}"
    );
    assert!(html.contains("<strong>"));
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn xep0393_html_rendering_of_mixed_blocks() {
    let blocks = parse_blocks("> _quoted_\n```\nx < y\n```");
    let html = blocks_to_html(&blocks);
    assert!(html.contains("<blockquote><p><em>quoted</em></p></blockquote>"));
    assert!(html.contains("<pre><code>x &lt; y</code></pre>"));
}

#[test]
fn xep0393_unclosed_directive_stays_plain() {
    let spans = parse_spans("*unterminated bold");
    assert_eq!(spans, vec![Span::Plain("*unterminated bold".to_owned())]);
}
