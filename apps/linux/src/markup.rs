//! XEP-0394 message markup: what the composer sends and what rows draw.
//!
//! Sending follows the web and Apple composers: the user types, or the
//! formatting bar inserts, markdown markers; at send time they become a
//! clean body plus spans (`**bold**`, `*italic*`, `~~strike~~`,
//! `` `code` ``, fenced code blocks and `>` quote lines). A blockquote
//! span leaves its `>` markers in the body, as the web does; receivers
//! strip them. Offsets count Unicode scalar values (Rust `char`s) over
//! the body, end exclusive.
//!
//! The shared timeline carries no markup, so received and own spans ride
//! in an app-side index beside it (like `media_index`), keyed by
//! conversation and message id. Each entry remembers the body it was
//! made for and is used only while the row's body is still that body, so
//! a correction can never draw old spans over new text.

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};

use waddle_xmpp_client_ffi::{WaddleMarkupSpan, WaddleMarkupSpanType};

/// Entries kept in the index.
const CAPACITY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Blockquote,
    /// Waddle's `urn:waddle:markup:0` link span; carries a `uri`.
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub style: Style,
    pub start: usize,
    pub end: usize,
    pub uri: Option<String>,
}

impl Span {
    fn new(style: Style, start: usize, end: usize) -> Self {
        Self {
            style,
            start,
            end,
            uri: None,
        }
    }
}

/// The markup spans of a message, as the wire carries them.
pub fn from_wire(spans: &[WaddleMarkupSpan]) -> Vec<Span> {
    spans
        .iter()
        .map(|span| Span {
            style: match span.span_type {
                WaddleMarkupSpanType::Bold => Style::Bold,
                WaddleMarkupSpanType::Italic => Style::Italic,
                WaddleMarkupSpanType::Strikethrough => Style::Strikethrough,
                WaddleMarkupSpanType::Code => Style::Code,
                WaddleMarkupSpanType::CodeBlock => Style::CodeBlock,
                WaddleMarkupSpanType::Blockquote => Style::Blockquote,
                WaddleMarkupSpanType::Link => Style::Link,
            },
            start: span.start as usize,
            end: span.end as usize,
            uri: span.uri.clone(),
        })
        .collect()
}

/// Spans for the wire, moved `offset` characters right (a reply's
/// fallback quote comes first in the body). Links are never composed.
pub fn to_wire(spans: &[Span], offset: usize) -> Vec<WaddleMarkupSpan> {
    spans
        .iter()
        .filter(|span| span.style != Style::Link)
        .filter_map(|span| {
            Some(WaddleMarkupSpan {
                span_type: match span.style {
                    Style::Bold => WaddleMarkupSpanType::Bold,
                    Style::Italic => WaddleMarkupSpanType::Italic,
                    Style::Strikethrough => WaddleMarkupSpanType::Strikethrough,
                    Style::Code => WaddleMarkupSpanType::Code,
                    Style::CodeBlock => WaddleMarkupSpanType::CodeBlock,
                    Style::Blockquote => WaddleMarkupSpanType::Blockquote,
                    Style::Link => return None,
                },
                start: u32::try_from(span.start + offset).ok()?,
                end: u32::try_from(span.end + offset).ok()?,
                uri: None,
            })
        })
        .collect()
}

/// Spans over a body with its XEP-0428 fallback range removed. A range
/// the body cannot hold removes nothing, as `strip_fallback` does.
pub fn without_fallback(
    spans: Vec<Span>,
    body_len: usize,
    fallback: Option<(u32, u32)>,
) -> Vec<Span> {
    let Some((start, end)) = fallback else {
        return spans;
    };
    let (start, end) = (start as usize, end as usize);
    if start > end || end > body_len {
        return spans;
    }
    let width = end - start;
    let map = |at: usize| {
        if at <= start {
            at
        } else if at >= end {
            at - width
        } else {
            start
        }
    };
    spans
        .into_iter()
        .filter_map(|span| {
            let (from, to) = (map(span.start), map(span.end));
            (from < to).then_some(Span {
                start: from,
                end: to,
                ..span
            })
        })
        .collect()
}

/// The same spans, whatever their order.
pub fn same_spans(a: &[Span], b: &[Span]) -> bool {
    a.len() == b.len() && a.iter().all(|span| b.contains(span))
}

