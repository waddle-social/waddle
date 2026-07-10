//! XEP-0045 + XEP-0359 + XEP-0421 canonicalization for groupchat
//! reflections.
//!
//! Mutates the in-flight message so subsequent room handlers (archive,
//! reflector) and the eventual wire write see the canonicalized form.
//!
//! Five rewrites in order:
//!
//! 0. **Strip client-supplied MUC-namespace payloads** (`muc`,
//!    `muc#user`, `muc#admin`, `muc#owner`). XEP-0313 §Security "MUC
//!    message spoofing" + XEP-0045 anti-spoofing: an occupant could
//!    otherwise forge `<x xmlns='muc#user'>` affiliation/role/status
//!    signalling that appears to come from `room/nick`, and the
//!    forgery would be reflected live AND persisted into the room
//!    archive (#1251). Canonicalize runs before the archive and
//!    reflector handlers, so this single strip covers both paths.
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
use crate::xep::xep0421::{generate_occupant_id, set_occupant_id_on_message};
use crate::xep::xep_waddle_forums::{extract_forum_action, ForumAction};
use jid::{BareJid, Jid};
use waddle_xmpp_core::mam::ThreadId;
use waddle_xmpp_core::xep0201::{
    build_thread_element, is_thread_element_for_stanza, set_thread_id,
    thread_info_from_message_in_stanza_ns, ThreadInfo, CLIENT_STANZA_NS, SERVER_STANZA_NS,
};
use waddle_xmpp_core::xep0359::{add_stanza_id, is_stanza_id_element, StanzaId};
use xmpp_parsers::message::Message;

/// XEP-0045 + XEP-0359 + XEP-0421 canonicalize handler for the room
/// chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucCanonicalizeHandler;

fn replace_stanza_thread(message: &mut Message, thread_id: impl Into<String>) {
    let thread_id = thread_id.into();
    // An empty `thread_id` would emit a malformed `<thread></thread>`
    // (XEP-0201/RFC 6121 implicitly require a non-empty body). Refuse
    // the rewrite — `ThreadInfo` enforces non-empty via `ThreadId`, and
    // there is no meaningful canonicalization for an empty thread.
    let Some(thread_id) = ThreadId::new(thread_id) else {
        return;
    };
    let parent = thread_info_from_message_in_stanza_ns(message, CLIENT_STANZA_NS)
        .filter(|info| info.id == thread_id)
        .and_then(|info| info.parent);
    message.payloads.retain(|element| {
        !is_thread_element_for_stanza(element, CLIENT_STANZA_NS)
            && !is_thread_element_for_stanza(element, SERVER_STANZA_NS)
    });
    if let Some(parent) = parent {
        message.thread = None;
        message.payloads.push(build_thread_element(
            &ThreadInfo::child(thread_id, parent),
            CLIENT_STANZA_NS,
        ));
    } else {
        set_thread_id(message, thread_id.as_str());
    }
}

impl RoomHandler for MucCanonicalizeHandler {
    fn name(&self) -> &'static str {
        "muc-canonicalize"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        // 0. Strip client-supplied MUC-namespace payloads. XEP-0313
        //    §Security "MUC message spoofing": "the archiving entity
        //    MUST strip any existing <x> element in the
        //    'http://jabber.org/protocol/muc#user' namespace from
        //    messages before archiving them"; XEP-0045 likewise
        //    reserves room signalling (`muc#user` items/statuses/
        //    invites, `muc#admin`/`muc#owner` payloads) to the
        //    service. The presence path (`muc_update.rs`) and the PM
        //    path (`muc_direct.rs`) already strip these — this closes
        //    the groupchat-message gap (#1251).
        //
        //    Done BEFORE the `sender_snapshot()` guard so the
        //    anti-spoofing rewrite is unconditional: the invariant
        //    "nothing client-forgeable in a MUC service namespace ever
        //    reaches archive or reflection" must not depend on the
        //    occupancy gate having run (defense-in-depth). The
        //    client-supplied XEP-0421 `<occupant-id/>` is stripped here
        //    too: the snapshot-less early return below skips step 4's
        //    strip-and-re-stamp, so without this a forged occupant-id
        //    could otherwise survive on that defensive path.
        message.payloads.retain(|p| {
            !crate::muc::is_muc_service_namespace(p.ns().as_str())
                && !crate::xep::xep0421::is_occupant_id_element(p)
        });

