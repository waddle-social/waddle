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
use jid::Jid;
use tracing::warn;
use xmpp_parsers::message::Message;

use super::{
    ArchivedMention, ArchivedMessage, ArchivedReactionSet, ArchivedReference, ArchivedReply,
    ArchivedRetraction, ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
};
use crate::parser::message_to_string;
use crate::xep::{
    extract_correction_from_message, extract_explicit_mentions, extract_reactions_from_message,
    extract_references_from_message, extract_retraction_from_message, parse_reply_from_message,
    RetractionKind, NS_REPLY,
};
use waddle_xmpp_core::xep0201::{thread_info_from_message_in_stanza_ns, CLIENT_STANZA_NS};
use waddle_xmpp_core::xep0359::{extract_origin_id, extract_stanza_id_by, StanzaId};

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
/// `from` and `to` are the canonical addressing tuple for the archive
/// row — the caller (interpreter) supplies them so the projection
/// does not duplicate the pass-aware logic that lives in
/// [`super::super::protocol::handlers::archive::ArchiveHandler`]. They
/// are typed as [`Jid`] (not `&str`) per the typed-payloads hard
/// rule; the archive write path now flows typed values end-to-end
/// from the wire-parse boundary down through MAM storage with no
/// intermediate string detour.
///
/// If the canonical stamp is missing (test fixture or misconfigured
/// chain), the row falls back to an empty `id` (the storage backend
/// will generate one) and `stanza_id` is taken from the wire
/// `<message id=...>` for legacy parity. Production paths always run
/// `CanonicalizeHandler` before `ArchiveHandler` per the locked
/// Q2(a) order, so this fallback is defensive only.
pub fn build_direct_archived_message(
    archive_jid: &Jid,
    from: Jid,
    to: Jid,
    message: &Message,
) -> ArchivedMessage {
    // RFC 6121 §5.2.3: `<body>` is optional. Preserve the wire-fidelity
    // distinction between "no `<body>` element on the wire" (`None`) and
    // "empty `<body></body>`" (`Some("")`) so consumers reading the
    // denormalized `body` field don't conflate subject-only or
    // reaction-only stanzas with truly empty bodies.
    let body = prototype_body(message);
    let reply = extract_reply_reference(message);
    let origin_id = extract_origin_id(message);
    let rich = rich_archive_payload(message);
    let stanza_xml = serialize_message_xml(message);
    // XEP-0359 §3 makes the `by` attribute REQUIRED on every
    // `<stanza-id/>`. The wire `id` attribute on the `<message/>`
    // element does not carry a `by` of its own — it's the message's
    // local identifier — so when we project it into the typed
    // `Option<StanzaId>` field we attribute it to the archive
    // (`archive_jid`). This matches the storage decode contract,
    // which reconstructs `StanzaId.by` from the row's archive jid.
    let stanza_id = message
        .id
        .clone()
        .map(|id| StanzaId::new(id.0, archive_jid.clone()));

    // Storage shape mirrors the legacy `archive_direct_message`:
    // `id` is the canonical XEP-0359 stamp (the archive's primary
    // key for MAM lookups), and `stanza_id` carries the message's
    // wire `id` attribute. XEP-0424 retraction lookups query the
    // `stanza_id` column via `get_message_by_message_id`, and
    // (per `get_message_by_stanza_id`) the XEP-0359 stamp is
    // already keyed by `id`, so both retract-by-wire-id and
    // retract-by-canonical-stamp resolve correctly. If the canonical
    // stamp is missing, `id` is left empty here and the storage
    // backend assigns the archive primary key at write time; the
    // wire id still lives in `stanza_id`.
    let canonical_stamp = extract_stanza_id_by(message, archive_jid);
    let id = canonical_stamp.unwrap_or_default();

    // XEP-0201: read the typed thread info (id + optional parent) from the
    // post-reattach payload form. The inbound parse boundary in
    // `protocol::frame::parse_stanza` calls `reattach_thread_parent` so the
    // parent attribute survives `Message::try_from`'s typed lossy parse.
    // The collapsed `Option<ThreadInfo>` field on `ArchivedMessage` accepts
    // the helper's result directly — no flattening required.
    let thread = thread_info_from_message_in_stanza_ns(message, CLIENT_STANZA_NS);

    ArchivedMessage {
        id,
        timestamp: Utc::now(),
        from,
        to,
        body,
        stanza_id,
        thread,
        reply,
        origin_id,
        // Typed propagation: the wire-parsed `MessageType` flows
        // straight from `Message::type_` into the archive row. Pre-#228
        // commit 8 this went through a lossy
        // `mam_message_type(&MessageType) -> String` stringifier that
        // round-tripped a typed value through a string and back; with
        // the typed `ArchivedMessage.message_type` field that helper
        // is gone and the value moves intact.
        message_type: message.type_.clone(),
        stanza_xml,
        rich,
        nickname_generation: None,
    }
}

fn prototype_body(message: &Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .cloned()
}

/// Extract the XEP-0461 `<reply id='X' to='Y'/>` reference from a
/// message into a typed [`ArchivedReply`].
///
/// XEP-0461 §3 makes `id` MUST and `to` SHOULD; if `to` is present but
/// fails JID parsing we drop the malformed `to` field and keep the
/// reply (logging a warning), rather than discarding the entire reply
/// reference. A reply element with no `id` attribute (or an empty `id`,
/// which `RichMessageId::new` rejects) is treated as no reply at all.
fn extract_reply_reference(message: &Message) -> Option<ArchivedReply> {
    let reply = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == NS_REPLY)?;
    let id = RichMessageId::new(reply.attr("id")?)?;
    let to = reply.attr("to").and_then(|raw| match raw.parse::<Jid>() {
        Ok(jid) => Some(jid),
        Err(error) => {
            warn!(
                message_id = message.id.as_ref().map(|id| id.0.as_str()).unwrap_or(""),
                reply_to = raw,
                %error,
                "XEP-0461 reply reference: malformed `to` JID dropped, keeping reply id only"
            );
            None
        }
    });
    Some(ArchivedReply { id, to })
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
                message_id = message.id.as_ref().map(|id| id.0.as_str()).unwrap_or(""),
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
                            retraction_id: message
                                .id
                                .clone()
                                .and_then(|id| RichMessageId::new(id.0)),
                        })
                    }),
                RetractionKind::Tombstone(retracted) => message.id.clone().and_then(|id| {
                    RichMessageId::new(id.0).map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: retracted
                                .stamp
                                .as_deref()
                                .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                                .map(|stamp| stamp.with_timezone(&Utc)),
                            retraction_id: RichMessageId::new(retracted.retraction_id),
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
        RichMessageId::new(reply.id.as_str()).map(|id| ArchivedReply { id, to: reply.to })
    });

    let references = extract_references_from_message(message)
        .into_iter()
        .filter_map(|reference| {
            let ref_type = RichText::new(reference.ref_type.as_str())?;
            Some(ArchivedReference {
                ref_type,
                begin: reference.begin.and_then(|value| value.try_into().ok()),
                end: reference.end.and_then(|value| value.try_into().ok()),
                uri: RichText::new(reference.uri),
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
            subjects: Default::default(),
            // 1:1 archive rows have no MUC occupant identity; the
            // groupchat projection (`rich_archive_payload` in the
            // interpreter) is the path that captures these.
            occupant_id: None,
            muc_sender: None,
        })
    }
}

#[cfg(test)]
mod tests;
