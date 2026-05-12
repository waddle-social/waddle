//! XEP-0428 Fallback Indication helpers for inbound message parsing.

use minidom::Element;

pub const NS_FALLBACK: &str = "urn:xmpp:fallback:0";

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
}
