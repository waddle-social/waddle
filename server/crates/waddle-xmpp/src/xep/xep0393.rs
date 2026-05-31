//! XEP-0393: Message Styling
//!
//! Parses inline text styling directives from XMPP message bodies.
//! The text styling grammar operates on `<body/>` text; the
//! `<unstyled/>` payload is the stanza-level opt-out defined by the XEP.
//!
//! ## Supported Styles
//!
//! - `*bold*` → Bold
//! - `_italic_` → Italic
//! - `~strikethrough~` → Strikethrough
//! - `` `monospace` `` → Inline code (no nesting)
//! - ```` ```\ncode block\n``` ```` → Preformatted block (no nesting)
//! - `> quote` at line start → Block quote
//!
//! ## Rules
//!
//! - Styling directives must start at the beginning of a line, after
//!   whitespace, or after a different opening styling directive.
//! - Opening directives must not be followed by whitespace; closing
//!   directives must not be preceded by whitespace.
//! - Inline code and preformatted blocks suppress all other formatting.
//! - Spans cannot cross line boundaries (except preformatted blocks).

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0393 Message Styling.
pub const NS_STYLING: &str = "urn:xmpp:styling:0";

/// A parsed span of styled text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    /// Plain unstyled text.
    Plain(String),
    /// `*bold*`
    Bold(Vec<Span>),
    /// `_italic_`
    Italic(Vec<Span>),
    /// `~strikethrough~`
    Strikethrough(Vec<Span>),
    /// `` `monospace` `` — no nesting allowed.
    InlineCode(String),
}

/// A parsed block of styled content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A paragraph of inline spans.
    Paragraph(Vec<Span>),
    /// A preformatted code block (``` fenced).
    CodeBlock(String),
    /// A block quote (`> ` prefixed lines).
    BlockQuote(Vec<Block>),
}

/// Trait for types whose body text can be parsed for styling.
pub trait StyledBody {
    /// Parse the message body into styled blocks.
    fn styled_blocks(&self) -> Vec<Block>;

    /// Strip all formatting and return plain text.
    fn plain_text(&self) -> String {
        blocks_to_plain(&self.styled_blocks())
    }
}

impl StyledBody for str {
    fn styled_blocks(&self) -> Vec<Block> {
        parse_blocks(self)
    }
}

impl StyledBody for String {
    fn styled_blocks(&self) -> Vec<Block> {
        parse_blocks(self)
    }
}

/// Trait for XMPP messages whose styling payload can be interpreted.
pub trait StylingCarrier {
    /// Returns `true` when the message carries XEP-0393 `<unstyled/>`.
    fn styling_disabled(&self) -> bool;

    /// Parse the default body, respecting the XEP-0393 `<unstyled/>` opt-out.
    fn styled_body_blocks(&self) -> Option<Vec<Block>>;
}

impl StylingCarrier for Message {
    fn styling_disabled(&self) -> bool {
        has_unstyled(self)
    }

    fn styled_body_blocks(&self) -> Option<Vec<Block>> {
        parse_message_body(self)
    }
}

// ── Stanza-level opt-out ────────────────────────────────────────────

/// Check if an element is `<unstyled xmlns='urn:xmpp:styling:0'/>`.
pub fn is_unstyled_element(elem: &Element) -> bool {
    elem.ns() == NS_STYLING
        && elem.name() == "unstyled"
        && elem.children().next().is_none()
        && elem.text().is_empty()
}

/// Check if a message carries the XEP-0393 message-level styling opt-out.
pub fn has_unstyled(msg: &Message) -> bool {
    msg.payloads.iter().any(is_unstyled_element)
}

/// Build an empty `<unstyled xmlns='urn:xmpp:styling:0'/>` element.
pub fn build_unstyled_element() -> Element {
    Element::builder("unstyled", NS_STYLING).build()
}

/// Add the XEP-0393 message-level styling opt-out if it is not present.
pub fn add_unstyled(msg: &mut Message) {
    if !has_unstyled(msg) {
        msg.payloads.push(build_unstyled_element());
    }
}

/// Remove all XEP-0393 styling opt-out payloads from a message.
pub fn strip_unstyled(msg: &mut Message) {
    msg.payloads.retain(|elem| !is_unstyled_element(elem));
}

/// Parse the default message body, bypassing styling when `<unstyled/>` exists.
pub fn parse_message_body(msg: &Message) -> Option<Vec<Block>> {
    let body = default_body(msg)?;
    if has_unstyled(msg) {
        return Some(vec![Block::Paragraph(vec![Span::Plain(body.to_owned())])]);
    }
    Some(parse_blocks(body))
}

fn default_body(msg: &Message) -> Option<&str> {
    msg.bodies.get("").map(String::as_str)
}

// ── Block-level parsing ──────────────────────────────────────────────

/// Parse a message body into blocks.
pub fn parse_blocks(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines = input.lines().peekable();

    while let Some(line) = lines.next() {
        // Preformatted block
        if line.starts_with("```") {
            let mut code = String::new();
            for inner in lines.by_ref() {
                if inner == "```" {
                    break;
                }
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(inner);
            }
            blocks.push(Block::CodeBlock(code));
            continue;
        }

        // Block quote
        if let Some(stripped) = quote_line_body(line) {
            let mut quote_lines = Vec::new();
            quote_lines.push(stripped.to_owned());

            while let Some(next) = lines.peek() {
                if let Some(stripped) = quote_line_body(next) {
                    let s = stripped.to_owned();
                    quote_lines.push(s);
                    lines.next();
                } else {
                    break;
                }
            }

            let inner_text = quote_lines.join("\n");
            let inner_blocks = parse_blocks(&inner_text);
            blocks.push(Block::BlockQuote(inner_blocks));
            continue;
        }

        // Skip empty lines between blocks
        if line.is_empty() {
            continue;
        }

        // Regular paragraph
        let spans = parse_spans(line);
        blocks.push(Block::Paragraph(spans));
    }

    blocks
}

