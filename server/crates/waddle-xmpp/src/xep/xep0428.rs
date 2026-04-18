//! XEP-0428: Fallback Indication
//!
//! Marks text ranges inside a message `<body/>` as fallback content that
//! reply-aware or other capability-aware clients should strip when rendering
//! the primary payload (for example, the quoted prefix prepended in front of
//! an XEP-0461 reply so legacy clients still see readable context).
//!
//! Wire shape:
//!
//! ```xml
//! <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>
//!   <body start='0' end='42'/>
//! </fallback>
//! ```
//!
//! A message may carry multiple `<fallback/>` payloads — one per feature
//! namespace the fallback targets.

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0428 Fallback Indication.
pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

/// A body character range `[start, end)` marked as fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackRange {
    /// Inclusive UTF-16 code-unit start offset into the body.
    pub start: usize,
    /// Exclusive UTF-16 code-unit end offset into the body.
    pub end: usize,
}

/// A parsed `<fallback/>` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackIndication {
    /// The feature namespace this fallback is for (e.g. `urn:xmpp:reply:0`).
    pub for_ns: String,
    /// Optional body range; absent means "the entire body is fallback".
    pub body_range: Option<FallbackRange>,
}

impl FallbackIndication {
    /// Construct a whole-body fallback for a feature namespace.
    pub fn whole_body(for_ns: impl Into<String>) -> Self {
        Self {
            for_ns: for_ns.into(),
            body_range: None,
        }
    }

    /// Construct a body-range fallback.
    pub fn for_range(for_ns: impl Into<String>, start: usize, end: usize) -> Self {
        Self {
            for_ns: for_ns.into(),
            body_range: Some(FallbackRange { start, end }),
        }
    }
}

/// Check whether an element is an XEP-0428 `<fallback/>` payload.
pub fn is_fallback_element(elem: &Element) -> bool {
    elem.name() == "fallback" && elem.ns() == NS_FALLBACK
}

fn parse_usize_attr(elem: &Element, name: &str) -> Option<usize> {
    elem.attr(name)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<usize>().ok())
}

fn parse_body_range(fallback_elem: &Element) -> Option<FallbackRange> {
    let body_elem = fallback_elem
        .children()
        .find(|child| child.name() == "body" && child.ns() == NS_FALLBACK)?;
    let start = parse_usize_attr(body_elem, "start").unwrap_or(0);
    let end = parse_usize_attr(body_elem, "end").unwrap_or(start);
    Some(FallbackRange { start, end })
}

/// Parse all `<fallback/>` payloads attached to a message.
pub fn parse_fallbacks_from_message(msg: &Message) -> Vec<FallbackIndication> {
    msg.payloads
        .iter()
        .filter(|elem| is_fallback_element(elem))
        .filter_map(|elem| {
            let for_ns = elem.attr("for").map(str::trim).filter(|v| !v.is_empty())?;
            Some(FallbackIndication {
                for_ns: for_ns.to_owned(),
                body_range: parse_body_range(elem),
            })
        })
        .collect()
}

/// Build an XEP-0428 `<fallback/>` element.
pub fn build_fallback_element(fallback: &FallbackIndication) -> Element {
    let mut builder =
        Element::builder("fallback", NS_FALLBACK).attr("for", fallback.for_ns.as_str());
    if let Some(range) = fallback.body_range {
        let body = Element::builder("body", NS_FALLBACK)
            .attr("start", range.start.to_string())
            .attr("end", range.end.to_string())
            .build();
        builder = builder.append(body);
    }
    builder.build()
}

/// Replace all fallback payloads on a message with the given set.
pub fn set_fallback_payloads(msg: &mut Message, fallbacks: &[FallbackIndication]) {
    msg.payloads.retain(|elem| !is_fallback_element(elem));
    for fallback in fallbacks {
        msg.payloads.push(build_fallback_element(fallback));
    }
}

