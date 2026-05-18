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
//! 1. Detects the subject-change shape: non-empty `subjects` plus no
//!    `<body/>` content (empty/whitespace-only bodies count as no
//!    body — see the inline rationale on the hostile-client guard).
//! 2. Enforces §8.1's role-based policy (moderators always; participants
//!    only in unmoderated rooms; visitors never) against the frozen
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
use super::super::handlers::errors::send_message_error;
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::types::Role;
use chrono::{DateTime, Utc};
use jid::Jid;
use minidom::Element;
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
        //
        // Hostile-client guard: an empty (or whitespace-only) <body/>
        // is treated as no-body for shape-detection purposes. Without
        // this, a visitor can send `<subject>x</subject><body/>` and
        // skip §8.1 authorization while clients (Conversations, Gajim,
        // Dino) still render the `<subject>` as a topic change. The
        // strict §8.1 reading is "no body element at all"; we tighten
        // to "no body content" because empty bodies have no legitimate
        // groupchat-message use and are the natural exfiltration path.
        if message.subjects.is_empty() || message.bodies.values().any(|b| !b.0.trim().is_empty()) {
            return RoomHandlerOutcome::Continue(Vec::new());
        }

        // Subject-change confirmed. Strip any empty/whitespace-only
        // body entries the hostile-client guard tolerated for
        // shape-detection — downstream archive and reflector will
        // serialize whatever remains in `bodies`, and a §8.1 broadcast
        // MUST be subject-only on the wire (no `<body/>`).
        message.bodies.retain(|_, body| !body.0.trim().is_empty());

        // Sender snapshot — `OccupancyValidationHandler` runs first and
        // halts when this is missing; defensive Continue if somehow not.
        let Some(sender) = ctx.sender_snapshot() else {
            return RoomHandlerOutcome::Continue(Vec::new());
        };

        // §8.1 authorization: moderators always; participants only in
        // unmoderated rooms; visitors / no-role never. The chain is
        // stateless against a frozen snapshot — `RoomContext` already
        // carries both inputs (`sender.role` and `room_moderated`) so
        // there is no `&MucRoom` to borrow. `OccupancyValidationHandler`
        // ahead of this handler ensures the sender's role/affiliation
        // were materialized into the snapshot at dispatch start, so the
        // check observes the same role assignment that produced the
        // canonicalized `from='room/nick'`.
        let allowed = match sender.role {
            Role::Moderator => true,
            Role::Participant => !ctx.room_moderated,
            Role::Visitor | Role::None => false,
        };
        if !allowed {
            let reply = forbidden_reply(message, ctx);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        }

        // Capture every `<subject xml:lang='...'>` variant so the
        // join-time replay built by `build_subject_message` from the
        // persisted state matches the live broadcast that
        // `ReflectorHandler` is about to emit. Persisting only the
        // first language would silently lose localized subjects on
        // next-join replay.
        let texts = crate::muc::RoomSubjectTexts::from_message_subjects(&message.subjects);

        // Single dispatch clock (matches the `dispatch_timestamp`
        // sharing precedent in inbox.rs / archive.rs). Conversion
        // never fails for sane timestamps; on the impossible
        // overflow case we fall back to `Utc::now()` so we still
        // produce a usable stamp.
        let set_at: DateTime<Utc> =
            DateTime::from_timestamp(ctx.dispatch_timestamp, 0).unwrap_or_else(Utc::now);

        let event = OutboundEvent::PersistRoomSubject {
            room: ctx.room.clone(),
            texts,
            setter: sender.bare_jid(),
            setter_nick: sender.nick.clone(),
            set_at,
        };
        RoomHandlerOutcome::Continue(vec![event])
    }
}

