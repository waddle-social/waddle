//! Channel pinned-messages handler (#414).
//!
//! Detects `<message type='groupchat'>` stanzas carrying a Waddle
//! `<pinned target='…'/>` or `<unpinned target='…'/>` element
//! (`urn:waddle:pin:0`), enforces affiliation-based authorization, and
//! emits a typed pair of events:
//!
//! 1. [`OutboundEvent::ApplyPinChange`] — mutates `MucRoom.pinned_entries`
//!    via the room actor.
//! 2. [`OutboundEvent::BroadcastRoomSystemMessage`] — emits a
//!    `<message type='groupchat' from='room@conf'>` carrying the same
//!    `<pinned/>` or `<unpinned/>` element (with `<preview/>` child for
//!    pin events), archived in MAM and fanned out to occupants.
//!
//! Authorization is **hard-coded** to admins/owners in this handler;
//! per-room configurable `urn:waddle:roomconfig:pinpermission` ships
//! in #415 and will replace the hard-coded gate.
//!
//! The handler runs after `MucCanonicalizeHandler` so the in-flight
//! message has its canonical XEP-0359 `<stanza-id by='room'/>` and
//! XEP-0421 occupant-id stamps; it runs before `MucArchiveHandler` and
//! `ReflectorHandler` so the original pin/unpin user message does
//! **not** enter the regular archive/reflect path. The system message
//! emitted via `BroadcastRoomSystemMessage` is the durable record of
//! the pin event in MAM.

use super::super::event::OutboundEvent;
use super::super::handlers::errors::{message_error_reply, send_message_error};
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::muc::pin::{PinChangeRequest, PinPreview};
use crate::types::{Affiliation, Role};
use crate::xep::xep_waddle_pin::{extract_pin_intent_from_message, PinIntent, NS_WADDLE_PIN_V0};
use chrono::{DateTime, Utc};
use jid::Jid;
use minidom::Element;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Body, Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Pin-event handler for the MUC room chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucPinHandler;

impl RoomHandler for MucPinHandler {
    fn name(&self) -> &'static str {
        "waddle-pin"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        // Look for a well-formed pin/unpin marker. The extractor rejects
        // ambiguous (both pinned and unpinned), missing target, empty
        // target, overlong target — those should be bad-request, not
        // silent pass-through.
        let intent_opt = extract_pin_intent_from_message(message);

        // Distinguish "no marker present" from "marker but malformed".
        if intent_opt.is_none() && !carries_pin_marker_namespace(message) {
            return RoomHandlerOutcome::Continue(Vec::new());
        }
        let Some(intent) = intent_opt else {
            // A urn:waddle:pin:0 element was present but malformed.
            let reply = bad_request_reply(message);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        };

        // Sender snapshot — `OccupancyValidationHandler` runs first and
        // halts when this is missing; defensive Continue if somehow not.
        let Some(sender) = ctx.sender_snapshot() else {
            return RoomHandlerOutcome::Continue(Vec::new());
        };

        // #415: gate on the room's pin_permission. AdminsOnly retains
        // the original Owner/Admin check; Anyone requires at least
        // Member affiliation AND Role::Participant|Moderator —
        // Visitors (silenced occupants in moderated rooms, XEP-0045
        // §5.1.2) cannot pin because they cannot speak; non-members
        // (Affiliation::None) are excluded so a casual occupant of an
        // open room cannot pin without being explicitly admitted.
        // Outcasts are filtered at the join gate but defensively
        // excluded anyway via both checks.
        let allowed = match ctx.pin_permission {
            crate::muc::PinPermission::AdminsOnly => {
                matches!(sender.affiliation, Affiliation::Owner | Affiliation::Admin)
            }
            crate::muc::PinPermission::Anyone => {
                matches!(
                    sender.affiliation,
                    Affiliation::Owner | Affiliation::Admin | Affiliation::Member
                ) && matches!(sender.role, Role::Participant | Role::Moderator)
            }
        };
        if !allowed {
            let reply = forbidden_reply(message);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        }

