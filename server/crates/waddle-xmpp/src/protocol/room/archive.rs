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
    has_file_sharing, is_moderation_request_message, is_moderation_result_message,
    is_reaction_message, is_retraction_message, is_sticker_message, should_skip_storage,
};
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
        || is_retraction_message(message)
        || is_moderation_request_message(message)
        || is_moderation_result_message(message)
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
            managed_room_forbidden: false,
            room_moderated: false,
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
}