fn quote_line_body(line: &str) -> Option<&str> {
    let quoted = line.strip_prefix('>')?;
    Some(trim_one_leading_whitespace(quoted))
}

fn trim_one_leading_whitespace(input: &str) -> &str {
    input
        .char_indices()
        .next()
        .filter(|(_, ch)| ch.is_whitespace())
        .map_or(input, |(idx, ch)| &input[idx + ch.len_utf8()..])
}

// ── Inline span parsing ─────────────────────────────────────────────

/// Parse inline styling spans from a single line of text.
pub fn parse_spans(input: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0;
    let mut plain = String::new();

    while pos < chars.len() {
        let ch = chars[pos];

        // Inline code: ` ... `
        if ch == '`' && is_span_start(&chars, pos, ch) {
            if let Some(end) = find_closing_directive(&chars, pos, ch) {
                flush_plain(&mut plain, &mut spans);
                let code: String = chars[pos + 1..end].iter().collect();
                spans.push(Span::InlineCode(code));
                pos = end + 1;
                continue;
            }
        }

        // Styled spans: * _ ~
        if matches!(ch, '*' | '_' | '~') && is_span_start(&chars, pos, ch) {
            if let Some(end) = find_closing_directive(&chars, pos, ch) {
                flush_plain(&mut plain, &mut spans);
                let inner: String = chars[pos + 1..end].iter().collect();
                let inner_spans = parse_spans(&inner);
                let styled = match ch {
                    '*' => Span::Bold(inner_spans),
                    '_' => Span::Italic(inner_spans),
                    '~' => Span::Strikethrough(inner_spans),
                    _ => unreachable!(),
                };
                spans.push(styled);
                pos = end + 1;
                continue;
            }
        }

        plain.push(ch);
        pos += 1;
    }

    flush_plain(&mut plain, &mut spans);
    spans
}

fn flush_plain(plain: &mut String, spans: &mut Vec<Span>) {
    if !plain.is_empty() {
        spans.push(Span::Plain(std::mem::take(plain)));
    }
}

fn find_closing_directive(chars: &[char], opening: usize, closing: char) -> Option<usize> {
    for pos in opening + 1..chars.len() {
        if chars[pos] != closing {
            continue;
        }
        if pos == opening + 1 {
            return None;
        }
        if chars[pos - 1].is_whitespace() {
            continue;
        }
        return Some(pos);
    }
    None
}

fn is_span_start(chars: &[char], pos: usize, directive: char) -> bool {
    if chars.get(pos + 1).is_none_or(|next| next.is_whitespace()) {
        return false;
    }

    if pos == 0 {
        return true;
    }

    let prev = chars[pos - 1];
    prev.is_whitespace() || is_after_different_opening_directive(chars, pos - 1, directive)
}

fn is_after_different_opening_directive(chars: &[char], mut pos: usize, directive: char) -> bool {
    let mut current = chars[pos];
    if !is_styling_directive(current) || current == directive {
        return false;
    }

    loop {
        if pos == 0 {
            return true;
        }

        let prev = chars[pos - 1];
        if prev.is_whitespace() {
            return true;
        }

        if !is_styling_directive(prev) || prev == current {
            return false;
        }

        pos -= 1;
        current = prev;
    }
}

fn is_styling_directive(ch: char) -> bool {
    matches!(ch, '*' | '_' | '~' | '`')
}

// ── Plain text extraction ────────────────────────────────────────────

/// Convert styled blocks back to plain text (strip formatting).
pub fn blocks_to_plain(blocks: &[Block]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match block {
            Block::Paragraph(spans) => out.push_str(&spans_to_plain(spans)),
            Block::CodeBlock(code) => out.push_str(code),
            Block::BlockQuote(inner) => out.push_str(&blocks_to_plain(inner)),
        }
    }
    out
}

/// Convert inline spans to plain text.
pub fn spans_to_plain(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            Span::Plain(text) | Span::InlineCode(text) => out.push_str(text),
            Span::Bold(inner) | Span::Italic(inner) | Span::Strikethrough(inner) => {
                out.push_str(&spans_to_plain(inner));
            }
        }
    }
    out
}

// ── HTML rendering ───────────────────────────────────────────────────

/// Render styled blocks to HTML.
pub fn blocks_to_html(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                out.push_str("<p>");
                out.push_str(&spans_to_html(spans));
                out.push_str("</p>");
            }
            Block::CodeBlock(code) => {
                out.push_str("<pre><code>");
                out.push_str(&html_escape(code));
                out.push_str("</code></pre>");
            }
            Block::BlockQuote(inner) => {
                out.push_str("<blockquote>");
                out.push_str(&blocks_to_html(inner));
                out.push_str("</blockquote>");
            }
        }
    }
    out
}

/// Render inline spans to HTML.
pub fn spans_to_html(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            Span::Plain(text) => out.push_str(&html_escape(text)),
            Span::Bold(inner) => {
                out.push_str("<strong>");
                out.push_str(&spans_to_html(inner));
                out.push_str("</strong>");
            }
            Span::Italic(inner) => {
                out.push_str("<em>");
                out.push_str(&spans_to_html(inner));
                out.push_str("</em>");
            }
            Span::Strikethrough(inner) => {
                out.push_str("<del>");
                out.push_str(&spans_to_html(inner));
                out.push_str("</del>");
            }
            Span::InlineCode(text) => {
                out.push_str("<code>");
                out.push_str(&html_escape(text));
                out.push_str("</code>");
            }
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
}

#[cfg(test)]
mod tests;
