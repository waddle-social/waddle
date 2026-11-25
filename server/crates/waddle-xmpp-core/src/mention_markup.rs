//! XEP-0513 explicit mentions on the wire, carried in XEP-0394 markup.
//!
//! XEP-0513 defines a `<mention xmlns='urn:xmpp:mentions:0'/>` element that
//! is a child of an XEP-0394 `<span/>` inside a `<markup/>` element:
//!
//! ```xml
//! <markup xmlns='urn:xmpp:markup:0'>
//!   <span start='7' end='12'>
//!     <mention xmlns='urn:xmpp:mentions:0'
//!       type='urn:xmpp:mentions:0#occupant'
//!       target='dd72603deec90a38ba552f7c68cbcc61bca202cd'/>
//!   </span>
//! </markup>
//! ```
//!
//! `type` is required, `target` is the XEP-0421 occupant-id for the
//! `#occupant` type and absent for `#channel`, and `active='true'` limits a
//! group mention to currently active users. The official namespace carries
//! nothing else.
//!
//! Waddle-only mention data (a mentioned bare JID, a scope URI, and the
//! `noping` suppression flag) has no XEP-0513 representation. It travels in a
//! sibling `<mention xmlns='urn:waddle:mentions:0'/>` element inside the same
//! `<span/>`.
//!
//! [`MentionSpan`] is the typed view shared by the live-stanza codec in
//! `waddle-xmpp` and the MAM replay builder in [`crate::mam`].

use jid::BareJid;
use minidom::{Element, Node};

/// XEP-0394 Message Markup namespace.
pub const NS_MARKUP: &str = "urn:xmpp:markup:0";

/// XEP-0513 Explicit Mentions namespace.
pub const NS_MENTIONS: &str = "urn:xmpp:mentions:0";

/// Waddle-only mention extension namespace (mentioned JID, scope URI,
/// `noping`). Never used for XEP-0513 semantics.
pub const NS_WADDLE_MENTIONS: &str = "urn:waddle:mentions:0";

/// XEP-0513 `type` for an individual mention by XEP-0421 occupant-id.
pub const MENTION_TYPE_OCCUPANT: &str = "urn:xmpp:mentions:0#occupant";

/// XEP-0513 `type` for a mention of every occupant of the room.
pub const MENTION_TYPE_CHANNEL: &str = "urn:xmpp:mentions:0#channel";

/// One mention with its body range, decoded from or encoded to a `<span/>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MentionSpan {
    /// XEP-0394 `start`: first affected code point.
    pub begin: Option<u32>,
    /// XEP-0394 `end`: one past the last affected code point.
    pub end: Option<u32>,
    /// Waddle extension: bare JID of the mentioned entity.
    pub jid: Option<BareJid>,
    /// XEP-0513 `target` of a `#occupant` mention (XEP-0421 occupant-id).
    pub occupant_id: Option<String>,
    /// XEP-0513 `type` of a group mention (for example `#channel`).
    pub mentions: Option<String>,
    /// Waddle extension: scope URI.
    pub uri: Option<String>,
    /// XEP-0513 `active='true'`.
    pub active: bool,
    /// Waddle extension: do not notify the target.
    pub noping: bool,
}

fn set_attr(elem: &mut Element, name: minidom::rxml::NcName, value: impl Into<String>) {
    elem.set_attr(minidom::rxml::Namespace::NONE, name, value.into());
}