// ---------------------------------------------------------------------
// Composing: markdown markers to body and spans.

/// A draft turned into its wire body and markup.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Composed {
    pub body: String,
    pub spans: Vec<Span>,
}

/// Markers removed from the text: `length` characters at `start`.
type Removals = Vec<(usize, usize)>;

fn map_offset(removals: &Removals, offset: usize) -> usize {
    let mut shifted = offset;
    for &(start, length) in removals {
        if offset <= start {
            break;
        }
        shifted -= length.min(offset - start);
    }
    shifted
}

/// Convert the markdown in `text` (already trimmed) to a body and spans.
pub fn compose(text: &str) -> Composed {
    let chars: Vec<char> = text.chars().collect();
    let (body, code_blocks) = extract_fences(&chars);
    let (body, mut spans, removals) = apply_inline(&body, &code_blocks);
    let code_blocks: Vec<(usize, usize)> = code_blocks
        .into_iter()
        .map(|(start, end)| (map_offset(&removals, start), map_offset(&removals, end)))
        .collect();
    let mut all: Vec<Span> = code_blocks
        .iter()
        .map(|&(start, end)| Span::new(Style::CodeBlock, start, end))
        .collect();
    all.append(&mut spans);
    all.extend(blockquotes(&body, &code_blocks));
    all.sort_by_key(|span| (span.start, span.end));
    Composed {
        body: body.into_iter().collect(),
        spans: all,
    }
}

fn has_prefix(chars: &[char], prefix: &[char], at: usize) -> bool {
    at + prefix.len() <= chars.len() && chars[at..at + prefix.len()] == *prefix
}

/// Pass A: fenced code blocks. Returns the body without fence lines,
/// and the code content ranges over it.
fn extract_fences(chars: &[char]) -> (Vec<char>, Vec<(usize, usize)>) {
    const FENCE: [char; 3] = ['`', '`', '`'];
    let lines: Vec<&[char]> = chars.split(|c| *c == '\n').collect();
    let mut keep = vec![true; lines.len()];
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut opener: Option<usize> = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(open) = opener {
            let trimmed: Vec<char> = line
                .iter()
                .copied()
                .collect::<String>()
                .trim()
                .chars()
                .collect();
            if trimmed != FENCE {
                continue;
            }
            keep[open] = false;
            keep[index] = false;
            if index > open + 1 {
                groups.push((open + 1, index - 1));
            }
            opener = None;
        } else if has_prefix(line, &FENCE, 0) {
            opener = Some(index);
        }
    }
    let mut kept: Vec<char> = Vec::new();
    let mut starts = vec![0usize; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            if !kept.is_empty() {
                kept.push('\n');
            }
            starts[index] = kept.len();
            kept.extend_from_slice(line);
        }
    }
    let blocks = groups
        .into_iter()
        .map(|(first, last)| (starts[first], starts[last] + lines[last].len()))
        .collect();
    (kept, blocks)
}

struct Rule {
    marker: &'static [char],
    style: Style,
    excluded: &'static [char],
    flanked: bool,
}

/// Priority at one position: code, then strike, bold, italic.
const RULES: [Rule; 4] = [
    Rule {
        marker: &['`'],
        style: Style::Code,
        excluded: &['`', '\n'],
        flanked: false,
    },
    Rule {
        marker: &['~', '~'],
        style: Style::Strikethrough,
        excluded: &['\n'],
        flanked: false,
    },
    Rule {
        marker: &['*', '*'],
        style: Style::Bold,
        excluded: &['\n'],
        flanked: true,
    },
    Rule {
        marker: &['*'],
        style: Style::Italic,
        excluded: &['*', '\n'],
        flanked: true,
    },
];

fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// End (after the closing marker) of `rule` matched at `start`.
fn match_end(rule: &Rule, body: &[char], start: usize) -> Option<usize> {
    if !has_prefix(body, rule.marker, start) {
        return None;
    }
    let from = start + rule.marker.len();
    let content_end = if rule.flanked {
        flanked_end(rule, body, from)
    } else {
        plain_end(rule, body, from)
    };
    content_end.map(|end| end + rule.marker.len())
}

