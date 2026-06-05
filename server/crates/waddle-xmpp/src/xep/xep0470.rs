//! XEP-0470: Pubsub Attachments
//!
//! Attach metadata (reactions, comments, votes) to existing PubSub items.
//! Enables social interactions on posts without modifying the original.
//!
//! ## XML Format
//!
//! Attach a reaction to a post:
//! ```xml
//! <item id='attachment-1' xmlns='http://jabber.org/protocol/pubsub'>
//!   <attachments xmlns='urn:xmpp:pubsub-attachments:1'>
//!     <reactions>
//!       <reaction>👍</reaction>
//!       <reaction>❤️</reaction>
//!     </reactions>
//!   </attachments>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - React to social feed posts
//! - Comment threads on announcements
//! - Vote/poll responses

use jid::BareJid;
use minidom::Element;
use std::collections::{BTreeMap, BTreeSet};
use unicode_segmentation::UnicodeSegmentation;

/// Namespace for XEP-0470 Pubsub Attachments.
pub const NS_PUBSUB_ATTACHMENTS: &str = "urn:xmpp:pubsub-attachments:1";

/// Namespace for XEP-0470 Pubsub Attachment summaries.
pub const NS_PUBSUB_ATTACHMENTS_SUMMARY: &str = "urn:xmpp:pubsub-attachments:summary:1";

/// Prefix for an attachment node name. The full node is
/// `{PUBSUB_ATTACHMENTS_NODE_PREFIX}/<target-item-XMPP-URI>`.
pub const PUBSUB_ATTACHMENTS_NODE_PREFIX: &str = NS_PUBSUB_ATTACHMENTS;

/// Maximum reaction count accepted for a single publisher's attachment item.
pub const MAX_REACTIONS_PER_ATTACHMENT: usize = 12;

/// An attachment payload type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentPayload {
    /// A set of reaction emoji attached by one bare JID.
    Reactions(ReactionSet),
    /// XEP-0470 `<noticed/>`; used by later slices.
    Noticed,
    /// XEP-0470 summary payload; used by later slices.
    Summary(AttachmentSummary),
    /// A generic XML payload.
    Custom(Element),
}

/// A pubsub attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Known attachment payloads.
    pub payloads: Vec<AttachmentPayload>,
    /// Unknown children preserved across read-modify-publish cycles.
    pub unknown_children: Vec<Element>,
}

impl Attachment {
    /// Create an attachment with a reaction set.
    pub fn reactions(reactions: ReactionSet) -> Self {
        Self {
            payloads: vec![AttachmentPayload::Reactions(reactions)],
            unknown_children: Vec::new(),
        }
    }

    /// Create an empty attachment container.
    pub fn empty() -> Self {
        Self {
            payloads: Vec::new(),
            unknown_children: Vec::new(),
        }
    }

    /// Preserve an unknown attachment child.
    pub fn with_unknown_child(mut self, child: Element) -> Self {
        self.unknown_children.push(child);
        self
    }

    /// Add a known payload.
    pub fn with_payload(mut self, payload: AttachmentPayload) -> Self {
        self.payloads.push(payload);
        self
    }

    /// Return the reaction set if present.
    pub fn reactions_set(&self) -> Option<&ReactionSet> {
        self.payloads.iter().find_map(|payload| match payload {
            AttachmentPayload::Reactions(reactions) => Some(reactions),
            _ => None,
        })
    }
}

/// XEP-0470 reaction set attached by one publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionSet {
    pub emojis: Vec<String>,
    pub timestamp: Option<String>,
}

impl ReactionSet {
    pub fn new<I, S>(emojis: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            emojis: normalize_reactions(emojis),
            timestamp: None,
        }
    }

    pub fn with_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.emojis.is_empty()
    }

    pub fn validate(&self) -> Result<(), AttachmentValidationError> {
        if self.emojis.len() > MAX_REACTIONS_PER_ATTACHMENT {
            return Err(AttachmentValidationError::TooManyReactions {
                actual: self.emojis.len(),
                max: MAX_REACTIONS_PER_ATTACHMENT,
            });
        }

        if let Some(emoji) = self
            .emojis
            .iter()
            .find(|emoji| !is_single_extended_grapheme_cluster(emoji))
        {
            return Err(AttachmentValidationError::ReactionNotSingleGrapheme(
                emoji.clone(),
            ));
        }

        Ok(())
    }
}

/// Summary reaction count for a single emoji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionSummary {
    pub emoji: String,
    pub count: u32,
}