        let dispatched_at: DateTime<Utc> =
            DateTime::from_timestamp(ctx.dispatch_timestamp, 0).unwrap_or_else(Utc::now);
        let pinner_jid = sender.bare_jid();
        let pinner_nick = sender.nick.clone();
        let target_stanza_id =
            StanzaId::new(intent.target().to_owned(), Jid::from(ctx.room.clone()));

        let request = match intent {
            PinIntent::Pin { .. } => PinChangeRequest::Pin {
                target_stanza_id,
                pinner_jid,
                pinner_nick,
                pinned_at: dispatched_at,
            },
            PinIntent::Unpin { .. } => PinChangeRequest::Unpin {
                target_stanza_id,
                pinner_jid,
                pinner_nick,
                reason: None,
            },
        };

        RoomHandlerOutcome::Halt(vec![OutboundEvent::ApplyPinChange {
            room: ctx.room.clone(),
            request,
        }])
    }
}

/// True if any payload of `message` is in the urn:waddle:pin:0
/// namespace — used to distinguish "no pin intent" from "pin intent
/// but malformed".
fn carries_pin_marker_namespace(message: &Message) -> bool {
    message
        .payloads
        .iter()
        .any(|elem| elem.ns() == NS_WADDLE_PIN_V0)
}

/// Build the system message broadcast on a successful pin. The
/// system message carries a structured `<pin-event xmlns='urn:waddle:pin:0'
/// action='pinned' …>` payload — clients parse pin events off the
/// stable `pin-event` element regardless of action.
pub fn build_pinned_system_message(
    room: &jid::BareJid,
    pinner_jid: &jid::BareJid,
    pinner_nick: &str,
    target_stanza_id: &StanzaId,
    preview: Option<&PinPreview>,
    reason: Option<&str>,
) -> Message {
    let element = build_pin_event_element("pinned", pinner_jid, target_stanza_id, preview, reason);
    new_room_message(
        room,
        format!(
            "{} pinned a message",
            display_label(pinner_nick, pinner_jid)
        ),
        element,
    )
}

/// Build the system message broadcast on a successful unpin (or on a
/// XEP-0424 retraction cascade, in which case `reason` is `Some("retracted")`).
/// Wraps the action in the same `<pin-event>` envelope as the pin path.
pub fn build_unpinned_system_message(
    room: &jid::BareJid,
    pinner_jid: &jid::BareJid,
    pinner_nick: &str,
    target_stanza_id: &StanzaId,
    reason: Option<&str>,
) -> Message {
    let element = build_pin_event_element("unpinned", pinner_jid, target_stanza_id, None, reason);
    let body = if reason == Some("retracted") {
        "Pinned message was retracted by its author".to_owned()
    } else {
        format!(
            "{} unpinned a message",
            display_label(pinner_nick, pinner_jid)
        )
    };
    new_room_message(room, body, element)
}

/// Build a `<pin-event xmlns='urn:waddle:pin:0' action='…' target='…' by='…'
/// [reason='…']>` element with an optional `<preview/>` child. The single
/// element type carries both pin and unpin events so clients can filter
/// the room timeline on `name() == "pin-event"` regardless of action.
fn build_pin_event_element(
    action: &str,
    pinner_jid: &jid::BareJid,
    target_stanza_id: &StanzaId,
    preview: Option<&PinPreview>,
    reason: Option<&str>,
) -> Element {
    let mut element = Element::builder("pin-event", NS_WADDLE_PIN_V0)
        .attr("action", action)
        .attr("target", target_stanza_id.id.as_str())
        .attr("by", pinner_jid.to_string().as_str())
        .build();
    if let Some(reason) = reason {
        element.set_attr("reason", reason);
    }
    if let Some(preview) = preview {
        element.append_child(build_preview_element(preview));
    }
    element
}

fn new_room_message(room: &jid::BareJid, body: String, payload: Element) -> Message {
    let mut msg = Message::new(Some(Jid::from(room.clone())));
    msg.from = Some(Jid::from(room.clone()));
    msg.type_ = MessageType::Groupchat;
    msg.bodies.insert(String::new(), Body(body));
    msg.payloads.push(payload);
    msg
}