fn plain_end(rule: &Rule, body: &[char], from: usize) -> Option<usize> {
    let mut end = from + 1;
    while end <= body.len() {
        if rule.excluded.contains(&body[end - 1]) {
            return None;
        }
        if has_prefix(body, rule.marker, end) {
            return Some(end);
        }
        end += 1;
    }
    None
}

/// Content starts and ends on non-space, with nothing excluded between.
fn flanked_end(rule: &Rule, body: &[char], from: usize) -> Option<usize> {
    if from >= body.len() || is_space(body[from]) {
        return None;
    }
    let mut end = from + 1;
    while end <= body.len() {
        if end >= from + 3 && rule.excluded.contains(&body[end - 2]) {
            return None;
        }
        if !is_space(body[end - 1]) && has_prefix(body, rule.marker, end) {
            return Some(end);
        }
        end += 1;
    }
    None
}

/// Pass B: inline markers, one left-to-right scan. A match that
/// overlaps a code block is skipped.
fn apply_inline(body: &[char], protected: &[(usize, usize)]) -> (Vec<char>, Vec<Span>, Removals) {
    let mut result: Vec<char> = Vec::new();
    let mut spans = Vec::new();
    let mut removals = Vec::new();
    let mut search_from = 0;
    while search_from < body.len() {
        let mut found = None;
        let mut position = search_from;
        while position < body.len() && found.is_none() {
            found = RULES
                .iter()
                .find_map(|rule| match_end(rule, body, position).map(|end| (rule, end)))
                .filter(|&(_, end)| {
                    !protected
                        .iter()
                        .any(|&(from, to)| position < to && from < end)
                })
                .map(|(rule, end)| (rule, position, end));
            position += 1;
        }
        let Some((rule, start, end)) = found else {
            break;
        };
        let marker = rule.marker.len();
        result.extend_from_slice(&body[search_from..start]);
        let span_start = result.len();
        result.extend_from_slice(&body[start + marker..end - marker]);
        spans.push(Span::new(rule.style, span_start, result.len()));
        removals.push((start, marker));
        removals.push((end - marker, marker));
        search_from = end;
    }
    result.extend_from_slice(&body[search_from.min(body.len())..]);
    (result, spans, removals)
}

/// Pass C: runs of `>` lines outside code blocks.
fn blockquotes(body: &[char], code_blocks: &[(usize, usize)]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut offset = 0;
    let mut group: Option<(usize, usize)> = None;
    for line in body.split(|c| *c == '\n') {
        let inside = code_blocks
            .iter()
            .any(|&(start, end)| start <= offset && offset < end);
        if line.first() == Some(&'>') && !inside {
            group = Some((
                group.map_or(offset, |(start, _)| start),
                offset + line.len(),
            ));
        } else if let Some((start, end)) = group.take() {
            spans.push(Span::new(Style::Blockquote, start, end));
        }
        offset += line.len() + 1;
    }
    if let Some((start, end)) = group {
        spans.push(Span::new(Style::Blockquote, start, end));
    }
    spans
}

// ---------------------------------------------------------------------
// Editing: spans back to markers, so a correction re-sends the markup.

fn markers(style: Style) -> Option<(&'static str, &'static str)> {
    match style {
        Style::Bold => Some(("**", "**")),
        Style::Italic => Some(("*", "*")),
        Style::Strikethrough => Some(("~~", "~~")),
        Style::Code => Some(("`", "`")),
        Style::CodeBlock => Some(("```\n", "\n```")),
        // `>` already lives in the body; links are plain text.
        Style::Blockquote | Style::Link => None,
    }
}

/// `body` with the markers that re-create `spans`, for the composer to
/// edit. Spans past the body are dropped.
pub fn editable(body: &str, spans: &[Span]) -> String {
    struct Insert {
        offset: usize,
        text: &'static str,
        close: bool,
        order: usize,
    }
    let chars: Vec<char> = body.chars().collect();
    let mut inserts = Vec::new();
    for (order, span) in spans.iter().enumerate() {
        let Some((open, close)) = markers(span.style) else {
            continue;
        };
        if span.start >= span.end || span.end > chars.len() {
            continue;
        }
        inserts.push(Insert {
            offset: span.start,
            text: open,
            close: false,
            order,
        });
        inserts.push(Insert {
            offset: span.end,
            text: close,
            close: true,
            order,
        });
    }
    // By offset; at one offset closes (innermost first) before opens.
    inserts.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then(b.close.cmp(&a.close))
            .then(if a.close {
                b.order.cmp(&a.order)
            } else {
                a.order.cmp(&b.order)
            })
    });
    let mut out = String::new();
    let mut cursor = 0;
    for insert in inserts {
        out.extend(&chars[cursor..insert.offset]);
        out.push_str(insert.text);
        cursor = insert.offset;
    }
    out.extend(&chars[cursor..]);
    out
}

