//! Channel pinned-messages handler (#414).
//!
//! Detects `<message type='groupchat'>` stanzas carrying a XEP-0470
//! `<attachments xmlns='urn:xmpp:pubsub-attachments:0' for='…' item='…'/>`
//! element with a `urn:waddle:pin:0` `<pinned/>` or `<unpinned/>` child,
//! enforces affiliation-based authorization, and emits a typed pair of
//! events:
//!
//! 1. [`OutboundEvent::ApplyPinChange`] — mutates `MucRoom.pinned_entries`
//!    via the room actor.
//! 2. [`OutboundEvent::BroadcastRoomSystemMessage`] — emits a
//!    `<message type='groupchat' from='room@conf'>` carrying a
//!    `urn:waddle:pin:0` `<pin-event/>` payload, archived in MAM and
//!    fanned out to occupants.
//!
//! Authorization is **hard-coded** to admins/owners in this handler; the
//! per-room configurable `urn:waddle:roomconfig:pinpermission` field
//! ships in #415 and will replace the hard-coded gate.
//!
//! The handler runs after `MucCanonicalizeHandler` so the in-flight
//! message has its canonical XEP-0359 `<stanza-id by='room'/>` and
//! XEP-0421 occupant-id stamps; it runs before `MucArchiveHandler` and
//! `ReflectorHandler` so the original pin/unpin user message does **not**
//! enter the regular archive/reflect path. The system message emitted
//! via `BroadcastRoomSystemMessage` is the durable record of the pin
//! event in MAM.

