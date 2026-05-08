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
use crate::muc::pin::{PinPreview, PinStateChange, PinnedEntry};
use crate::types::Affiliation;
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

        // Hard-coded admin/owner gate. #415 makes this configurable.
        if !matches!(sender.affiliation, Affiliation::Owner | Affiliation::Admin) {
            let reply = forbidden_reply(message);
            return RoomHandlerOutcome::Halt(vec![send_message_error(reply)]);
        }

        let dispatched_at: DateTime<Utc> =
            DateTime::from_timestamp(ctx.dispatch_timestamp, 0).unwrap_or_else(Utc::now);
        let pinner = sender.bare_jid();
        let target_stanza_id =
            StanzaId::new(intent.target().to_owned(), Jid::from(ctx.room.clone()));

        let (change, system_message) = match intent {
            PinIntent::Pin { .. } => {
                let preview = pin_preview_from(message, ctx, dispatched_at);
                let entry = PinnedEntry {
                    target_stanza_id: target_stanza_id.clone(),
                    pinner_jid: pinner.clone(),
                    pinned_at: dispatched_at,
                    preview: preview.clone(),
                };
                let sys = build_pinned_system_message(
                    ctx.room,
                    &pinner,
                    &sender.nick,
                    &target_stanza_id,
                    Some(&preview),
                    None,
                );
                (PinStateChange::Pin(entry), sys)
            }
            PinIntent::Unpin { .. } => {
                let sys = build_unpinned_system_message(
                    ctx.room,
                    &pinner,
                    &sender.nick,
                    &target_stanza_id,
                    None,
                );
                (
                    PinStateChange::Unpin {
                        target_stanza_id: target_stanza_id.clone(),
                    },
                    sys,
                )
            }
        };

        RoomHandlerOutcome::Halt(vec![
            OutboundEvent::ApplyPinChange {
                room: ctx.room.clone(),
                change,
            },
            OutboundEvent::BroadcastRoomSystemMessage {
                room: ctx.room.clone(),
                message: Box::new(system_message),
            },
        ])
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

/// Capture a frozen preview of the original message at pin time. Note:
/// the in-flight pin/unpin message itself usually carries no body —
/// this preview is built from whatever metadata the sender included
/// for testing/debugging. The chat client populates the preview at
/// pin time via the IQ query response after publishing the pin marker.
///
/// In practice, the pin message can include a `<body>` for testing
/// purposes, in which case we use it; otherwise the preview text is
/// empty and the chat client falls back to fetching from MAM.
fn pin_preview_from(
    message: &Message,
    ctx: &RoomContext<'_>,
    dispatched_at: DateTime<Utc>,
) -> PinPreview {
    let body = message
        .bodies
        .get("")
        .map(|Body(b)| b.as_str())
        .or_else(|| message.bodies.values().next().map(|Body(b)| b.as_str()))
        .unwrap_or("");
    let sender = ctx
        .sender_snapshot()
        .map(|s| (s.bare_jid(), s.nick.clone()));
    let (author_jid, author_nick) = sender.unwrap_or_else(|| (ctx.room.clone(), String::new()));
    let nick = if author_nick.is_empty() {
        None
    } else {
        Some(author_nick)
    };
    PinPreview::new(author_jid, nick, body, dispatched_at)
}

