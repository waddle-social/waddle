//! Waddle pin/unpin wire shape (`urn:waddle:pin:0`).
//!
//! Channel pinning is a Waddle-only feature — no XEP defines a "pin a
//! message" payload. We therefore use a project-local namespace
//! (`urn:waddle:pin:0`) rather than borrowing an official one with a
//! non-conformant shape (CLAUDE.md hard rule on XEP conformance).
//!
//! ## Inbound (member → MUC)
//!
//! ```xml
//! <message type="groupchat" to="room@conf.example">
//!   <pinned xmlns="urn:waddle:pin:0" target="<stanza-id>"/>
//! </message>
//! ```
//!
//! The same markers are also valid on 1:1 `<message type='chat'>`
//! stanzas for Waddle DM pair pinning.
//!
//! Or for unpin:
//!
//! ```xml
//! <message type="groupchat" to="room@conf.example">
//!   <unpinned xmlns="urn:waddle:pin:0" target="<stanza-id>"/>
//! </message>
//! ```
//!
//! ## Outbound system message (MUC → all occupants, archived in MAM)
//!
//! ```xml
//! <message type="groupchat" from="room@conf.example">
//!   <body>alice pinned a message</body>
//!   <pinned xmlns="urn:waddle:pin:0" target="<stanza-id>" by="<bare-jid>">
//!     <preview><author jid="…" nick="…"/><text>…</text><ts>…</ts></preview>
//!   </pinned>
//! </message>
//! ```
//!
//! `<unpinned/>` is symmetric and may carry an optional `reason="retracted"`
//! when the unpin was triggered by a XEP-0424 retraction cascade.

use minidom::Element;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

/// Waddle pin namespace (`urn:waddle:pin:0`).
pub const NS_WADDLE_PIN_V0: &str = "urn:waddle:pin:0";

const PINNED_ELEMENT: &str = "pinned";
const UNPINNED_ELEMENT: &str = "unpinned";
const TARGET_ATTR: &str = "target";

/// Maximum length of a target stanza-id in bytes. Stanza-ids are
/// XEP-0359 random tokens — even a UUID v4 fits in 36 bytes. Allow
/// generous headroom for archive-ids without admitting unbounded
/// abuse.
pub const MAX_TARGET_STANZA_ID_LEN: usize = 256;

/// Pin or unpin intent extracted from an inbound `<message>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinIntent {
    /// Pin the message identified by `target`.
    Pin { target: String },
    /// Unpin the message identified by `target`.
    Unpin { target: String },
}

impl PinIntent {
    /// The `target` stanza-id this intent refers to.
    pub fn target(&self) -> &str {
        match self {
            PinIntent::Pin { target } | PinIntent::Unpin { target } => target,
        }
    }
}

/// Detect a pin or unpin intent in the given message. Returns `None`
/// when no pin marker is present; returns `Some(intent)` for the first
/// well-formed marker found. Empty-string targets and overlong targets
/// are rejected (mapped to `None`) — callers may treat that as
/// `<bad-request/>` if needed.
///
/// Ambiguous shapes (both `<pinned>` and `<unpinned>` in the same
/// message) return `None` — the caller should reject as bad-request.
pub fn extract_pin_intent_from_message(message: &Message) -> Option<PinIntent> {
    let mut found: Option<PinIntent> = None;
    for elem in &message.payloads {
        if elem.ns() != NS_WADDLE_PIN_V0 {
            continue;
        }
        let candidate = match elem.name() {
            PINNED_ELEMENT => parse_pinned_element(elem),
            UNPINNED_ELEMENT => parse_unpinned_element(elem),
            _ => continue,
        }?;
        // Reject ambiguous: two markers, especially of differing kind,
        // are nonsensical and easier to refuse than to disambiguate.
        if found.is_some() {
            return None;
        }
        found = Some(candidate);
    }
    found
}

/// Parse a `<pinned target="…"/>` element. Returns `None` if the
/// element is not a pin marker, the target is missing or empty, or
/// the target exceeds [`MAX_TARGET_STANZA_ID_LEN`].
pub fn parse_pinned_element(elem: &Element) -> Option<PinIntent> {
    if elem.name() != PINNED_ELEMENT || elem.ns() != NS_WADDLE_PIN_V0 {
        return None;
    }
    let target = parse_target_attr(elem)?;
    Some(PinIntent::Pin { target })
}

/// Parse an `<unpinned target="…"/>` element. Same rules as
/// [`parse_pinned_element`].
pub fn parse_unpinned_element(elem: &Element) -> Option<PinIntent> {
    if elem.name() != UNPINNED_ELEMENT || elem.ns() != NS_WADDLE_PIN_V0 {
        return None;
    }
    let target = parse_target_attr(elem)?;
    Some(PinIntent::Unpin { target })
}

fn parse_target_attr(elem: &Element) -> Option<String> {
    let raw = elem.attr(TARGET_ATTR)?;
    if raw.is_empty() || raw.len() > MAX_TARGET_STANZA_ID_LEN {
        return None;
    }
    Some(raw.to_owned())
}

/// Build the inbound `<pinned target="…"/>` element. Used by tests and
/// by the chat-side wasm bindings when wrapping a pin request from the
/// client.
pub fn build_pinned_element(target: &StanzaId) -> Element {
    Element::builder(PINNED_ELEMENT, NS_WADDLE_PIN_V0)
        .attr(
            minidom::rxml::xml_ncname!("target").to_owned(),
            target.id.as_str(),
        )
        .build()
}

