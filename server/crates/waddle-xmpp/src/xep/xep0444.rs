//! XEP-0444: Message Reactions
//!
//! Provides helpers for detecting, parsing, and building emoji reaction
//! elements within XMPP message stanzas.
//!
//! ## XML Format
//!
//! Send/update reactions to a message:
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <reactions id='target-msg-id' xmlns='urn:xmpp:reactions:0'>
//!     <reaction>👍</reaction>
//!     <reaction>❤️</reaction>
//!   </reactions>
//! </message>
//! ```
//!
//! Remove all reactions (empty set):
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <reactions id='target-msg-id' xmlns='urn:xmpp:reactions:0'/>
//! </message>
//! ```
//!
//! ## Semantics
//!
//! - Each `<reactions/>` message replaces ALL previous reactions from that sender.
//! - An empty `<reactions/>` set removes all reactions from the sender.
//! - The `id` attribute references the message being reacted to.
//! - Reactions are body-less messages (no `<body/>`).
//!
//! ## Server Behavior
//!
//! The server transparently routes reaction messages. They should not be
//! archived as regular messages but may be stored for reaction aggregation.

use std::collections::HashSet;

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0444 Message Reactions.
pub const NS_REACTIONS: &str = "urn:xmpp:reactions:0";

/// Errors that can occur when parsing reaction elements.
#[derive(Debug, Error)]
pub enum ReactionError {
    /// A `<reactions/>` element is missing its required `id` attribute.
    #[error("reactions element missing id attribute")]
    MissingId,
}

/// A set of reactions from one sender to a specific message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionSet {
    /// The id of the message being reacted to.
    pub message_id: String,
    /// The emoji reactions. Empty means "remove all my reactions."
    pub emojis: Vec<String>,
}

impl ReactionSet {
    /// Create a new reaction set for the given message.
    pub fn new(message_id: impl Into<String>, emojis: Vec<String>) -> Self {
        Self {
            message_id: message_id.into(),
            emojis: normalize_reactions(emojis),
        }
    }

    /// Returns `true` if this is a removal (empty reaction set).
    pub fn is_removal(&self) -> bool {
        self.emojis.is_empty()
    }
}

/// Trait for types that can carry reaction elements.
pub trait ReactionCarrier {
    /// Extract the reaction set from this carrier, if present.
    fn reactions(&self) -> Option<ReactionSet>;

    /// Returns `true` if this carrier contains reactions.
    fn has_reactions(&self) -> bool {
        self.reactions().is_some()
    }

    /// Returns the message id being reacted to, if this carries reactions.
    fn reacted_message_id(&self) -> Option<String> {
        self.reactions().map(|r| r.message_id)
    }
}