use super::super::event::OutboundEvent;
use super::super::handlers::errors::{message_error_reply, send_message_error};
use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::muc::pin::{PinPreview, PinStateChange, PinnedEntry};
use crate::types::Affiliation;
use crate::xep::xep0470::{
    has_pin_marker, has_unpin_marker, is_attachments_element, parse_attachment_target,
    NS_WADDLE_PIN_V0,
};
use chrono::{DateTime, Utc};
use jid::Jid;
use minidom::Element;
use xmpp_parsers::message::{Body, Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Pin-event handler for the MUC room chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct MucPinHandler;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinIntent {
    Pin,
    Unpin,
}

impl RoomHandler for MucPinHandler {
    fn name(&self) -> &'static str {
        "waddle-pin"
    }

    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
        let Some((intent, target_stanza_id)) = detect_pin_intent(message) else {
            return RoomHandlerOutcome::Continue(Vec::new());
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

        let (change, system_message) = match intent {
            PinIntent::Pin => {
                let preview = pin_preview_from(message, ctx, dispatched_at);
                let entry = PinnedEntry {
                    target_stanza_id: target_stanza_id.clone(),
                    pinner_jid: pinner.clone(),
                    pinned_at: dispatched_at,
                    preview: preview.clone(),
                };
                let sys = build_pin_event_message(
                    ctx,
                    PinIntent::Pin,
                    &pinner,
                    &sender.nick,
                    &target_stanza_id,
                    Some(&preview),
                );
                (PinStateChange::Pin(entry), sys)
            }
            PinIntent::Unpin => {
                let sys = build_pin_event_message(
                    ctx,
                    PinIntent::Unpin,
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

/// Inspect the in-flight message for a pin/unpin XEP-0470 attachment.
/// Returns the intent and the target stanza-id from the attachment's
/// `item` attribute on the first matching element. The attachment's
/// `for` attribute is ignored — the target is identified by stanza-id
/// regardless of which conceptual node the publisher chose.
fn detect_pin_intent(message: &Message) -> Option<(PinIntent, String)> {
    for elem in &message.payloads {
        if !is_attachments_element(elem) {
            continue;
        }
        let target = parse_attachment_target(elem)?;
        if has_pin_marker(elem) {
            return Some((PinIntent::Pin, target.item_id));
        }
        if has_unpin_marker(elem) {
            return Some((PinIntent::Unpin, target.item_id));
        }
    }
    None
}

/// Capture a frozen preview of the original message at pin time.
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

/// Build the system message emitted by the room itself summarizing the
/// pin or unpin event. Carries a human-readable `<body>` plus a typed
/// `<pin-event xmlns='urn:waddle:pin:0'/>` payload for rich client
/// rendering.
fn build_pin_event_message(
    ctx: &RoomContext<'_>,
    intent: PinIntent,
    pinner_jid: &jid::BareJid,
    pinner_nick: &str,
    target_stanza_id: &str,
    preview: Option<&PinPreview>,
) -> Message {
    let action = match intent {
        PinIntent::Pin => "pinned",
        PinIntent::Unpin => "unpinned",
    };
    let mut event = Element::builder("pin-event", NS_WADDLE_PIN_V0)
        .attr("action", action)
        .attr("by", pinner_jid.to_string().as_str())
        .build();
    event.append_child(
        Element::builder("ref", NS_WADDLE_PIN_V0)
            .attr("id", target_stanza_id)
            .build(),
    );
    if let Some(preview) = preview {
        event.append_child(build_preview_element(preview));
    }

    let mut msg = Message::new(Some(Jid::from(ctx.room.clone())));
    msg.from = Some(Jid::from(ctx.room.clone()));
    msg.type_ = MessageType::Groupchat;
    let body_text = match intent {
        PinIntent::Pin => format!(
            "{} pinned a message",
            display_label(pinner_nick, pinner_jid)
        ),
        PinIntent::Unpin => format!(
            "{} unpinned a message",
            display_label(pinner_nick, pinner_jid)
        ),
    };
    msg.bodies.insert(String::new(), Body(body_text));
    msg.payloads.push(event);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::room::context::OccupantSnapshot;
    use crate::types::{Affiliation, Role};
    use crate::xep::xep0421::OccupantIdSecret;
    use crate::xep::xep0470::{Attachment, AttachmentTarget};
    use jid::{BareJid, FullJid};
    use std::str::FromStr;

    fn bare(s: &str) -> BareJid {
        BareJid::from_str(s).expect("valid bare jid")
    }

    fn full(s: &str) -> FullJid {
        FullJid::from_str(s).expect("valid full jid")
    }

    fn pin_message(target_stanza_id: &str, sender_full: &FullJid) -> Message {
        let mut msg = Message::new(Some(Jid::from(bare("room@conf.example"))));
        msg.from = Some(Jid::from(sender_full.clone()));
        msg.type_ = MessageType::Groupchat;
        let attachment = Attachment::pin(AttachmentTarget::new("urn:xmpp:mam:2", target_stanza_id));
        msg.payloads
            .push(crate::xep::xep0470::build_attachments_element(&attachment));
        msg
    }

    fn unpin_message(target_stanza_id: &str, sender_full: &FullJid) -> Message {
        let mut msg = Message::new(Some(Jid::from(bare("room@conf.example"))));
        msg.from = Some(Jid::from(sender_full.clone()));
        msg.type_ = MessageType::Groupchat;
        let attachment =
            Attachment::unpin(AttachmentTarget::new("urn:xmpp:mam:2", target_stanza_id));
        msg.payloads
            .push(crate::xep::xep0470::build_attachments_element(&attachment));
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
    fn ignores_message_without_pin_attachment() {
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
                        assert_eq!(entry.target_stanza_id, "stanza-target");
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
                let event = message
                    .payloads
                    .iter()
                    .find(|e| e.name() == "pin-event" && e.ns() == NS_WADDLE_PIN_V0)
                    .expect("pin-event payload present");
                assert_eq!(event.attr("action"), Some("pinned"));
                assert_eq!(event.attr("by"), Some("alice@example.com"));
                let preview = event
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
                    assert_eq!(target_stanza_id, "stanza-target");
                }
                other => panic!("expected Unpin change, got {other:?}"),
            },
            other => panic!("expected ApplyPinChange first, got {other:?}"),
        }
        match &events[1] {
            OutboundEvent::BroadcastRoomSystemMessage { message, .. } => {
                let event = message
                    .payloads
                    .iter()
                    .find(|e| e.name() == "pin-event" && e.ns() == NS_WADDLE_PIN_V0)
                    .expect("pin-event payload present");
                assert_eq!(event.attr("action"), Some("unpinned"));
                assert!(
                    event.children().all(|c| c.name() != "preview"),
                    "unpin event must not carry preview"
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
