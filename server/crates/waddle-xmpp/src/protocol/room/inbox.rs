//! Durable-recipient inbox projection for the room handler chain.
//!
//! Mirrors the legacy `deliver_groupchat_via_room_actor` groupchat
//! `inbox_storage.upsert(...)` calls (channel-level + thread-level
//! entries) by emitting one sender row plus one
//! [`super::super::event::OutboundEvent::ProjectGroupchatInbox`] event
//! per durable affiliation-derived recipient. The interpreter performs
//! the actual upsert and groupchat notification candidate projection.
//!
//! Eligibility mirrors the legacy
//! [`crate::inbox::runtime::should_project_message`] check — body- /
//! subject-bearing messages plus the body-less protocol-event family
//! (reactions, retractions, moderation, file shares, stickers) so MAM
//! replay reconstructs the channel timeline.
//!
//! The sender is projected with `is_recipient = false` (no unread bump)
//! to mirror the legacy `upsert(..., false)` for the sender's own row;
//! durable recipients get `is_recipient = true`.

use super::super::event::{GroupchatThreadProjection, OutboundEvent};
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::inbox::runtime::{preview_text, should_project_message};
use crate::types::Role;
use crate::xep::xep0508::{extract_forum_action, ForumAction};
use jid::BareJid;
use std::collections::HashSet;
use waddle_xmpp_core::xep0201::thread_info_from_message;
use xmpp_parsers::message::Message;