impl MentionSpan {
    /// Build the XEP-0394 `<span/>` carrying this mention.
    ///
    /// A group `type` is authoritative: when both a group URI and an
    /// occupant-id are set, only the group mention is emitted (XEP-0513 says
    /// a group mention MUST NOT carry a `target`).
    #[must_use]
    pub fn to_span_element(&self) -> Element {
        let mut span = Element::builder("span", NS_MARKUP).build();
        if let Some(begin) = self.begin {
            set_attr(
                &mut span,
                minidom::rxml::xml_ncname!("start").to_owned(),
                begin.to_string(),
            );
        }
        if let Some(end) = self.end {
            set_attr(
                &mut span,
                minidom::rxml::xml_ncname!("end").to_owned(),
                end.to_string(),
            );
        }

        let standard_type = match (&self.mentions, &self.occupant_id) {
            (Some(group), _) => Some((group.as_str(), None)),
            (None, Some(occupant)) => Some((MENTION_TYPE_OCCUPANT, Some(occupant.as_str()))),
            (None, None) => None,
        };
        if let Some((kind, target)) = standard_type {
            let mut mention = Element::builder("mention", NS_MENTIONS).build();
            set_attr(
                &mut mention,
                minidom::rxml::xml_ncname!("type").to_owned(),
                kind,
            );
            if let Some(target) = target {
                set_attr(
                    &mut mention,
                    minidom::rxml::xml_ncname!("target").to_owned(),
                    target,
                );
            }
            if self.active {
                set_attr(
                    &mut mention,
                    minidom::rxml::xml_ncname!("active").to_owned(),
                    "true",
                );
            }
            span.append_child(mention);
        }

        if self.jid.is_some() || self.uri.is_some() || self.noping {
            let mut extension = Element::builder("mention", NS_WADDLE_MENTIONS).build();
            if let Some(jid) = &self.jid {
                set_attr(
                    &mut extension,
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    jid.to_string(),
                );
            }
            if let Some(uri) = &self.uri {
                set_attr(
                    &mut extension,
                    minidom::rxml::xml_ncname!("uri").to_owned(),
                    uri.as_str(),
                );
            }
            if self.noping {
                set_attr(
                    &mut extension,
                    minidom::rxml::xml_ncname!("noping").to_owned(),
                    "true",
                );
            }
            span.append_child(extension);
        }
        span
    }
}

/// Wrap `spans` in a `<markup xmlns='urn:xmpp:markup:0'/>` element.
/// Returns `None` when there is nothing to carry.
#[must_use]
pub fn mention_markup_element(spans: &[MentionSpan]) -> Option<Element> {
    if spans.is_empty() {
        return None;
    }
    let mut markup = Element::builder("markup", NS_MARKUP).build();
    for span in spans {
        markup.append_child(span.to_span_element());
    }
    Some(markup)
}

fn flag(elem: &Element, name: &str) -> bool {
    matches!(elem.attr(name), Some("true" | "1"))
}

fn standard_mentions(span: &Element) -> impl Iterator<Item = &Element> {
    span.children().filter(|c| c.is("mention", NS_MENTIONS))
}

fn waddle_mentions(span: &Element) -> impl Iterator<Item = &Element> {
    span.children()
        .filter(|c| c.is("mention", NS_WADDLE_MENTIONS))
}

fn span_range(span: &Element) -> (Option<u32>, Option<u32>) {
    (
        span.attr("start").and_then(|v| v.parse().ok()),
        span.attr("end").and_then(|v| v.parse().ok()),
    )
}

fn extension_fields(extension: &Element, into: &mut MentionSpan) {
    into.jid = extension.attr("jid").and_then(|v| v.parse().ok());
    into.uri = extension.attr("uri").map(str::to_string);
    into.noping = flag(extension, "noping");
}

/// Decode the mentions carried by a `<markup/>` element. Spans without
/// mention children contribute nothing; other markup is ignored.
///
/// Each XEP-0513 `<mention/>` (one with a `type`) is one mention. A Waddle
/// extension element in the same span merges into the first one; an
/// extension with no XEP-0513 sibling, or a further extension, is a mention
/// of its own.
#[must_use]
pub fn mentions_from_markup(markup: &Element) -> Vec<MentionSpan> {
    let mut out = Vec::new();
    if !markup.is("markup", NS_MARKUP) {
        return out;
    }
    for span in markup.children().filter(|c| c.is("span", NS_MARKUP)) {
        let (begin, end) = span_range(span);
        let mut spans: Vec<MentionSpan> = standard_mentions(span)
            .filter_map(|mention| {
                let kind = mention.attr("type").filter(|kind| !kind.is_empty())?;
                let mut decoded = MentionSpan {
                    begin,
                    end,
                    active: flag(mention, "active"),
                    ..MentionSpan::default()
                };
                if kind == MENTION_TYPE_OCCUPANT {
                    decoded.occupant_id =
                        Some(mention.attr("target").unwrap_or_default().to_string());
                } else {
                    decoded.mentions = Some(kind.to_string());
                }
                Some(decoded)
            })
            .collect();
        for (index, extension) in waddle_mentions(span).enumerate() {
            match (index, spans.first_mut()) {
                (0, Some(first)) => extension_fields(extension, first),
                _ => {
                    let mut standalone = MentionSpan {
                        begin,
                        end,
                        ..MentionSpan::default()
                    };
                    extension_fields(extension, &mut standalone);
                    if standalone.jid.is_some() || standalone.uri.is_some() || standalone.noping {
                        spans.push(standalone);
                    }
                }
            }
        }
        out.extend(spans);
    }
    out
}

