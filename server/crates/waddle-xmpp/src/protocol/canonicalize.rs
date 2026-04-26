//! XEP-0359 inbound canonicalization.
//!
//! A single point that prepares an inbound message for archiving and
//! broadcast. It always enforces the XEP-0359 strip rule (a generating entity
//! MUST delete any `<stanza-id/>` whose `by` matches its own scope) and, when
//! the message is of an archivable type, stamps a fresh `<stanza-id/>` whose
//! `by` is the archive owner — `account@server` for personal archives,
//! `room@muc.server` for MUC archives — per XEP-0359 § "the assigning entity
//! is the account / room".
//!
//! The same canonicalized message is what gets persisted to MAM *and* what
//! gets broadcast on the wire, so clients see one stable identifier across
//! both surfaces. Origin-ids are preserved untouched (XEP-0359 MUST).

use crate::xep::xep0359;
use jid::BareJid;
use uuid::Uuid;
use xmpp_parsers::message::{Message, MessageType};

/// Output of [`canonicalize`].
#[derive(Debug, Clone)]
pub struct Canonicalized {
    /// The canonicalized message — strip rule applied, plus a server-assigned
    /// `<stanza-id/>` when the message is of an archivable type.
    pub message: Message,
    /// `Some(uuid)` when the message was stamped (i.e. it is archivable),
    /// `None` for `headline` / `error` / bodyless `normal`.
    pub stanza_id: Option<String>,
}

/// Whether this message type carries content the server should archive.
///
/// Per the project decision aligned with XEP-0313 §4.1.1: archive `chat`,
/// `groupchat`, and `normal` messages that have a body. Skip `headline` and
/// `error` entirely.
pub fn should_archive(msg: &Message) -> bool {
    match msg.type_ {
        MessageType::Chat | MessageType::Groupchat => true,
        MessageType::Normal => has_non_empty_body(msg),
        MessageType::Headline | MessageType::Error => false,
    }
}

/// Canonicalize `msg` for the archive owned by `archive_owner`, generating a
/// fresh v7 UUID when stamping is needed.
pub fn canonicalize(msg: Message, archive_owner: &BareJid) -> Canonicalized {
    canonicalize_with_id(msg, archive_owner, || Uuid::now_v7().to_string())
}

/// Variant of [`canonicalize`] that lets callers inject the stanza-id value —
/// used by tests to make outputs deterministic.
pub fn canonicalize_with_id<F>(
    mut msg: Message,
    archive_owner: &BareJid,
    fresh_id: F,
) -> Canonicalized
where
    F: FnOnce() -> String,
{
    let by = archive_owner.to_string();
    xep0359::remove_stanza_ids_by(&mut msg, &by);

    let stanza_id = if should_archive(&msg) {
        let id = fresh_id();
        xep0359::add_stanza_id(&mut msg, &id, &by);
        Some(id)
    } else {
        None
    };

    Canonicalized {
        message: msg,
        stanza_id,
    }
}

