use super::*;
use xmpp_parsers::message::{Lang, Message};

// ── Inline span tests ────────────────────────────────────

#[test]
fn test_plain_text() {
    let spans = parse_spans("Hello world");
    assert_eq!(spans, vec![Span::Plain("Hello world".into())]);
}

#[test]
fn test_bold() {
    let spans = parse_spans("Hello *world*");
    assert_eq!(
        spans,
        vec![
            Span::Plain("Hello ".into()),
            Span::Bold(vec![Span::Plain("world".into())]),
        ]
    );
}

#[test]
fn test_italic() {
    let spans = parse_spans("Hello _world_");
    assert_eq!(
        spans,
        vec![
            Span::Plain("Hello ".into()),
            Span::Italic(vec![Span::Plain("world".into())]),
        ]
    );
}

#[test]
fn test_strikethrough() {
    let spans = parse_spans("Hello ~world~");
    assert_eq!(
        spans,
        vec![
            Span::Plain("Hello ".into()),
            Span::Strikethrough(vec![Span::Plain("world".into())]),
        ]
    );
}

#[test]
fn test_inline_code() {
    let spans = parse_spans("Use `println!` here");
    assert_eq!(
        spans,
        vec![
            Span::Plain("Use ".into()),
            Span::InlineCode("println!".into()),
            Span::Plain(" here".into()),
        ]
    );
}

#[test]
fn test_nested_bold_italic() {
    let spans = parse_spans("*bold _and italic_*");
    assert_eq!(
        spans,
        vec![Span::Bold(vec![
            Span::Plain("bold ".into()),
            Span::Italic(vec![Span::Plain("and italic".into())]),
        ])]
    );
}

#[test]
fn test_no_styling_mid_word() {
    // * mid-word should not trigger styling
    let spans = parse_spans("foo*bar*baz");
    assert_eq!(spans, vec![Span::Plain("foo*bar*baz".into())]);
}

#[test]
fn test_styling_at_start_of_line() {
    let spans = parse_spans("*bold* text");
    assert_eq!(
        spans,
        vec![
            Span::Bold(vec![Span::Plain("bold".into())]),
            Span::Plain(" text".into()),
        ]
    );
}

#[test]
fn xep0393_span_closing_may_be_followed_by_plain_text() {
    let spans = parse_spans("*strong*plain*");
    assert_eq!(
        spans,
        vec![
            Span::Bold(vec![Span::Plain("strong".into())]),
            Span::Plain("plain*".into()),
        ]
    );
}

#[test]
fn xep0393_empty_directives_are_plain() {
    for input in ["**", "***", "****", "__", "~~", "``"] {
        assert_eq!(
            parse_spans(input),
            vec![Span::Plain(input.into())],
            "{input} must not create an empty span"
        );
    }
}

#[test]
fn xep0393_opening_followed_by_whitespace_is_plain() {
    for input in ["* strong*", "_ emphasis_", "~ strike~", "` code`"] {
        assert_eq!(
            parse_spans(input),
            vec![Span::Plain(input.into())],
            "{input} must not style when opener is followed by whitespace"
        );
    }
}

#[test]
fn xep0393_closing_preceded_by_whitespace_is_plain() {
    for input in ["*strong *", "_emphasis _", "~strike ~", "`code `"] {
        assert_eq!(
            parse_spans(input),
            vec![Span::Plain(input.into())],
            "{input} must not style when closer is preceded by whitespace"
        );
    }
}

#[test]
fn xep0393_opening_after_punctuation_is_not_a_directive() {
    let spans = parse_spans("a(*strong*)");
    assert_eq!(spans, vec![Span::Plain("a(*strong*)".into())]);
}

#[test]
fn xep0393_nested_opening_after_different_directive_is_valid() {
    let spans = parse_spans("*_strong emphasis_*");
    assert_eq!(
        spans,
        vec![Span::Bold(vec![Span::Italic(vec![Span::Plain(
            "strong emphasis".into()
        )])])]
    );
}

#[test]
fn xep0393_invalid_directives_are_skipped_when_matching() {
    let spans = parse_spans("*a * b*");
    assert_eq!(spans, vec![Span::Bold(vec![Span::Plain("a * b".into())])]);
}

