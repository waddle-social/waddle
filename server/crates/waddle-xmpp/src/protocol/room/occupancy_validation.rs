//! XEP-0045 §7.4 sender-occupancy gate.
//!
//! Per [XEP-0045 §7.4]:
//!
//! > Only occupants are allowed to send messages to the room. If a
//! > non-occupant sends a message to the room, the service MUST refuse
//! > to deliver the message and return a `<not-acceptable/>` error to
//! > the sender.
//!
//! [XEP-0045 §7.4]: https://xmpp.org/extensions/xep-0045.html#message
//!
//! This handler also enforces the Waddle-specific managed-room policy
//! for the `announcements` room: only server owners may post. The
//! [`super::context::RoomContext`] field `managed_room_forbidden` is
//! pre-derived by the interpreter (so the handler stays sync) — when
//! true the handler emits `<forbidden type='auth'/>` and halts.
//!
//! Closes the gap PR16 documented: the legacy
//! `deliver_groupchat_via_room_actor` bridge silently dropped on
//! `BuildGroupchatBroadcast` errors and never produced a typed XEP-0045
//! reply. This handler emits the typed reply directly.
//!
//! Typed-error construction is centralized in
//! [`super::errors`] — this handler only decides *which* of the three
//! XEP-0045 §7.4 / §7.5 / managed-room constructors to invoke.

use super::super::handlers::errors::send_message_error;
use super::context::RoomContext;
use super::errors::{
    managed_room_forbidden_reply, xep_0045_not_acceptable_reply, xep_0045_visitor_forbidden_reply,
};
use super::traits::{RoomHandler, RoomHandlerOutcome};
use xmpp_parsers::message::Message;

/// Sender-occupancy gate for the room handler chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct OccupancyValidationHandler;

impl RoomHandler for OccupancyValidationHandler {
    fn name(&self) -> &'static str {
        "xep-0045-occupancy-validation"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        // Managed-room owner check (Waddle-specific). The interpreter
        // resolves this against the authenticated session and the
        // `announcements` localpart marker before constructing the
        // context, so the handler stays sync.
        if ctx.managed_room_forbidden {
            let reply = managed_room_forbidden_reply(message, ctx.room, ctx.sender_full);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        }

        // XEP-0045 §7.4: non-occupants cannot send to the room.
        let Some(sender) = ctx.sender_snapshot() else {
            let reply = xep_0045_not_acceptable_reply(message, ctx.room, ctx.sender_full);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        };

        // XEP-0045 §7.5: visitors may not send messages in moderated
        // rooms. The legacy `RoomActor::BuildGroupchatBroadcast` path
        // enforced this via `RoomActorError::VisitorMayNotSpeak`; the
        // chain mirrors it here so the cutover doesn't drop the
        // conformance gate (Copilot review on PR #279).
        // The voice predicate is shared with the SFU media-grant
        // derivation (`Role::voice`) so text and media authorization
        // can never disagree about who may speak.
        if !sender
            .role
            .voice(crate::types::Moderation::from_moderated_flag(
                ctx.room_moderated,
            ))
            .is_voiced()
        {
            let reply = xep_0045_visitor_forbidden_reply(message, ctx.room, ctx.sender_full);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        }