impl ReactionCarrier for Message {
    fn reactions(&self) -> Option<ReactionSet> {
        extract_reactions_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<reactions/>` element.
pub fn is_reactions_element(elem: &Element) -> bool {
    elem.ns() == NS_REACTIONS && elem.name() == "reactions"
}

/// Check if a message contains a `<reactions/>` element.
pub fn is_reaction_message(msg: &Message) -> bool {
    msg.payloads.iter().any(is_reactions_element)
}

/// A reaction-ONLY message: carries `<reactions/>` and no substantive
/// body (#780). This is the push-suppression predicate — "Alice
/// reacted 👍" should not fire an OS push, but a message that adjusts
/// reactions AND carries a real body still notifies. Whitespace-only
/// bodies do not count as substantive, matching the archive layer's
/// body test.
pub fn is_reaction_only_message(msg: &Message) -> bool {
    is_reaction_message(msg) && !msg.bodies.values().any(|text| !text.trim().is_empty())
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the reaction set from a message.
pub fn extract_reactions_from_message(msg: &Message) -> Option<ReactionSet> {
    let elem = msg.payloads.iter().find(|e| is_reactions_element(e))?;
    let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();

    let emojis = elem
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == NS_REACTIONS)
        .map(|child| child.text())
        .collect();

    Some(ReactionSet::new(id, emojis))
}

/// Extract just the target message id from a reaction message.
pub fn extract_reacted_id(msg: &Message) -> Option<String> {
    msg.payloads
        .iter()
        .find(|e| is_reactions_element(e))
        .and_then(|e| e.attr("id"))
        .filter(|id| !id.is_empty())
        .map(|id| id.to_owned())
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<reaction>emoji</reaction>` element.
pub fn build_reaction_element(emoji: &str) -> Element {
    let mut elem = Element::builder("reaction", NS_REACTIONS).build();
    elem.append_text_node(emoji);
    elem
}

fn normalize_reactions<I, S>(emojis: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();

    for emoji in emojis {
        let emoji = emoji.as_ref().trim();
        if emoji.is_empty() {
            continue;
        }

        let emoji = emoji.to_owned();
        if seen.insert(emoji.clone()) {
            normalized.push(emoji);
        }
    }

    normalized
}

/// Build a `<reactions id='...'><reaction>...</reaction>...</reactions>` element.
pub fn build_reactions_element(message_id: &str, emojis: &[&str]) -> Element {
    let mut reactions = Element::builder("reactions", NS_REACTIONS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), message_id)
        .build();
    for emoji in normalize_reactions(emojis.iter().copied()) {
        reactions.append_child(build_reaction_element(&emoji));
    }
    reactions
}

/// Build a reaction message.
///
/// Creates a body-less `<message>` containing a `<reactions/>` element.
pub fn build_reaction_message(
    to: impl Into<Option<jid::Jid>>,
    from: impl Into<Option<jid::Jid>>,
    message_id: &str,
    emojis: &[&str],
    message_type: xmpp_parsers::message::MessageType,
) -> Message {
    let mut msg = Message::new(to.into());
    msg.from = from.into();
    msg.type_ = message_type;
    msg.id = Some(xmpp_parsers::message::Id(uuid::Uuid::new_v4().to_string()));
    msg.payloads
        .push(build_reactions_element(message_id, emojis));
    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Set reactions on a message, replacing any existing reactions.
pub fn set_reactions(msg: &mut Message, message_id: &str, emojis: &[&str]) {
    strip_reactions(msg);
    msg.payloads
        .push(build_reactions_element(message_id, emojis));
}

/// Remove all reaction elements from a message.
pub fn strip_reactions(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_REACTIONS);
}

// ── Conversion ───────────────────────────────────────────────────────

impl From<xmpp_parsers::reactions::Reactions> for ReactionSet {
    fn from(r: xmpp_parsers::reactions::Reactions) -> Self {
        Self::new(r.id, r.reactions.into_iter().map(|rx| rx.emoji).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};

    #[test]
    fn test_is_reactions_element() {
        let reactions = Element::builder("reactions", NS_REACTIONS)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "msg-1")
            .build();
        assert!(is_reactions_element(&reactions));

        let wrong_ns = Element::builder("reactions", "jabber:client").build();
        assert!(!is_reactions_element(&wrong_ns));

        let wrong_name = Element::builder("reaction", NS_REACTIONS).build();
        assert!(!is_reactions_element(&wrong_name));
    }

    #[test]
    fn reaction_only_requires_reactions_and_no_substantive_body() {
        // Reaction with no body → reaction-only (#780 push suppression).
        let mut reaction_only = Message::new(None::<jid::Jid>);
        reaction_only
            .payloads
            .push(build_reactions_element("msg-1", &["👍"]));
        assert!(is_reaction_only_message(&reaction_only));

        // Whitespace-only body still counts as reaction-only.
        let mut whitespace_body = reaction_only.clone();
        whitespace_body
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), "  ".to_string());
        assert!(is_reaction_only_message(&whitespace_body));

