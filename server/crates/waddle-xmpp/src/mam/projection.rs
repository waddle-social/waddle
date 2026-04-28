//! Typed projection from a wire-shape [`Message`] to the persistent
//! [`ArchivedMessage`] shape MAM storage expects.
//!
//! Used by the sans-I/O dispatcher's storage interpreter
//! ([`OutboundEvent::ArchiveDirect`]) to keep the conversion in one
//! place so dispatcher-emitted typed archive events are projected
//! consistently before persistence. The legacy `message.rs` path
//! still owns its own copies of these helpers until #229 PR9 cuts
//! over and deletes them; this module supersedes that code.
//!
//! Archive eligibility (XEP-0313 §5.1.3 + XEP-0334 hint precedence) is
//! enforced by [`super::super::protocol::handlers::archive::ArchiveHandler`]
//! upstream; this module is a pure data projection and assumes the
//! caller already vetted the message.
//!
//! [`OutboundEvent::ArchiveDirect`]: super::super::protocol::event::OutboundEvent::ArchiveDirect

use chrono::Utc;
use tracing::warn;
use xmpp_parsers::message::{Message, MessageType};

use super::{
    ArchivedMention, ArchivedMessage, ArchivedReactionSet, ArchivedReference, ArchivedReply,
    ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
    STANZA_ID_NS,
};
use crate::parser::message_to_string;
use crate::xep::xep0359::extract_stanza_id_by;
use crate::xep::{
    extract_correction_from_message, extract_explicit_mentions, extract_reactions_from_message,
    extract_references_from_message, extract_retraction_from_message, parse_reply_from_message,
    RetractionKind, NS_REPLY,
};