/// XEP-0470 summary payload. Later slices can populate this from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSummary {
    pub reactions: Vec<ReactionSummary>,
    pub noticed_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentValidationError {
    TooManyReactions { actual: usize, max: usize },
    ReactionNotSingleGrapheme(String),
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is an `<attachments/>` element.
pub fn is_attachments_element(elem: &Element) -> bool {
    elem.ns() == NS_PUBSUB_ATTACHMENTS && elem.name() == "attachments"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse an `<attachments/>` element.
pub fn parse_attachments_element(elem: &Element) -> Option<Attachment> {
    if !is_attachments_element(elem) {
        return None;
    }

    let mut attachment = Attachment::empty();
    for child in elem.children() {
        let child_ns = child.ns();
        match (child.name(), child_ns.as_str()) {
            ("reactions", NS_PUBSUB_ATTACHMENTS) => attachment
                .payloads
                .push(AttachmentPayload::Reactions(parse_reactions_element(child))),
            ("noticed", NS_PUBSUB_ATTACHMENTS) => {
                attachment.payloads.push(AttachmentPayload::Noticed)
            }
            ("summary", NS_PUBSUB_ATTACHMENTS_SUMMARY) => attachment
                .payloads
                .push(AttachmentPayload::Summary(parse_summary_element(child))),
            _ => attachment.unknown_children.push(child.clone()),
        }
    }

    Some(attachment)
}

fn parse_reactions_element(elem: &Element) -> ReactionSet {
    let emojis = elem
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == NS_PUBSUB_ATTACHMENTS)
        .map(Element::text);
    let mut reactions = ReactionSet::new(emojis);
    reactions.timestamp = elem.attr("timestamp").map(ToOwned::to_owned);
    reactions
}

fn parse_summary_element(elem: &Element) -> AttachmentSummary {
    let reactions = elem
        .children()
        .find(|child| child.name() == "reactions" && child.ns() == NS_PUBSUB_ATTACHMENTS_SUMMARY)
        .into_iter()
        .flat_map(Element::children)
        .filter(|child| child.name() == "reaction" && child.ns() == NS_PUBSUB_ATTACHMENTS_SUMMARY)
        .map(|child| ReactionSummary {
            emoji: child.text(),
            count: child
                .attr("count")
                .and_then(|count| count.parse::<u32>().ok())
                .unwrap_or(1),
        })
        .collect();

    let noticed_count = elem
        .children()
        .find(|child| child.name() == "noticed" && child.ns() == NS_PUBSUB_ATTACHMENTS_SUMMARY)
        .and_then(|child| child.attr("count"))
        .and_then(|count| count.parse::<u32>().ok());

    AttachmentSummary {
        reactions,
        noticed_count,
    }
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<attachments/>` element.
pub fn build_attachments_element(attachment: &Attachment) -> Element {
    let mut elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS).build();

    for payload in &attachment.payloads {
        match payload {
            AttachmentPayload::Reactions(reactions) => {
                elem.append_child(build_reactions_element(reactions));
            }
            AttachmentPayload::Noticed => {
                elem.append_child(Element::builder("noticed", NS_PUBSUB_ATTACHMENTS).build());
            }
            AttachmentPayload::Summary(summary) => {
                elem.append_child(build_summary_element(summary));
            }
            AttachmentPayload::Custom(child) => {
                elem.append_child(child.clone());
            }
        }
    }

    for child in &attachment.unknown_children {
        elem.append_child(child.clone());
    }

    elem
}

pub fn build_reactions_element(reactions: &ReactionSet) -> Element {
    let mut elem = Element::builder("reactions", NS_PUBSUB_ATTACHMENTS).build();
    if let Some(timestamp) = &reactions.timestamp {
        elem.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("timestamp").to_owned(),
            timestamp,
        );
    }

    for emoji in &reactions.emojis {
        let mut reaction = Element::builder("reaction", NS_PUBSUB_ATTACHMENTS).build();
        reaction.append_text_node(emoji);
        elem.append_child(reaction);
    }

    elem
}

pub fn build_summary_element(summary: &AttachmentSummary) -> Element {
    let mut root = Element::builder("summary", NS_PUBSUB_ATTACHMENTS_SUMMARY).build();
    let mut reactions = Element::builder("reactions", NS_PUBSUB_ATTACHMENTS_SUMMARY).build();

    for reaction_summary in &summary.reactions {
        let mut reaction = Element::builder("reaction", NS_PUBSUB_ATTACHMENTS_SUMMARY).build();
        reaction.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("count").to_owned(),
            reaction_summary.count.to_string(),
        );
        reaction.append_text_node(reaction_summary.emoji.clone());
        reactions.append_child(reaction);
    }

    root.append_child(reactions);
    if let Some(count) = summary.noticed_count {
        let noticed = Element::builder("noticed", NS_PUBSUB_ATTACHMENTS_SUMMARY)
            .attr(
                minidom::rxml::xml_ncname!("count").to_owned(),
                count.to_string(),
            )
            .build();
        root.append_child(noticed);
    }
    root
}

pub fn summarize_attachments<I>(attachments: I) -> AttachmentSummary
where
    I: IntoIterator<Item = (BareJid, Attachment)>,
{
    let mut reactions_by_emoji: BTreeMap<String, BTreeSet<BareJid>> = BTreeMap::new();
    let mut noticed_publishers = BTreeSet::new();

    for (publisher, attachment) in attachments {
        for payload in attachment.payloads {
            match payload {
                AttachmentPayload::Reactions(reactions) => {
                    for emoji in reactions.emojis {
                        reactions_by_emoji
                            .entry(emoji)
                            .or_default()
                            .insert(publisher.clone());
                    }
                }
                AttachmentPayload::Noticed => {
                    noticed_publishers.insert(publisher.clone());
                }
                AttachmentPayload::Summary(_) | AttachmentPayload::Custom(_) => {}
            }
        }
    }

    AttachmentSummary {
        reactions: reactions_by_emoji
            .into_iter()
            .map(|(emoji, publishers)| ReactionSummary {
                emoji,
                count: publishers.len() as u32,
            })
            .collect(),
        noticed_count: (!noticed_publishers.is_empty()).then_some(noticed_publishers.len() as u32),
    }
}

fn normalize_reactions<I, S>(emojis: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut normalized = Vec::new();
    let mut seen = BTreeSet::new();

    for emoji in emojis {
        let emoji = emoji.as_ref().trim();
        if emoji.is_empty() {
            continue;
        }
        if seen.insert(emoji.to_owned()) {
            normalized.push(emoji.to_owned());
        }
    }

    normalized
}

fn is_single_extended_grapheme_cluster(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !trimmed.chars().any(char::is_whitespace)
        && trimmed.graphemes(true).count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_attachments_element() {
        let elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS).build();
        assert!(is_attachments_element(&elem));

        let wrong = Element::builder("attachments", "jabber:client").build();
        assert!(!is_attachments_element(&wrong));
    }

    #[test]
    fn test_parse_target() {
        let mut reactions = Element::builder("reactions", NS_PUBSUB_ATTACHMENTS).build();
        let mut reaction = Element::builder("reaction", NS_PUBSUB_ATTACHMENTS).build();
        reaction.append_text_node("👍");
        reactions.append_child(reaction);

        let mut elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS).build();
        elem.append_child(reactions);

        let attachment = parse_attachments_element(&elem).expect("parseable");
        assert_eq!(
            attachment.reactions_set().expect("reactions").emojis,
            vec!["👍"]
        );
    }

    #[test]
    fn test_parse_target_missing_attrs() {
        let elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS).build();
        let attachment = parse_attachments_element(&elem).expect("parseable");
        assert!(attachment.payloads.is_empty());
    }

    #[test]
    fn test_build_comment() {
        let attachment = Attachment::empty().with_payload(AttachmentPayload::Noticed);
        let elem = build_attachments_element(&attachment);

        assert_eq!(elem.name(), "attachments");
        assert_eq!(elem.ns(), NS_PUBSUB_ATTACHMENTS);

        let noticed = elem.children().next().expect("has child");
        assert_eq!(noticed.name(), "noticed");
    }

    #[test]
    fn test_build_reaction() {
        let attachment = Attachment::reactions(ReactionSet::new(["👍", "❤️"]));
        let elem = build_attachments_element(&attachment);

        let reactions = elem.children().next().expect("has child");
        assert_eq!(reactions.name(), "reactions");
        assert_eq!(
            reactions.children().map(Element::text).collect::<Vec<_>>(),
            vec!["👍", "❤️"]
        );
    }

    #[test]
    fn summarizes_reactions_and_noticed_by_distinct_publisher() {
        let alice = "alice@example.test".parse::<BareJid>().expect("alice jid");
        let bob = "bob@example.test".parse::<BareJid>().expect("bob jid");
        let summary = summarize_attachments([
            (
                alice.clone(),
                Attachment::reactions(ReactionSet::new(["👍", "❤️", "👍"]))
                    .with_payload(AttachmentPayload::Noticed),
            ),
            (
                bob,
                Attachment::reactions(ReactionSet::new(["👍"]))
                    .with_payload(AttachmentPayload::Noticed),
            ),
            (
                alice,
                Attachment::empty()
                    .with_unknown_child(Element::builder("future", "urn:example:future").build()),
            ),
        ]);

        assert_eq!(
            summary,
            AttachmentSummary {
                reactions: vec![
                    ReactionSummary {
                        emoji: "❤️".to_owned(),
                        count: 1,
                    },
                    ReactionSummary {
                        emoji: "👍".to_owned(),
                        count: 2,
                    },
                ],
                noticed_count: Some(2),
            }
        );
    }

    #[test]
    fn builds_summary_payload_with_counts() {
        let summary = AttachmentSummary {
            reactions: vec![ReactionSummary {
                emoji: "👍".to_owned(),
                count: 1,
            }],
            noticed_count: Some(2),
        };
        let elem = build_summary_element(&summary);
        let reactions = elem
            .children()
            .find(|child| child.name() == "reactions")
            .expect("reactions child");
        let reaction = reactions.children().next().expect("reaction child");
        let noticed = elem
            .children()
            .find(|child| child.name() == "noticed")
            .expect("noticed child");

        assert_eq!(elem.name(), "summary");
        assert_eq!(elem.ns(), NS_PUBSUB_ATTACHMENTS_SUMMARY);
        assert_eq!(reaction.attr("count"), Some("1"));
        assert_eq!(reaction.text(), "👍");
        assert_eq!(noticed.attr("count"), Some("2"));
    }

    #[test]
    fn test_attachment_helpers() {
        let attachment = Attachment::reactions(ReactionSet::new(["👍"]));
        assert_eq!(
            attachment.reactions_set().expect("reactions").emojis,
            vec!["👍"]
        );
    }

    #[test]
    fn test_attachment_target_new() {
        let set = ReactionSet::new(["👍", "👍", "", " ❤️ "]);
        assert_eq!(set.emojis, vec!["👍", "❤️"]);
    }

    #[test]
    fn test_is_attachments_element_negative() {
        let other = Element::builder("other", NS_PUBSUB_ATTACHMENTS).build();
        assert!(!is_attachments_element(&other));
        let wrong_ns = Element::builder("attachments", "wrong:ns").build();
        assert!(!is_attachments_element(&wrong_ns));
    }

    #[test]
    fn test_namespace_constant() {
        assert_eq!(NS_PUBSUB_ATTACHMENTS, "urn:xmpp:pubsub-attachments:1");
    }

    #[test]
    fn test_attachment_with_author() {
        let set = ReactionSet::new(["👍"]).with_timestamp("2022-07-11T12:07:48Z");
        let elem = build_reactions_element(&set);
        assert_eq!(elem.attr("timestamp"), Some("2022-07-11T12:07:48Z"));
    }

    #[test]
    fn test_build_and_parse_roundtrip() {
        let unknown = Element::builder("future", "urn:example:future").build();
        let attachment = Attachment::reactions(ReactionSet::new(["👍", "❤️"]))
            .with_unknown_child(unknown.clone());
        let elem = build_attachments_element(&attachment);
        let parsed = parse_attachments_element(&elem).expect("parseable");
        assert_eq!(
            parsed.reactions_set().expect("reactions").emojis,
            vec!["👍", "❤️"]
        );
        assert_eq!(parsed.unknown_children, vec![unknown]);
    }

    #[test]
    fn validation_rejects_too_many_reactions() {
        let set = ReactionSet::new([
            "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13",
        ]);

        assert_eq!(
            set.validate(),
            Err(AttachmentValidationError::TooManyReactions {
                actual: 13,
                max: MAX_REACTIONS_PER_ATTACHMENT
            })
        );
    }

    #[test]
    fn validation_rejects_multiple_grapheme_like_reaction() {
        let set = ReactionSet::new(["👍👍"]);

        assert_eq!(
            set.validate(),
            Err(AttachmentValidationError::ReactionNotSingleGrapheme(
                "👍👍".to_owned()
            ))
        );
    }

    #[test]
    fn validation_accepts_zwj_emoji_as_single_grapheme() {
        let set = ReactionSet::new(["👩🏾‍❤️‍👩🏼"]);

        assert_eq!(set.validate(), Ok(()));
    }

    #[test]
    fn validation_rejects_ascii_word_as_multiple_graphemes() {
        let set = ReactionSet::new(["abc"]);

        assert_eq!(
            set.validate(),
            Err(AttachmentValidationError::ReactionNotSingleGrapheme(
                "abc".to_owned()
            ))
        );
    }
}
