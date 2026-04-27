//! Per-pass message routing — the final stage of the message
//! pipeline.
//!
//! Locality + type matrix (per #229 Q4 + Q7):
//!
//! - **Sender pass, `Chat`/`Normal`** → emit
//!   [`OutboundEvent::RouteToConnection`] for the recipient. The
//!   interpreter feeds it into the destination's state machine as
//!   [`InboundEvent::StanzaFromPeer`] (post-PR4 cutover) so the
//!   recipient pipeline runs on the destination side.
//! - **Sender pass, `Groupchat`** → if [`MessageContext::muc_occupancy`]
//!   shows the local user is currently joined to the room, emit
//!   [`OutboundEvent::DispatchToRoom`]. Otherwise halt with
//!   `<not-acceptable type='cancel'/>` per XEP-0045 §7.4 (non-occupant
//!   may not send to a room).
//! - **Recipient pass, `Chat`/`Normal`/`Headline`** → emit
//!   [`OutboundEvent::SendStanza`] (write to the local wire). The
//!   sender-side handler-chain processing already happened on the
//!   originating connection; this pass terminates in delivery.
//! - **Recipient pass, `Groupchat`** → emit `SendStanza`. The room
//!   chain (PR5) reflects to occupants by feeding individual
//!   `RouteToConnection` events; by the time we're in the recipient
//!   pass, the message is targeted at this connection.
//! - **`Both`** (true self-loop) → run sender-side branch only;
//!   recipient-side covered by the eventual recipient-pass dispatch
//!   on this same connection via `StanzaFromPeer`.
//! - **`Neither`** → no-op (third-party stanza arriving via routing,
//!   should be rare and is benign).
//! - **Type `Error`** → emit `SendStanza` on recipient pass (forward
//!   the error to its addressee); ignore on sender pass (sender-pass
//!   error sends are for typed error replies built by other handlers
//!   and emitted via `SendStanza` directly).

use super::errors::{not_acceptable_reply, send_message_error};
use crate::protocol::event::OutboundEvent;
use crate::protocol::message_context::MessageContext;
use crate::protocol::session_state::Locality;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use crate::Stanza;
use xmpp_parsers::message::{Message, MessageType};

/// Final routing handler for the user-side message pipeline.
#[derive(Debug, Default, Clone, Copy)]
pub struct RouteHandler;