#[test]
fn xep0393_invalid_opening_does_not_block_later_span() {
    let spans = parse_spans("* plain *strong*");
    assert_eq!(
        spans,
        vec![
            Span::Plain("* plain ".into()),
            Span::Bold(vec![Span::Plain("strong".into())]),
        ]
    );
}

#[test]
fn xep0393_unclosed_or_invalid_closing_directive_is_plain() {
    for input in ["not strong*", "*not strong", "*not *strong"] {
        assert_eq!(parse_spans(input), vec![Span::Plain(input.into())]);
    }
}

#[test]
fn test_unclosed_directive_is_plain() {
    let spans = parse_spans("Hello *world");
    assert_eq!(spans, vec![Span::Plain("Hello *world".into())]);
}

// ── Block-level tests ────────────────────────────────────

#[test]
fn test_code_block() {
    let input = "```\nfn main() {\n    println!(\"hello\");\n}\n```";
    let blocks = parse_blocks(input);
    assert_eq!(
        blocks,
        vec![Block::CodeBlock(
            "fn main() {\n    println!(\"hello\");\n}".into()
        )]
    );
}

#[test]
fn xep0393_preformatted_block_closes_only_on_exact_fence() {
    let input = "```\ncode\n```ignored\nstill code\n```\nplain";
    let blocks = parse_blocks(input);
    assert_eq!(
        blocks,
        vec![
            Block::CodeBlock("code\n```ignored\nstill code".into()),
            Block::Paragraph(vec![Span::Plain("plain".into())]),
        ]
    );
}

#[test]
fn test_block_quote() {
    let input = "> This is quoted\n> Second line";
    let blocks = parse_blocks(input);
    assert_eq!(
        blocks,
        vec![Block::BlockQuote(vec![
            Block::Paragraph(vec![Span::Plain("This is quoted".into())]),
            Block::Paragraph(vec![Span::Plain("Second line".into())]),
        ])]
    );
}

#[test]
fn xep0393_quote_line_may_begin_with_greater_than_without_space() {
    let blocks = parse_blocks(">quoted\n>> nested");
    assert_eq!(
        blocks,
        vec![Block::BlockQuote(vec![
            Block::Paragraph(vec![Span::Plain("quoted".into())]),
            Block::BlockQuote(vec![Block::Paragraph(vec![Span::Plain("nested".into())])]),
        ])]
    );
}

#[test]
fn test_mixed_blocks() {
    let input = "Hello *world*\n\n> A quote\n\n```\ncode\n```";
    let blocks = parse_blocks(input);
    assert_eq!(blocks.len(), 3);
    assert!(matches!(&blocks[0], Block::Paragraph(_)));
    assert!(matches!(&blocks[1], Block::BlockQuote(_)));
    assert!(matches!(&blocks[2], Block::CodeBlock(_)));
}

#[test]
fn test_empty_input() {
    let blocks = parse_blocks("");
    assert!(blocks.is_empty());
}

// ── Plain text extraction ────────────────────────────────

#[test]
fn test_plain_text_extraction() {
    let blocks = parse_blocks("Hello *bold* and `code`");
    assert_eq!(blocks_to_plain(&blocks), "Hello bold and code");
}

#[test]
fn test_styled_body_trait() {
    let text = "*hello* _world_";
    assert_eq!(text.plain_text(), "hello world");
}

// ── HTML rendering ───────────────────────────────────────

#[test]
fn test_html_paragraph() {
    let blocks = parse_blocks("Hello *bold*");
    assert_eq!(
        blocks_to_html(&blocks),
        "<p>Hello <strong>bold</strong></p>"
    );
}

#[test]
fn test_html_code_block() {
    let blocks = parse_blocks("```\n<script>alert(1)</script>\n```");
    assert_eq!(
        blocks_to_html(&blocks),
        "<pre><code>&lt;script&gt;alert(1)&lt;/script&gt;</code></pre>"
    );
}

