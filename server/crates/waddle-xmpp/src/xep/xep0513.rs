//! XEP-0513: Explicit Mentions.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::message::Message;

use super::xep0372::extract_references_from_message;

/// Namespace for XEP-0513 Explicit Mentions.
pub const NS_EXPLICIT_MENTIONS: &str = "urn:xmpp:mentions:0";

/// XEP-0513 channel-wide mention URI.
pub const CHANNEL_MENTION: &str = "urn:xmpp:mentions:0#channel";

/// XEP-0513 §301 example value for the `mentions#count` form field.
///
/// XEP-0513 §304: "Receiving entities SHOULD ignore all mentions if
/// the message contains more mentions than the threshold specified by
/// `mentions#count`." Used as the server-internal default until the
/// per-room override IQ (XEP-0513 §295) lands in a follow-up slice.
pub const DEFAULT_MENTIONS_COUNT: u32 = 5;

/// Counts the mention TARGETS on `message`. Includes:
///
/// - every parsed XEP-0513 `<mention/>` payload (the slice already
///   filters out structurally-empty elements that target nothing
///   per `parse_mention_element`);
/// - every XEP-0372 `<reference type='mention'/>` element — XEP-0513
///   §304's "more mentions than the threshold" cap defensively
///   extends to the XEP-0372 fallback path, otherwise an attacker
///   bypasses the cap by encoding the spam via XEP-0372 references
///   instead of XEP-0513 mentions (XEP-0513 §526 authorises
///   server-internal filtering "according to their own rules").
///
/// The result is clamped to `u32::MAX`; in practice a single message
/// would never legitimately approach that count.
pub fn mention_target_count(explicit_mentions: &[ExplicitMention], message: &Message) -> u32 {
    let xep0513 = explicit_mentions.len();
    let xep0372 = extract_references_from_message(message)
        .into_iter()
        .filter(|reference| reference.is_mention())
        .count();
    u32::try_from(xep0513.saturating_add(xep0372)).unwrap_or(u32::MAX)
}

/// Returns `true` when the mention payloads on `message` exceed the
/// configured `threshold` (XEP-0513 §304). Callers handle the
/// SHOULD-ignore-all-mentions consequence themselves.
pub fn mentions_exceed_threshold(
    explicit_mentions: &[ExplicitMention],
    message: &Message,
    threshold: u32,
) -> bool {
    mention_target_count(explicit_mentions, message) > threshold
}

/// A single top-level `<mention/>` payload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMention {
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub jid: Option<BareJid>,
    pub occupant_id: Option<String>,
    pub mentions: Option<String>,
    pub uri: Option<String>,
    pub active: bool,
    pub noping: bool,
}

impl ExplicitMention {
    pub fn jid(jid: BareJid) -> Self {
        Self {
            jid: Some(jid),
            ..Self::default()
        }
    }

    pub fn occupant_id(occupant_id: impl Into<String>) -> Self {
        Self {
            occupant_id: Some(occupant_id.into()),
            ..Self::default()
        }
    }

    pub fn channel() -> Self {
        Self {
            mentions: Some(CHANNEL_MENTION.to_string()),
            ..Self::default()
        }
    }

    pub fn active_channel() -> Self {
        Self {
            active: true,
            ..Self::channel()
        }
    }

    pub fn is_channel(&self) -> bool {
        self.mentions.as_deref() == Some(CHANNEL_MENTION)
    }

    pub fn is_individual(&self) -> bool {
        self.jid.is_some() || self.occupant_id.is_some()
    }
}

/// A set of explicit mentions in a message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplicitMentions {
    pub mentions: Vec<ExplicitMention>,
}

