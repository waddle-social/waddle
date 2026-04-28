//! Typed projection from a wire-shape [`Message`] to the persistent
//! [`ArchivedMessage`] shape MAM storage expects.
//!
//! Used by the sans-I/O dispatcher's storage interpreter
//! ([`OutboundEvent::ArchiveDirect`]) — keeps the conversion in one
//! place so handler-emitted typed events are projected consistently
//! whether the call site is the legacy `message.rs` path (until #229
//! PR9 cuts over) or the dispatcher.
//!
//! Archive eligibility (XEP-0313 §5.1.3 + XEP-0334 hint precedence) is
//! enforced by [`super::super::protocol::handlers::archive::ArchiveHandler`]
//! upstream; this module is a pure data projection and assumes the
//! caller already vetted the message.
//!
//! [`OutboundEvent::ArchiveDirect`]: super::super::protocol::event::OutboundEvent::ArchiveDirect

use chrono::Utc;
use xmpp_parsers::message::{Message, MessageType};

use super::{
    ArchivedMention, ArchivedMessage, ArchivedReactionSet, ArchivedReference, ArchivedReply,
    ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
    STANZA_ID_NS,
};
use crate::parser::message_to_string;
use crate::xep::{
    extract_correction_from_message, extract_explicit_mentions, extract_reactions_from_message,
    extract_references_from_message, extract_retraction_from_message, parse_reply_from_message,
    RetractionKind, NS_REPLY,
};

/// Build the [`ArchivedMessage`] persisted form of a one-to-one
/// (chat / normal) `<message/>` for direct MAM storage.
///
/// `from` and `to` are the canonical bare-JID tuple for the archive
/// row — the caller (interpreter) supplies them so the projection
/// does not duplicate the pass-aware logic that lives in the
/// [`super::super::protocol::handlers::archive::ArchiveHandler`]. The
/// `id` field is left empty: storage assigns the row id at write time.
pub fn build_direct_archived_message(from: &str, to: &str, message: &Message) -> ArchivedMessage {
    let body = prototype_body(message)
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);
    let rich = rich_archive_payload(message);
    let stanza_xml = serialize_message_xml(message);

    ArchivedMessage {
        id: String::new(),
        timestamp: Utc::now(),
        from: from.to_string(),
        to: to.to_string(),
        body,
        stanza_id: message.id.clone(),
        thread_id: message.thread.as_ref().map(|thread| thread.0.clone()),
        reply_to_id,
        reply_to_jid,
        origin_id,
        message_type: mam_message_type(&message.type_),
        stanza_xml,
        rich,
        nickname_generation: None,
    }
}

fn mam_message_type(message_type: &MessageType) -> String {
    match message_type {
        MessageType::Chat => "chat".to_string(),
        MessageType::Error => "error".to_string(),
        MessageType::Groupchat => "groupchat".to_string(),
        MessageType::Headline => "headline".to_string(),
        MessageType::Normal => "normal".to_string(),
    }
}

fn prototype_body(message: &Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(|body| body.0.clone())
}

fn extract_origin_id(message: &Message) -> Option<String> {
    message
        .payloads
        .iter()
        .find(|payload| payload.name() == "origin-id" && payload.ns() == STANZA_ID_NS)
        .and_then(|origin| origin.attr("id").map(ToOwned::to_owned))
}

fn extract_reply_reference(message: &Message) -> (Option<String>, Option<String>) {
    let Some(reply) = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == NS_REPLY)
    else {
        return (None, None);
    };
    (
        reply.attr("id").map(ToOwned::to_owned),
        reply.attr("to").map(ToOwned::to_owned),
    )
}

fn serialize_message_xml(message: &Message) -> Option<String> {
    message_to_string(message).ok()
}

