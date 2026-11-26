//! Parsing of message markup, references, and broadcast mentions.

use minidom::Element;

use waddle_xmpp_core::mention_markup::{mentions_from_markup, MENTION_TYPE_CHANNEL};

use super::super::mentions::{BroadcastMention, BroadcastSpan};
use super::super::namespaces::{NS_MARKUP, NS_WADDLE_MARKUP};
use super::super::types::{MarkupSpan, MarkupSpanType};

/// The room-wide mention carried by XEP-0513 `#channel` markup, as the
/// label applications consume (`everyone` / `here`). `@everyone` wins when
/// a message carries both kinds.
pub(super) fn parse_broadcast_mention(markups_el: &Element) -> Option<String> {
    mentions_from_markup(markups_el)
        .into_iter()
        .filter(|mention| mention.mentions.as_deref() == Some(MENTION_TYPE_CHANNEL))
        .map(|mention| BroadcastMention::from_channel_mention(mention.active))
        .min_by_key(|kind| match kind {
            BroadcastMention::Everyone => 0,
            BroadcastMention::Here => 1,
        })
        .map(|kind| kind.label().to_string())
}

/// Body ranges of the XEP-0513 `#channel` mentions in `markups_el`. A
/// mention without a usable `start`/`end` has no range to highlight and is
/// left out (it still counts toward `parse_broadcast_mention`).
pub(super) fn parse_broadcast_spans(markups_el: &Element) -> Vec<BroadcastSpan> {
    mentions_from_markup(markups_el)
        .into_iter()
        .filter(|mention| mention.mentions.as_deref() == Some(MENTION_TYPE_CHANNEL))
        .filter_map(|mention| {
            let (begin, end) = (mention.begin?, mention.end?);
            (begin < end).then_some(BroadcastSpan {
                kind: BroadcastMention::from_channel_mention(mention.active),
                begin,
                end,
            })
        })
        .collect()
}

fn range(el: &Element) -> Option<(usize, usize)> {
    let start: usize = el.attr("start")?.parse().ok()?;
    let end: usize = el.attr("end")?.parse().ok()?;
    Some((start, end))
}

fn block_span(el: &Element, span_type: MarkupSpanType) -> Option<MarkupSpan> {
    let (start, end) = range(el)?;
    Some(MarkupSpan {
        span_type,
        start,
        end,
        uri: None,
    })
}

fn inline_style(inner: &Element) -> Option<MarkupSpanType> {
    if inner.ns() != NS_MARKUP {
        return None;
    }
    match inner.name() {
        "strong" => Some(MarkupSpanType::Bold),
        "emphasis" => Some(MarkupSpanType::Italic),
        "deleted" => Some(MarkupSpanType::Strikethrough),
        "code" => Some(MarkupSpanType::Code),
        _ => None,
    }
}

pub(super) fn parse_markup_spans(markups_el: &Element) -> Vec<MarkupSpan> {
    // XEP-0394: direct children of <markup> are, by (namespace, name):
    //   (urn:xmpp:markup:0, span)   start/end, one or more of <strong/>, <emphasis/>,
    //                               <code/>, <deleted/> children — one MarkupSpan per style
    //   (urn:xmpp:markup:0, bcode)  code block
    //   (urn:xmpp:markup:0, bquote) blockquote
    //   (urn:waddle:markup:0, span) start/end/uri — link (not defined by XEP-0394)
    // "Entities MUST silently ignore elements and attributes (arbitrarly deep) in
    // <markup/> which they do not understand": anything else is skipped, including
    // same-named elements in other namespaces and unknown attributes such as `uri`
    // on an official span.
    let mut spans = Vec::new();
    for child in markups_el.children() {
        let ns = child.ns();
        if ns == NS_MARKUP {
            match child.name() {
                "span" => {
                    let Some((start, end)) = range(child) else {
                        continue;
                    };
                    let mut styles: Vec<MarkupSpanType> = Vec::new();
                    for style in child.children().filter_map(inline_style) {
                        if !styles.contains(&style) {
                            styles.push(style);
                        }
                    }
                    spans.extend(styles.into_iter().map(|span_type| MarkupSpan {
                        span_type,
                        start,
                        end,
                        uri: None,
                    }));
                }
                "bcode" => spans.extend(block_span(child, MarkupSpanType::CodeBlock)),
                "bquote" => spans.extend(block_span(child, MarkupSpanType::Blockquote)),
                _ => {}
            }
        } else if ns == NS_WADDLE_MARKUP && child.name() == "span" {
            if let (Some((start, end)), Some(uri)) = (range(child), child.attr("uri")) {
                spans.push(MarkupSpan {
                    span_type: MarkupSpanType::Link,
                    start,
                    end,
                    uri: Some(uri.to_string()),
                });
            }
        }
    }
    spans
}