impl ExplicitMentions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mention(mut self, mention: ExplicitMention) -> Self {
        self.mentions.push(mention);
        self
    }

    pub fn with_channel(self) -> Self {
        self.with_mention(ExplicitMention::channel())
    }

    pub fn with_active_channel(self) -> Self {
        self.with_mention(ExplicitMention::active_channel())
    }

    pub fn has_channel(&self) -> bool {
        self.mentions.iter().any(ExplicitMention::is_channel)
    }

    pub fn mentions_jid(&self, jid: &BareJid) -> bool {
        self.mentions.iter().any(|mention| {
            mention
                .jid
                .as_ref()
                .is_some_and(|mentioned| mentioned == jid)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.mentions.is_empty()
    }
}

/// Trait for types that can carry explicit mentions.
pub trait ExplicitMentionCarrier {
    fn explicit_mentions(&self) -> Option<ExplicitMentions>;

    fn has_explicit_mentions(&self) -> bool {
        self.explicit_mentions().is_some_and(|m| !m.is_empty())
    }
}

impl ExplicitMentionCarrier for Message {
    fn explicit_mentions(&self) -> Option<ExplicitMentions> {
        extract_explicit_mentions(self)
    }
}

pub fn is_mention_element(elem: &Element) -> bool {
    elem.is("mention", NS_EXPLICIT_MENTIONS)
}

pub fn has_explicit_mentions(msg: &Message) -> bool {
    msg.payloads.iter().any(is_mention_element)
}

pub fn extract_explicit_mentions(msg: &Message) -> Option<ExplicitMentions> {
    let mentions: Vec<ExplicitMention> = msg
        .payloads
        .iter()
        .filter(|elem| is_mention_element(elem))
        .filter_map(parse_mention_element)
        .collect();

    if mentions.is_empty() {
        None
    } else {
        Some(ExplicitMentions { mentions })
    }
}

pub fn parse_mention_element(elem: &Element) -> Option<ExplicitMention> {
    let begin = elem.attr("begin").and_then(|value| value.parse().ok());
    let end = elem.attr("end").and_then(|value| value.parse().ok());
    let jid = elem.attr("jid").and_then(|value| value.parse().ok());
    let occupant_id = elem.attr("occupantid").map(str::to_string);
    let mentions = elem.attr("mentions").map(str::to_string);
    let uri = elem.attr("uri").map(str::to_string);
    let active = elem.get_child("active", NS_EXPLICIT_MENTIONS).is_some();
    let noping = elem.get_child("noping", NS_EXPLICIT_MENTIONS).is_some();

    if jid.is_none()
        && occupant_id.is_none()
        && mentions.is_none()
        && uri.is_none()
        && !active
        && !noping
    {
        return None;
    }

    Some(ExplicitMention {
        begin,
        end,
        jid,
        occupant_id,
        mentions,
        uri,
        active,
        noping,
    })
}

pub fn build_mention_element(mention: &ExplicitMention) -> Element {
    let mut elem = Element::builder("mention", NS_EXPLICIT_MENTIONS).build();

    if let Some(begin) = mention.begin {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("begin").to_owned(),
            begin.to_string(),
        );
    }
    if let Some(end) = mention.end {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("end").to_owned(),
            end.to_string(),
        );
    }
    if let Some(jid) = &mention.jid {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("jid").to_owned(),
            jid.to_string(),
        );
    }
    if let Some(occupant_id) = &mention.occupant_id {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("occupantid").to_owned(),
            occupant_id,
        );
    }
    if let Some(mentions) = &mention.mentions {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("mentions").to_owned(),
            mentions,
        );
    }
    if let Some(uri) = &mention.uri {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("uri").to_owned(),
            uri,
        );
    }
    if mention.active {
        elem.append_child(Element::builder("active", NS_EXPLICIT_MENTIONS).build());
    }
    if mention.noping {
        elem.append_child(Element::builder("noping", NS_EXPLICIT_MENTIONS).build());
    }

    elem
}

pub fn build_mentions_elements(mentions: &ExplicitMentions) -> Vec<Element> {
    mentions
        .mentions
        .iter()
        .map(build_mention_element)
        .collect()
}

