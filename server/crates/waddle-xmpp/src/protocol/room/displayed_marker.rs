//! XEP-0333 displayed-marker → inbox mark-read for the room handler chain.
//!
//! The chat client sends a `<message type='groupchat'>` containing a
//! `<displayed xmlns='urn:xmpp:chat-markers:0' id='…'/>` element when
//! the user has displayed a room message. The room reflects the marker
//! to every occupant per XEP-0333 §3.4 (existing
//! [`super::reflector::ReflectorHandler`] behaviour); this handler runs
//! the **sender-side** side-effect: emit
//! [`super::super::event::OutboundEvent::MarkInboxReadFromDisplayed`]
//! so the interpreter clears the sender's inbox unread for the
//! channel-level row and — when the displayed message belongs to a
//! thread — the thread-level row too.
//!
//! Only the sender's mark-read is emitted. A recipient observing a
//! reflected marker is observing *somebody else's* read state, which
//! must not cross-clear their own inbox. The state machine has no
//! identity of the displayed message's archive thread here — that lives
//! in MAM — so the interpreter performs the lookup and applies the
//! mark-read on both rows.

use super::super::event::OutboundEvent;
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::xep::xep0333::{extract_marker_from_message, Marker};
use xmpp_parsers::message::Message;

/// XEP-0333 displayed-marker handler for the room handler chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucDisplayedMarkerHandler;

impl RoomHandler for MucDisplayedMarkerHandler {
    fn name(&self) -> &'static str {
        "xep-0333-muc-displayed-marker"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        let displayed_id = match extract_marker_from_message(message) {
            Some(Marker::Displayed(id)) => id,
            _ => return RoomHandlerOutcome::Continue(Vec::new()),
        };
        // Sender's inbox is the only one we mark-read from a displayed
        // marker: the marker reports the sender's own read state.
        // Synthetic sends without an inbox projection also skip here.
        if !ctx.project_sender_inbox {
            return RoomHandlerOutcome::Continue(Vec::new());
        }
        let Some(sender_snapshot) = ctx.sender_snapshot() else {
            return RoomHandlerOutcome::Continue(Vec::new());
        };
        let owner = sender_snapshot.bare_jid();
        let event = OutboundEvent::MarkInboxReadFromDisplayed {
            owner,
            room: ctx.room.clone(),
            displayed_message_id: displayed_id,
        };
        RoomHandlerOutcome::Continue(vec![event])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0333::build_displayed_element;
    use crate::xep::xep0421::OccupantIdSecret;
    use jid::{BareJid, FullJid, Jid};
    use xmpp_parsers::message::{Message, MessageType};

    fn full(value: &str) -> FullJid {
        value.parse().expect("valid full jid")
    }

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare jid")
    }

    fn occupant(full_jid: FullJid, nick: &str) -> OccupantSnapshot {
        OccupantSnapshot {
            full_jid,
            nick: nick.to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }
    }

    fn groupchat_with_marker(sender: &FullJid, id: &str) -> Message {
        let mut msg = Message::new(None::<Jid>);
        msg.from = Some(Jid::from(sender.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(build_displayed_element(id));
        msg
    }

    fn run(
        room: &BareJid,
        sender: &FullJid,
        occupants: &[OccupantSnapshot],
        msg: &mut Message,
    ) -> Vec<OutboundEvent> {
        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
            sender_full: sender,
            occupants,
            durable_recipient_bare_jids: &[],
            managed_room_forbidden: false,
            room_moderated: false,
            room_members_only: false,
            pin_permission: crate::muc::PinPermission::default(),
            id_gen: &id_gen,
            occupant_id_secret: &secret,
            sender_nickname_generation: 0,
            project_sender_inbox: true,
            dispatch_timestamp: 0,
        };
        match MucDisplayedMarkerHandler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(events) => events,
            RoomHandlerOutcome::Halt(_) => panic!("displayed-marker handler never halts"),
        }
    }

    #[test]
    fn emits_mark_read_for_sender_on_displayed_marker() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob = full("bob@example.com/desk");
        let occupants = vec![occupant(alice.clone(), "alice"), occupant(bob, "bob")];
        let mut msg = groupchat_with_marker(&alice, "msg-42");

        let events = run(&room, &alice, &occupants, &mut msg);

        assert_eq!(events.len(), 1, "expected exactly one mark-read event");
        match &events[0] {
            OutboundEvent::MarkInboxReadFromDisplayed {
                owner,
                room: marked_room,
                displayed_message_id,
            } => {
                assert_eq!(owner, &bare("alice@example.com"));
                assert_eq!(marked_room, &room);
                assert_eq!(displayed_message_id, "msg-42");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn ignores_messages_without_a_displayed_marker() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let occupants = vec![occupant(alice.clone(), "alice")];
        let mut msg = Message::new(None::<Jid>);
        msg.from = Some(Jid::from(alice.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.bodies
            .insert(String::new(), xmpp_parsers::message::Body("hi".into()));

        let events = run(&room, &alice, &occupants, &mut msg);

        assert!(events.is_empty(), "non-marker messages must not emit");
    }

    #[test]
    fn ignores_markable_only_messages() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let occupants = vec![occupant(alice.clone(), "alice")];
        let mut msg = Message::new(None::<Jid>);
        msg.from = Some(Jid::from(alice.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.id = Some("anchor".into());
        msg.bodies
            .insert(String::new(), xmpp_parsers::message::Body("hi".into()));
        msg.payloads
            .push(crate::xep::xep0333::build_markable_element());

        let events = run(&room, &alice, &occupants, &mut msg);

        assert!(
            events.is_empty(),
            "markable-only request must not produce mark-read"
        );
    }

    #[test]
    fn ignores_displayed_with_empty_id() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let occupants = vec![occupant(alice.clone(), "alice")];
        let mut msg = groupchat_with_marker(&alice, "");

        let events = run(&room, &alice, &occupants, &mut msg);

        assert!(
            events.is_empty(),
            "empty id is malformed and must not emit mark-read"
        );
    }

    #[test]
    fn ignores_sender_not_present_in_occupants() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let other = full("bob@example.com/desk");
        let occupants = vec![occupant(other, "bob")];
        let mut msg = groupchat_with_marker(&alice, "msg-42");

        let events = run(&room, &alice, &occupants, &mut msg);

        assert!(
            events.is_empty(),
            "the gate enforces occupancy; this handler degenerates safely when the snapshot omits the sender"
        );
    }
}
