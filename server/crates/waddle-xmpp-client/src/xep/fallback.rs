//! XEP-0428 Fallback Indication helpers for inbound message parsing.

use minidom::Element;

pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyFallback {
    Whole,
    Ranges(Vec<FallbackRange>),
}

pub fn has_whole_body_fallback_for(message: &Element, feature_ns: &str) -> bool {
    message.children().any(|child| {
        child.name() == "fallback"
            && child.ns() == NS_FALLBACK
            && child.attr("for") == Some(feature_ns)
            && fallback_marks_whole_body(child)
    })
}

fn fallback_marks_whole_body(fallback: &Element) -> bool {
    let mut body_children = fallback
        .children()
        .filter(|child| child.name() == "body" && child.ns() == NS_FALLBACK);
    let Some(body) = body_children.next() else {
        return fallback.children().next().is_none();
    };
    body.attr("start").is_none() && body.attr("end").is_none() && body_children.next().is_none()
}

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
    let body_children: Vec<&Element> = fallback
        .children()
        .filter(|child| child.name() == "body" && child.ns() == NS_FALLBACK)
        .collect();

    if body_children.is_empty() {
        return None;
    }

    let mut ranges = Vec::new();
    let body_count = body_children.len();
    for body in body_children {
        match (body.attr("start"), body.attr("end")) {
            (None, None) if body_count == 1 => return Some(BodyFallback::Whole),
            (None, None) => return None,
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
    Some(BodyFallback::Ranges(ranges))
}

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
        .filter_map(|(unit, keep)| keep.then_some(*unit))
        .collect();
    String::from_utf16_lossy(&kept)
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
    fn rejects_mixed_whole_body_and_ranged_body_fallback() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
              <body/>\
              <body start='9' end='14'/>\
            </fallback>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert!(body_fallbacks_for(&message, "urn:xmpp:reply:0").is_empty());
    }

    #[test]
    fn ignores_childless_feature_scoped_fallback_for_body_stripping() {
        let message = "<message xmlns='jabber:client'>\
            <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'/>\
        </message>"
            .parse::<Element>()
            .expect("message");

        assert!(body_fallbacks_for(&message, "urn:xmpp:reply:0").is_empty());
    }

    #[test]
    fn strips_utf16_fallback_ranges() {
        let ranges = [FallbackRange { start: 0, end: 4 }];

        assert_eq!(
            strip_fallback_ranges("\u{1f642}ab visible", &ranges),
            " visible"
        );
    }
}
