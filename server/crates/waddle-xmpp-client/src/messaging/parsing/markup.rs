//! Parsing of message markup, references, and broadcast mentions.

use minidom::Element;

use waddle_xmpp_core::mention_markup::{mentions_from_markup, MENTION_TYPE_CHANNEL};

use super::super::mentions::BroadcastMention;
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

pub(super) fn parse_markup_spans(markups_el: &Element) -> Vec<MarkupSpan> {
    // XEP-0394: direct children of <markup> are:
    //   <span start="..." end="..."><emphasis/|<strong/>|<code/>|<deleted/></span>  — inline (NS_MARKUP)
    //   <span start="..." end="..." uri="..."/>                                     — link (NS_WADDLE_MARKUP)
    //   <bcode start="..." end="..."/>                                              — code block (NS_MARKUP)
    //   <bquote start="..." end="..."/>                                             — blockquote (NS_MARKUP)
    markups_el
        .children()
        .filter_map(|child| match child.name() {
            "span" => {
                let start: usize = child.attr("start")?.parse().ok()?;
                let end: usize = child.attr("end")?.parse().ok()?;
                // Link: <span uri="..."/> with no inline child element
                if let Some(uri) = child.attr("uri") {
                    return Some(MarkupSpan {
                        span_type: MarkupSpanType::Link,
                        start,
                        end,
                        uri: Some(uri.to_string()),
                    });
                }
                // Inline markup: inspect the single child element
                let span_type = child.children().find_map(|inner| match inner.name() {
                    "strong" => Some(MarkupSpanType::Bold),
                    "emphasis" => Some(MarkupSpanType::Italic),
                    "deleted" => Some(MarkupSpanType::Strikethrough),
                    "code" => Some(MarkupSpanType::Code),
                    _ => None,
                })?;
                Some(MarkupSpan {
                    span_type,
                    start,
                    end,
                    uri: None,
                })
            }
            "bcode" => {
                let start: usize = child.attr("start")?.parse().ok()?;
                let end: usize = child.attr("end")?.parse().ok()?;
                Some(MarkupSpan {
                    span_type: MarkupSpanType::CodeBlock,
                    start,
                    end,
                    uri: None,
                })
            }
            "bquote" => {
                let start: usize = child.attr("start")?.parse().ok()?;
                let end: usize = child.attr("end")?.parse().ok()?;
                Some(MarkupSpan {
                    span_type: MarkupSpanType::Blockquote,
                    start,
                    end,
                    uri: None,
                })
            }
            _ => None,
        })
        .collect()
}
