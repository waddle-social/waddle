//! Per-occupant fan-out: emit one
//! [`super::super::event::OutboundEvent::RouteToConnection`] per
//! occupant of the room.
//!
//! Per XEP-0045 §7.2.13 (and the legacy
//! `MucRoom::broadcast_message`), the room MUST reflect the message to
//! every occupant *including the sender* so the sender gets a confirmed
//! echo of their own send. We follow the same semantic here: each
//! occupant gets a `RouteToConnection` carrying a personalized copy of
//! the canonicalized message with `to=<recipient_full>` filled in.
//!
//! The interpreter's `RouteToConnection` arm handles the per-recipient
//! delivery + detached XEP-0198 SM session queueing already (#229 PR12 +
//! PR15), so the reflector handler is dumb fan-out: build N copies, set
//! `to`, emit N events.

use super::super::event::OutboundEvent;
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::Stanza;
use jid::Jid;
use xmpp_parsers::message::Message;

/// Per-occupant fan-out for the room handler chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReflectorHandler;

impl RoomHandler for ReflectorHandler {
    fn name(&self) -> &'static str {
        "xep-0045-reflector"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        let mut events = Vec::with_capacity(ctx.occupants.len());
        for occupant in ctx.recipient_occupants() {
            let mut copy = message.clone();
            copy.to = Some(Jid::from(occupant.full_jid.clone()));
            events.push(OutboundEvent::RouteToConnection {
                jid: Jid::from(occupant.full_jid.clone()),
                stanza: Box::new(Stanza::Message(copy)),
                call_setup: None,
            });
        }
        RoomHandlerOutcome::Continue(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0421::OccupantIdSecret;
    use jid::{BareJid, FullJid};
    use xmpp_parsers::message::{Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn run_with_occupants(
        room: &BareJid,
        sender: &FullJid,
        occupants: Vec<OccupantSnapshot>,
        msg: &mut Message,
    ) -> Vec<OutboundEvent> {
        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
            sender_full: sender,
            occupants: &occupants,
            durable_recipient_bare_jids: &[],
            managed_room_forbidden: false,
            room_moderated: false,
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
        match ReflectorHandler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(e) => e,
            RoomHandlerOutcome::Halt(_) => panic!("reflector never halts"),
        }
    }

    fn occ(full_jid: FullJid, nick: &str) -> OccupantSnapshot {
        OccupantSnapshot {
            full_jid,
            nick: nick.to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }
    }

    fn route_targets(events: &[OutboundEvent]) -> Vec<jid::Jid> {
        events
            .iter()
            .filter_map(|e| match e {
                OutboundEvent::RouteToConnection { jid, .. } => Some(jid.clone()),
                _ => None,
            })
            .collect()
    }

    fn make_groupchat() -> Message {
        let mut m = Message::new(None::<jid::Jid>);
        m.type_ = MessageType::Groupchat;
        m.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "hi everyone".to_string(),
        );
        m
    }

    #[test]
    fn xep_0045_fans_out_route_to_connection_per_occupant() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob = full("bob@example.com/desk");
        let charlie = full("charlie@example.com/lap");
        let occupants = vec![
            occ(alice.clone(), "alice"),
            occ(bob.clone(), "bob"),
            occ(charlie.clone(), "charlie"),
        ];
        let mut msg = make_groupchat();
        let events = run_with_occupants(&room, &alice, occupants, &mut msg);
        let targets = route_targets(&events);
        assert_eq!(targets.len(), 3);
        let target_strings: Vec<String> = targets.iter().map(|j| j.to_string()).collect();
        assert!(target_strings.contains(&alice.to_string()));
        assert!(target_strings.contains(&bob.to_string()));
        assert!(target_strings.contains(&charlie.to_string()));
    }

    #[test]
    fn xep_0045_includes_sender_in_reflection() {
        // XEP-0045 §7.2.13: room reflects to every occupant *including*
        // the sender so the sender confirms their message landed.
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let occupants = vec![occ(alice.clone(), "alice")];
        let mut msg = make_groupchat();
        let events = run_with_occupants(&room, &alice, occupants, &mut msg);
        let targets = route_targets(&events);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].to_string(), alice.to_string());
    }

    #[test]
    fn xep_0045_each_route_carries_personalized_to_address() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob = full("bob@example.com/desk");
        let occupants = vec![occ(alice.clone(), "alice"), occ(bob.clone(), "bob")];
        let mut msg = make_groupchat();
        let events = run_with_occupants(&room, &alice, occupants, &mut msg);
        for event in &events {
            if let OutboundEvent::RouteToConnection { jid, stanza, .. } = event {
                if let Stanza::Message(m) = stanza.as_ref() {
                    assert_eq!(
                        m.to.as_ref().map(|j| j.to_string()),
                        Some(jid.to_string()),
                        "each routed copy carries `to` matching its target"
                    );
                }
            }
        }
    }
}
