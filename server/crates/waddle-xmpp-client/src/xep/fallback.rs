//! XEP-0428 Fallback Indication helpers for inbound message parsing.

use minidom::Element;

/// Protocol token `urn:xmpp:fallback:0`.
pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Half-open Unicode-character range marking fallback text in a message body.
pub struct FallbackRange {
    /// Inclusive character offset in the message body.
    pub start: usize,
    /// Exclusive character offset in the message body.
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A whole-body or character-range XEP-0428 fallback declaration.
pub enum BodyFallback {
    /// The entire message body is fallback text for the feature.
    Whole,
    /// Only the supplied body ranges are fallback text for this feature.
    Ranges(Vec<FallbackRange>),
}

/// Whether this message declares its entire body as fallback text for the supplied feature namespace.
#[must_use]
pub fn has_whole_body_fallback_for(message: &Element, feature_ns: &str) -> bool {
    message.children().any(|child| {
        child.name() == "fallback"
            && child.ns() == NS_FALLBACK
            && child.attr("for") == Some(feature_ns)
            && fallback_marks_whole_body(child)
    })
}

fn fallback_marks_whole_body(fallback: &Element) -> bool {
    parse_body_fallback(fallback) == Some(BodyFallback::Whole)
}

/// Collect valid body fallback declarations for the requested feature, ignoring malformed declarations.
#[must_use]
pub fn body_fallbacks_for(message: &Element, feature_ns: &str) -> Vec<BodyFallback> {
    message
        .children()
        .filter(|child| {
            child.name() == "fallback"
                && child.ns() == NS_FALLBACK
                && child.attr("for") == Some(feature_ns)
        })
        .filter_map(parse_body_fallback)
        .collect()
}

fn parse_body_fallback(fallback: &Element) -> Option<BodyFallback> {
    // A childless <fallback/> applies to every body and subject (XEP-0428 §2.2).
    if fallback.children().next().is_none() {
        return Some(BodyFallback::Whole);
    }

    let mut body_children = fallback
        .children()
        .filter(|child| child.name() == "body" && child.ns() == NS_FALLBACK)
        .peekable();
    body_children.peek()?;

    // A body child without offsets covers the whole body and subsumes any
    // range siblings; malformed offsets reject the declaration.
    let mut whole = false;
    let mut ranges = Vec::new();
    for body in body_children {
        match (body.attr("start"), body.attr("end")) {
            (None, None) => whole = true,
            (Some(start), Some(end)) => {
                let start = start.parse::<usize>().ok()?;
                let end = end.parse::<usize>().ok()?;
                if end < start {
                    return None;
                }
                ranges.push(FallbackRange { start, end });
            }
            _ => return None,
        }
    }
    Some(if whole {
        BodyFallback::Whole
    } else {
        BodyFallback::Ranges(ranges)
    })
}

/// Remove the specified Unicode-character ranges from the body, clamping ranges to its length.
#[must_use]
pub fn strip_fallback_ranges(body: &str, ranges: &[FallbackRange]) -> String {
    if ranges.is_empty() {
        return body.to_string();
    }
    let points: Vec<char> = body.chars().collect();
    let total = points.len();
    let mut keep = vec![true; total];
    for range in ranges {
        let start = range.start.min(total);
        let end = range.end.min(total).max(start);
        for flag in keep.iter_mut().take(end).skip(start) {
            *flag = false;
        }
    }
    points
        .into_iter()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(point))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_whole_body_fallback_for_requested_feature() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:waddle:extension:1'>\
              <body/>\
            </fallback>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert!(has_whole_body_fallback_for(
            &message,
            "urn:waddle:extension:1"
        ));
        assert!(!has_whole_body_fallback_for(&message, "urn:xmpp:reply:0"));
    }

    #[test]
    fn parses_multiple_body_ranges_for_requested_feature() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
              <body start='0' end='4'/>\
              <body start='9' end='14'/>\
            </fallback>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert_eq!(
            body_fallbacks_for(&message, "urn:xmpp:reply:0"),
            vec![BodyFallback::Ranges(vec![
                FallbackRange { start: 0, end: 4 },
                FallbackRange { start: 9, end: 14 }
            ])]
        );
    }

    #[test]
    fn whole_body_child_subsumes_ranged_body_siblings() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
              <body/>\
              <body start='9' end='14'/>\
            </fallback>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert_eq!(
            body_fallbacks_for(&message, "urn:xmpp:reply:0"),
            vec![BodyFallback::Whole]
        );
    }

    #[test]
    fn multiple_offsetless_body_children_are_whole() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
              <body/><body/>\
            </fallback>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert_eq!(
            body_fallbacks_for(&message, "urn:xmpp:reply:0"),
            vec![BodyFallback::Whole]
        );
    }

    #[test]
    fn childless_feature_scoped_fallback_covers_whole_body() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'/>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert_eq!(
            body_fallbacks_for(&message, "urn:xmpp:reply:0"),
            vec![BodyFallback::Whole]
        );
        assert!(has_whole_body_fallback_for(&message, "urn:xmpp:reply:0"));
    }

    #[test]
    fn code_point_ranges_preserve_unselected_scalars_and_union_overlaps() {
        assert_eq!(
            strip_fallback_ranges("a🙂e\u{301}z", &[FallbackRange { start: 1, end: 2 }]),
            "ae\u{301}z"
        );
        assert_eq!(
            strip_fallback_ranges(
                "a🙂e\u{301}z",
                &[
                    FallbackRange { start: 1, end: 3 },
                    FallbackRange { start: 2, end: 4 }
                ],
            ),
            "az"
        );
    }

    #[test]
    fn strips_unicode_code_point_fallback_ranges() {
        let ranges = [FallbackRange { start: 0, end: 3 }];

        assert_eq!(
            strip_fallback_ranges("\u{1f642}ab visible", &ranges),
            " visible"
        );
    }
}