        let Some(sender) = ctx.sender_snapshot() else {
            // OccupancyValidationHandler should have halted before us.
            // Be defensive: skip the rest of canonicalization if
            // somehow not — the anti-spoofing strip above already ran.
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
        let by_jid = Jid::from(ctx.room.clone());
        add_stanza_id(message, &StanzaId::new(id.clone(), by_jid));
        match extract_forum_action(message) {
            Some(ForumAction::CreateThread(_)) => {
                replace_stanza_thread(message, &id);
            }
            Some(ForumAction::Reply(reply)) => {
                replace_stanza_thread(message, reply.thread_id);
            }
            _ => {}
        }

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
    use crate::xep::xep0421::{extract_occupant_id_from_message, OccupantId, OccupantIdSecret};
    use crate::xep::xep_waddle_forums::{set_thread_create, ThreadCreate};
    use jid::FullJid;
    use waddle_xmpp_core::xep0201::thread_info_from_message;
    use waddle_xmpp_core::xep0359::{build_stanza_id_element, extract_stanza_ids};
    use xmpp_parsers::message::{Message, MessageType};

    fn jid(s: &str) -> Jid {
        s.parse().expect("valid jid")
    }

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }
    fn tid(s: &str) -> ThreadId {
        ThreadId::new(s).expect("non-empty")
    }

