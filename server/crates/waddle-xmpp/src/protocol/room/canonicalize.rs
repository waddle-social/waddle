//! XEP-0045 + XEP-0359 + XEP-0421 canonicalization for groupchat
//! reflections.
//!
//! Mutates the in-flight message so subsequent room handlers (archive,
//! reflector) and the eventual wire write see the canonicalized form.
//!
//! Four rewrites in order:
//!
//! 1. **Strip same-`by=room`** XEP-0359 `<stanza-id>` siblings (XEP-0359
//!    §5 strip rule, room scope). Defends against client spoofing of a
//!    stamp claiming to come from the room archive.
//! 2. **Stamp** a fresh `<stanza-id by='room' id='<UUID>'>` from the
//!    [`super::super::id_gen::IdGenerator`].
//! 3. **Rewrite** `from='room/<sender_nick>'` per XEP-0045 §7.2.13:
//!    every reflected message MUST carry the room JID + the sender's
//!    nickname as the resource. Drops `to` so the per-occupant fan-out
//!    in [`super::reflector::ReflectorHandler`] can stamp the
//!    occupant-specific `to`.
//! 4. **Stamp** the XEP-0421 stable `<occupant-id>` for the sender,
//!    derived as `HMAC-SHA256(room_secret, room_jid + ':' + sender_bare)`.
//!    Closes the gap PR16 documented: the legacy fan-out path stamped
//!    occupant-id only on presence joins/leaves, never on outgoing
//!    groupchat reflections.

use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::xep::xep0359::{add_stanza_id, is_stanza_id_element};
use crate::xep::xep0421::{generate_occupant_id, set_occupant_id_on_message};
use jid::{BareJid, Jid};
use xmpp_parsers::message::Message;

/// XEP-0045 + XEP-0359 + XEP-0421 canonicalize handler for the room
/// chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucCanonicalizeHandler;

impl RoomHandler for MucCanonicalizeHandler {
    fn name(&self) -> &'static str {
        "muc-canonicalize"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        let Some(sender) = ctx.sender_snapshot() else {
            // OccupancyValidationHandler should have halted before us.
            // Be defensive: skip canonicalization if somehow not.
            return RoomHandlerOutcome::Continue(Vec::new());
        };

        // 1. Strip same-`by=room` stanza-id siblings (typed BareJid
        //    equality so case-folded variants strip; mirrors the 1:1
        //    `CanonicalizeHandler`'s logic).
        message.payloads.retain(|p| {
            if !is_stanza_id_element(p) {
                return true;
            }
            match p.attr("by").and_then(|raw| raw.parse::<BareJid>().ok()) {
                Some(parsed) => parsed != *ctx.room,
                None => true,
            }
        });

        // 2. Stamp a fresh stanza-id.
        let id = ctx.id_gen.fresh_stanza_id();
        add_stanza_id(message, &id, &ctx.room.to_string());

        // 3. Rewrite `from='room/nick'` and drop `to` (the reflector
        //    fills `to` per occupant). If the sender nick is somehow
        //    not representable as a resourcepart, do NOT fall back to
        //    the sender's real full JID — that would leak the user's
        //    identity in the reflection and violate XEP-0045 §7.2.13
        //    (Copilot review on PR #277). Use a non-identifying
        //    `room/unknown` resource instead.
        let from_room_nick = ctx
            .room
            .clone()
            .with_resource_str(&sender.nick)
            .or_else(|_| ctx.room.clone().with_resource_str("unknown"))
            .map(Jid::from)
            .unwrap_or_else(|_| Jid::from(ctx.room.clone()));
        message.from = Some(from_room_nick);
        message.to = None;

