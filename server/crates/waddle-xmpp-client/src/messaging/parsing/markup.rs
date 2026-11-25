//! Parsing of message markup, references, and broadcast mentions.

use minidom::Element;

use super::super::namespaces::{NS_MARKUP, NS_WADDLE_MARKUP};
use super::super::types::{MarkupSpan, MarkupSpanType};

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