/// Whether `markup` carries any mention element (either namespace).
#[must_use]
pub fn markup_has_mention(markup: &Element) -> bool {
    markup.is("markup", NS_MARKUP)
        && markup
            .children()
            .filter(|c| c.is("span", NS_MARKUP))
            .any(|span| {
                standard_mentions(span).next().is_some() || waddle_mentions(span).next().is_some()
            })
}

/// Remove every mention element from `markup`, then any span left without
/// children. Returns `true` when `markup` itself is left with no children
/// and should be dropped. Other markup is preserved. Text that surrounded a
/// removed node is merged into one text node, as a parsed tree holds no two
/// adjacent text nodes and `minidom` equality counts text nodes.
pub fn strip_mentions_from_markup(markup: &mut Element) -> bool {
    if !markup.is("markup", NS_MARKUP) {
        return false;
    }
    for node in markup.take_nodes() {
        match node {
            Node::Element(mut child) if child.is("span", NS_MARKUP) && span_has_mention(&child) => {
                for grandchild in child.take_nodes() {
                    let is_mention = matches!(
                        &grandchild,
                        Node::Element(e)
                            if e.is("mention", NS_MENTIONS) || e.is("mention", NS_WADDLE_MENTIONS)
                    );
                    match grandchild {
                        _ if is_mention => {}
                        Node::Text(text) => child.append_text(text),
                        other => child.append_node(other),
                    }
                }
                if child.children().next().is_some() {
                    markup.append_child(child);
                }
            }
            Node::Text(text) => markup.append_text(text),
            other => markup.append_node(other),
        }
    }
    markup.children().next().is_none()
}

fn span_has_mention(span: &Element) -> bool {
    standard_mentions(span).next().is_some() || waddle_mentions(span).next().is_some()
}

