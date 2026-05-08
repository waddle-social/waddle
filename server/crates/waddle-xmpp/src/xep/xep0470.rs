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
//!   <attachments xmlns='urn:xmpp:pubsub-attachments:0'
//!                for='urn:xmpp:pubsub-social-feed:0'
//!                item='post-123'>
//!     <reaction xmlns='urn:xmpp:reactions:0'>👍</reaction>
//!   </attachments>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - React to social feed posts
//! - Comment threads on announcements
//! - Vote/poll responses

use minidom::Element;

/// Namespace for XEP-0470 Pubsub Attachments.
pub const NS_PUBSUB_ATTACHMENTS: &str = "urn:xmpp:pubsub-attachments:0";

/// Waddle pin attachment namespace. The marker element `<pinned/>` is published
/// as an attachment child to indicate the targeted message is pinned in its
/// host MUC. Used because no XEP defines a "pin" payload — `urn:waddle:pin:0`
/// is the project's custom namespace per the XEP conformance hard rule.
pub const NS_WADDLE_PIN_V0: &str = "urn:waddle:pin:0";

/// An attachment target: which PubSub item this attaches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTarget {
    /// The PubSub node (namespace) of the target item.
    pub node: String,
    /// The item ID being attached to.
    pub item_id: String,
}

impl AttachmentTarget {
    /// Create a new attachment target.
    pub fn new(node: impl Into<String>, item_id: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            item_id: item_id.into(),
        }
    }
}

/// An attachment payload type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentPayload {
    /// A text comment.
    Comment(String),
    /// A reaction emoji.
    Reaction(String),
    /// A pin marker (`urn:waddle:pin:0` `<pinned/>`). Carries no inline data;
    /// pin metadata (pinner, timestamp, preview) is held in the room-level
    /// projection list, not in the per-message attachment.
    Pin,
    /// An unpin marker (`urn:waddle:pin:0` `<unpinned/>`). Symmetric to
    /// `Pin` — used to clear an existing pin on the targeted message.
    Unpin,
    /// A generic XML payload.
    Custom(Element),
}

/// A pubsub attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// What this attaches to.
    pub target: AttachmentTarget,
    /// The attachment content.
    pub payload: AttachmentPayload,
    /// Who created this attachment.
    pub author: Option<String>,
}

impl Attachment {
    /// Create a comment attachment.
    pub fn comment(target: AttachmentTarget, text: impl Into<String>) -> Self {
        Self {
            target,
            payload: AttachmentPayload::Comment(text.into()),
            author: None,
        }
    }

    /// Create a reaction attachment.
    pub fn reaction(target: AttachmentTarget, emoji: impl Into<String>) -> Self {
        Self {
            target,
            payload: AttachmentPayload::Reaction(emoji.into()),
            author: None,
        }
    }

    /// Create a pin attachment marker.
    pub fn pin(target: AttachmentTarget) -> Self {
        Self {
            target,
            payload: AttachmentPayload::Pin,
            author: None,
        }
    }

    /// Create an unpin attachment marker.
    pub fn unpin(target: AttachmentTarget) -> Self {
        Self {
            target,
            payload: AttachmentPayload::Unpin,
            author: None,
        }
    }

    /// Set the author.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Returns `true` if this is a comment.
    pub fn is_comment(&self) -> bool {
        matches!(self.payload, AttachmentPayload::Comment(_))
    }

    /// Returns `true` if this is a reaction.
    pub fn is_reaction(&self) -> bool {
        matches!(self.payload, AttachmentPayload::Reaction(_))
    }

    /// Returns `true` if this is a pin marker.
    pub fn is_pin(&self) -> bool {
        matches!(self.payload, AttachmentPayload::Pin)
    }