// ---------------------------------------------------------------------
// The formatting bar.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Quote,
}

impl Format {
    pub const ALL: [Self; 6] = [
        Self::Bold,
        Self::Italic,
        Self::Strikethrough,
        Self::Code,
        Self::CodeBlock,
        Self::Quote,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Bold => "Bold (Ctrl+B)",
            Self::Italic => "Italic (Ctrl+I)",
            Self::Strikethrough => "Strikethrough",
            Self::Code => "Code",
            Self::CodeBlock => "Code Block",
            Self::Quote => "Quote",
        }
    }
}

/// The text after applying a format, and where the selection sits in it
/// (characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub text: String,
    pub selection: (usize, usize),
}

/// Wrap the selection in the format's markers, keeping it selected. An
/// empty selection inserts an empty pair with the caret between.
pub fn apply(format: Format, text: &str, selection: (usize, usize)) -> TextEdit {
    let chars: Vec<char> = text.chars().collect();
    let end = selection.1.min(chars.len());
    let start = selection.0.min(end);
    match format {
        Format::Bold => wrap(&chars, start, end, "**"),
        Format::Italic => wrap(&chars, start, end, "*"),
        Format::Strikethrough => wrap(&chars, start, end, "~~"),
        Format::Code => wrap(&chars, start, end, "`"),
        Format::CodeBlock => fence(&chars, start, end),
        Format::Quote => quote(&chars, start, end),
    }
}

fn string(chars: &[char]) -> String {
    chars.iter().collect()
}

fn wrap(chars: &[char], start: usize, end: usize, marker: &str) -> TextEdit {
    let shift = marker.chars().count();
    TextEdit {
        text: format!(
            "{}{marker}{}{marker}{}",
            string(&chars[..start]),
            string(&chars[start..end]),
            string(&chars[end..])
        ),
        selection: (start + shift, end + shift),
    }
}

/// The selection on its own lines between ``` fences.
fn fence(chars: &[char], start: usize, end: usize) -> TextEdit {
    let starts_line = start == 0 || chars[start - 1] == '\n';
    let ends_line = end == chars.len() || chars[end] == '\n';
    let opening = format!("{}```\n", if starts_line { "" } else { "\n" });
    let closing = format!("\n```{}", if ends_line { "" } else { "\n" });
    let from = start + opening.chars().count();
    TextEdit {
        text: format!(
            "{}{opening}{}{closing}{}",
            string(&chars[..start]),
            string(&chars[start..end]),
            string(&chars[end..])
        ),
        selection: (from, from + (end - start)),
    }
}

/// Prefix every line the selection touches with `> `.
fn quote(chars: &[char], start: usize, end: usize) -> TextEdit {
    let first = chars[..start]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |at| at + 1);
    // A selection ending right after a newline does not touch the next line.
    let last = if end > start && chars[end - 1] == '\n' {
        end - 1
    } else {
        end
    };
    let mut starts = vec![first];
    starts.extend(
        (first..last.max(first))
            .filter(|&at| chars[at] == '\n')
            .map(|at| at + 1),
    );
    let mut out: Vec<char> = chars.to_vec();
    for &line in starts.iter().rev() {
        out.splice(line..line, ['>', ' ']);
    }
    TextEdit {
        text: string(&out),
        selection: (start + 2, end + 2 * starts.len()),
    }
}

// ---------------------------------------------------------------------
// The index.

#[derive(Default)]
struct Index {
    entries: HashMap<String, (String, Vec<Span>)>,
    order: VecDeque<String>,
}