fn rich_archive_payload(message: &Message) -> Option<ArchivedRichMessage> {
    let payload = extract_correction_from_message(message)
        .and_then(|correction| {
            RichMessageId::new(correction.replaces_id)
                .map(|replaces_id| ArchivedRichPayload::Correction { replaces_id })
        })
        .or_else(|| {
            extract_retraction_from_message(message).and_then(|kind| match kind {
                RetractionKind::Request(retraction) => RichMessageId::new(retraction.retracts_id)
                    .map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: None,
                            retraction_id: message.id.clone().and_then(RichMessageId::new),
                        })
                    }),
                RetractionKind::Tombstone(retracted) => message.id.clone().and_then(|id| {
                    RichMessageId::new(id).map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: chrono::DateTime::parse_from_rfc3339(&retracted.stamp)
                                .ok()
                                .map(|stamp| stamp.with_timezone(&Utc)),
                            retraction_id: None,
                        })
                    })
                }),
            })
        })
        .or_else(|| {
            extract_reactions_from_message(message).and_then(|reactions| {
                RichMessageId::new(reactions.message_id).map(|target_id| {
                    ArchivedRichPayload::Reactions(ArchivedReactionSet {
                        target_id,
                        emojis: reactions
                            .emojis
                            .into_iter()
                            .filter_map(RichText::new)
                            .collect(),
                    })
                })
            })
        });

    let reply = parse_reply_from_message(message).and_then(|reply| {
        RichMessageId::new(reply.id).map(|id| ArchivedReply {
            id,
            to: reply.to.and_then(|to| to.parse().ok()),
        })
    });

    let references = extract_references_from_message(message)
        .into_iter()
        .filter_map(|reference| {
            let ref_type = RichText::new(reference.ref_type.as_str())?;
            Some(ArchivedReference {
                ref_type,
                begin: reference.begin.and_then(|value| value.try_into().ok()),
                end: reference.end.and_then(|value| value.try_into().ok()),
                uri: reference.uri.and_then(RichText::new),
                anchor: reference.anchor.and_then(RichText::new),
            })
        })
        .collect::<Vec<_>>();

    let mentions = extract_explicit_mentions(message)
        .map(|mentions| {
            mentions
                .mentions
                .into_iter()
                .map(|mention| ArchivedMention {
                    begin: mention.begin,
                    end: mention.end,
                    jid: mention.jid,
                    occupant_id: mention.occupant_id.and_then(RichText::new),
                    mentions: mention.mentions.and_then(RichText::new),
                    uri: mention.uri.and_then(RichText::new),
                    active: mention.active,
                    noping: mention.noping,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if payload.is_none() && reply.is_none() && references.is_empty() && mentions.is_empty() {
        None
    } else {
        Some(ArchivedRichMessage {
            payload,
            reply,
            references,
            mentions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Body, MessageType};

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m.id = Some("orig-1".to_string());
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    #[test]
    fn xep_0313_projects_canonical_fields_for_direct_chat() {
        let msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let archived = build_direct_archived_message("alice@example.com", "bob@example.com", &msg);
        assert!(archived.id.is_empty(), "id assigned by storage at write");
        assert_eq!(archived.from, "alice@example.com");
        assert_eq!(archived.to, "bob@example.com");
        assert_eq!(archived.body, "hi");
        assert_eq!(archived.stanza_id.as_deref(), Some("orig-1"));
        assert_eq!(archived.message_type, "chat");
        assert!(archived.rich.is_none());
        assert!(
            archived.stanza_xml.is_some(),
            "stanza_xml is serialized for replay fidelity"
        );
    }

    #[test]
    fn xep_0313_projects_correction_into_rich_payload() {
        use crate::xep::xep0308::build_correction_message;
        let to: jid::Jid = "bob@example.com".parse().expect("jid");
        let from: jid::Jid = "alice@example.com/web".parse().expect("jid");
        let msg = build_correction_message(Some(to), Some(from), "fixed text", "old-id");
        let archived = build_direct_archived_message("alice@example.com", "bob@example.com", &msg);
        let rich = archived.rich.expect("correction projects rich payload");
        match rich.payload {
            Some(ArchivedRichPayload::Correction { replaces_id }) => {
                assert_eq!(replaces_id.as_str(), "old-id");
            }
            other => panic!("expected Correction, got {other:?}"),
        }
    }
}