/// Build the [`ArchivedMessage`] persisted form of a one-to-one
/// (chat / normal) `<message/>` for direct MAM storage.
///
/// `archive_jid` identifies whose personal archive this row belongs
/// to and is used to look up the canonical XEP-0359
/// `<stanza-id by='archive_jid' id='…'/>` stamp that
/// [`super::super::protocol::handlers::canonicalize::CanonicalizeHandler`]
/// stamped upstream. That id is then used as **both** the storage
/// primary key (`ArchivedMessage.id`) and the canonical lookup field
/// (`ArchivedMessage.stanza_id`) so:
///
/// - inbox `archive_ref` (also sourced from the same XEP-0359 stamp)
///   pivots cleanly to the MAM row by id, and
/// - [`super::storage::MamStorage::get_message_by_stanza_id`] resolves
///   that same id against `archive_jid`.
///
/// `from` and `to` are the canonical bare-JID tuple for the archive
/// row — the caller (interpreter) supplies them so the projection
/// does not duplicate the pass-aware logic that lives in
/// [`super::super::protocol::handlers::archive::ArchiveHandler`].
///
/// If the canonical stamp is missing (test fixture or misconfigured
/// chain), the row falls back to an empty `id` (the storage backend
/// will generate one) and `stanza_id` is taken from the wire
/// `<message id=...>` for legacy parity. Production paths always run
/// `CanonicalizeHandler` before `ArchiveHandler` per the locked
/// Q2(a) order, so this fallback is defensive only.
pub fn build_direct_archived_message(
    archive_jid: &str,
    from: &str,
    to: &str,
    message: &Message,
) -> ArchivedMessage {
    let body = prototype_body(message)
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let (reply_to_id, reply_to_jid) = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);
    let rich = rich_archive_payload(message);
    let stanza_xml = serialize_message_xml(message);

    // Storage shape mirrors the legacy `archive_direct_message`:
    // `id` is the canonical XEP-0359 stamp (the archive's primary
    // key for MAM lookups), and `stanza_id` carries the message's
    // wire `id` attribute. XEP-0424 retraction lookups query the
    // `stanza_id` column via `get_message_by_message_id`, and
    // (per `get_message_by_stanza_id`) the XEP-0359 stamp is
    // already keyed by `id`, so both retract-by-wire-id and
    // retract-by-canonical-stamp resolve correctly. Falling back to
    // the wire id when the canonical stamp is missing matches the
    // legacy primary-key shape.
    let canonical_stamp = extract_stanza_id_by(message, archive_jid);
    let id = canonical_stamp.unwrap_or_default();

    ArchivedMessage {
        id,
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
    match message_to_string(message) {
        Ok(xml) => Some(xml),
        Err(error) => {
            // Replay fidelity is degraded if we can't capture the wire
            // form, but the archive row still persists with body and
            // typed metadata; warn-log so serializer regressions don't
            // hide behind a silent `None`.
            warn!(
                message_id = message.id.as_deref().unwrap_or(""),
                %error,
                "MAM projection: failed to serialize message XML; storing without stanza_xml"
            );
            None
        }
    }
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
        // No XEP-0359 stamp on the message — defensive fallback path.
        let msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let archived = build_direct_archived_message(
            "alice@example.com",
            "alice@example.com",
            "bob@example.com",
            &msg,
        );
        assert!(
            archived.id.is_empty(),
            "no canonical stamp -> storage assigns id at write"
        );
        assert_eq!(archived.from, "alice@example.com");
        assert_eq!(archived.to, "bob@example.com");
        assert_eq!(archived.body, "hi");
        assert_eq!(
            archived.stanza_id.as_deref(),
            Some("orig-1"),
            "fallback stanza_id is the wire <message id=...>"
        );
        assert_eq!(archived.message_type, "chat");
        assert!(archived.rich.is_none());
        assert!(
            archived.stanza_xml.is_some(),
            "stanza_xml is serialized for replay fidelity"
        );
    }

    #[test]
    fn xep_0359_canonical_stamp_drives_archive_id_and_stanza_id() {
        // Production path: CanonicalizeHandler stamps a fresh
        // `<stanza-id by='alice@example.com' id='canon-1'/>` on the
        // message before ArchiveHandler emits ArchiveDirect. The
        // projection uses that id as the storage primary key (`id`).
        // `stanza_id` mirrors the original wire `id` attribute so
        // XEP-0424 retract lookups (which historically resolved
        // against the wire id, and which the dispatcher path's
        // `MessageRef::StanzaId` arm queries via
        // `get_message_by_message_id`) keep working. Inbox->MAM
        // pivot uses the canonical stamp via `id`.
        use crate::xep::xep0359::build_stanza_id_element;
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        msg.id = Some("wire-id-1".to_string());
        msg.payloads
            .push(build_stanza_id_element("canon-1", "alice@example.com"));
        let archived = build_direct_archived_message(
            "alice@example.com",
            "alice@example.com",
            "bob@example.com",
            &msg,
        );
        assert_eq!(
            archived.id, "canon-1",
            "canonical XEP-0359 stamp becomes the storage primary key"
        );
        assert_eq!(
            archived.stanza_id.as_deref(),
            Some("wire-id-1"),
            "stanza_id mirrors the wire id attribute for retraction-by-wire-id lookups"
        );
    }

    #[test]
    fn xep_0359_canonical_stamp_picks_archive_jid_specific_stamp() {
        // Recipient pass: the message carries Alice's stamp AND Bob's
        // stamp. The projection picks the one matching `archive_jid`
        // — Bob's, since this is Bob's archive write — and uses it
        // as the storage primary key. `stanza_id` continues to track
        // the wire `id` attribute for legacy-style retraction lookups.
        use crate::xep::xep0359::build_stanza_id_element;
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        msg.id = Some("wire-id-2".to_string());
        msg.payloads
            .push(build_stanza_id_element("alice-A1", "alice@example.com"));
        msg.payloads
            .push(build_stanza_id_element("bob-B1", "bob@example.com"));
        let archived = build_direct_archived_message(
            "bob@example.com",
            "alice@example.com",
            "bob@example.com",
            &msg,
        );
        assert_eq!(archived.id, "bob-B1");
        assert_eq!(archived.stanza_id.as_deref(), Some("wire-id-2"));
    }

    #[test]
    fn xep_0313_projects_correction_into_rich_payload() {
        use crate::xep::xep0308::build_correction_message;
        let to: jid::Jid = "bob@example.com".parse().expect("jid");
        let from: jid::Jid = "alice@example.com/web".parse().expect("jid");
        let msg = build_correction_message(Some(to), Some(from), "fixed text", "old-id");
        let archived = build_direct_archived_message(
            "alice@example.com",
            "alice@example.com",
            "bob@example.com",
            &msg,
        );
        let rich = archived.rich.expect("correction projects rich payload");
        match rich.payload {
            Some(ArchivedRichPayload::Correction { replaces_id }) => {
                assert_eq!(replaces_id.as_str(), "old-id");
            }
            other => panic!("expected Correction, got {other:?}"),
        }
    }
}
