//! XEP-0393: Message Styling
//!
//! Parses inline text styling directives from XMPP message bodies.
//! This is a body-level formatting spec (not XML element payloads).
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
//! - Styling directives must start at a word boundary (start of line,
//!   or preceded by whitespace/punctuation).
//! - Closing directive must end at a word boundary.
//! - Inline code and preformatted blocks suppress all other formatting.
//! - Spans cannot cross line boundaries (except preformatted blocks).

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
                if inner.starts_with("```") {
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
        if line.starts_with("> ") || line == ">" {
            let mut quote_lines = Vec::new();
            let stripped = if line == ">" { "" } else { &line[2..] };
            quote_lines.push(stripped.to_owned());

            while let Some(next) = lines.peek() {
                if next.starts_with("> ") || *next == ">" {
                    let s = if *next == ">" {
                        "".to_owned()
                    } else {
                        next[2..].to_owned()
                    };
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
        if ch == '`' {
            if let Some(end) = find_closing_char(&chars, pos + 1, '`') {
                flush_plain(&mut plain, &mut spans);
                let code: String = chars[pos + 1..end].iter().collect();
                spans.push(Span::InlineCode(code));
                pos = end + 1;
                continue;
            }
        }

        // Styled spans: * _ ~
        if matches!(ch, '*' | '_' | '~') && is_span_start(&chars, pos) {
            if let Some(end) = find_closing_char(&chars, pos + 1, ch) {
                if is_span_end(&chars, end) {
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

fn find_closing_char(chars: &[char], start: usize, closing: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == closing)
}

fn is_span_start(chars: &[char], pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    let prev = chars[pos - 1];
    prev.is_whitespace() || is_punctuation(prev)
}

fn is_span_end(chars: &[char], pos: usize) -> bool {
    if pos + 1 >= chars.len() {
        return true;
    }
    let next = chars[pos + 1];
    next.is_whitespace() || is_punctuation(next)
}

fn is_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
    )
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