    fn groupchat(room: &BareJid, sender: &FullJid, body: &str) -> Message {
        let mut m = Message::new(Some(Jid::from(room.clone())));
        m.from = Some(Jid::from(sender.clone()));
        m.type_ = MessageType::Groupchat;
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
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
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room,
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
            synthetic_sender_authority: None,
            dispatch_timestamp: 0,
        };
        match MucCanonicalizeHandler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(e) => e,
            RoomHandlerOutcome::Halt(_) => panic!("canonicalize never halts"),
        }
    }

    /// Defense-in-depth (#1251 / Greptile P1): even on the snapshot-less
    /// early-return path — where step 4's occupant-id strip-and-re-stamp
    /// never runs — a client-forged MUC service payload AND a forged
    /// XEP-0421 `<occupant-id/>` MUST be stripped before archive/reflection.
    #[test]
    fn anti_spoof_strip_runs_without_sender_snapshot() {
        let room = bare("team@conf.example.com");
        let sender = full("mallory@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        msg.payloads.push(
            minidom::Element::builder("x", crate::muc::presence::NS_MUC_USER)
                .append(
                    minidom::Element::builder("item", crate::muc::presence::NS_MUC_USER)
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            "owner",
                        )
                        .build(),
                )
                .build(),
        );
        msg.payloads
            .push(crate::xep::xep0421::build_occupant_id_element(
                &crate::xep::xep0421::OccupantId::new("forged-id"),
            ));

        // Empty occupant list ⇒ `sender_snapshot()` is None ⇒ the
        // handler takes the defensive early return after the strip.
        let occupants: Vec<OccupantSnapshot> = Vec::new();
        let id_gen = FixedIdGenerator("ignored".to_string());
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let ctx = RoomContext {
            room: &room,
            sender_full: &sender,
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
            synthetic_sender_authority: None,
            dispatch_timestamp: 0,
        };
        assert!(ctx.sender_snapshot().is_none(), "test requires no snapshot");
        match MucCanonicalizeHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Continue(_) => {}
            RoomHandlerOutcome::Halt(_) => panic!("canonicalize never halts"),
        }

        assert!(
            !msg.payloads
                .iter()
                .any(|p| p.ns() == crate::muc::presence::NS_MUC_USER),
            "forged muc#user must be stripped even without a sender snapshot"
        );
        assert!(
            !msg.payloads
                .iter()
                .any(crate::xep::xep0421::is_occupant_id_element),
            "forged occupant-id must be stripped even without a sender snapshot"
        );
    }

    #[test]
    fn xep_0359_strips_same_by_room_stanza_id_siblings() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        // Client-supplied claim with same `by=room`.
        msg.payloads.push(build_stanza_id_element(
            "spoofed",
            &jid("team@conf.example.com"),
        ));
        run(&room, &sender, "alice-nick", &mut msg, "fresh-room-id");

        let stamps = extract_stanza_ids(&msg);
        let room_jid = jid("team@conf.example.com");
        let room_stamps: Vec<_> = stamps.iter().filter(|s| s.by == room_jid).collect();
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
        let room_jid = jid("team@conf.example.com");
        assert!(stamps.iter().any(|s| s.by == room_jid && s.id == "stamp-1"));
    }

    #[test]
    fn thread_create_root_uses_room_stanza_id_as_thread_id() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "new topic");
        msg.id = Some(xmpp_parsers::message::Id("client-id".to_string()));
        set_thread_id(&mut msg, "spoofed-thread");
        set_thread_create(&mut msg, &ThreadCreate::new("Topic"));

        run(&room, &sender, "alice-nick", &mut msg, "room-stanza-id");

        assert_eq!(
            msg.thread.as_ref().map(|thread| thread.id.as_str()),
            Some("room-stanza-id")
        );
    }

    #[test]
    fn thread_reply_uses_forum_thread_id_when_xep_thread_missing() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "reply");
        crate::xep::xep_waddle_forums::set_thread_reply(
            &mut msg,
            &crate::xep::xep_waddle_forums::ThreadReply::new("topic-root"),
        );

        run(&room, &sender, "alice-nick", &mut msg, "reply-stanza-id");

        assert_eq!(
            msg.thread.as_ref().map(|thread| thread.id.as_str()),
            Some("topic-root")
        );
    }

    #[test]
    fn thread_reply_replaces_conflicting_xep_thread() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "reply");
        set_thread_id(&mut msg, "explicit-thread");
        crate::xep::xep_waddle_forums::set_thread_reply(
            &mut msg,
            &crate::xep::xep_waddle_forums::ThreadReply::new("topic-root"),
        );

        run(&room, &sender, "alice-nick", &mut msg, "reply-stanza-id");

        assert_eq!(
            msg.thread.as_ref().map(|thread| thread.id.as_str()),
            Some("topic-root")
        );
    }

    #[test]
    fn thread_reply_replaces_conflicting_xep_thread_payload() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "reply");
        set_thread_id(&mut msg, "typed-conflict");
        msg.payloads.push(build_thread_element(
            &ThreadInfo::child(tid("payload-conflict"), tid("parent-conflict")),
            CLIENT_STANZA_NS,
        ));
        crate::xep::xep_waddle_forums::set_thread_reply(
            &mut msg,
            &crate::xep::xep_waddle_forums::ThreadReply::new("topic-root"),
        );

        run(&room, &sender, "alice-nick", &mut msg, "reply-stanza-id");

        assert_eq!(
            thread_info_from_message(&msg),
            Some(ThreadInfo::root(tid("topic-root")))
        );
    }

    #[test]
    fn thread_reply_preserves_matching_xep_thread_parent() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "nested reply");
        msg.payloads.push(build_thread_element(
            &ThreadInfo::child(tid("child-thread"), tid("root-thread")),
            CLIENT_STANZA_NS,
        ));
        crate::xep::xep_waddle_forums::set_thread_reply(
            &mut msg,
            &crate::xep::xep_waddle_forums::ThreadReply::new("child-thread"),
        );

        run(&room, &sender, "alice-nick", &mut msg, "reply-stanza-id");

        assert_eq!(
            thread_info_from_message(&msg),
            Some(ThreadInfo::child(tid("child-thread"), tid("root-thread")))
        );
    }

    #[test]
    fn thread_reply_does_not_promote_server_namespace_thread_parent() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "nested reply");
        msg.payloads.push(build_thread_element(
            &ThreadInfo::child(tid("topic-root"), tid("bad-parent")),
            SERVER_STANZA_NS,
        ));
        crate::xep::xep_waddle_forums::set_thread_reply(
            &mut msg,
            &crate::xep::xep_waddle_forums::ThreadReply::new("topic-root"),
        );

        run(&room, &sender, "alice-nick", &mut msg, "reply-stanza-id");

        assert_eq!(
            thread_info_from_message(&msg),
            Some(ThreadInfo::root(tid("topic-root")))
        );
    }

    #[test]
    fn xep_0359_preserves_cross_archive_stanza_ids() {
        // A foreign `<stanza-id by='other'/>` must be preserved.
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        msg.payloads.push(build_stanza_id_element(
            "alice-A1",
            &jid("alice@example.com"),
        ));
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let stamps = extract_stanza_ids(&msg);
        let alice = jid("alice@example.com");
        let room_jid = jid("team@conf.example.com");
        assert!(stamps.iter().any(|s| s.by == alice && s.id == "alice-A1"));
        assert!(stamps.iter().any(|s| s.by == room_jid && s.id == "stamp-1"));
    }

    #[test]
    fn xep_0421_stamps_stable_occupant_id_on_groupchat_reflection() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");
        let id = extract_occupant_id_from_message(&msg).expect("occupant-id stamped");

        // Stable: same (user, room) yields same id with the same secret.
        let secret = OccupantIdSecret::for_testing(b"test-secret".to_vec());
        let expected = generate_occupant_id(&sender.to_bare(), &room, &secret);
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

    /// XEP-0313 §Security "MUC message spoofing" + XEP-0045
    /// anti-spoofing (#1251): a client-supplied
    /// `<x xmlns='http://jabber.org/protocol/muc#user'>` carrying
    /// forged `<item/>` / `<status/>` room signalling MUST be
    /// stripped before the message is reflected or archived.
    #[test]
    fn xep_0045_strips_client_supplied_muc_user_x() {
        let room = bare("team@conf.example.com");
        let sender = full("mallory@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        let ns_user = crate::muc::presence::NS_MUC_USER;
        msg.payloads.push(
            minidom::Element::builder("x", ns_user)
                .append(
                    minidom::Element::builder("item", ns_user)
                        .attr(
                            minidom::rxml::xml_ncname!("jid").to_owned(),
                            "victim@example.com",
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            "owner",
                        )
                        .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                        .build(),
                )
                .append(
                    minidom::Element::builder("status", ns_user)
                        .attr(minidom::rxml::xml_ncname!("code").to_owned(), "110")
                        .build(),
                )
                .build(),
        );

        run(&room, &sender, "mallory", &mut msg, "stamp-1");

        assert!(
            !msg.payloads.iter().any(|p| p.ns() == ns_user),
            "client-supplied muc#user <x> must be stripped before reflect/archive"
        );
        // The legitimate body survives.
        assert!(!msg.bodies.is_empty());
    }

    /// The other MUC service namespaces (`muc`, `muc#admin`,
    /// `muc#owner`) are equally service-authored; a client must not be
    /// able to smuggle payloads in them through a groupchat message.
    #[test]
    fn xep_0045_strips_all_client_supplied_muc_service_namespaces() {
        let room = bare("team@conf.example.com");
        let sender = full("mallory@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        for ns in [
            crate::muc::presence::NS_MUC,
            crate::muc::NS_MUC_ADMIN,
            crate::muc::NS_MUC_OWNER,
        ] {
            msg.payloads
                .push(minidom::Element::builder("x", ns).build());
        }

        run(&room, &sender, "mallory", &mut msg, "stamp-1");

        for ns in [
            crate::muc::presence::NS_MUC,
            crate::muc::presence::NS_MUC_USER,
            crate::muc::NS_MUC_ADMIN,
            crate::muc::NS_MUC_OWNER,
        ] {
            assert!(
                !msg.payloads.iter().any(|p| p.ns() == ns),
                "payloads in `{ns}` must be stripped from occupant groupchat messages"
            );
        }
    }

    /// Non-MUC payloads (the message's own extensions) must survive
    /// the anti-spoofing strip untouched.
    #[test]
    fn xep_0045_strip_preserves_non_muc_payloads() {
        let room = bare("team@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = groupchat(&room, &sender, "hi");
        msg.payloads
            .push(minidom::Element::builder("x", crate::muc::presence::NS_MUC_USER).build());
        msg.payloads.push(
            minidom::Element::builder("active", "http://jabber.org/protocol/chatstates").build(),
        );

        run(&room, &sender, "alice-nick", &mut msg, "stamp-1");

        assert!(
            msg.payloads
                .iter()
                .any(|p| p.name() == "active" && p.ns() == "http://jabber.org/protocol/chatstates"),
            "non-MUC payloads must survive the anti-spoofing strip"
        );
    }
}