        // Reaction + substantive body → NOT reaction-only (an edit
        // that also adjusts reactions must still notify).
        let mut with_body = reaction_only.clone();
        with_body.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "also saying something".to_string(),
        );
        assert!(!is_reaction_only_message(&with_body));

        // Plain body message → not reaction-only.
        let mut plain = Message::new(None::<jid::Jid>);
        plain
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());
        assert!(!is_reaction_only_message(&plain));
    }

    #[test]
    fn test_is_reaction_message() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='msg-1'>\
                      <reaction>👍</reaction>\
                    </reactions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(is_reaction_message(&msg));
    }

    #[test]
    fn test_extract_reactions() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='msg-42'>\
                      <reaction>👍</reaction>\
                      <reaction>❤️</reaction>\
                    </reactions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let set = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set.message_id, "msg-42");
        assert_eq!(set.emojis, vec!["👍", "❤️"]);
        assert!(!set.is_removal());
    }

    #[test]
    fn test_extract_empty_reactions_is_removal() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='msg-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let set = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set.message_id, "msg-1");
        assert!(set.emojis.is_empty());
        assert!(set.is_removal());
    }

    #[test]
    fn test_extract_reactions_deduplicates_duplicates() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='msg-42'>\
                      <reaction> 👍 </reaction>\
                      <reaction>👍</reaction>\
                      <reaction>❤️</reaction>\
                      <reaction>👍</reaction>\
                      <reaction>   </reaction>\
                    </reactions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let set = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set.emojis, vec!["👍", "❤️"]);
    }

    #[test]
    fn test_extract_reactions_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_reactions_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_reactions_missing_id() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id=''>\
                      <reaction>👍</reaction>\
                    </reactions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert!(extract_reactions_from_message(&msg).is_none());
    }

    #[test]
    fn test_extract_reacted_id() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='abc-123'>\
                      <reaction>🎉</reaction>\
                    </reactions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");
        assert_eq!(extract_reacted_id(&msg), Some("abc-123".to_owned()));
    }

    #[test]
    fn test_build_reaction_element() {
        let elem = build_reaction_element("👍");
        assert_eq!(elem.name(), "reaction");
        assert_eq!(elem.ns(), NS_REACTIONS);
        assert_eq!(elem.text(), "👍");
    }

    #[test]
    fn test_build_reactions_element() {
        let elem = build_reactions_element("msg-99", &["👍", "❤️"]);
        assert_eq!(elem.name(), "reactions");
        assert_eq!(elem.ns(), NS_REACTIONS);
        assert_eq!(elem.attr("id"), Some("msg-99"));
        let children: Vec<_> = elem.children().collect();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].text(), "👍");
        assert_eq!(children[1].text(), "❤️");
    }

    #[test]
    fn test_build_reactions_element_deduplicates_duplicates() {
        let elem = build_reactions_element("msg-99", &["👍", " 👍 ", "", "❤️", "👍"]);
        let children: Vec<_> = elem.children().collect();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].text(), "👍");
        assert_eq!(children[1].text(), "❤️");
    }

    #[test]
    fn test_build_empty_reactions_element() {
        let elem = build_reactions_element("msg-1", &[]);
        assert_eq!(elem.children().count(), 0);
        assert_eq!(elem.attr("id"), Some("msg-1"));
    }

    #[test]
    fn test_build_reaction_message() {
        let to: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let msg = build_reaction_message(
            to.clone(),
            None::<jid::Jid>,
            "orig-1",
            &["👍"],
            MessageType::Groupchat,
        );

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.type_, MessageType::Groupchat);
        assert!(msg.bodies.is_empty());
        let set = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set.message_id, "orig-1");
        assert_eq!(set.emojis, vec!["👍"]);
    }

    #[test]
    fn test_set_reactions() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_reactions(&mut msg, "msg-1", &["👍", "🎉"]);

        let set = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set.emojis.len(), 2);

        // Replace
        set_reactions(&mut msg, "msg-1", &["❤️"]);
        let set2 = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set2.emojis, vec!["❤️"]);
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_REACTIONS)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_reactions() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='msg-1'>\
                      <reaction>👍</reaction>\
                    </reactions>\
                    </message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        strip_reactions(&mut msg);
        assert!(!is_reaction_message(&msg));
    }

    #[test]
    fn test_reaction_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <reactions xmlns='urn:xmpp:reactions:0' id='msg-1'>\
                      <reaction>👍</reaction>\
                    </reactions>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_reactions());
        assert_eq!(msg.reacted_message_id(), Some("msg-1".to_owned()));
        let set = msg.reactions().expect("has reactions");
        assert_eq!(set.emojis, vec!["👍"]);

        let plain = Message::new(None::<jid::Jid>);
        assert!(!plain.has_reactions());
    }

    #[test]
    fn test_reaction_set_new() {
        let set = ReactionSet::new(
            "msg-1",
            vec![
                "👍".to_owned(),
                " 👍 ".to_owned(),
                "❤️".to_owned(),
                "   ".to_owned(),
            ],
        );
        assert_eq!(set.message_id, "msg-1");
        assert_eq!(set.emojis, vec!["👍", "❤️"]);
        assert!(!set.is_removal());

        let empty = ReactionSet::new("msg-2", vec![]);
        assert!(empty.is_removal());
    }

    #[test]
    fn test_conversion_from_xmpp_parsers() {
        let parser_reactions = xmpp_parsers::reactions::Reactions {
            id: "msg-1".to_owned(),
            reactions: vec![
                xmpp_parsers::reactions::Reaction {
                    emoji: "👍".to_owned(),
                },
                xmpp_parsers::reactions::Reaction {
                    emoji: "👍".to_owned(),
                },
                xmpp_parsers::reactions::Reaction {
                    emoji: "🎉".to_owned(),
                },
            ],
        };
        let set: ReactionSet = parser_reactions.into();
        assert_eq!(set.message_id, "msg-1");
        assert_eq!(set.emojis, vec!["👍", "🎉"]);
    }

    #[test]
    fn test_zwj_emoji() {
        let emoji = "👩🏾‍❤️‍👩🏼";
        let elem = build_reaction_element(emoji);
        assert_eq!(elem.text(), emoji);

        let reactions = build_reactions_element("msg-1", &[emoji]);
        let msg_elem: Element = {
            let mut m = Element::builder("message", "jabber:client")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
                .build();
            m.append_child(reactions);
            m
        };
        let msg = Message::try_from(msg_elem).expect("valid message");
        let set = extract_reactions_from_message(&msg).expect("has reactions");
        assert_eq!(set.emojis, vec![emoji]);
    }
}
