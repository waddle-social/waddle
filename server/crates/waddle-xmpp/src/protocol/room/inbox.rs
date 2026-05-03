//! Per-occupant inbox projection for the room handler chain.
//!
//! Mirrors the legacy `deliver_groupchat_via_room_actor` per-occupant
//! `inbox_storage.upsert(...)` calls (channel-level + thread-level
//! entries) by emitting one
//! [`super::super::event::OutboundEvent::ProjectGroupchatInbox`] event
//! per occupant. The interpreter performs the actual upsert and
//! per-occupant inbox push (XEP-0430).
//!
//! Eligibility mirrors the legacy
//! [`crate::inbox::runtime::should_project_message`] check — body- /
//! subject-bearing messages plus the body-less protocol-event family
//! (reactions, retractions, moderation, file shares, stickers) so MAM
//! replay reconstructs the channel timeline.
//!
//! The sender is projected with `is_recipient = false` (no unread bump)
//! to mirror the legacy `upsert(..., false)` for the sender's own row;
//! every other occupant gets `is_recipient = true`.

use super::super::event::{GroupchatThreadProjection, OutboundEvent};
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::inbox::runtime::{preview_text, should_project_message};
use crate::xep::xep0508::{extract_forum_action, ForumAction};
use jid::BareJid;
use std::collections::HashSet;
use waddle_xmpp_core::xep0201::thread_info_from_message;
use xmpp_parsers::message::Message;

/// Per-occupant inbox projection handler for the room handler chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucInboxHandler;

impl RoomHandler for MucInboxHandler {
    fn name(&self) -> &'static str {
        "xep-0430-muc-inbox-projection"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        if !should_project_message(message) {
            return RoomHandlerOutcome::Continue(Vec::new());
        }
        let thread = thread_projection(message);
        let mut seen: HashSet<BareJid> = HashSet::new();
        let mut events = Vec::with_capacity(ctx.occupants.len());
        let sender_bare = ctx.sender_full.to_bare();
        // Always project the sender's own row (no unread bump). The
        // legacy code did this independent of whether the sender was
        // also enumerated as an occupant, and this matches RFC
        // expectations for "your own outgoing copy" surfacing in your
        // inbox.
        if ctx.project_sender_inbox
            && ctx.sender_snapshot().is_some()
            && seen.insert(sender_bare.clone())
        {
            events.push(OutboundEvent::ProjectGroupchatInbox {
                owner: sender_bare,
                room: ctx.room.clone(),
                message: Box::new(message.clone()),
                is_recipient: false,
                thread: thread.clone(),
                dispatch_timestamp: ctx.dispatch_timestamp,
            });
        }
        for occupant in ctx.recipient_occupants() {
            let bare = occupant.bare_jid();
            if !seen.insert(bare.clone()) {
                continue;
            }
            events.push(OutboundEvent::ProjectGroupchatInbox {
                owner: bare,
                room: ctx.room.clone(),
                message: Box::new(message.clone()),
                is_recipient: true,
                thread: thread.clone(),
                dispatch_timestamp: ctx.dispatch_timestamp,
            });
        }
        RoomHandlerOutcome::Continue(events)
    }
}

