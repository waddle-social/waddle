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
    /// Optional body ranges; absent means "the entire body is fallback".
    pub body_ranges: Option<Vec<FallbackRange>>,
}

impl FallbackIndication {
    /// Construct a whole-body fallback for a feature namespace.
    pub fn whole_body(for_ns: impl Into<String>) -> Self {
        Self {
            for_ns: for_ns.into(),
            body_ranges: None,
        }
    }

    /// Construct a body-range fallback.
    pub fn for_range(for_ns: impl Into<String>, start: usize, end: usize) -> Self {
        Self::for_ranges(for_ns, [FallbackRange { start, end }])
    }

    /// Construct a fallback with multiple body ranges.
    pub fn for_ranges(
        for_ns: impl Into<String>,
        body_ranges: impl IntoIterator<Item = FallbackRange>,
    ) -> Self {
        let body_ranges: Vec<FallbackRange> = body_ranges.into_iter().collect();
        Self {
            for_ns: for_ns.into(),
            body_ranges: (!body_ranges.is_empty()).then_some(body_ranges),
        }
    }
}

/// Check whether an element is an XEP-0428 `<fallback/>` payload.
pub fn is_fallback_element(elem: &Element) -> bool {
    elem.name() == "fallback" && elem.ns() == NS_FALLBACK
}

fn parse_usize_attr(elem: &Element, name: &str) -> Result<Option<usize>, ()> {
    match elem.attr(name).map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => value.parse::<usize>().map(Some).map_err(|_| ()),
        None => Ok(None),
    }
}

fn parse_body_ranges(fallback_elem: &Element) -> Result<Option<Vec<FallbackRange>>, ()> {
    let body_children: Vec<&Element> = fallback_elem
        .children()
        .filter(|child| child.name() == "body" && child.ns() == NS_FALLBACK)
        .collect();
    if body_children.is_empty() {
        if fallback_elem.children().next().is_none() {
            return Ok(None);
        }
        return Err(());
    }

    let mut ranges = Vec::with_capacity(body_children.len());
    let mut whole_body = false;
    for body_elem in body_children {
        let start = parse_usize_attr(body_elem, "start")?;
        let end = parse_usize_attr(body_elem, "end")?;
        match (start, end) {
            (None, None) => whole_body = true,
            (Some(start), Some(end)) if end >= start => ranges.push(FallbackRange { start, end }),
            _ => return Err(()),
        }
    }

    if whole_body {
        if ranges.is_empty() {
            Ok(None)
        } else {
            Err(())
        }
    } else {
        Ok(Some(ranges))
    }
}

/// Parse all `<fallback/>` payloads attached to a message.
pub fn parse_fallbacks_from_message(msg: &Message) -> Vec<FallbackIndication> {
    msg.payloads
        .iter()
        .filter(|elem| is_fallback_element(elem))
        .filter_map(|elem| {
            let for_ns = elem.attr("for").map(str::trim).filter(|v| !v.is_empty())?;
            let body_ranges = parse_body_ranges(elem).ok()?;
            Some(FallbackIndication {
                for_ns: for_ns.to_owned(),
                body_ranges,
            })
        })
        .collect()
}

/// Build an XEP-0428 `<fallback/>` element.
pub fn build_fallback_element(fallback: &FallbackIndication) -> Element {
    let mut builder =
        Element::builder("fallback", NS_FALLBACK).attr("for", fallback.for_ns.as_str());
    match &fallback.body_ranges {
        None => {
            builder = builder.append(Element::builder("body", NS_FALLBACK).build());
        }
        Some(ranges) => {
            for range in ranges {
                let body = Element::builder("body", NS_FALLBACK)
                    .attr("start", range.start.to_string())
                    .attr("end", range.end.to_string())
                    .build();
                builder = builder.append(body);
            }
        }
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
        assert_eq!(fallbacks[0].body_ranges, None);
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
            fallbacks[0].body_ranges,
            Some(vec![FallbackRange { start: 0, end: 42 }])
        );
    }

    #[test]
    fn test_parse_body_child_without_offsets_means_whole_body() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:sfs:0'>\
                <body/>\
            </fallback>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        let fallbacks = parse_fallbacks_from_message(&msg);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0].for_ns, "urn:xmpp:sfs:0");
        assert_eq!(fallbacks[0].body_ranges, None);
    }

    #[test]
    fn test_parse_multiple_body_ranges() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
                <body start='20' end='30'/>\
            </fallback>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        let fallbacks = parse_fallbacks_from_message(&msg);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(
            fallbacks[0].body_ranges,
            Some(vec![
                FallbackRange { start: 0, end: 10 },
                FallbackRange { start: 20, end: 30 },
            ])
        );
    }

    #[test]
    fn test_parse_mixed_whole_body_and_range_is_skipped() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0' end='10'/>\
                <body/>\
            </fallback>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        assert!(parse_fallbacks_from_message(&msg).is_empty());
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
    fn test_parse_subject_only_fallback_is_skipped() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <subject start='0' end='4'/>\
            </fallback>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        assert!(parse_fallbacks_from_message(&msg).is_empty());
    }

    #[test]
    fn test_parse_partial_body_range_is_skipped() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='0'/>\
            </fallback>\
        </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("parse message");
        assert!(parse_fallbacks_from_message(&msg).is_empty());
    }

    #[test]
    fn test_parse_end_before_start_is_skipped() {
        let xml = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
                <body start='4' end='1'/>\
            </fallback>\
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
    fn test_build_whole_body_fallback_emits_body_child_without_offsets() {
        let elem = build_fallback_element(&FallbackIndication::whole_body("urn:xmpp:sfs:0"));
        assert_eq!(elem.attr("for"), Some("urn:xmpp:sfs:0"));
        let body = elem.get_child("body", NS_FALLBACK).expect("body child");
        assert!(body.attr("start").is_none());
        assert!(body.attr("end").is_none());
    }

    #[test]
    fn test_empty_body_ranges_canonicalize_to_whole_body() {
        let fallback = FallbackIndication::for_ranges("urn:xmpp:sfs:0", std::iter::empty());
        assert_eq!(fallback.body_ranges, None);
        let elem = build_fallback_element(&fallback);
        assert!(elem.get_child("body", NS_FALLBACK).is_some());
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
            fallbacks[0].body_ranges,
            Some(vec![FallbackRange { start: 2, end: 5 }])
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
