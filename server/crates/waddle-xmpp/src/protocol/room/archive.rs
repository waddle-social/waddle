//! XEP-0313 §5.1.3 archive-eligibility for groupchat reflections.
//!
//! Mirrors [`super::super::handlers::archive::ArchiveHandler`] for the
//! room-locality chain. On eligibility, emits
//! [`super::super::event::OutboundEvent::ArchiveGroupchat`]; the
//! interpreter persists under the room's bare JID (XEP-0313 §5.1.3
//! groupchat archives are keyed by room).
//!
//! Eligibility rules (XEP-0313 §5.1.3 + Waddle protocol-event archive
//! semantics):
//!
//! - `<no-store/>` (XEP-0334 §3) suppresses; `<store/>` overrides.
//! - Body / subject-bearing groupchat messages are archived.
//! - Body-less *protocol* messages (reactions, retractions, moderation,
//!   file shares, stickers) are archived so MAM replay reproduces the
//!   timeline. Mirrors the legacy `should_archive_groupchat_message`
//!   heuristic from `message.rs`.

use super::super::event::OutboundEvent;
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::xep::xep0424::{extract_retraction_from_message, RetractionKind};
use crate::xep::{
    extract_forum_action, has_file_sharing, is_moderation_result_message, is_reaction_message,
    is_sticker_message, should_skip_storage,
};
use waddle_xmpp_core::xep0201::{thread_info_from_message_in_stanza_ns, CLIENT_STANZA_NS};
use xmpp_parsers::message::{Message, MessageType};

/// XEP-0313 archive handler for the room handler chain.
///
/// Emits [`OutboundEvent::ArchiveGroupchat`] for archivable messages and,
/// when the message is a XEP-0424 retraction request, also emits an
/// [`OutboundEvent::ApplyGroupchatRetractionTombstone`] so the
/// interpreter can replace the target row in the room archive with a
/// tombstone (XEP-0424 §"prevent further distribution"). Mirrors the
/// 1:1 path in
/// [`super::super::handlers::archive::ArchiveHandler`] +
/// [`OutboundEvent::ArchiveDirect`]'s retraction branch.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucArchiveHandler;

impl RoomHandler for MucArchiveHandler {
    fn name(&self) -> &'static str {
        "xep-0313-muc-archive"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        let mut events = Vec::new();
        if is_archivable(message) {
            events.push(OutboundEvent::ArchiveGroupchat {
                room: ctx.room.clone(),
                sender: ctx.sender_full.clone(),
                message: Box::new(message.clone()),
                sender_nickname_generation: ctx.sender_nickname_generation,
            });
        }
        if let Some(RetractionKind::Request(retraction)) = extract_retraction_from_message(message)
        {
            events.push(OutboundEvent::ApplyGroupchatRetractionTombstone {
                room: ctx.room.clone(),
                target_message_id: retraction.retracts_id,
                retraction_message: Box::new(message.clone()),
            });
        }
        RoomHandlerOutcome::Continue(events)
    }
}

