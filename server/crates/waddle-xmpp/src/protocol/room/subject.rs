//! XEP-0045 §8.1 subject-change capture.
//!
//! Per [XEP-0045 §8.1]:
//!
//! > In a moderated room, only moderators are allowed to change the
//! > subject; in an unmoderated room, any participant or higher is
//! > allowed to change it.
//!
//! [XEP-0045 §8.1]: https://xmpp.org/extensions/xep-0045.html#subject-mod
//!
//! When an authorized occupant sends a subject-change message
//! (`<message><subject>...</subject></message>`, no `<body/>`), this
//! handler:
//!
//! 1. Detects the subject-change shape via empty `bodies` + non-empty
//!    `subjects` (matching `MucMessage::is_subject_change`).
//! 2. Mirrors `MucRoom::can_change_subject`'s policy against the frozen
//!    sender snapshot in [`RoomContext`].
//! 3. On allow: emits [`OutboundEvent::PersistRoomSubject`] so the
//!    interpreter writes a `SubjectState` onto the room actor — which
//!    then powers the XEP-0045 §7.2.15 historical replay on next join.
//!    Continues so [`super::archive::MucArchiveHandler`] can archive the
//!    change and [`super::reflector::ReflectorHandler`] can broadcast it.
//! 4. On deny: emits a typed `<error type='auth'><forbidden/></error>`
//!    reply and halts the chain (no archive, no broadcast).
//!
//! The handler runs **after** [`super::canonicalize::MucCanonicalizeHandler`]
//! so the live broadcast inherits the canonical `from='room/nick'` and
//! XEP-0421 occupant-id stamp; it runs **before**
//! [`super::archive::MucArchiveHandler`] so an unauthorized subject
//! change cannot leak into the room archive.
//!
//! Note: subject persistence is eventually-consistent with the live
//! broadcast (both are emitted in the same dispatch but persistence is
//! an actor ask while the broadcast is direct routing). Joiners between
//! the broadcast and the persistence may briefly see the previous
//! stored subject — acceptable per §7.2.15 since the live broadcast and
//! historical replay are independent stanzas.

use super::super::event::OutboundEvent;
use super::super::handlers::errors::{message_error_reply, send_message_error};
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::types::Role;
use chrono::{DateTime, Utc};
use jid::Jid;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// XEP-0045 §8.1 subject-change capture handler for the room chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucSubjectHandler;

impl RoomHandler for MucSubjectHandler {
    fn name(&self) -> &'static str {
        "xep-0045-subject"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        // Subject-change shape per XEP-0045 §8.1: <subject/> present,
        // <body/> absent. A message with both is a regular groupchat
        // message that happens to have an attached subject element —
        // not a subject change — and passes through untouched.
        if message.subjects.is_empty() || !message.bodies.is_empty() {
            return RoomHandlerOutcome::Continue(Vec::new());
        }

        // Sender snapshot — `OccupancyValidationHandler` runs first and
        // halts when this is missing; defensive Continue if somehow not.
        let Some(sender) = ctx.sender_snapshot() else {
            return RoomHandlerOutcome::Continue(Vec::new());
        };

        // §8.1 authorization. Mirrors `MucRoom::can_change_subject`'s
        // policy: moderators always; participants only in unmoderated
        // rooms; visitors / no-role never. The check is inline rather
        // than a borrow of `MucRoom` because the chain is stateless
        // against a frozen snapshot — `RoomContext` already carries
        // both inputs (`sender.role` and `room_moderated`).
        let allowed = match sender.role {
            Role::Moderator => true,
            Role::Participant => !ctx.room_moderated,
            Role::Visitor | Role::None => false,
        };
        if !allowed {
            let reply = forbidden_reply(message, ctx);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        }

        // First subject text wins — multi-language subjects are
        // §8.1-conformant but we persist a single canonical text for
        // replay. The `<subject>` elements still flow through to
        // archive and reflector unchanged.
        let text = message
            .subjects
            .iter()
            .next()
            .map(|(_, s)| s.0.clone())
            .unwrap_or_default();