fn build_preview_element(preview: &PinPreview) -> Element {
    let mut elem = Element::builder("preview", NS_WADDLE_PIN_V0).build();
    let mut author = Element::builder("author", NS_WADDLE_PIN_V0)
        .attr("jid", preview.author_jid.to_string().as_str())
        .build();
    if let Some(ref nick) = preview.author_nick {
        author.set_attr("nick", nick);
    }
    elem.append_child(author);

    let mut text = Element::builder("text", NS_WADDLE_PIN_V0).build();
    text.append_text_node(&preview.text);
    elem.append_child(text);

    let mut ts = Element::builder("ts", NS_WADDLE_PIN_V0).build();
    ts.append_text_node(preview.message_timestamp.to_rfc3339());
    elem.append_child(ts);

    elem
}

fn display_label(nick: &str, bare_jid: &jid::BareJid) -> String {
    if nick.is_empty() {
        bare_jid.to_string()
    } else {
        nick.to_string()
    }
}

/// Build the typed `<forbidden type='auth'/>` reply addressed back to
/// the sender when the affiliation check rejects a pin/unpin.
fn forbidden_reply(incoming: &Message) -> Message {
    let error = StanzaError::new(
        ErrorType::Auth,
        DefinedCondition::Forbidden,
        "en",
        "Sender is not permitted to pin or unpin messages in this room.",
    );
    message_error_reply(incoming, error)
}