impl Index {
    fn record(&mut self, key: String, body: String, spans: Vec<Span>) {
        if self.entries.insert(key.clone(), (body, spans)).is_none() {
            self.order.push_back(key);
            while self.order.len() > CAPACITY {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
    }
}

static INDEX: LazyLock<Mutex<Index>> = LazyLock::new(Mutex::default);

fn key(conversation: &str, id: &str) -> String {
    format!("{conversation}\n{id}")
}

/// Remember the markup of `body` under each of a message's ids. Plain
/// messages are not stored unless `always` (a correction must overwrite
/// the formatting it replaces).
pub fn record(conversation: &str, ids: &[&str], body: &str, spans: &[Span], always: bool) {
    if spans.is_empty() && !always {
        return;
    }
    let Ok(mut index) = INDEX.lock() else {
        return;
    };
    for id in ids.iter().filter(|id| !id.is_empty()) {
        index.record(key(conversation, id), body.to_string(), spans.to_vec());
    }
}

/// The spans recorded for a row, when they were made for this body.
pub fn lookup(conversation: &str, ids: &[Option<&str>], body: &str) -> Vec<Span> {
    let Ok(index) = INDEX.lock() else {
        return Vec::new();
    };
    ids.iter()
        .flatten()
        .filter_map(|id| index.entries.get(&key(conversation, id)))
        .find(|(recorded, _)| recorded == body)
        .map(|(_, spans)| spans.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(style: Style, start: usize, end: usize) -> Span {
        Span::new(style, start, end)
    }

    #[test]
    fn inline_markers_become_spans_and_leave_the_body_clean() {
        let composed = compose("a **bold** and *it* and ~~no~~ and `x`");
        assert_eq!(composed.body, "a bold and it and no and x");
        assert_eq!(
            composed.spans,
            [
                span(Style::Bold, 2, 6),
                span(Style::Italic, 11, 13),
                span(Style::Strikethrough, 18, 20),
                span(Style::Code, 25, 26),
            ]
        );
    }

    #[test]
    fn lazy_matching_keeps_two_spans_apart() {
        let composed = compose("**a** mid **b**");
        assert_eq!(composed.body, "a mid b");
        assert_eq!(
            composed.spans,
            [span(Style::Bold, 0, 1), span(Style::Bold, 6, 7)]
        );
    }

    #[test]
    fn unflanked_stars_and_unclosed_markers_stay_text() {
        for text in ["2 * 3 * 4", "**open", "plain", "a ~~ b", "``"] {
            let composed = compose(text);
            assert_eq!(composed.body, text);
            assert!(composed.spans.is_empty(), "{text}");
        }
    }

    #[test]
    fn markers_inside_code_are_literal() {
        let composed = compose("`**x**`");
        assert_eq!(composed.body, "**x**");
        assert_eq!(composed.spans, [span(Style::Code, 0, 5)]);
    }

    #[test]
    fn fences_become_code_blocks_with_literal_content() {
        let composed = compose("before\n```rust\nlet *a* = 1;\n```\nafter");
        assert_eq!(composed.body, "before\nlet *a* = 1;\nafter");
        assert_eq!(composed.spans, [span(Style::CodeBlock, 7, 19)]);
    }

    #[test]
    fn quote_lines_keep_their_markers_in_the_body() {
        let composed = compose("> one\n> two\nplain\n> three");
        assert_eq!(composed.body, "> one\n> two\nplain\n> three");
        assert_eq!(
            composed.spans,
            [
                span(Style::Blockquote, 0, 11),
                span(Style::Blockquote, 18, 25)
            ]
        );
    }

    #[test]
    fn quote_markers_in_code_blocks_are_not_quotes() {
        let composed = compose("```\n> x\n```");
        assert_eq!(composed.body, "> x");
        assert_eq!(composed.spans, [span(Style::CodeBlock, 0, 3)]);
    }

    #[test]
    fn offsets_count_characters_not_bytes() {
        let composed = compose("héllo **wörld**");
        assert_eq!(composed.body, "héllo wörld");
        assert_eq!(composed.spans, [span(Style::Bold, 6, 11)]);
    }

    #[test]
    fn wire_spans_shift_past_a_reply_fallback_and_drop_links() {
        let spans = vec![
            span(Style::Bold, 0, 2),
            Span {
                style: Style::Link,
                start: 0,
                end: 1,
                uri: Some("https://x".into()),
            },
        ];
        let wire = to_wire(&spans, 10);
        assert_eq!(wire.len(), 1);
        assert_eq!((wire[0].start, wire[0].end), (10, 12));
        assert_eq!(wire[0].span_type, WaddleMarkupSpanType::Bold);
        assert!(wire[0].uri.is_none());
    }

    #[test]
    fn received_spans_are_rebased_over_the_stripped_fallback() {
        let spans = vec![
            span(Style::Blockquote, 0, 9),
            span(Style::Bold, 8, 12),
            span(Style::Italic, 12, 14),
        ];
        let rebased = without_fallback(spans.clone(), 20, Some((0, 10)));
        assert_eq!(
            rebased,
            [span(Style::Bold, 0, 2), span(Style::Italic, 2, 4)]
        );
        // An invalid range strips nothing, so nothing moves.
        assert_eq!(without_fallback(spans.clone(), 5, Some((0, 10))), spans);
        assert_eq!(without_fallback(spans.clone(), 20, None), spans);
    }

    #[test]
    fn editing_restores_the_markers_and_composing_again_is_stable() {
        let composed = compose("a **bold** *it* `x`\n```\nlet a;\n```");
        let text = editable(&composed.body, &composed.spans);
        assert_eq!(compose(&text), composed);
        assert_eq!(editable("plain", &[]), "plain");
    }

    #[test]
    fn nested_spans_close_innermost_first() {
        let text = editable("abc", &[span(Style::Bold, 0, 3), span(Style::Italic, 0, 3)]);
        assert_eq!(text, "***abc***");
    }

    #[test]
    fn formats_wrap_the_selection() {
        let edit = apply(Format::Bold, "say hi now", (4, 6));
        assert_eq!(edit.text, "say **hi** now");
        assert_eq!(edit.selection, (6, 8));
        let edit = apply(Format::Code, "", (0, 0));
        assert_eq!((edit.text.as_str(), edit.selection), ("``", (1, 1)));
        let edit = apply(Format::Strikethrough, "ab", (0, 2));
        assert_eq!(edit.text, "~~ab~~");
    }

    #[test]
    fn code_block_goes_on_its_own_lines() {
        let edit = apply(Format::CodeBlock, "x = 1", (0, 5));
        assert_eq!(edit.text, "```\nx = 1\n```");
        assert_eq!(edit.selection, (4, 9));
        let edit = apply(Format::CodeBlock, "say x now", (4, 5));
        assert_eq!(edit.text, "say \n```\nx\n```\n now");
    }

    #[test]
    fn quote_prefixes_each_touched_line() {
        let edit = apply(Format::Quote, "one\ntwo\nthree", (2, 9));
        assert_eq!(edit.text, "> one\n> two\n> three");
        let edit = apply(Format::Quote, "one\ntwo", (0, 4));
        assert_eq!(edit.text, "> one\ntwo");
        let edit = apply(Format::Quote, "", (0, 0));
        assert_eq!(edit.text, "> ");
    }

    #[test]
    fn the_index_matches_only_the_body_it_was_made_for() {
        let spans = [span(Style::Bold, 0, 1)];
        record("c@x", &["o1", "s1"], "a", &spans, false);
        assert_eq!(lookup("c@x", &[Some("s1")], "a"), spans);
        assert!(lookup("c@x", &[Some("s1")], "b").is_empty());
        assert!(lookup("d@x", &[Some("s1")], "a").is_empty());
        // A correction without markup overwrites the old spans.
        record("c@x", &["o1"], "a", &[], true);
        assert!(lookup("c@x", &[Some("o1"), Some("s1")], "a").is_empty());
        // Plain messages are not stored.
        record("c@x", &["p1"], "a", &[], false);
        assert!(lookup("c@x", &[Some("p1")], "a").is_empty());
    }

    #[test]
    fn the_index_forgets_the_oldest() {
        let mut index = Index::default();
        for n in 0..CAPACITY + 10 {
            index.record(format!("k{n}"), "a".into(), Vec::new());
        }
        assert_eq!(index.entries.len(), CAPACITY);
        assert!(!index.entries.contains_key("k0"));
    }
}
