use super::*;

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