pub fn set_explicit_mentions(msg: &mut Message, mentions: &ExplicitMentions) {
    strip_explicit_mentions(msg);
    msg.payloads.extend(build_mentions_elements(mentions));
}

pub fn strip_explicit_mentions(msg: &mut Message) {
    msg.payloads
        .retain(|elem| elem.ns() != NS_EXPLICIT_MENTIONS);
}

#[cfg(test)]
mod count_tests {
    use super::*;
    use crate::xep::xep0372::{add_reference, Reference};
    use xmpp_parsers::message::Message;

    fn empty_message() -> Message {
        Message::new(None::<jid::Jid>)
    }

    fn with_xep0513_mention(msg: &mut Message, mention: ExplicitMention) {
        msg.payloads.push(build_mention_element(&mention));
    }

    /// XEP-0513 `<mention/>` payloads contribute one mention TARGET
    /// per parsed element. The parser already drops structurally-empty
    /// elements that target nothing (see `parse_mention_element`), so
    /// every element in the slice is a real mention.
    #[test]
    fn mention_target_count_counts_xep0513_mentions() {
        let mut msg = empty_message();
        with_xep0513_mention(
            &mut msg,
            ExplicitMention::jid("alice@example.com".parse().expect("alice bare")),
        );
        with_xep0513_mention(&mut msg, ExplicitMention::occupant_id("room-stable-bob"));
        with_xep0513_mention(&mut msg, ExplicitMention::channel());
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mention_target_count(&mentions, &msg), 3);
    }

    /// XEP-0372 `<reference type='mention'/>` elements ALSO contribute
    /// to the per-message mention count — without this an attacker
    /// bypasses the XEP-0513 §304 cap by encoding spam as XEP-0372
    /// references instead.
    #[test]
    fn mention_target_count_counts_xep0372_mention_references() {
        let mut msg = empty_message();
        add_reference(&mut msg, &Reference::mention("xmpp:alice@example.com"));
        add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));
        // XEP-0372 references with type='data' MUST NOT be counted —
        // they're file attachments, not mentions.
        add_reference(
            &mut msg,
            &Reference::data("https://files.example.com/cat.jpg"),
        );
        assert_eq!(mention_target_count(&[], &msg), 2);
    }

    /// Sum of both XEPs is reported.
    #[test]
    fn mention_target_count_sums_xep0513_and_xep0372() {
        let mut msg = empty_message();
        with_xep0513_mention(
            &mut msg,
            ExplicitMention::jid("alice@example.com".parse().expect("alice bare")),
        );
        add_reference(&mut msg, &Reference::mention("xmpp:bob@example.com"));
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mention_target_count(&mentions, &msg), 2);
    }

    /// Zero mentions → zero count. The threshold check via
    /// `mentions_exceed_threshold` is `false` regardless of threshold.
    #[test]
    fn mention_target_count_is_zero_for_unmentioned_message() {
        let msg = empty_message();
        assert_eq!(mention_target_count(&[], &msg), 0);
        assert!(!mentions_exceed_threshold(&[], &msg, 0));
    }

    /// XEP-0513 §304 boundary: "more than the threshold" is strict.
    /// Equal count does NOT exceed.
    #[test]
    fn mentions_exceed_threshold_is_strict_inequality() {
        let mut msg = empty_message();
        for i in 0..5 {
            with_xep0513_mention(
                &mut msg,
                ExplicitMention::jid(
                    format!("user{i}@example.com")
                        .parse()
                        .expect("target bare jid"),
                ),
            );
        }
        let mentions = extract_explicit_mentions(&msg)
            .map(|m| m.mentions)
            .unwrap_or_default();
        assert_eq!(mention_target_count(&mentions, &msg), 5);
        assert!(
            !mentions_exceed_threshold(&mentions, &msg, 5),
            "count == threshold MUST NOT exceed; §304 says \"more than\""
        );
        assert!(
            mentions_exceed_threshold(&mentions, &msg, 4),
            "count > threshold MUST exceed"
        );
    }
}