    /// Returns `true` if this is an unpin marker.
    pub fn is_unpin(&self) -> bool {
        matches!(self.payload, AttachmentPayload::Unpin)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is an `<attachments/>` element.
pub fn is_attachments_element(elem: &Element) -> bool {
    elem.ns() == NS_PUBSUB_ATTACHMENTS && elem.name() == "attachments"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse an attachment target from an `<attachments/>` element.
pub fn parse_attachment_target(elem: &Element) -> Option<AttachmentTarget> {
    if !is_attachments_element(elem) {
        return None;
    }
    let node = elem.attr("for").filter(|s| !s.is_empty())?.to_owned();
    let item_id = elem.attr("item").filter(|s| !s.is_empty())?.to_owned();
    Some(AttachmentTarget::new(node, item_id))
}

/// Returns `true` if the given `<attachments/>` element carries a Waddle pin
/// marker (`<pinned xmlns="urn:waddle:pin:0"/>`) as a direct child.
pub fn has_pin_marker(elem: &Element) -> bool {
    if !is_attachments_element(elem) {
        return false;
    }
    elem.children()
        .any(|child| child.name() == "pinned" && child.ns() == NS_WADDLE_PIN_V0)
}

/// Returns `true` if the given `<attachments/>` element carries a Waddle unpin
/// marker (`<unpinned xmlns="urn:waddle:pin:0"/>`) as a direct child.
pub fn has_unpin_marker(elem: &Element) -> bool {
    if !is_attachments_element(elem) {
        return false;
    }
    elem.children()
        .any(|child| child.name() == "unpinned" && child.ns() == NS_WADDLE_PIN_V0)
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<attachments/>` element.
pub fn build_attachments_element(attachment: &Attachment) -> Element {
    let mut elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS)
        .attr("for", attachment.target.node.as_str())
        .attr("item", attachment.target.item_id.as_str())
        .build();

    match &attachment.payload {
        AttachmentPayload::Comment(text) => {
            let mut comment = Element::builder("comment", NS_PUBSUB_ATTACHMENTS).build();
            comment.append_text_node(text);
            if let Some(ref author) = attachment.author {
                comment.set_attr("author", author);
            }
            elem.append_child(comment);
        }
        AttachmentPayload::Reaction(emoji) => {
            let mut reaction = Element::builder("reaction", NS_PUBSUB_ATTACHMENTS).build();
            reaction.append_text_node(emoji);
            elem.append_child(reaction);
        }
        AttachmentPayload::Pin => {
            elem.append_child(Element::builder("pinned", NS_WADDLE_PIN_V0).build());
        }
        AttachmentPayload::Unpin => {
            elem.append_child(Element::builder("unpinned", NS_WADDLE_PIN_V0).build());
        }
        AttachmentPayload::Custom(child) => {
            elem.append_child(child.clone());
        }
    }

    elem
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
        let elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS)
            .attr("for", "urn:xmpp:pubsub-social-feed:0")
            .attr("item", "post-123")
            .build();

        let target = parse_attachment_target(&elem).expect("parseable");
        assert_eq!(target.node, "urn:xmpp:pubsub-social-feed:0");
        assert_eq!(target.item_id, "post-123");
    }

    #[test]
    fn test_parse_target_missing_attrs() {
        let elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS).build();
        assert!(parse_attachment_target(&elem).is_none());
    }

    #[test]
    fn test_build_comment() {
        let target = AttachmentTarget::new("feed:0", "post-1");
        let attachment =
            Attachment::comment(target, "Great post!").with_author("alice@example.com");
        let elem = build_attachments_element(&attachment);

        assert_eq!(elem.name(), "attachments");
        assert_eq!(elem.attr("for"), Some("feed:0"));
        assert_eq!(elem.attr("item"), Some("post-1"));

        let comment = elem.children().next().expect("has child");
        assert_eq!(comment.name(), "comment");
        assert_eq!(comment.text(), "Great post!");
        assert_eq!(comment.attr("author"), Some("alice@example.com"));
    }

    #[test]
    fn test_build_reaction() {
        let target = AttachmentTarget::new("feed:0", "post-1");
        let attachment = Attachment::reaction(target, "👍");
        let elem = build_attachments_element(&attachment);

        let reaction = elem.children().next().expect("has child");
        assert_eq!(reaction.name(), "reaction");
        assert_eq!(reaction.text(), "👍");
    }

    #[test]
    fn test_attachment_helpers() {
        let target = AttachmentTarget::new("n", "i");
        assert!(Attachment::comment(target.clone(), "hi").is_comment());
        assert!(!Attachment::comment(target.clone(), "hi").is_reaction());
        assert!(Attachment::reaction(target.clone(), "👍").is_reaction());
        assert!(!Attachment::reaction(target.clone(), "👍").is_comment());
        assert!(Attachment::pin(target.clone()).is_pin());
        assert!(!Attachment::pin(target.clone()).is_comment());
        assert!(!Attachment::pin(target.clone()).is_reaction());
        assert!(Attachment::unpin(target.clone()).is_unpin());
        assert!(!Attachment::unpin(target.clone()).is_pin());
        assert!(!Attachment::pin(target).is_unpin());
    }

    #[test]
    fn test_build_unpin_marker_and_detect() {
        let target = AttachmentTarget::new("urn:xmpp:mam:2", "stanza-abc");
        let elem = build_attachments_element(&Attachment::unpin(target));
        assert!(has_unpin_marker(&elem));
        assert!(!has_pin_marker(&elem));
        let unpinned = elem.children().next().expect("unpinned child");
        assert_eq!(unpinned.name(), "unpinned");
        assert_eq!(unpinned.ns(), NS_WADDLE_PIN_V0);
    }

    #[test]
    fn test_pin_and_unpin_are_distinct_markers() {
        let target = AttachmentTarget::new("urn:xmpp:mam:2", "stanza-abc");
        let pin_elem = build_attachments_element(&Attachment::pin(target.clone()));
        let unpin_elem = build_attachments_element(&Attachment::unpin(target));
        assert!(has_pin_marker(&pin_elem));
        assert!(!has_unpin_marker(&pin_elem));
        assert!(has_unpin_marker(&unpin_elem));
        assert!(!has_pin_marker(&unpin_elem));
    }

    #[test]
    fn test_pin_namespace_constant() {
        assert_eq!(NS_WADDLE_PIN_V0, "urn:waddle:pin:0");
    }

    #[test]
    fn test_build_pin_marker() {
        let target = AttachmentTarget::new("urn:xmpp:mam:2", "stanza-abc");
        let attachment = Attachment::pin(target).with_author("alice@waddle.example");
        let elem = build_attachments_element(&attachment);

        assert_eq!(elem.name(), "attachments");
        assert_eq!(elem.attr("for"), Some("urn:xmpp:mam:2"));
        assert_eq!(elem.attr("item"), Some("stanza-abc"));

        let pinned = elem.children().next().expect("pinned child");
        assert_eq!(pinned.name(), "pinned");
        assert_eq!(pinned.ns(), NS_WADDLE_PIN_V0);
        assert!(pinned.text().is_empty());
    }

    #[test]
    fn test_has_pin_marker_positive() {
        let target = AttachmentTarget::new("urn:xmpp:mam:2", "stanza-abc");
        let elem = build_attachments_element(&Attachment::pin(target));
        assert!(has_pin_marker(&elem));
    }

    #[test]
    fn test_has_pin_marker_negative_for_reaction() {
        let target = AttachmentTarget::new("urn:xmpp:mam:2", "stanza-abc");
        let elem = build_attachments_element(&Attachment::reaction(target, "👍"));
        assert!(!has_pin_marker(&elem));
    }

    #[test]
    fn test_has_pin_marker_rejects_wrong_namespace() {
        let mut elem = Element::builder("attachments", NS_PUBSUB_ATTACHMENTS)
            .attr("for", "urn:xmpp:mam:2")
            .attr("item", "stanza-abc")
            .build();
        elem.append_child(Element::builder("pinned", "wrong:ns").build());
        assert!(!has_pin_marker(&elem));
    }

    #[test]
    fn test_has_pin_marker_rejects_non_attachments_root() {
        let mut elem = Element::builder("other", NS_PUBSUB_ATTACHMENTS).build();
        elem.append_child(Element::builder("pinned", NS_WADDLE_PIN_V0).build());
        assert!(!has_pin_marker(&elem));
    }

    #[test]
    fn test_attachment_target_new() {
        let t = AttachmentTarget::new("node", "item");
        assert_eq!(t.node, "node");
        assert_eq!(t.item_id, "item");
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
        assert_eq!(NS_PUBSUB_ATTACHMENTS, "urn:xmpp:pubsub-attachments:0");
    }

    #[test]
    fn test_attachment_with_author() {
        let target = AttachmentTarget::new("n", "i");
        let a = Attachment::comment(target, "hi").with_author("bob@example.com");
        assert_eq!(a.author.as_deref(), Some("bob@example.com"));
    }

    #[test]
    fn test_build_and_parse_roundtrip() {
        let target = AttachmentTarget::new("feed:0", "post-99");
        let attachment = Attachment::comment(target, "Round trip!");
        let elem = build_attachments_element(&attachment);
        let parsed = parse_attachment_target(&elem).expect("parseable");
        assert_eq!(parsed.node, "feed:0");
        assert_eq!(parsed.item_id, "post-99");
    }
}