/// Build the system message broadcast on a successful pin.
pub fn build_pinned_system_message(
    room: &jid::BareJid,
    pinner_jid: &jid::BareJid,
    pinner_nick: &str,
    target_stanza_id: &StanzaId,
    preview: Option<&PinPreview>,
    reason: Option<&str>,
) -> Message {
    let mut element = Element::builder("pinned", NS_WADDLE_PIN_V0)
        .attr("target", target_stanza_id.id.as_str())
        .attr("by", pinner_jid.to_string().as_str())
        .build();
    if let Some(reason) = reason {
        element.set_attr("reason", reason);
    }
    if let Some(preview) = preview {
        element.append_child(build_preview_element(preview));
    }
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
pub fn build_unpinned_system_message(
    room: &jid::BareJid,
    pinner_jid: &jid::BareJid,
    pinner_nick: &str,
    target_stanza_id: &StanzaId,
    reason: Option<&str>,
) -> Message {
    let mut element = Element::builder("unpinned", NS_WADDLE_PIN_V0)
        .attr("target", target_stanza_id.id.as_str())
        .attr("by", pinner_jid.to_string().as_str())
        .build();
    if let Some(reason) = reason {
        element.set_attr("reason", reason);
    }
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
        OccupantSnapshot {
            full_jid: full,
            nick: nick.into(),
            affiliation,
            role: Role::Participant,
        }
    }

    fn ctx_for<'a>(
        room: &'a BareJid,
        sender_full: &'a FullJid,
        occupants: &'a [OccupantSnapshot],
        id_gen: &'a FixedIdGenerator,
        secret: &'a OccupantIdSecret,
    ) -> RoomContext<'a> {
        RoomContext {
            room,
            sender_full,
            occupants,
            managed_room_forbidden: false,
            room_moderated: false,
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
    fn admin_pin_emits_apply_and_broadcast_and_halts() {
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
        assert_eq!(events.len(), 2, "expected ApplyPinChange + Broadcast");
        match &events[0] {
            OutboundEvent::ApplyPinChange { room: r, change } => {
                assert_eq!(r, &room);
                match change {
                    PinStateChange::Pin(entry) => {
                        assert_eq!(entry.target_stanza_id.id, "stanza-target");
                        assert_eq!(entry.pinner_jid, bare("alice@example.com"));
                    }
                    other => panic!("expected Pin change, got {other:?}"),
                }
            }
            other => panic!("expected ApplyPinChange first, got {other:?}"),
        }
        match &events[1] {
            OutboundEvent::BroadcastRoomSystemMessage { room: r, message } => {
                assert_eq!(r, &room);
                assert_eq!(message.type_, MessageType::Groupchat);
                let from_jid = message.from.as_ref().expect("from set");
                assert_eq!(from_jid.to_string(), room.to_string());
                let pinned = message
                    .payloads
                    .iter()
                    .find(|e| e.name() == "pinned" && e.ns() == NS_WADDLE_PIN_V0)
                    .expect("pinned element present");
                assert_eq!(pinned.attr("target"), Some("stanza-target"));
                assert_eq!(pinned.attr("by"), Some("alice@example.com"));
                let preview = pinned
                    .children()
                    .find(|c| c.name() == "preview")
                    .expect("preview present on pin");
                assert!(preview.children().any(|c| c.name() == "author"));
            }
            other => panic!("expected BroadcastRoomSystemMessage second, got {other:?}"),
        }
    }

    #[test]
    fn admin_unpin_emits_unpin_change_without_preview() {
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
        match &events[0] {
            OutboundEvent::ApplyPinChange { change, .. } => match change {
                PinStateChange::Unpin { target_stanza_id } => {
                    assert_eq!(target_stanza_id.id, "stanza-target");
                }
                other => panic!("expected Unpin change, got {other:?}"),
            },
            other => panic!("expected ApplyPinChange first, got {other:?}"),
        }
        match &events[1] {
            OutboundEvent::BroadcastRoomSystemMessage { message, .. } => {
                let unpinned = message
                    .payloads
                    .iter()
                    .find(|e| e.name() == "unpinned" && e.ns() == NS_WADDLE_PIN_V0)
                    .expect("unpinned element present");
                assert_eq!(unpinned.attr("target"), Some("stanza-target"));
                assert!(
                    unpinned.children().all(|c| c.name() != "preview"),
                    "unpin must not carry preview"
                );
            }
            other => panic!("expected BroadcastRoomSystemMessage second, got {other:?}"),
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
}