        // Single dispatch clock (matches the `dispatch_timestamp`
        // sharing precedent in inbox.rs / archive.rs). Conversion
        // never fails for sane timestamps; on the impossible
        // overflow case we fall back to `Utc::now()` so we still
        // produce a usable stamp.
        let set_at: DateTime<Utc> =
            DateTime::from_timestamp(ctx.dispatch_timestamp, 0).unwrap_or_else(Utc::now);

        let event = OutboundEvent::PersistRoomSubject {
            room: ctx.room.clone(),
            text,
            setter: sender.bare_jid(),
            setter_nick: sender.nick.clone(),
            set_at,
        };
        RoomHandlerOutcome::Continue(vec![event])
    }
}

/// Build the XEP-0045 §8.1 typed `<forbidden type='auth'/>` reply
/// addressed from the room JID back to the sender.
fn forbidden_reply(incoming: &Message, ctx: &RoomContext<'_>) -> Message {
    let mut reply = message_error_reply(
        incoming,
        StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "en",
            "Sender is not permitted to change the room subject.",
        ),
    );
    reply.from = Some(Jid::from(ctx.room.clone()));
    reply.to = Some(Jid::from(ctx.sender_full.clone()));
    reply.type_ = MessageType::Error;
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::event::OutboundEvent;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0421::OccupantIdSecret;
    use jid::{BareJid, FullJid};
    use xmpp_parsers::message::{Body, Message, MessageType, Subject};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn subject_change(room: &BareJid, sender: &FullJid, text: &str) -> Message {
        let mut m = Message::new(Some(Jid::from(room.clone())));
        m.from = Some(Jid::from(sender.clone()));
        m.type_ = MessageType::Groupchat;
        m.subjects.insert(String::new(), Subject(text.to_string()));
        m
    }

    fn ctx<'a>(
        room: &'a BareJid,
        sender: &'a FullJid,
        occupants: &'a [OccupantSnapshot],
        room_moderated: bool,
        id_gen: &'a FixedIdGenerator,
        secret: &'a OccupantIdSecret,
    ) -> RoomContext<'a> {
        RoomContext {
            room,
            sender_full: sender,
            occupants,
            managed_room_forbidden: false,
            room_moderated,
            id_gen,
            occupant_id_secret: secret,
            sender_nickname_generation: 0,
            project_sender_inbox: true,
            dispatch_timestamp: 1_700_000_000,
        }
    }

    fn run(
        msg: &mut Message,
        room: &BareJid,
        sender: &FullJid,
        nick: &str,
        role: Role,
        room_moderated: bool,
    ) -> RoomHandlerOutcome {
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: nick.to_string(),
            affiliation: Affiliation::Member,
            role,
        }];
        let id_gen = FixedIdGenerator("test".to_string());
        let secret = OccupantIdSecret::for_testing(b"subject-handler-test".to_vec());
        let ctx = ctx(room, sender, &occupants, room_moderated, &id_gen, &secret);
        MucSubjectHandler.handle(msg, &ctx)
    }

    fn extract_persist(events: &[OutboundEvent]) -> Option<&OutboundEvent> {
        events
            .iter()
            .find(|e| matches!(e, OutboundEvent::PersistRoomSubject { .. }))
    }

    #[test]
    fn subject_change_from_moderator_emits_persist_event_and_continues() {
        let room = bare("team@muc.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = subject_change(&room, &sender, "New topic");
        let outcome = run(
            &mut msg,
            &room,
            &sender,
            "alice-nick",
            Role::Moderator,
            false,
        );
        let RoomHandlerOutcome::Continue(events) = outcome else {
            panic!("moderator subject change should Continue, got {outcome:?}");
        };
        let persist = extract_persist(&events).expect("PersistRoomSubject emitted");
        let OutboundEvent::PersistRoomSubject {
            text,
            setter,
            setter_nick,
            ..
        } = persist
        else {
            unreachable!()
        };
        assert_eq!(text, "New topic");
        assert_eq!(setter, &sender.to_bare());
        assert_eq!(setter_nick, "alice-nick");
    }

    #[test]
    fn subject_change_from_participant_in_unmoderated_room_emits_persist_event() {
        let room = bare("team@muc.example.com");
        let sender = full("bob@example.com/phone");
        let mut msg = subject_change(&room, &sender, "Topic by participant");
        let outcome = run(
            &mut msg,
            &room,
            &sender,
            "bob-nick",
            Role::Participant,
            false,
        );
        let RoomHandlerOutcome::Continue(events) = outcome else {
            panic!("participant in unmoderated room should be allowed, got {outcome:?}");
        };
        assert!(extract_persist(&events).is_some());
    }

    #[test]
    fn subject_change_from_participant_in_moderated_room_halts_with_forbidden() {
        let room = bare("team@muc.example.com");
        let sender = full("bob@example.com/phone");
        let mut msg = subject_change(&room, &sender, "Forbidden topic");
        let outcome = run(
            &mut msg,
            &room,
            &sender,
            "bob-nick",
            Role::Participant,
            true,
        );
        let RoomHandlerOutcome::Halt(events) = outcome else {
            panic!("participant in moderated room should be denied, got {outcome:?}");
        };
        assert_send_forbidden(&events);
    }

    #[test]
    fn subject_change_from_visitor_halts_with_forbidden() {
        let room = bare("team@muc.example.com");
        let sender = full("eve@example.com/web");
        let mut msg = subject_change(&room, &sender, "Visitor topic");
        let outcome = run(&mut msg, &room, &sender, "eve-nick", Role::Visitor, false);
        let RoomHandlerOutcome::Halt(events) = outcome else {
            panic!("visitor should be denied, got {outcome:?}");
        };
        assert_send_forbidden(&events);
    }

    #[test]
    fn non_subject_message_passes_through_unchanged() {
        let room = bare("team@muc.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = Message::new(Some(Jid::from(room.clone())));
        msg.from = Some(Jid::from(sender.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.bodies.insert(String::new(), Body("hi".to_string()));
        let outcome = run(
            &mut msg,
            &room,
            &sender,
            "alice-nick",
            Role::Moderator,
            false,
        );
        let RoomHandlerOutcome::Continue(events) = outcome else {
            panic!("regular message must Continue, got {outcome:?}");
        };
        assert!(extract_persist(&events).is_none());
    }

    #[test]
    fn subject_with_body_is_not_a_subject_change() {
        // §8.1 distinguishes subject-changes from regular messages by
        // the presence/absence of <body/>. A message carrying both is
        // a regular groupchat message.
        let room = bare("team@muc.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = subject_change(&room, &sender, "Topic-ish");
        msg.bodies
            .insert(String::new(), Body("plus body".to_string()));
        let outcome = run(&mut msg, &room, &sender, "alice-nick", Role::Visitor, false);
        // Visitor would be denied if this were a subject change, but
        // because <body/> is present it isn't — handler must Continue.
        let RoomHandlerOutcome::Continue(events) = outcome else {
            panic!("body+subject is not a subject change, got {outcome:?}");
        };
        assert!(extract_persist(&events).is_none());
    }

    #[test]
    fn persist_event_set_at_matches_dispatch_timestamp() {
        let room = bare("team@muc.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = subject_change(&room, &sender, "Topic");
        let outcome = run(
            &mut msg,
            &room,
            &sender,
            "alice-nick",
            Role::Moderator,
            false,
        );
        let RoomHandlerOutcome::Continue(events) = outcome else {
            unreachable!()
        };
        let OutboundEvent::PersistRoomSubject { set_at, .. } =
            extract_persist(&events).expect("persist emitted")
        else {
            unreachable!()
        };
        assert_eq!(set_at.timestamp(), 1_700_000_000);
    }

    fn assert_send_forbidden(events: &[OutboundEvent]) {
        let send = events
            .iter()
            .find_map(|e| match e {
                OutboundEvent::SendStanza(s) => Some(s),
                _ => None,
            })
            .expect("error reply emitted");
        let stanza_xml = format!("{send:?}");
        assert!(
            stanza_xml.contains("Forbidden") || stanza_xml.contains("forbidden"),
            "deny path must reply with <forbidden/>; got {stanza_xml}"
        );
    }
}