/// Build the typed `<bad-request type='modify'/>` reply for a
/// malformed pin marker (missing target, ambiguous markers, oversize
/// target).
fn bad_request_reply(incoming: &Message) -> Message {
    let error = StanzaError::new(
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        "en",
        "Pin marker is malformed (missing target, oversized target, or ambiguous markers).",
    );
    message_error_reply(incoming, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0421::OccupantIdSecret;
    use crate::xep::xep_waddle_pin::{build_pinned_element, build_unpinned_element};
    use jid::{BareJid, FullJid};
    use std::str::FromStr;

    fn bare(s: &str) -> BareJid {
        BareJid::from_str(s).expect("valid bare jid")
    }

    fn full(s: &str) -> FullJid {
        FullJid::from_str(s).expect("valid full jid")
    }

    fn room_stanza_id(target: &str) -> StanzaId {
        StanzaId::new(target.to_owned(), Jid::from(bare("room@conf.example")))
    }

    fn pin_message(target: &str, sender_full: &FullJid) -> Message {
        let mut msg = Message::new(Some(Jid::from(bare("room@conf.example"))));
        msg.from = Some(Jid::from(sender_full.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.payloads
            .push(build_pinned_element(&room_stanza_id(target)));
        msg
    }

    fn unpin_message(target: &str, sender_full: &FullJid) -> Message {
        let mut msg = Message::new(Some(Jid::from(bare("room@conf.example"))));
        msg.from = Some(Jid::from(sender_full.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.payloads
            .push(build_unpinned_element(&room_stanza_id(target)));
        msg
    }

    fn occupant(full: FullJid, nick: &str, affiliation: Affiliation) -> OccupantSnapshot {
        occupant_with_role(full, nick, affiliation, Role::Participant)
    }

    fn occupant_with_role(
        full: FullJid,
        nick: &str,
        affiliation: Affiliation,
        role: Role,
    ) -> OccupantSnapshot {
        OccupantSnapshot {
            full_jid: full,
            nick: nick.into(),
            affiliation,
            role,
        }
    }

    fn ctx_for<'a>(
        room: &'a BareJid,
        sender_full: &'a FullJid,
        occupants: &'a [OccupantSnapshot],
        id_gen: &'a FixedIdGenerator,
        secret: &'a OccupantIdSecret,
    ) -> RoomContext<'a> {
        ctx_with_permission(
            room,
            sender_full,
            occupants,
            id_gen,
            secret,
            crate::muc::PinPermission::default(),
        )
    }

    fn ctx_with_permission<'a>(
        room: &'a BareJid,
        sender_full: &'a FullJid,
        occupants: &'a [OccupantSnapshot],
        id_gen: &'a FixedIdGenerator,
        secret: &'a OccupantIdSecret,
        pin_permission: crate::muc::PinPermission,
    ) -> RoomContext<'a> {
        RoomContext {
            room,
            sender_full,
            occupants,
            managed_room_forbidden: false,
            room_moderated: false,
            pin_permission,
            id_gen,
            occupant_id_secret: secret,
            sender_nickname_generation: 1,
            project_sender_inbox: true,
            dispatch_timestamp: 1_700_000_000,
        }
    }

    fn occupant_id_secret() -> OccupantIdSecret {
        OccupantIdSecret::new(b"secret-secret-secret-secret-12345".to_vec()).expect("valid secret")
    }

    #[test]
    fn ignores_message_without_pin_marker() {
        let room = bare("room@conf.example");
        let sender = full("alice@example.com/web");
        let occupants = vec![occupant(sender.clone(), "alice", Affiliation::Admin)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = Message::new(Some(Jid::from(room.clone())));
        msg.from = Some(Jid::from(sender.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.bodies.insert(String::new(), Body("hi".into()));
        match MucPinHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Continue(events) => assert!(events.is_empty()),
            RoomHandlerOutcome::Halt(_) => panic!("non-pin message should pass through"),
        }
    }

    #[test]
    fn admin_pin_emits_apply_request_and_halts() {
        let room = bare("room@conf.example");
        let sender = full("alice@example.com/web");
        let occupants = vec![occupant(sender.clone(), "alice", Affiliation::Admin)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = pin_message("stanza-target", &sender);
        let outcome = MucPinHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            RoomHandlerOutcome::Halt(events) => events,
            RoomHandlerOutcome::Continue(_) => panic!("authorized pin must Halt"),
        };
        assert_eq!(
            events.len(),
            1,
            "handler emits only ApplyPinChange — interpreter does MAM lookup + broadcast"
        );
        match &events[0] {
            OutboundEvent::ApplyPinChange { room: r, request } => {
                assert_eq!(r, &room);
                match request {
                    PinChangeRequest::Pin {
                        target_stanza_id,
                        pinner_jid,
                        pinner_nick,
                        ..
                    } => {
                        assert_eq!(target_stanza_id.id, "stanza-target");
                        assert_eq!(pinner_jid, &bare("alice@example.com"));
                        assert_eq!(pinner_nick, "alice");
                    }
                    other => panic!("expected Pin request, got {other:?}"),
                }
            }
            other => panic!("expected ApplyPinChange, got {other:?}"),
        }
    }

    #[test]
    fn admin_unpin_emits_unpin_request_without_preview() {
        let room = bare("room@conf.example");
        let sender = full("alice@example.com/web");
        let occupants = vec![occupant(sender.clone(), "alice", Affiliation::Owner)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = unpin_message("stanza-target", &sender);
        let outcome = MucPinHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            RoomHandlerOutcome::Halt(events) => events,
            RoomHandlerOutcome::Continue(_) => panic!("authorized unpin must Halt"),
        };
        assert_eq!(events.len(), 1);
        match &events[0] {
            OutboundEvent::ApplyPinChange { request, .. } => match request {
                PinChangeRequest::Unpin {
                    target_stanza_id,
                    reason,
                    ..
                } => {
                    assert_eq!(target_stanza_id.id, "stanza-target");
                    assert!(reason.is_none(), "manual unpin has no reason");
                }
                other => panic!("expected Unpin request, got {other:?}"),
            },
            other => panic!("expected ApplyPinChange, got {other:?}"),
        }
    }

    #[test]
    fn member_pin_is_rejected_with_forbidden() {
        let room = bare("room@conf.example");
        let sender = full("eve@example.com/phone");
        let occupants = vec![occupant(sender.clone(), "eve", Affiliation::Member)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = pin_message("stanza-target", &sender);
        let outcome = MucPinHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            RoomHandlerOutcome::Halt(events) => events,
            RoomHandlerOutcome::Continue(_) => panic!("unauthorized pin must Halt"),
        };
        assert_eq!(events.len(), 1, "only error reply expected");
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                crate::Stanza::Message(msg) => {
                    assert_eq!(msg.type_, MessageType::Error);
                    let has_forbidden = msg
                        .payloads
                        .iter()
                        .any(|p| p.children().any(|c| c.name() == "forbidden"));
                    assert!(has_forbidden, "forbidden condition required");
                }
                other => panic!("expected message stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn malformed_pin_marker_returns_bad_request() {
        let room = bare("room@conf.example");
        let sender = full("alice@example.com/web");
        let occupants = vec![occupant(sender.clone(), "alice", Affiliation::Admin)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = Message::new(Some(Jid::from(room.clone())));
        msg.from = Some(Jid::from(sender.clone()));
        msg.type_ = MessageType::Groupchat;
        // Empty target — should be bad-request.
        msg.payloads.push(
            Element::builder("pinned", NS_WADDLE_PIN_V0)
                .attr("target", "")
                .build(),
        );
        let outcome = MucPinHandler.handle(&mut msg, &ctx);
        let events = match outcome {
            RoomHandlerOutcome::Halt(events) => events,
            RoomHandlerOutcome::Continue(_) => panic!("malformed marker must Halt"),
        };
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                crate::Stanza::Message(msg) => {
                    assert_eq!(msg.type_, MessageType::Error);
                    let has_bad_request = msg
                        .payloads
                        .iter()
                        .any(|p| p.children().any(|c| c.name() == "bad-request"));
                    assert!(has_bad_request, "bad-request condition required");
                }
                other => panic!("expected message stanza, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_pin_and_unpin_returns_bad_request() {
        let room = bare("room@conf.example");
        let sender = full("alice@example.com/web");
        let occupants = vec![occupant(sender.clone(), "alice", Affiliation::Admin)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = Message::new(Some(Jid::from(room.clone())));
        msg.from = Some(Jid::from(sender.clone()));
        msg.type_ = MessageType::Groupchat;
        msg.payloads
            .push(build_pinned_element(&room_stanza_id("a")));
        msg.payloads
            .push(build_unpinned_element(&room_stanza_id("b")));
        let outcome = MucPinHandler.handle(&mut msg, &ctx);
        match outcome {
            RoomHandlerOutcome::Halt(events) => {
                assert_eq!(events.len(), 1);
                match &events[0] {
                    OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                        crate::Stanza::Message(msg) => {
                            assert_eq!(msg.type_, MessageType::Error);
                        }
                        other => panic!("expected message stanza, got {other:?}"),
                    },
                    other => panic!("expected SendStanza, got {other:?}"),
                }
            }
            RoomHandlerOutcome::Continue(_) => panic!("ambiguous markers must Halt"),
        }
    }

    #[test]
    fn outsider_pin_continues_when_sender_snapshot_missing() {
        let room = bare("room@conf.example");
        let sender = full("ghost@example.com/x");
        let occupants: Vec<OccupantSnapshot> = Vec::new();
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_for(&room, &sender, &occupants, &id_gen, &secret);
        let mut msg = pin_message("stanza-target", &sender);
        match MucPinHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Continue(events) => assert!(events.is_empty()),
            RoomHandlerOutcome::Halt(_) => {
                panic!("missing sender snapshot is the gate's concern, not pin's")
            }
        }
    }

    /// #415: when the room is configured `pin_permission=anyone`, a
    /// non-admin member's pin succeeds (Halt with ApplyPinChange).
    #[test]
    fn member_pin_admitted_when_permission_is_anyone() {
        let room = bare("room@conf.example");
        let sender = full("eve@example.com/phone");
        let occupants = vec![occupant(sender.clone(), "eve", Affiliation::Member)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_with_permission(
            &room,
            &sender,
            &occupants,
            &id_gen,
            &secret,
            crate::muc::PinPermission::Anyone,
        );
        let mut msg = pin_message("stanza-target", &sender);
        let events = match MucPinHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Halt(events) => events,
            RoomHandlerOutcome::Continue(_) => {
                panic!("anyone-pin permission must admit a member's pin")
            }
        };
        assert_eq!(events.len(), 1, "single ApplyPinChange expected");
        assert!(matches!(events[0], OutboundEvent::ApplyPinChange { .. }));
    }

    /// #415: a non-member (Affiliation::None, e.g. a casual occupant
    /// of an open room) cannot pin even when `pin_permission=anyone`.
    /// Compliance requires at least Member affiliation under Anyone.
    #[test]
    fn non_member_pin_rejected_even_when_permission_is_anyone() {
        let room = bare("room@conf.example");
        let sender = full("guest@example.com/web");
        let occupants = vec![occupant(sender.clone(), "guest", Affiliation::None)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_with_permission(
            &room,
            &sender,
            &occupants,
            &id_gen,
            &secret,
            crate::muc::PinPermission::Anyone,
        );
        let mut msg = pin_message("stanza-target", &sender);
        match MucPinHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Halt(events) => match events.as_slice() {
                [OutboundEvent::SendStanza(stanza)] => match stanza.as_ref() {
                    crate::Stanza::Message(m) => {
                        assert_eq!(m.type_, MessageType::Error);
                    }
                    other => panic!("expected message stanza, got {other:?}"),
                },
                other => panic!("expected single SendStanza, got {other:?}"),
            },
            RoomHandlerOutcome::Continue(_) => panic!("non-member must be rejected"),
        }
    }

    /// #415: a Visitor (silenced occupant in a moderated room,
    /// XEP-0045 §5.1.2) cannot pin even when `pin_permission=anyone`,
    /// because they cannot speak in the room.
    #[test]
    fn visitor_pin_rejected_even_when_permission_is_anyone() {
        let room = bare("room@conf.example");
        let sender = full("eve@example.com/phone");
        let occupants = vec![occupant_with_role(
            sender.clone(),
            "eve",
            Affiliation::Member,
            Role::Visitor,
        )];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_with_permission(
            &room,
            &sender,
            &occupants,
            &id_gen,
            &secret,
            crate::muc::PinPermission::Anyone,
        );
        let mut msg = pin_message("stanza-target", &sender);
        match MucPinHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Halt(events) => match events.as_slice() {
                [OutboundEvent::SendStanza(stanza)] => match stanza.as_ref() {
                    crate::Stanza::Message(m) => {
                        assert_eq!(m.type_, MessageType::Error);
                    }
                    other => panic!("expected message stanza, got {other:?}"),
                },
                other => panic!("expected single SendStanza, got {other:?}"),
            },
            RoomHandlerOutcome::Continue(_) => panic!("visitor must be rejected"),
        }
    }

    /// #415: when the room is configured `pin_permission=admins-only`
    /// (the default), a non-admin member is still rejected even if
    /// they're a current occupant.
    #[test]
    fn member_pin_still_rejected_when_permission_is_admins_only() {
        let room = bare("room@conf.example");
        let sender = full("eve@example.com/phone");
        let occupants = vec![occupant(sender.clone(), "eve", Affiliation::Member)];
        let id_gen = FixedIdGenerator("ignored".into());
        let secret = occupant_id_secret();
        let ctx = ctx_with_permission(
            &room,
            &sender,
            &occupants,
            &id_gen,
            &secret,
            crate::muc::PinPermission::AdminsOnly,
        );
        let mut msg = pin_message("stanza-target", &sender);
        match MucPinHandler.handle(&mut msg, &ctx) {
            RoomHandlerOutcome::Halt(events) => match events.as_slice() {
                [OutboundEvent::SendStanza(stanza)] => match stanza.as_ref() {
                    crate::Stanza::Message(m) => {
                        assert_eq!(m.type_, MessageType::Error);
                    }
                    other => panic!("expected message stanza, got {other:?}"),
                },
                other => panic!("expected single SendStanza, got {other:?}"),
            },
            RoomHandlerOutcome::Continue(_) => panic!("must reject"),
        }
    }
}