impl MessageHandler for RouteHandler {
    fn name(&self) -> &'static str {
        "waddle-message-route"
    }

    fn handle(&self, message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
        match (ctx.locality, message.type_.clone()) {
            // Sender pass, groupchat — XEP-0045 §7.4 occupancy check
            // then dispatch to the room chain.
            (Locality::Sender, MessageType::Groupchat)
            | (Locality::Both, MessageType::Groupchat) => {
                let Some(room) = message.to.as_ref().map(|j| j.to_bare()) else {
                    return HandlerOutcome::Continue(Vec::new());
                };
                if !ctx.muc_occupancy.is_occupant(&room) {
                    let reply = not_acceptable_reply(
                        message,
                        "Sender is not currently a member of the room.",
                    );
                    return HandlerOutcome::Halt(vec![send_message_error(reply)]);
                }
                HandlerOutcome::Continue(vec![OutboundEvent::DispatchToRoom {
                    room,
                    message: Box::new(message.clone()),
                }])
            }
            // Sender pass, 1:1 chat / normal / headline — route to
            // the recipient connection. Headline is included so
            // server-originated notifications (e.g. PEP, MUC
            // invitations forwarded as headlines) keep flowing under
            // the cutover.
            (Locality::Sender, MessageType::Chat)
            | (Locality::Sender, MessageType::Normal)
            | (Locality::Sender, MessageType::Headline)
            | (Locality::Both, MessageType::Chat)
            | (Locality::Both, MessageType::Normal)
            | (Locality::Both, MessageType::Headline) => {
                let Some(jid) = message.to.as_ref() else {
                    return HandlerOutcome::Continue(Vec::new());
                };
                // Pass the typed Jid through verbatim — full or bare.
                // The interpreter performs resource selection per
                // RFC 6121 §8.5 (bare → highest-priority resources;
                // full → exact resource). No string synthesis.
                HandlerOutcome::Continue(vec![OutboundEvent::RouteToConnection {
                    jid: jid.clone(),
                    stanza: Box::new(Stanza::Message(message.clone())),
                }])
            }
            // Recipient pass — write to local wire.
            (Locality::Recipient, _) => HandlerOutcome::Continue(vec![OutboundEvent::SendStanza(
                Box::new(Stanza::Message(message.clone())),
            )]),
            // Sender pass with non-routable type, or Neither locality
            // — no-op.
            (Locality::Sender, _) | (Locality::Both, _) | (Locality::Neither, _) => {
                HandlerOutcome::Continue(Vec::new())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::protocol::message_context::MessageContextEnv;
    use crate::protocol::session_state::{Blocklist, CarbonsState, MucOccupancy, OccupancyEntry};
    use jid::{BareJid, FullJid};
    use xmpp_parsers::message::{Body, Message, MessageType};
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn run(local: &FullJid, occ: &MucOccupancy, msg: &mut Message) -> HandlerOutcome {
        let bl = Blocklist::empty();
        let gen = FixedIdGenerator("test".to_string());
        let env = MessageContextEnv {
            domain: "example.com",
            full_jid: local,
            blocklist: &bl,
            carbons: CarbonsState::Disabled,
            muc_occupancy: occ,
            id_gen: &gen,
        };
        let ctx = MessageContext::derive(env, msg);
        RouteHandler.handle(msg, &ctx)
    }

    fn extract_event(outcome: &HandlerOutcome) -> &OutboundEvent {
        match outcome {
            HandlerOutcome::Continue(events) | HandlerOutcome::Halt(events) => {
                assert_eq!(events.len(), 1, "expected exactly one event");
                &events[0]
            }
            other => panic!("expected Continue/Halt, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // 1:1 routing
    // -----------------------------------------------------------------

    #[test]
    fn route_sender_pass_chat_to_full_jid_emits_route_to_connection() {
        let local = full("alice@example.com/web");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com/desk", "hi");
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::RouteToConnection { jid, .. } => {
                assert_eq!(jid.to_string(), "bob@example.com/desk");
            }
            other => panic!("expected RouteToConnection, got {other:?}"),
        }
    }

    #[test]
    fn route_sender_pass_chat_to_bare_jid_emits_route_to_connection_with_bare_jid() {
        // Bare-targeted messages keep the typed bare JID — the
        // interpreter performs resource selection. Earlier drafts
        // synthesized a fake full JID via `format!("{}/", bare)`;
        // that violated the typed-payloads rule and produced an
        // invalid resource that ConnectionRegistry::send_to dropped.
        let local = full("alice@example.com/web");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::RouteToConnection { jid, .. } => {
                assert_eq!(jid.to_string(), "bob@example.com");
                assert!(jid.resource().is_none(), "bare JID preserved as bare");
            }
            other => panic!("expected RouteToConnection, got {other:?}"),
        }
    }

    #[test]
    fn route_sender_pass_headline_to_full_jid_emits_route_to_connection() {
        // Server-originated notifications use Headline; legacy
        // message.rs treats Headline as a deliverable 1:1 type.
        let local = full("server@example.com/notify");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("server@example.com/notify", "alice@example.com/web", "ping");
        msg.type_ = MessageType::Headline;
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::RouteToConnection { jid, .. } => {
                assert_eq!(jid.to_string(), "alice@example.com/web");
            }
            other => panic!("expected RouteToConnection, got {other:?}"),
        }
    }

    #[test]
    fn route_recipient_pass_chat_emits_send_stanza_to_wire() {
        let local = full("bob@example.com/desk");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::SendStanza(_) => {}
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn route_neither_locality_emits_nothing() {
        let local = full("eve@example.com/web");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "hi");
        let outcome = run(&local, &occ, &mut msg);
        assert!(matches!(outcome, HandlerOutcome::Continue(ref e) if e.is_empty()));
    }

    // -----------------------------------------------------------------
    // XEP-0045 §7.4 — groupchat occupancy gate
    // -----------------------------------------------------------------

    #[test]
    fn xep_0045_sender_pass_groupchat_for_occupant_emits_dispatch_to_room() {
        let local = full("alice@example.com/web");
        let occ = MucOccupancy::new([(
            bare("room@conf.example.com"),
            OccupancyEntry {
                nick: "alice".to_string(),
                generation: 1,
            },
        )]);
        let mut msg = chat_with_body("alice@example.com/web", "room@conf.example.com", "shouted");
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::DispatchToRoom { room, .. } => {
                assert_eq!(*room, bare("room@conf.example.com"));
            }
            other => panic!("expected DispatchToRoom, got {other:?}"),
        }
    }

    #[test]
    fn xep_0045_sender_pass_groupchat_for_non_occupant_halts_with_not_acceptable() {
        let local = full("alice@example.com/web");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("alice@example.com/web", "room@conf.example.com", "shouted");
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &occ, &mut msg);
        let events = match outcome {
            HandlerOutcome::Halt(ref events) => events,
            other => panic!("expected Halt, got {other:?}"),
        };
        assert_eq!(events.len(), 1);
        let stanza = match &events[0] {
            OutboundEvent::SendStanza(s) => s,
            other => panic!("expected SendStanza, got {other:?}"),
        };
        let msg = match stanza.as_ref() {
            Stanza::Message(m) => m,
            other => panic!("expected Message, got {other:?}"),
        };
        let elem = msg
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload");
        let parsed = StanzaError::try_from(elem.clone()).expect("typed parse");
        assert_eq!(parsed.type_, ErrorType::Cancel);
        assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
    }

    #[test]
    fn route_recipient_pass_groupchat_emits_send_stanza() {
        // Recipient-pass groupchat means the room chain has already
        // selected this resource; we just write to the wire.
        let local = full("alice@example.com/web");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body(
            "room@conf.example.com/bob",
            "alice@example.com",
            "from-room",
        );
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::SendStanza(_) => {}
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }
}