/// Build the XEP-0045 §8.1 typed `<forbidden type='auth'/>` reply
/// addressed from the room JID back to the sender.
///
/// Clones `incoming` so the rejected `<subject/>` and any other
/// payloads (stanza-id, occupant-id stamped by canonicalize) flow back
/// to the sender — clients use the original stanza context to
/// correlate the rejection. Address overrides come last because by
/// the time this handler runs `MucCanonicalizeHandler` has already
/// rewritten `incoming.from` to `room/nick` and cleared `incoming.to`,
/// so a verbatim swap-and-reply would produce nonsense addresses.
fn forbidden_reply(incoming: &Message, ctx: &RoomContext<'_>) -> Message {
    let mut reply = incoming.clone();
    reply.from = Some(Jid::from(ctx.room.clone()));
    reply.to = Some(Jid::from(ctx.sender_full.clone()));
    reply.type_ = MessageType::Error;
    let error = StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::Forbidden,
        "en",
        "Sender is not permitted to change the room subject.",
    );
    reply.payloads.push(Element::from(error));
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
            durable_recipient_bare_jids: &[],
            managed_room_forbidden: false,
            room_moderated,
            room_members_only: false,
            pin_permission: crate::muc::PinPermission::default(),
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
            texts,
            setter,
            setter_nick,
            ..
        } = persist
        else {
            unreachable!()
        };
        assert_eq!(
            texts.get(""),
            Some("New topic"),
            "default-language subject persisted"
        );
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
    fn subject_change_captures_every_xml_lang_variant_into_persist_event() {
        // Multi-language subjects are §8.1-conformant and the live
        // broadcast carries every <subject xml:lang='...'>. The
        // handler must persist every variant so the join-time replay
        // built by `build_subject_message` matches.
        let room = bare("team@muc.example.com");
        let sender = full("alice@example.com/web");
        let mut msg = subject_change(&room, &sender, "Default subject");
        msg.subjects
            .insert("en".to_string(), Subject("English subject".to_string()));
        msg.subjects
            .insert("fr".to_string(), Subject("Sujet français".to_string()));
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
        let OutboundEvent::PersistRoomSubject { texts, .. } = persist else {
            unreachable!()
        };
        assert_eq!(texts.len(), 3, "every language variant captured");
        assert_eq!(texts.get(""), Some("Default subject"));
        assert_eq!(texts.get("en"), Some("English subject"));
        assert_eq!(texts.get("fr"), Some("Sujet français"));
    }

    #[test]
    fn empty_body_does_not_bypass_subject_change_authorization() {
        // Hostile-client guard: a visitor sending
        // `<subject>x</subject><body xml:lang="x"></body>` cannot skip
        // §8.1 authorization just because xmpp_parsers populates the
        // bodies map with an empty entry. The handler treats whitespace-
        // only bodies as no-body for shape detection, so this still
        // halts with <forbidden/>.
        let room = bare("team@muc.example.com");
        let sender = full("eve@example.com/web");
        let mut msg = subject_change(&room, &sender, "Topic via empty-body trick");
        msg.bodies.insert(String::new(), Body(String::new()));
        let outcome = run(&mut msg, &room, &sender, "eve-nick", Role::Visitor, false);
        let RoomHandlerOutcome::Halt(events) = outcome else {
            panic!(
                "empty-body must not bypass §8.1 authz; visitor must be denied, got {outcome:?}"
            );
        };
        assert_send_forbidden(&events);
    }

    #[test]
    fn whitespace_only_body_does_not_bypass_subject_change_authorization() {
        // Same hostile-client guard, with whitespace inside the body
        // instead of an empty string — still no meaningful body content.
        let room = bare("team@muc.example.com");
        let sender = full("eve@example.com/web");
        let mut msg = subject_change(&room, &sender, "Whitespace bypass attempt");
        msg.bodies
            .insert(String::new(), Body("   \t\n  ".to_string()));
        let outcome = run(&mut msg, &room, &sender, "eve-nick", Role::Visitor, false);
        let RoomHandlerOutcome::Halt(events) = outcome else {
            panic!("whitespace-only body must not bypass §8.1 authz, got {outcome:?}");
        };
        assert_send_forbidden(&events);
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
        use crate::Stanza;
        use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
        let stanza = events
            .iter()
            .find_map(|e| match e {
                OutboundEvent::SendStanza(s) => Some(s.as_ref()),
                _ => None,
            })
            .expect("typed error reply emitted to sender");
        let Stanza::Message(reply) = stanza else {
            panic!("expected SendStanza(Message), got {stanza:?}");
        };
        assert_eq!(
            reply.type_,
            MessageType::Error,
            "§8.1 deny replies as type='error'"
        );
        let error_elem = reply
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload attached");
        let parsed = StanzaError::try_from(error_elem.clone()).expect("typed StanzaError parse");
        assert_eq!(parsed.type_, ErrorType::Auth, "§8.1 deny is type='auth'");
        assert_eq!(
            parsed.defined_condition,
            DefinedCondition::Forbidden,
            "§8.1 deny carries <forbidden/>"
        );
    }
}