        // 4. Stamp XEP-0421 occupant-id (server-derived stable id —
        //    same user across nicks/sessions yields the same id).
        //    Typed JIDs in/out (typed-payloads hard rule); HMAC bytes
        //    are produced at the I/O boundary inside the helper.
        let bare = sender.bare_jid();
        let occupant_id = generate_occupant_id(&bare, ctx.room, ctx.occupant_id_secret);
        set_occupant_id_on_message(message, &occupant_id);

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
    use crate::xep::xep0359::{build_stanza_id_element, extract_stanza_ids};
    use crate::xep::xep0421::{extract_occupant_id_from_message, OccupantId};
    use jid::FullJid;
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
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn run<'a>(
        room: &'a BareJid,
        sender: &'a FullJid,
        nick: &'a str,
        msg: &'a mut Message,
        fresh: &'a str,
    ) -> Vec<OutboundEvent> {
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: nick.to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }];
        let id_gen = FixedIdGenerator(fresh.to_string());
        let ctx = RoomContext {
            room,
            sender_full: sender,
            occupants: &occupants,
            managed_room_forbidden: false,
            id_gen: &id_gen,
            occupant_id_secret: b"test-secret",
        };
        match MucCanonicalizeHandler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(e) => e,
            RoomHandlerOutcome::Halt(_) => panic!("canonicalize never halts"),
        }
    }

    #[test]
    fn xep_0359_strips_same_by_room_stanza_id_siblings() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        // Client-supplied claim with same `by=room`.
        msg.payloads
            .push(build_stanza_id_element("spoofed", "team@conf.example.com"));
        run(&room, &sender, "alice-nick", &mut msg, "fresh-room-id");

        let stamps = extract_stanza_ids(&msg);
        let room_stamps: Vec<_> = stamps
            .iter()
            .filter(|s| s.by == "team@conf.example.com")
            .collect();
        assert_eq!(room_stamps.len(), 1);
        assert_eq!(room_stamps[0].id, "fresh-room-id");
    }

    #[test]
    fn xep_0359_stamps_canonical_stanza_id_by_room() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let stamps = extract_stanza_ids(&msg);
        assert!(stamps
            .iter()
            .any(|s| s.by == "team@conf.example.com" && s.id == "stamp-1"));
    }

    #[test]
    fn xep_0359_preserves_cross_archive_stanza_ids() {
        // A foreign `<stanza-id by='other'/>` must be preserved.
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        msg.payloads
            .push(build_stanza_id_element("alice-A1", "alice@example.com"));
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let stamps = extract_stanza_ids(&msg);
        assert!(stamps
            .iter()
            .any(|s| s.by == "alice@example.com" && s.id == "alice-A1"));
        assert!(stamps
            .iter()
            .any(|s| s.by == "team@conf.example.com" && s.id == "stamp-1"));
    }

    #[test]
    fn xep_0421_stamps_stable_occupant_id_on_groupchat_reflection() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let id = extract_occupant_id_from_message(&msg).expect("occupant-id stamped");

        // Stable: same (user, room) yields same id with the same secret.
        let expected = generate_occupant_id(&sender.to_bare(), &room, b"test-secret");
        assert_eq!(id, expected);
        // Non-empty.
        assert!(!id.as_str().is_empty());
        assert_ne!(id, OccupantId::new("placeholder"));
    }

    #[test]
    fn xep_0421_strips_client_spoofed_occupant_id() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        // Client tries to spoof someone else's occupant-id.
        msg.payloads
            .push(crate::xep::xep0421::build_occupant_id_element(
                &OccupantId::new("attacker-supplied-id"),
            ));
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let id = extract_occupant_id_from_message(&msg).expect("occupant-id present");
        assert_ne!(id, OccupantId::new("attacker-supplied-id"));
        // Exactly one occupant-id element after canonicalization.
        let count = msg
            .payloads
            .iter()
            .filter(|p| crate::xep::xep0421::is_occupant_id_element(p))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn xep_0045_rewrites_from_to_room_slash_nick() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let from = msg.from.as_ref().expect("from set");
        assert_eq!(from.to_string(), "team@conf.example.com/alice-nick");
        assert!(msg.to.is_none(), "to is dropped for fan-out");
    }
}