/// Whether `markup` carries a malformed mention: an XEP-0513 `<mention/>`
/// without a `type`, a `#occupant` mention without a `target`, a Waddle
/// extension carrying a mentioned `jid`, or an extension with no XEP-0513
/// sibling to qualify.
#[must_use]
pub fn markup_has_malformed_mention(markup: &Element) -> bool {
    if !markup.is("markup", NS_MARKUP) {
        return false;
    }
    markup
        .children()
        .filter(|c| c.is("span", NS_MARKUP))
        .any(|span| {
            let bad_standard = standard_mentions(span).any(|mention| match mention.attr("type") {
                None | Some("") => true,
                Some(MENTION_TYPE_OCCUPANT) => mention.attr("target").is_none_or(str::is_empty),
                Some(_) => false,
            });
            let has_standard = standard_mentions(span).next().is_some();
            let bad_extension = waddle_mentions(span)
                .any(|extension| extension.attr("jid").is_some() || !has_standard);
            bad_standard || bad_extension
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Element {
        xml.parse().expect("valid xml")
    }

    #[test]
    fn occupant_mention_matches_spec_example() {
        let span = MentionSpan {
            begin: Some(7),
            end: Some(12),
            occupant_id: Some("dd72603deec90a38ba552f7c68cbcc61bca202cd".into()),
            ..MentionSpan::default()
        };
        let markup = mention_markup_element(std::slice::from_ref(&span)).expect("markup");
        let expected = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='7' end='12'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' \
             target='dd72603deec90a38ba552f7c68cbcc61bca202cd'/></span></markup>",
        );
        assert_eq!(markup, expected);
        assert_eq!(mentions_from_markup(&markup), vec![span]);
    }

    #[test]
    fn active_channel_mention_has_no_target() {
        let span = MentionSpan {
            begin: Some(0),
            end: Some(5),
            mentions: Some("urn:xmpp:mentions:0#channel".into()),
            active: true,
            ..MentionSpan::default()
        };
        let markup = mention_markup_element(std::slice::from_ref(&span)).expect("markup");
        let expected = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='5'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel' \
             active='true'/></span></markup>",
        );
        assert_eq!(markup, expected);
        assert_eq!(mentions_from_markup(&markup), vec![span]);
    }

    #[test]
    fn waddle_data_lives_outside_the_official_namespace() {
        let span = MentionSpan {
            begin: Some(0),
            end: Some(3),
            occupant_id: Some("occ".into()),
            jid: Some("alice@example.com".parse().expect("jid")),
            uri: Some("urn:x".into()),
            noping: true,
            ..MentionSpan::default()
        };
        let element = span.to_span_element();
        let standard = element.get_child("mention", NS_MENTIONS).expect("standard");
        assert_eq!(standard.attrs().into_iter().count(), 2);
        assert!(standard.attr("type").is_some() && standard.attr("target").is_some());
        let extension = element
            .get_child("mention", NS_WADDLE_MENTIONS)
            .expect("extension");
        assert_eq!(extension.attr("jid"), Some("alice@example.com"));
        assert_eq!(extension.attr("noping"), Some("true"));
        let markup = mention_markup_element(std::slice::from_ref(&span)).expect("markup");
        assert_eq!(mentions_from_markup(&markup), vec![span]);
    }

    #[test]
    fn group_type_is_authoritative_over_occupant_target() {
        let span = MentionSpan {
            occupant_id: Some("occ".into()),
            mentions: Some("urn:xmpp:mentions:0#channel".into()),
            ..MentionSpan::default()
        };
        let element = span.to_span_element();
        let standard = element.get_child("mention", NS_MENTIONS).expect("standard");
        assert_eq!(standard.attr("target"), None);
        assert_eq!(standard.attr("type"), Some("urn:xmpp:mentions:0#channel"));
    }

    #[test]
    fn multiple_mentions_in_one_span_decode_separately() {
        let markup = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='3' end='14'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' target='a'/>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' target='b'/>\
             </span></markup>",
        );
        let decoded = mentions_from_markup(&markup);
        assert_eq!(decoded.len(), 2);
        assert!(decoded
            .iter()
            .all(|m| m.begin == Some(3) && m.end == Some(14)));
        assert_eq!(decoded[1].occupant_id.as_deref(), Some("b"));
    }

    #[test]
    fn mention_without_type_is_not_decoded_and_is_malformed() {
        let markup = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='1'>\
             <mention xmlns='urn:xmpp:mentions:0' target='a'/></span></markup>",
        );
        assert!(mentions_from_markup(&markup).is_empty());
        assert!(markup_has_malformed_mention(&markup));
    }

    #[test]
    fn malformed_detection_covers_target_and_jid_rules() {
        let no_target = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='1'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant'/>\
             </span></markup>",
        );
        assert!(markup_has_malformed_mention(&no_target));
        let with_jid = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='1'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' target='a'/>\
             <mention xmlns='urn:waddle:mentions:0' jid='a@example.com'/></span></markup>",
        );
        assert!(markup_has_malformed_mention(&with_jid));
        let fine = parse(
            "<markup xmlns='urn:xmpp:markup:0'><span start='0' end='1'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel'/>\
             </span></markup>",
        );
        assert!(!markup_has_malformed_mention(&fine));
    }

    #[test]
    fn strip_keeps_unrelated_markup() {
        let mut markup = parse(
            "<markup xmlns='urn:xmpp:markup:0'><bquote start='0' end='4'/>\
             <span start='5' end='9'><emphasis/>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel'/></span>\
             <span start='1' end='2'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#channel'/></span>\
             </markup>",
        );
        assert!(!strip_mentions_from_markup(&mut markup));
        assert!(!markup_has_mention(&markup));
        assert_eq!(markup.children().count(), 2);
        let mut only = mention_markup_element(&[MentionSpan {
            mentions: Some("urn:xmpp:mentions:0#channel".into()),
            ..MentionSpan::default()
        }])
        .expect("markup");
        assert!(strip_mentions_from_markup(&mut only));
    }

    #[test]
    fn strip_mentions_coalesces_text_around_removed_nodes() {
        let mut markup = parse(
            "<markup xmlns='urn:xmpp:markup:0'>a<span start='0' end='1'>\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' \
             target='x'/></span>b<span start='1' end='2'>x\
             <mention xmlns='urn:xmpp:mentions:0' type='urn:xmpp:mentions:0#occupant' \
             target='x'/>y<emphasis xmlns='urn:xmpp:markup:0'/></span></markup>",
        );
        assert!(!strip_mentions_from_markup(&mut markup));
        let expected =
            parse("<markup xmlns='urn:xmpp:markup:0'>ab<span start='1' end='2'>xy<emphasis xmlns='urn:xmpp:markup:0'/></span></markup>");
        assert_eq!(markup, expected);
    }
}