/// XEP-0313 §5.1.3 archive-eligibility for groupchat. Mirrors the
/// legacy `should_archive_groupchat_message` heuristic.
pub fn is_archivable(message: &Message) -> bool {
    if matches!(message.type_, MessageType::Error) || should_skip_storage(message) {
        return false;
    }
    if !message.bodies.is_empty() || !message.subjects.is_empty() {
        return true;
    }
    is_reaction_message(message)
        || matches!(
            extract_retraction_from_message(message),
            Some(RetractionKind::Request(_))
        )
        || is_moderation_result_message(message)
        || thread_info_from_message_in_stanza_ns(message, CLIENT_STANZA_NS).is_some()
        || extract_forum_action(message).is_some()
        || has_file_sharing(message)
        || is_sticker_message(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0334::Hint;
    use crate::xep::xep0421::OccupantIdSecret;
    use jid::{BareJid, FullJid, Jid};
    use minidom::Element;
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn groupchat(room: &BareJid, sender: &FullJid, body: &str) -> Message {
        let mut m = Message::new(Some(Jid::from(room.clone())));
        m.from = Some(Jid::from(sender.clone()));
        m.type_ = MessageType::Groupchat;
        if !body.is_empty() {
            m.bodies.insert(String::new(), Body(body.to_string()));
        }
        m
    }

    fn run<'a>(room: &'a BareJid, sender: &'a FullJid, msg: &'a mut Message) -> Vec<OutboundEvent> {
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }];
        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
            sender_full: sender,
            occupants: &occupants,
            durable_recipient_bare_jids: &[],
            managed_room_forbidden: false,
            room_moderated: false,
            room_members_only: false,
            mention_permissions: crate::xep::xep0513::MentionPermissions::default(),
            pin_permission: crate::muc::PinPermission::default(),
            id_gen: &id_gen,
            occupant_id_secret: &secret,
            sender_nickname_generation: 0,
            project_sender_inbox: true,
            dispatch_timestamp: 0,
        };
        match MucArchiveHandler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(e) => e,
            RoomHandlerOutcome::Halt(_) => panic!("archive never halts"),
        }
    }

    #[test]
    fn xep_0313_archives_eligible_groupchat() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        let events = run(&room, &sender, &mut msg);
        let archives: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                OutboundEvent::ArchiveGroupchat { room, .. } => Some(room.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(archives, vec![room]);
    }

    #[test]
    fn xep_0313_skips_no_store_groupchat() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "ephemeral");
        msg.payloads.push(
            Element::builder(Hint::NoStore.element_name(), crate::xep::xep0334::NS_HINTS).build(),
        );
        let events = run(&room, &sender, &mut msg);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "no-store hint must suppress archive"
        );
    }

    #[test]
    fn xep_0313_skips_bodyless_groupchat() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        // Bodyless, subjectless, no rich payload.
        let mut msg = groupchat(&room, &sender, "");
        let events = run(&room, &sender, &mut msg);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "bodyless protocol-event-less message is not archived"
        );
    }

    #[test]
    fn xep_0424_skips_inbound_groupchat_tombstones() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "");
        msg.payloads.push(
            Element::builder("retracted", crate::xep::xep0424::NS_MESSAGE_RETRACT)
                .attr("id", "retract-1")
                .attr("stamp", "2024-06-01T09:00:00Z")
                .build(),
        );

        let events = run(&room, &sender, &mut msg);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "archive-side tombstones must not be treated as inbound groupchat messages"
        );
    }

    #[test]
    fn xep_0313_archives_bodyless_forum_thread_create_metadata() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "");
        crate::xep::xep0508::set_thread_create(
            &mut msg,
            &crate::xep::xep0508::ThreadCreate::new("Roadmap"),
        );

        let events = run(&room, &sender, &mut msg);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "forum metadata is durable thread UI state and must be archived"
        );
    }

    #[test]
    fn xep_0313_archives_bodyless_standard_muc_thread_metadata() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "");
        waddle_xmpp_core::xep0201::set_thread_id(&mut msg, "topic-root");

        let events = run(&room, &sender, &mut msg);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "XEP-0201 thread metadata is durable MUC UI state and must be archived"
        );
    }

    #[test]
    fn xep_0313_archives_bodyless_forum_thread_reply_metadata() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "");
        crate::xep::xep0508::set_thread_reply(
            &mut msg,
            &crate::xep::xep0508::ThreadReply::new("topic-root"),
        );

        let events = run(&room, &sender, &mut msg);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "forum reply metadata is durable thread UI state and must be archived"
        );
    }

    /// Parse-side wire-shape pin for the reaction stanza emitted by
    /// `waddle-xmpp-client::messaging::builders::build_reaction_message`
    /// (the WASM client's `send_reaction` path). Catches detection-side
    /// regressions: xmpp_parsers upgrades, payload field renames,
    /// namespace drift.
    ///
    /// NOTE: this test does NOT cover the actual storage-write path
    /// where the production "reactions vanish after refresh" incident
    /// (#457) lived — that bug was a `NOT NULL` constraint mismatch
    /// at the SQL boundary, not a parse-side detection failure. See
    /// `mam::storage::tests::reaction_lands_after_legacy_body_not_null_schema_migrated`
    /// for the SQL-write regression coverage that incident needed.
    #[test]
    fn xep_0444_groupchat_reaction_wire_shape_is_archivable() {
        // Build the wire shape via typed XEP-0444 + XEP-0334 builders
        // (no raw XML — AGENTS.md XML-generation hard rule), then
        // round-trip through the `Message::try_from(Element)` parse
        // boundary that the test claims to cover. This is structurally
        // identical to the bytes `waddle-xmpp-client::messaging::
        // builders::build_reaction_message` puts on the wire for the
        // WASM client's `send_reaction` path.
        let stanza = Element::builder("message", CLIENT_STANZA_NS)
            .attr("to", "room@muc.example.com")
            .attr("type", "groupchat")
            .attr("id", "reaction-uuid-1")
            .append(crate::xep::xep0444::build_reactions_element(
                "target-sid",
                &["❤️"],
            ))
            .append(Element::builder("store", crate::xep::xep0334::NS_HINTS).build())
            .build();
        let msg = Message::try_from(stanza).expect("parse message");
        assert!(
            crate::xep::is_reaction_message(&msg),
            "is_reaction_message must detect the <reactions/> payload"
        );
        assert!(
            is_archivable(&msg),
            "is_archivable must accept the bodyless reaction stanza"
        );
    }

    #[test]
    fn xep_0313_skips_malformed_bodyless_forum_metadata() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "");
        msg.payloads
            .push(Element::builder("thread-create", crate::xep::xep0508::NS_FORUMS).build());
        msg.payloads.push(
            Element::builder("thread-create", crate::xep::xep0508::NS_FORUMS)
                .attr("title", "")
                .build(),
        );
        msg.payloads.push(
            Element::builder("thread-create", crate::xep::xep0508::NS_FORUMS)
                .attr("title", "   ")
                .build(),
        );
        msg.payloads
            .push(Element::builder("thread-reply", crate::xep::xep0508::NS_FORUMS).build());
        msg.payloads.push(
            Element::builder("thread-reply", crate::xep::xep0508::NS_FORUMS)
                .attr("thread-id", "")
                .build(),
        );
        msg.payloads.push(
            Element::builder("thread-reply", crate::xep::xep0508::NS_FORUMS)
                .attr("thread-id", "   ")
                .build(),
        );

        let events = run(&room, &sender, &mut msg);

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
            "malformed forum metadata must not become durable MAM state"
        );
    }
}