fn has_non_empty_body(msg: &Message) -> bool {
    msg.bodies.values().any(|b| !b.0.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xmpp_parsers::message::{Body, MessageType};

    const SERVER: &str = "waddle.test";

    fn alice() -> BareJid {
        format!("alice@{SERVER}").parse().expect("valid bare jid")
    }

    fn bob() -> BareJid {
        format!("bob@{SERVER}").parse().expect("valid bare jid")
    }

    fn room() -> BareJid {
        format!("room@conference.{SERVER}")
            .parse()
            .expect("valid bare jid")
    }

    fn chat_with_body(text: &str) -> Message {
        let mut msg = Message::new(None);
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), Body(text.to_owned()));
        msg
    }

    fn groupchat_with_body(text: &str) -> Message {
        let mut msg = Message::new(None);
        msg.type_ = MessageType::Groupchat;
        msg.bodies.insert(String::new(), Body(text.to_owned()));
        msg
    }

    fn normal_without_body() -> Message {
        let mut msg = Message::new(None);
        msg.type_ = MessageType::Normal;
        msg
    }

    fn normal_with_body(text: &str) -> Message {
        let mut msg = Message::new(None);
        msg.type_ = MessageType::Normal;
        msg.bodies.insert(String::new(), Body(text.to_owned()));
        msg
    }

    fn headline_with_body(text: &str) -> Message {
        let mut msg = Message::new(None);
        msg.type_ = MessageType::Headline;
        msg.bodies.insert(String::new(), Body(text.to_owned()));
        msg
    }

    fn error_with_body(text: &str) -> Message {
        let mut msg = Message::new(None);
        msg.type_ = MessageType::Error;
        msg.bodies.insert(String::new(), Body(text.to_owned()));
        msg
    }

    fn forged_stanza_id_for(by: &BareJid) -> Element {
        Element::builder("stanza-id", xep0359::NS_SID)
            .attr("id", "forged-by-client")
            .attr("by", by.to_string())
            .build()
    }

    #[test]
    fn stamps_chat_with_archive_owner_by() {
        let result = canonicalize_with_id(chat_with_body("hi"), &alice(), || "uuid-1".into());

        assert_eq!(result.stanza_id.as_deref(), Some("uuid-1"));
        let ids = xep0359::extract_stanza_ids(&result.message);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "uuid-1");
        assert_eq!(ids[0].by, alice().to_string());
    }

    #[test]
    fn stamps_groupchat_with_room_by() {
        let result =
            canonicalize_with_id(groupchat_with_body("hello"), &room(), || "room-uuid".into());

        assert_eq!(result.stanza_id.as_deref(), Some("room-uuid"));
        let ids = xep0359::extract_stanza_ids(&result.message);
        assert_eq!(ids[0].by, room().to_string());
    }

    #[test]
    fn strips_forged_stanza_id_matching_owner_then_stamps_fresh() {
        let mut msg = chat_with_body("hi");
        msg.payloads.push(forged_stanza_id_for(&alice()));

        let result = canonicalize_with_id(msg, &alice(), || "fresh".into());

        let ids = xep0359::extract_stanza_ids(&result.message);
        assert_eq!(ids.len(), 1, "exactly one stanza-id remains");
        assert_eq!(ids[0].id, "fresh");
        assert_eq!(ids[0].by, alice().to_string());
    }

    #[test]
    fn preserves_stanza_ids_from_other_entities() {
        let mut msg = chat_with_body("hi");
        msg.payloads.push(forged_stanza_id_for(&bob()));

        let result = canonicalize_with_id(msg, &alice(), || "alice-id".into());

        let ids = xep0359::extract_stanza_ids(&result.message);
        assert_eq!(ids.len(), 2, "bob's stanza-id stays, alice's gets added");
        assert!(ids.iter().any(|s| s.by == bob().to_string()));
        assert!(ids.iter().any(|s| s.by == alice().to_string()));
    }

    #[test]
    fn preserves_origin_id() {
        let mut msg = chat_with_body("hi");
        xep0359::add_origin_id(&mut msg, "client-origin");

        let result = canonicalize_with_id(msg, &alice(), || "uuid".into());

        assert_eq!(
            xep0359::extract_origin_id_str(&result.message).as_deref(),
            Some("client-origin")
        );
    }

    #[test]
    fn skips_stamp_for_headline_but_still_strips() {
        let mut msg = headline_with_body("notification");
        msg.payloads.push(forged_stanza_id_for(&alice()));

        let result = canonicalize_with_id(msg, &alice(), || {
            panic!("should not be called for headline")
        });

        assert!(result.stanza_id.is_none());
        assert!(
            xep0359::extract_stanza_ids(&result.message).is_empty(),
            "forged stanza-id was stripped even though we did not re-stamp"
        );
    }

    #[test]
    fn skips_stamp_for_error_but_still_strips() {
        let mut msg = error_with_body("err");
        msg.payloads.push(forged_stanza_id_for(&alice()));

        let result = canonicalize_with_id(msg, &alice(), || panic!("should not stamp"));

        assert!(result.stanza_id.is_none());
        assert!(xep0359::extract_stanza_ids(&result.message).is_empty());
    }

    #[test]
    fn skips_normal_without_body() {
        let result = canonicalize_with_id(normal_without_body(), &alice(), || {
            panic!("should not stamp")
        });

        assert!(result.stanza_id.is_none());
    }

    #[test]
    fn stamps_normal_with_body() {
        let result =
            canonicalize_with_id(normal_with_body("system"), &alice(), || "norm-id".into());

        assert_eq!(result.stanza_id.as_deref(), Some("norm-id"));
    }

    #[test]
    fn should_archive_classification() {
        assert!(should_archive(&chat_with_body("x")));
        assert!(should_archive(&groupchat_with_body("x")));
        assert!(should_archive(&normal_with_body("x")));
        assert!(!should_archive(&normal_without_body()));
        assert!(!should_archive(&headline_with_body("x")));
        assert!(!should_archive(&error_with_body("x")));
    }
}