/// XEP-0513 §"Multi-User Chats Permissions": server-internal default
/// for `mentions#channel` is `moderators` — only senders with role
/// `Role::Moderator` may broadcast `urn:xmpp:mentions:0#channel` for
/// push purposes. Returning `false` when the sender has no occupancy
/// snapshot is the strict reading of XEP-0045 §7.4 (only joined
/// occupants may message the room) combined with XEP-0513 §"Multi-User
/// Chats Permissions" (receiving entities SHOULD ignore mentions from
/// senders below the minimum role).
fn sender_may_broadcast_channel_mention(ctx: &RoomContext<'_>) -> bool {
    ctx.sender_snapshot()
        .is_some_and(|sender| sender.role >= Role::Moderator)
}

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
        let live_recipient_bares: HashSet<BareJid> = ctx
            .recipient_occupants()
            .map(|occupant| occupant.bare_jid())
            .collect();
        let mut events = Vec::with_capacity(1 + ctx.durable_recipient_bare_jids.len());
        let sender_bare = ctx.sender_full.to_bare();
        // XEP-0513 §"Multi-User Chats Permissions": freeze the sender's
        // permission to broadcast `urn:xmpp:mentions:0#channel` for push
        // purposes at dispatch time, before the per-recipient fan-out.
        // Same shape as `room_members_only`: one typed bool snapshotted
        // here, read by the T0 candidate classifier; never re-derived
        // per recipient.
        let sender_can_broadcast_channel_mention = sender_may_broadcast_channel_mention(ctx);
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
                owner: sender_bare.clone(),
                room: ctx.room.clone(),
                message: Box::new(message.clone()),
                is_recipient: false,
                is_durable_recipient: false,
                is_live_occupant: true,
                room_members_only: ctx.room_members_only,
                sender_can_broadcast_channel_mention,
                thread: thread.clone(),
                dispatch_timestamp: ctx.dispatch_timestamp,
            });
        }
        for bare in ctx.durable_recipient_bare_jids {
            if bare == &sender_bare || !seen.insert(bare.clone()) {
                continue;
            }
            events.push(OutboundEvent::ProjectGroupchatInbox {
                owner: bare.clone(),
                room: ctx.room.clone(),
                message: Box::new(message.clone()),
                is_recipient: true,
                is_durable_recipient: true,
                is_live_occupant: live_recipient_bares.contains(bare),
                room_members_only: ctx.room_members_only,
                sender_can_broadcast_channel_mention,
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
    let is_thread_root = info.parent.is_none()
        && message.id.as_ref().map(|id| id.0.as_str()) == Some(info.id.as_str());
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
    use xmpp_parsers::message::{Message, MessageType};

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
            m.bodies
                .insert(xmpp_parsers::message::Lang::new(), body.to_string());
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
        run_with_durable(room, sender_full, occupants, &[], false, msg)
    }

    fn run_with_durable(
        room: &BareJid,
        sender_full: &FullJid,
        occupants: &[OccupantSnapshot],
        durable_recipient_bare_jids: &[BareJid],
        room_members_only: bool,
        msg: &mut Message,
    ) -> Vec<OutboundEvent> {
        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
            sender_full,
            occupants,
            durable_recipient_bare_jids,
            managed_room_forbidden: false,
            room_moderated: false,
            room_members_only,
            pin_permission: crate::muc::PinPermission::default(),
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
    fn xep_0430_projects_durable_inbox_with_sender_unread_false() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob = full("bob@example.com/desk");
        let occupants = vec![occ(alice.clone(), "alice"), occ(bob.clone(), "bob")];
        let durable_recipients = vec![bob.to_bare()];
        let mut msg = groupchat(&room, &alice, "hi everyone");
        let events = run_with_durable(
            &room,
            &alice,
            &occupants,
            &durable_recipients,
            false,
            &mut msg,
        );
        let proj = projections(&events);
        assert_eq!(proj.len(), 2);
        let alice_bare: BareJid = "alice@example.com".parse().unwrap();
        let bob_bare: BareJid = "bob@example.com".parse().unwrap();
        let alice_row = proj.iter().find(|(o, _, _)| o == &alice_bare).unwrap();
        let bob_row = proj.iter().find(|(o, _, _)| o == &bob_bare).unwrap();
        assert!(!alice_row.1, "sender's own row must not bump unread");
        assert!(bob_row.1, "durable recipients get unread bumped");
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
    fn xep_0430_dedups_multi_session_live_durable_recipient_to_one_inbox_row() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob_web = full("bob@example.com/web");
        let bob_desk = full("bob@example.com/desk");
        let bob_bare = bob_web.to_bare();
        let occupants = vec![
            occ(alice.clone(), "alice"),
            occ(bob_web, "bob"),
            occ(bob_desk, "bob"),
        ];
        let durable_recipients = vec![bob_bare];
        let mut msg = groupchat(&room, &alice, "hello bob");
        let events = run_with_durable(
            &room,
            &alice,
            &occupants,
            &durable_recipients,
            false,
            &mut msg,
        );
        let proj = projections(&events);
        // Sender + one bob row (the second bob session collapsed).
        assert_eq!(proj.len(), 2);
    }

    #[test]
    fn xep_0430_projects_durable_affiliates_without_duplicating_live_occupants() {
        let room = bare("team@conf.example.com");
        let alice = full("alice@example.com/web");
        let bob = full("bob@example.com/desk");
        let dave = full("dave@example.com/phone");
        let charlie = bare("charlie@example.com");
        let occupants = vec![
            occ(alice.clone(), "alice"),
            occ(bob, "bob"),
            occ(dave, "dave"),
        ];
        let durable_recipients = vec![bare("bob@example.com"), charlie];
        let mut msg = groupchat(&room, &alice, "hello everyone");

        let events = run_with_durable(
            &room,
            &alice,
            &occupants,
            &durable_recipients,
            true,
            &mut msg,
        );

        let projections: Vec<(String, bool, bool, bool, bool)> = events
            .iter()
            .filter_map(|event| match event {
                OutboundEvent::ProjectGroupchatInbox {
                    owner,
                    is_recipient,
                    is_durable_recipient,
                    is_live_occupant,
                    room_members_only,
                    ..
                } => Some((
                    owner.to_string(),
                    *is_recipient,
                    *is_durable_recipient,
                    *is_live_occupant,
                    *room_members_only,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            projections,
            vec![
                ("alice@example.com".to_string(), false, false, true, true),
                ("bob@example.com".to_string(), true, true, true, true),
                ("charlie@example.com".to_string(), true, true, false, true),
            ]
        );
        assert!(
            !projections
                .iter()
                .any(|(owner, _, _, _, _)| owner == "dave@example.com"),
            "live occupants outside the durable affiliation set must not get inbox/push projection"
        );
    }

    /// XEP-0513 §"Multi-User Chats Permissions" §304: the typed
    /// frozen permission snapshot is taken at room-dispatch time, NOT
    /// later. This test pins the helper: only senders whose frozen
    /// occupancy role is `Role::Moderator` may broadcast a channel
    /// mention; participants and visitors get `false`. The class
    /// downgrade itself is exercised in the `groupchat_inbox.rs`
    /// classifier tests — this test only locks the snapshot helper.
    #[test]
    fn xep0513_sender_may_broadcast_channel_mention_requires_moderator() {
        let room = bare("team@conf.example.com");
        let moderator = full("alice@example.com/web");
        let participant = full("bob@example.com/desk");
        let outsider = full("eve@example.com/cli");

        let mut moderator_occ = occ(moderator.clone(), "alice");
        moderator_occ.role = Role::Moderator;
        let mut participant_occ = occ(participant.clone(), "bob");
        participant_occ.role = Role::Participant;
        let occupants = vec![moderator_occ, participant_occ];

        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());

        for (sender, expected, label) in [
            (&moderator, true, "Role::Moderator MUST be permitted"),
            (
                &participant,
                false,
                "Role::Participant MUST NOT be permitted — XEP-0513 §304 \
                 default policy is `mentions#channel = moderators`",
            ),
            // Sender outside the occupancy snapshot (XEP-0045 §7.4
            // gate would already reject this before the inbox handler
            // runs, but the helper MUST still deny defensively).
            (
                &outsider,
                false,
                "absent occupancy snapshot MUST deny channel broadcast",
            ),
        ] {
            let ctx = RoomContext {
                room: &room,
                sender_full: sender,
                occupants: &occupants,
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
            assert_eq!(
                sender_may_broadcast_channel_mention(&ctx),
                expected,
                "{label}"
            );
        }
    }

    /// Lock-in: the typed `sender_can_broadcast_channel_mention` field
    /// flows from the frozen sender role to EVERY emitted
    /// `ProjectGroupchatInbox` event in a single dispatch (sender row
    /// plus every durable recipient row). This is the same snapshot
    /// invariant as `room_members_only` (Q5 frozen-snapshot semantic).
    #[test]
    fn xep0513_channel_permission_is_frozen_per_dispatch_across_all_recipients() {
        let room = bare("team@conf.example.com");
        let moderator = full("alice@example.com/web");
        let participant = full("bob@example.com/desk");
        let mut mod_occ = occ(moderator.clone(), "alice");
        mod_occ.role = Role::Moderator;
        let participant_occ = occ(participant.clone(), "bob");
        let occupants = vec![mod_occ, participant_occ];
        let durable_recipients = vec![bob_bare(), bare("dave@example.com")];

        let mut msg = groupchat(&room, &moderator, "hello everyone");
        let events = run_with_durable(
            &room,
            &moderator,
            &occupants,
            &durable_recipients,
            false,
            &mut msg,
        );

        let permissions: Vec<bool> = events
            .iter()
            .filter_map(|event| match event {
                OutboundEvent::ProjectGroupchatInbox {
                    sender_can_broadcast_channel_mention,
                    ..
                } => Some(*sender_can_broadcast_channel_mention),
                _ => None,
            })
            .collect();
        assert!(
            permissions.iter().all(|granted| *granted),
            "moderator sender must produce a permission bool of `true` on \
             every emitted ProjectGroupchatInbox event — frozen snapshot \
             must not vary per recipient"
        );

        // Same dispatch with a participant sender → `false` everywhere.
        let mut msg = groupchat(&room, &participant, "another message");
        let events = run_with_durable(
            &room,
            &participant,
            &occupants,
            &durable_recipients,
            false,
            &mut msg,
        );
        let permissions: Vec<bool> = events
            .iter()
            .filter_map(|event| match event {
                OutboundEvent::ProjectGroupchatInbox {
                    sender_can_broadcast_channel_mention,
                    ..
                } => Some(*sender_can_broadcast_channel_mention),
                _ => None,
            })
            .collect();
        assert!(
            permissions.iter().all(|granted| !*granted),
            "participant sender must produce a permission bool of `false` \
             on every emitted ProjectGroupchatInbox event"
        );
    }

    fn bob_bare() -> BareJid {
        bare("bob@example.com")
    }

    #[test]
    fn thread_projection_does_not_publish_reply_metadata() {
        let room = bare("team@conf.example.com");
        let bot = full("team@conf.example.com/waddle");
        let mut msg = groupchat(&room, &bot, "AI answer");
        msg.id = Some(xmpp_parsers::message::Id("reply-stanza".to_string()));
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
        msg.id = Some(xmpp_parsers::message::Id("root-stanza".to_string()));
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
        msg.id = Some(xmpp_parsers::message::Id("reply-stanza".to_string()));
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