/// Strip every fallback range from a body string, returning the caller-visible
/// text. Offsets are treated as UTF-16 code units per XEP-0428 §2, which
/// matches how JavaScript/browser clients slice strings; the body is
/// round-tripped through UTF-16 so emoji and other non-BMP characters are
/// stripped correctly.
pub fn strip_fallback_ranges(body: &str, ranges: &[FallbackRange]) -> String {
    if ranges.is_empty() {
        return body.to_string();
    }
    let units: Vec<u16> = body.encode_utf16().collect();
    let total = units.len();
    let mut keep = vec![true; total];
    for range in ranges {
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        for flag in keep.iter_mut().take(end).skip(start) {
            *flag = false;
        }
    }
    let kept: Vec<u16> = units
        .iter()
        .zip(keep.iter())
        .filter_map(|(u, k)| if *k { Some(*u) } else { None })
        .collect();
    String::from_utf16_lossy(&kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_fallback_element() {
        let elem = Element::builder("fallback", NS_FALLBACK).build();
        assert!(is_fallback_element(&elem));
        let wrong = Element::builder("fallback", "other:ns").build();
        assert!(!is_fallback_element(&wrong));
    }

    #[test]
    fn test_parse_whole_body_fallback() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'/>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        let fallbacks = parse_fallbacks_from_message(&msg);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0].for_ns, "urn:xmpp:reply:0");
        assert_eq!(fallbacks[0].body_range, None);
    }

    #[test]
    fn test_parse_body_range_fallback() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='42'/>\
            </fallback>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        let fallbacks = parse_fallbacks_from_message(&msg);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(
            fallbacks[0].body_range,
            Some(FallbackRange { start: 0, end: 42 })
        );
    }

    #[test]
    fn test_parse_multiple_fallbacks() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
            </fallback>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:example:other'/>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        let fallbacks = parse_fallbacks_from_message(&msg);
        assert_eq!(fallbacks.len(), 2);
        assert_eq!(fallbacks[0].for_ns, "urn:xmpp:reply:0");
        assert_eq!(fallbacks[1].for_ns, "urn:example:other");
    }

    #[test]
    fn test_parse_missing_for_attr_skipped() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0'/>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        assert!(parse_fallbacks_from_message(&msg).is_empty());
    }

    #[test]
    fn test_build_round_trip() {
        let original = FallbackIndication::for_range("urn:xmpp:reply:0", 0, 17);
        let elem = build_fallback_element(&original);
        assert_eq!(elem.ns(), NS_FALLBACK);
        assert_eq!(elem.attr("for"), Some("urn:xmpp:reply:0"));
        let body = elem
            .children()
            .find(|c| c.name() == "body")
            .expect("body child");
        assert_eq!(body.attr("start"), Some("0"));
        assert_eq!(body.attr("end"), Some("17"));
    }

    #[test]
    fn test_set_fallback_payloads_replaces_existing() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:old'/>\
        </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");

        set_fallback_payloads(
            &mut msg,
            &[FallbackIndication::for_range("urn:xmpp:reply:0", 2, 5)],
        );

        let fallbacks = parse_fallbacks_from_message(&msg);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0].for_ns, "urn:xmpp:reply:0");
        assert_eq!(
            fallbacks[0].body_range,
            Some(FallbackRange { start: 2, end: 5 })
        );
    }

    #[test]
    fn test_strip_fallback_ranges_basic() {
        let stripped = strip_fallback_ranges(
            "> quoted\n\nreply body",
            &[FallbackRange { start: 0, end: 10 }],
        );
        assert_eq!(stripped, "reply body");
    }

    #[test]
    fn test_strip_fallback_ranges_multiple_overlapping() {
        let stripped = strip_fallback_ranges(
            "abcdefg",
            &[
                FallbackRange { start: 0, end: 2 },
                FallbackRange { start: 4, end: 6 },
            ],
        );
        assert_eq!(stripped, "cdg");
    }

    #[test]
    fn test_strip_fallback_ranges_utf16_emoji_prefix() {
        // "👋 hi\n\n" as a quoted prefix: 👋 is a surrogate pair (2 UTF-16 units),
        // space is 1, 'h' is 1, 'i' is 1, '\n' is 1, '\n' is 1 — total 7 units.
        let body = "👋 hi\n\nreply";
        let prefix_units = "👋 hi\n\n".encode_utf16().count();
        let stripped = strip_fallback_ranges(
            body,
            &[FallbackRange {
                start: 0,
                end: prefix_units,
            }],
        );
        assert_eq!(stripped, "reply");
    }

    #[test]
    fn test_strip_fallback_ranges_out_of_bounds_saturates() {
        let stripped = strip_fallback_ranges(
            "short",
            &[FallbackRange {
                start: 2,
                end: 9999,
            }],
        );
        assert_eq!(stripped, "sh");
    }
}