        RoomHandlerOutcome::Continue(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::event::OutboundEvent;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0421::OccupantIdSecret;
    use crate::Stanza;
    use jid::{BareJid, FullJid, Jid};
    use xmpp_parsers::message::MessageType;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn groupchat_to(room: &BareJid, sender: &FullJid, body: &str) -> Message {
        let mut m = Message::new(Some(Jid::from(room.clone())));
        m.from = Some(Jid::from(sender.clone()));
        m.type_ = MessageType::Groupchat;
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
        m
    }

    fn run_with(
        room: &BareJid,
        sender: &FullJid,
        occupants: Vec<OccupantSnapshot>,
        managed_forbidden: bool,
        msg: &mut Message,
    ) -> RoomHandlerOutcome {
        run_with_moderation(room, sender, occupants, managed_forbidden, false, msg)
    }

    fn run_with_moderation(
        room: &BareJid,
        sender: &FullJid,
        occupants: Vec<OccupantSnapshot>,
        managed_forbidden: bool,
        moderated: bool,
        msg: &mut Message,
    ) -> RoomHandlerOutcome {
        let id_gen = FixedIdGenerator("fresh".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
            sender_full: sender,
            occupants: &occupants,
            durable_recipient_bare_jids: &[],
            managed_room_forbidden: managed_forbidden,
            room_moderated: moderated,
            room_occupants_may_change_subject: false,
            room_members_only: false,
            pin_permission: crate::muc::PinPermission::default(),
            id_gen: &id_gen,
            occupant_id_secret: &secret,
            sender_nickname_generation: 0,
            project_sender_inbox: true,
            synthetic_sender_authority: None,
            dispatch_timestamp: 0,
        };
        OccupancyValidationHandler.handle(msg, &ctx)
    }

    fn extract_error(outcome: &RoomHandlerOutcome) -> &Message {
        let events = match outcome {
            RoomHandlerOutcome::Halt(e) => e,
            RoomHandlerOutcome::Continue(_) => panic!("expected Halt"),
        };
        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Message(m) => m,
                _ => panic!("expected Message stanza"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn xep_0045_non_occupant_sender_receives_not_acceptable_error() {
        let room = bare("room@conf.example.com");
        let sender = full("alice@example.com/web");
        // Empty occupant list — alice is not a member.
        let mut msg = groupchat_to(&room, &sender, "hi");
        let outcome = run_with(&room, &sender, Vec::new(), false, &mut msg);
        let reply = extract_error(&outcome);
        assert_eq!(reply.type_, MessageType::Error);
        assert_eq!(
            reply.to.as_ref().map(|j| j.to_string()),
            Some(sender.to_string())
        );
        assert_eq!(
            reply.from.as_ref().map(|j| j.to_string()),
            Some(room.to_string())
        );
        // Inspect the typed StanzaError to confirm the defined-condition.
        let err_elem = reply
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload present");
        let parsed =
            StanzaError::try_from(err_elem.clone()).expect("typed StanzaError parses from element");
        assert_eq!(parsed.type_, ErrorType::Cancel);
        assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
    }

    #[test]
    fn xep_0045_occupant_sender_passes_through() {
        let room = bare("room@conf.example.com");
        let sender = full("alice@example.com/web");
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }];
        let mut msg = groupchat_to(&room, &sender, "hi");
        let outcome = run_with(&room, &sender, occupants, false, &mut msg);
        match outcome {
            RoomHandlerOutcome::Continue(events) => assert!(events.is_empty()),
            RoomHandlerOutcome::Halt(_) => panic!("occupant must pass through"),
        }
    }

    #[test]
    fn xep_0045_visitor_in_moderated_room_receives_forbidden_error() {
        // XEP-0045 §7.5: visitors may not send messages in moderated
        // rooms. Sender IS an occupant (passes §7.4) but role=Visitor
        // and the room is moderated → typed `<forbidden type='auth'/>`.
        let room = bare("moderated@conf.example.com");
        let sender = full("alice@example.com/web");
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::None,
            role: Role::Visitor,
        }];
        let mut msg = groupchat_to(&room, &sender, "hi (as visitor)");
        let outcome = run_with_moderation(&room, &sender, occupants, false, true, &mut msg);
        let reply = extract_error(&outcome);
        let err_elem = reply
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload");
        let parsed = StanzaError::try_from(err_elem.clone()).expect("typed StanzaError");
        assert_eq!(parsed.type_, ErrorType::Auth);
        assert_eq!(parsed.defined_condition, DefinedCondition::Forbidden);
    }

    #[test]
    fn xep_0045_participant_in_moderated_room_passes_through() {
        // Same room moderated=true, but sender is a Participant (not
        // Visitor) — must pass through without error.
        let room = bare("moderated@conf.example.com");
        let sender = full("alice@example.com/web");
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }];
        let mut msg = groupchat_to(&room, &sender, "hi");
        let outcome = run_with_moderation(&room, &sender, occupants, false, true, &mut msg);
        match outcome {
            RoomHandlerOutcome::Continue(events) => assert!(events.is_empty()),
            RoomHandlerOutcome::Halt(_) => panic!("participant in moderated room must pass"),
        }
    }

    #[test]
    fn xep_0045_visitor_in_unmoderated_room_passes_through() {
        // moderated=false → visitors may speak.
        let room = bare("open@conf.example.com");
        let sender = full("alice@example.com/web");
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::None,
            role: Role::Visitor,
        }];
        let mut msg = groupchat_to(&room, &sender, "hi");
        let outcome = run_with_moderation(&room, &sender, occupants, false, false, &mut msg);
        match outcome {
            RoomHandlerOutcome::Continue(events) => assert!(events.is_empty()),
            RoomHandlerOutcome::Halt(_) => panic!("visitor in unmoderated room must pass"),
        }
    }

    #[test]
    fn managed_room_forbidden_emits_typed_forbidden_error() {
        let room = bare("announcements@conf.example.com");
        let sender = full("alice@example.com/web");
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }];
        let mut msg = groupchat_to(&room, &sender, "important announcement");
        let outcome = run_with(&room, &sender, occupants, true, &mut msg);
        let reply = extract_error(&outcome);
        let err_elem = reply
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload");
        let parsed = StanzaError::try_from(err_elem.clone()).expect("typed StanzaError");
        assert_eq!(parsed.type_, ErrorType::Auth);
        assert_eq!(parsed.defined_condition, DefinedCondition::Forbidden);
    }
}