/// Resolve thread metadata for actual roots. Replies must not publish
/// title/author metadata, otherwise a bot or later human reply can overwrite
/// the root's inbox projection.
fn thread_projection(message: &Message) -> Option<GroupchatThreadProjection> {
    // `parser_utils::reattach_thread_parent` moves `<thread parent=...>` into
    // payloads and clears `message.thread`, so reading only the typed field
    // would miss every XEP-0201 child reply on MAM replay.
    let info = thread_info_from_message(message)?;
    let forum_title = extract_forum_action(message).and_then(|action| match action {
        ForumAction::CreateThread(tc) => Some(tc.title),
        _ => None,
    });
    let is_thread_root = info.parent.is_none() && message.id.as_deref() == Some(info.id.as_str());
    let title = forum_title.or_else(|| is_thread_root.then(|| preview_text(message)).flatten());
    let author_nick = title.as_ref().and_then(|_| {
        message
            .from
            .as_ref()
            .and_then(|jid| jid.resource().map(|r| r.to_string()))
    });
    Some(GroupchatThreadProjection {
        thread_id: info.id.as_str().to_owned(),
        title,
        author_nick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0421::OccupantIdSecret;
    use jid::{FullJid, Jid};
    use waddle_xmpp_core::parser_utils::reattach_thread_parent;
    use waddle_xmpp_core::xep0201::{set_thread_id, CLIENT_STANZA_NS};
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn occ(full_jid: FullJid, nick: &str) -> OccupantSnapshot {
        OccupantSnapshot {
            full_jid,
            nick: nick.to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }
    }

    fn groupchat(room: &BareJid, sender_nick_jid: &FullJid, body: &str) -> Message {
        let mut m = Message::new(None::<jid::Jid>);
        m.from = Some(Jid::from(sender_nick_jid.clone()));
        m.type_ = MessageType::Groupchat;
        if !body.is_empty() {
            m.bodies.insert(String::new(), Body(body.to_string()));
        }
        let _ = room;
        m
    }

    fn run_with(
        room: &BareJid,
        sender_full: &FullJid,
        occupants: &[OccupantSnapshot],
        msg: &mut Message,
    ) -> Vec<OutboundEvent> {
        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
            sender_full,
            occupants,
            managed_room_forbidden: false,
            room_moderated: false,
            id_gen: &id_gen,
            occupant_id_secret: &secret,
            sender_nickname_generation: 0,
            project_sender_inbox: true,
            dispatch_timestamp: 0,
        };
        match MucInboxHandler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(events) => events,
            RoomHandlerOutcome::Halt(_) => panic!("inbox handler never halts"),
        }
    }

    fn projections(events: &[OutboundEvent]) -> Vec<(BareJid, bool, Option<String>)> {
        events
            .iter()
            .filter_map(|event| match event {
                OutboundEvent::ProjectGroupchatInbox {
                    owner,
                    is_recipient,
                    thread,
                    ..
                } => Some((
                    owner.clone(),
                    *is_recipient,
                    thread.as_ref().map(|t| t.thread_id.clone()),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn xep_0430_projects_per_occupant_inbox_with_sender_unread_false() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob = full("bob@example.com/desk");
        let occupants = vec![occ(alice.clone(), "alice"), occ(bob.clone(), "bob")];
        let mut msg = groupchat(&room, &alice, "hi everyone");
        let events = run_with(&room, &alice, &occupants, &mut msg);
        let proj = projections(&events);
        assert_eq!(proj.len(), 2);
        let alice_bare: BareJid = "alice@example.com".parse().unwrap();
        let bob_bare: BareJid = "bob@example.com".parse().unwrap();
        let alice_row = proj.iter().find(|(o, _, _)| o == &alice_bare).unwrap();
        let bob_row = proj.iter().find(|(o, _, _)| o == &bob_bare).unwrap();
        assert!(!alice_row.1, "sender's own row must not bump unread");
        assert!(bob_row.1, "other occupants get unread bumped");
    }

    #[test]
    fn xep_0430_skips_bodyless_protocol_events_not_on_allowlist() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let occupants = vec![occ(alice.clone(), "alice")];
        let mut msg = groupchat(&room, &alice, ""); // no body, no protocol payloads
        let events = run_with(&room, &alice, &occupants, &mut msg);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboundEvent::ProjectGroupchatInbox { .. })),
            "bodyless message with no protocol-event payload must not project"
        );
    }

    #[test]
    fn xep_0430_dedups_multi_session_occupant_to_one_inbox_row() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob_web = full("bob@example.com/web");
        let bob_desk = full("bob@example.com/desk");
        let occupants = vec![
            occ(alice.clone(), "alice"),
            occ(bob_web, "bob"),
            occ(bob_desk, "bob"),
        ];
        let mut msg = groupchat(&room, &alice, "hello bob");
        let events = run_with(&room, &alice, &occupants, &mut msg);
        let proj = projections(&events);
        // Sender + one bob row (the second bob session collapsed).
        assert_eq!(proj.len(), 2);
    }

    #[test]
    fn thread_projection_does_not_publish_reply_metadata() {
        let room = bare("team@conf.example.com");
        let bot = full("team@conf.example.com/waddle");
        let mut msg = groupchat(&room, &bot, "AI answer");
        msg.id = Some("reply-stanza".to_string());
        set_thread_id(&mut msg, "root-stanza");

        let thread = thread_projection(&msg).expect("thread projection");

        assert_eq!(thread.thread_id, "root-stanza");
        assert_eq!(thread.title, None);
        assert_eq!(thread.author_nick, None);
    }

    #[test]
    fn thread_projection_publishes_root_metadata() {
        let room = bare("team@conf.example.com");
        let alice = full("team@conf.example.com/alice");
        let mut msg = groupchat(&room, &alice, "Root prompt");
        msg.id = Some("root-stanza".to_string());
        set_thread_id(&mut msg, "root-stanza");

        let thread = thread_projection(&msg).expect("thread projection");

        assert_eq!(thread.thread_id, "root-stanza");
        assert_eq!(thread.title.as_deref(), Some("Root prompt"));
        assert_eq!(thread.author_nick.as_deref(), Some("alice"));
    }

    #[test]
    fn thread_projection_reads_xep_0201_payload_when_typed_thread_is_cleared() {
        // After reattach_thread_parent runs at the inbound parse boundary,
        // <thread parent='root-1'>child-2</thread> lives in payloads and
        // message.thread is None. The inbox must still resolve the thread id.
        let room = bare("team@conf.example.com");
        let alice = full("team@conf.example.com/alice");
        let mut msg = groupchat(&room, &alice, "child reply");
        msg.id = Some("reply-stanza".to_string());
        set_thread_id(&mut msg, "child-2");
        reattach_thread_parent(&mut msg, "root-1".to_string(), CLIENT_STANZA_NS);
        assert!(
            msg.thread.is_none(),
            "reattach_thread_parent must clear the typed thread"
        );

        let thread = thread_projection(&msg).expect("payload-form thread projection");

        assert_eq!(thread.thread_id, "child-2");
        // Child replies must never publish title/author metadata, otherwise
        // they overwrite the root's inbox row on MAM replay.
        assert_eq!(thread.title, None);
        assert_eq!(thread.author_nick, None);
    }
}