#[test]
fn test_html_inline_code() {
    let blocks = parse_blocks("Use `<div>` here");
    assert_eq!(
        blocks_to_html(&blocks),
        "<p>Use <code>&lt;div&gt;</code> here</p>"
    );
}

#[test]
fn test_html_blockquote() {
    let blocks = parse_blocks("> Quoted *text*");
    assert_eq!(
        blocks_to_html(&blocks),
        "<blockquote><p>Quoted <strong>text</strong></p></blockquote>"
    );
}

#[test]
fn test_html_all_styles() {
    let blocks = parse_blocks("*bold* _italic_ ~strike~ `code`");
    assert_eq!(
        blocks_to_html(&blocks),
        "<p><strong>bold</strong> <em>italic</em> <del>strike</del> <code>code</code></p>"
    );
}

#[test]
fn test_html_escaping() {
    let blocks = parse_blocks("a < b & c > d");
    assert_eq!(blocks_to_html(&blocks), "<p>a &lt; b &amp; c &gt; d</p>");
}

#[test]
fn test_empty_quote_line() {
    let blocks = parse_blocks(">\n> text");
    assert_eq!(
        blocks,
        vec![Block::BlockQuote(vec![Block::Paragraph(vec![
            Span::Plain("text".into())
        ])])]
    );
}

// ── Stanza-level opt-out ────────────────────────────────

#[test]
fn xep0393_unstyled_element_uses_canonical_namespace() {
    let elem = build_unstyled_element();
    assert_eq!(elem.name(), "unstyled");
    assert_eq!(elem.ns(), NS_STYLING);
}

#[test]
fn xep0393_message_with_unstyled_child_returns_plain_blocks() {
    let mut msg = message_with_body("> _ <");
    add_unstyled(&mut msg);

    assert!(msg.styling_disabled());
    assert_eq!(
        msg.styled_body_blocks(),
        Some(vec![Block::Paragraph(vec![Span::Plain("> _ <".into())])])
    );
}

#[test]
fn xep0393_unstyled_wrong_namespace_does_not_disable_styling() {
    let mut msg = message_with_body("*styled*");
    msg.payloads
        .push(Element::builder("unstyled", "urn:example:styling").build());

    assert!(!has_unstyled(&msg));
    assert_eq!(
        parse_message_body(&msg),
        Some(vec![Block::Paragraph(vec![Span::Bold(vec![Span::Plain(
            "styled".into()
        )])])])
    );
}

#[test]
fn xep0393_non_empty_unstyled_element_does_not_disable_styling() {
    let mut msg = message_with_body("*styled*");
    msg.payloads.push(
        Element::builder("unstyled", NS_STYLING)
            .append("not empty")
            .build(),
    );

    assert!(!has_unstyled(&msg));
    assert_eq!(
        parse_message_body(&msg),
        Some(vec![Block::Paragraph(vec![Span::Bold(vec![Span::Plain(
            "styled".into()
        )])])])
    );
}

#[test]
fn xep0393_message_body_parser_requires_default_language_body() {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.bodies.insert(Lang::from("cy"), "*styled*".to_owned());

    assert_eq!(parse_message_body(&msg), None);
}

#[test]
fn xep0393_adjacent_directive_chain_is_checked_iteratively() {
    let mut input = String::with_capacity(10_002);
    for directive in ['*', '_', '~', '`'].into_iter().cycle().take(10_000) {
        input.push(directive);
    }
    input.push_str("plain");
    input.push('*');

    let spans = parse_spans(&input);

    assert!(!spans.is_empty());
    assert!(spans_to_plain(&spans).contains("plain"));
}

#[test]
fn xep0393_strip_unstyled_removes_only_styling_opt_out() {
    let mut msg = message_with_body("*styled*");
    add_unstyled(&mut msg);
    msg.payloads
        .push(Element::builder("unstyled", "urn:example:styling").build());

    strip_unstyled(&mut msg);

    assert!(!has_unstyled(&msg));
    assert_eq!(msg.payloads.len(), 1);
    assert_eq!(msg.payloads[0].ns(), "urn:example:styling");
}

fn message_with_body(body: &str) -> Message {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.bodies.insert(Lang::new(), body.to_owned());
    msg
}
