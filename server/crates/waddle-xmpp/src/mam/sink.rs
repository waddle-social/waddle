//! Storage-boundary conversion from typed [`Message`] to [`ArchivedMessage`].
//!
//! Per the project's typed-payloads rule, protocol values flow through the
//! sans-I/O state machine and handlers as typed Rust values. This module is
//! the *single* serialization seam where a typed `Message` is decomposed
//! into [`ArchivedMessage`]'s string-shaped storage form (the SQL row
//! schema). Anywhere else that constructs an `ArchivedMessage` literal is a
//! historical artefact tracked under #228; new archive writes go through
//! [`message_to_archived`].

use chrono::{DateTime, Utc};
use jid::BareJid;
use xmpp_parsers::message::{Message, MessageType};

use crate::mam::ArchivedMessage;
use crate::parser::message_to_string;
use crate::xep::{xep0359, xep0461};

/// Convert a fully-canonicalized message into its archive row.
///
/// `archive_id` MUST equal the `<stanza-id by=$archive_owner/>` value the
/// canonicalizer stamped — both fields end up populated to that same value
/// for cross-row lookup symmetry.
pub fn message_to_archived(
    message: &Message,
    archive_owner: &BareJid,
    archive_id: &str,
    timestamp: DateTime<Utc>,
) -> ArchivedMessage {
    let from = message
        .from
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();

    let to = message
        .to
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| archive_owner.to_string());

    let body = message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(|b| b.0.clone())
        .unwrap_or_default();

    let reply = xep0461::parse_reply_from_message(message);

    ArchivedMessage {
        id: archive_id.to_owned(),
        timestamp,
        from,
        to,
        body,
        stanza_id: Some(archive_id.to_owned()),
        thread_id: xep0461::thread_id_from_message(message),
        reply_to_id: reply.as_ref().map(|r| r.id.clone()),
        reply_to_jid: reply.and_then(|r| r.to),
        origin_id: xep0359::extract_origin_id_str(message),
        message_type: message_type_to_str(&message.type_).to_owned(),
        stanza_xml: message_to_string(message).ok(),
    }
}

fn message_type_to_str(t: &MessageType) -> &'static str {
    match t {
        MessageType::Chat => "chat",
        MessageType::Groupchat => "groupchat",
        MessageType::Normal => "normal",
        MessageType::Headline => "headline",
        MessageType::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use xmpp_parsers::jid::Jid;
    use xmpp_parsers::message::Body;

    fn alice_bare() -> BareJid {
        "alice@waddle.test".parse().expect("valid bare jid")
    }

    fn bob_bare() -> BareJid {
        "bob@waddle.test".parse().expect("valid bare jid")
    }

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 26, 12, 0, 0).unwrap()
    }

    #[test]
    fn maps_typed_chat_to_archive_row() {
        let mut msg = Message::new(Some(Jid::from(bob_bare())));
        msg.from = Some(Jid::from(alice_bare()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), Body("hi bob".into()));
        xep0359::add_origin_id(&mut msg, "client-origin-1");

        let row = message_to_archived(&msg, &bob_bare(), "archive-uuid-1", ts());

        assert_eq!(row.id, "archive-uuid-1");
        assert_eq!(row.stanza_id.as_deref(), Some("archive-uuid-1"));
        assert_eq!(row.from, "alice@waddle.test");
        assert_eq!(row.to, "bob@waddle.test");
        assert_eq!(row.body, "hi bob");
        assert_eq!(row.origin_id.as_deref(), Some("client-origin-1"));
        assert_eq!(row.message_type, "chat");
        assert!(row.stanza_xml.is_some());
    }

    #[test]
    fn falls_back_to_archive_owner_when_to_is_missing() {
        let mut msg = Message::new(None);
        msg.from = Some(Jid::from(alice_bare()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), Body("self note".into()));

        let row = message_to_archived(&msg, &alice_bare(), "id", ts());

        assert_eq!(row.to, "alice@waddle.test");
    }

    #[test]
    fn extracts_reply_reference() {
        let mut msg = Message::new(Some(Jid::from(bob_bare())));
        msg.from = Some(Jid::from(alice_bare()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), Body("ack".into()));
        xep0461::set_reply_payload(
            &mut msg,
            &xep0461::ReplyReference::new("orig-1").with_to("bob@waddle.test"),
        );

        let row = message_to_archived(&msg, &bob_bare(), "id", ts());

        assert_eq!(row.reply_to_id.as_deref(), Some("orig-1"));
        assert_eq!(row.reply_to_jid.as_deref(), Some("bob@waddle.test"));
    }
}