/// Build the inbound `<unpinned target="…"/>` element.
pub fn build_unpinned_element(target: &StanzaId) -> Element {
    Element::builder(UNPINNED_ELEMENT, NS_WADDLE_PIN_V0)
        .attr(
            minidom::rxml::xml_ncname!("target").to_owned(),
            target.id.as_str(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::{BareJid, Jid};
    use minidom::rxml::xml_ncname;
    use std::str::FromStr;
    use xmpp_parsers::message::MessageType;

    fn stanza_id(id: &str) -> StanzaId {
        StanzaId::new(
            id.to_owned(),
            Jid::from(BareJid::from_str("room@conf.example").expect("valid jid")),
        )
    }

    fn pin_message(target: &str) -> Message {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(build_pinned_element(&stanza_id(target)));
        msg
    }

    fn unpin_message(target: &str) -> Message {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads
            .push(build_unpinned_element(&stanza_id(target)));
        msg
    }

    #[test]
    fn ns_constant_is_waddle_pin_v0() {
        assert_eq!(NS_WADDLE_PIN_V0, "urn:waddle:pin:0");
    }

    #[test]
    fn extract_pin_intent_returns_pin_for_pinned_marker() {
        let intent = extract_pin_intent_from_message(&pin_message("stanza-1"));
        assert_eq!(
            intent,
            Some(PinIntent::Pin {
                target: "stanza-1".into()
            })
        );
    }

    #[test]
    fn extract_pin_intent_accepts_direct_chat_marker() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("bob@example.com").expect("valid jid"),
        )));
        msg.type_ = MessageType::Chat;
        msg.payloads.push(build_pinned_element(&stanza_id("dm-1")));
        assert_eq!(
            extract_pin_intent_from_message(&msg),
            Some(PinIntent::Pin {
                target: "dm-1".into()
            })
        );
    }

    #[test]
    fn extract_pin_intent_returns_unpin_for_unpinned_marker() {
        let intent = extract_pin_intent_from_message(&unpin_message("stanza-1"));
        assert_eq!(
            intent,
            Some(PinIntent::Unpin {
                target: "stanza-1".into()
            })
        );
    }

    #[test]
    fn extract_pin_intent_returns_none_when_marker_absent() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn extract_pin_intent_rejects_empty_target() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(
            Element::builder(PINNED_ELEMENT, NS_WADDLE_PIN_V0)
                .attr(xml_ncname!("target").to_owned(), "")
                .build(),
        );
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn extract_pin_intent_rejects_missing_target() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads
            .push(Element::builder(PINNED_ELEMENT, NS_WADDLE_PIN_V0).build());
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn extract_pin_intent_rejects_overlong_target() {
        let oversized = "x".repeat(MAX_TARGET_STANZA_ID_LEN + 1);
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(
            Element::builder(PINNED_ELEMENT, NS_WADDLE_PIN_V0)
                .attr(xml_ncname!("target").to_owned(), oversized.as_str())
                .build(),
        );
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn extract_pin_intent_rejects_ambiguous_pin_and_unpin() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(build_pinned_element(&stanza_id("a")));
        msg.payloads.push(build_unpinned_element(&stanza_id("b")));
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn extract_pin_intent_rejects_two_pinned_markers() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(build_pinned_element(&stanza_id("a")));
        msg.payloads.push(build_pinned_element(&stanza_id("b")));
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn extract_pin_intent_ignores_wrong_namespace() {
        let mut msg = Message::new(Some(Jid::from(
            BareJid::from_str("room@conf.example").expect("valid jid"),
        )));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(
            Element::builder(PINNED_ELEMENT, "wrong:ns")
                .attr(xml_ncname!("target").to_owned(), "stanza-1")
                .build(),
        );
        assert!(extract_pin_intent_from_message(&msg).is_none());
    }

    #[test]
    fn build_and_parse_pinned_roundtrip() {
        let elem = build_pinned_element(&stanza_id("stanza-1"));
        assert_eq!(elem.name(), PINNED_ELEMENT);
        assert_eq!(elem.ns(), NS_WADDLE_PIN_V0);
        assert_eq!(elem.attr(TARGET_ATTR), Some("stanza-1"));
        assert_eq!(
            parse_pinned_element(&elem),
            Some(PinIntent::Pin {
                target: "stanza-1".into()
            })
        );
    }

    #[test]
    fn build_and_parse_unpinned_roundtrip() {
        let elem = build_unpinned_element(&stanza_id("stanza-1"));
        assert_eq!(elem.name(), UNPINNED_ELEMENT);
        assert_eq!(elem.ns(), NS_WADDLE_PIN_V0);
        assert_eq!(
            parse_unpinned_element(&elem),
            Some(PinIntent::Unpin {
                target: "stanza-1".into()
            })
        );
    }

    #[test]
    fn pin_intent_target_accessor() {
        assert_eq!(
            PinIntent::Pin {
                target: "abc".into()
            }
            .target(),
            "abc"
        );
        assert_eq!(
            PinIntent::Unpin {
                target: "xyz".into()
            }
            .target(),
            "xyz"
        );
    }
}
