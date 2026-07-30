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
            // Sender pass, groupchat — dispatch to the room chain.
            //
            // The XEP-0045 §7.4 occupancy check (non-occupant may not
            // send to a room) is intentionally not enforced here.
            // `ctx.muc_occupancy` is the user-side snapshot and may
            // not include the room (the room actor is the
            // authoritative source). The interpreter-side
            // `OutboundEvent::DispatchToRoom` arm queries the per-room
            // actor for a frozen `RoomChainSnapshot` and runs the
            // room handler chain (#229 PR17 + PR18, option C);
            // `OccupancyValidationHandler` halts non-occupant senders
            // there with a typed XEP-0045 §7.4 `<not-acceptable/>`
            // reply.
            (Locality::Sender, MessageType::Groupchat)
            | (Locality::Both, MessageType::Groupchat) => {
                let Some(room) = message.to.as_ref().map(|j| j.to_bare()) else {
                    return HandlerOutcome::Continue(Vec::new());
                };
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
                    call_setup: None,
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
    use xmpp_parsers::message::{Message, MessageType};

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
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), body.to_string());
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
            has_live_transport: true,
            delivery_fanout: &[],
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
    fn xep_0045_sender_pass_groupchat_dispatches_to_room_unconditionally() {
        // Until the room handler chain (PR17) populates the
        // `MucOccupancy` snapshot, the sender-side route handler
        // emits `DispatchToRoom` unconditionally and lets the room
        // actor enforce XEP-0045 §7.4 occupancy. The room actor's
        // `BuildGroupchatBroadcast` returns `Err` for non-occupants
        // and the dispatch bridge silently drops, mirroring legacy
        // `handle_message`'s no-error-reply behaviour.
        let local = full("alice@example.com/web");
        let occ = MucOccupancy::empty();
        let mut msg = chat_with_body("alice@example.com/web", "room@conf.example.com", "shouted");
        msg.type_ = MessageType::Groupchat;
        let outcome = run(&local, &occ, &mut msg);
        match extract_event(&outcome) {
            OutboundEvent::DispatchToRoom { room, .. } => {
                assert_eq!(room.to_string(), "room@conf.example.com");
            }
            other => panic!("expected DispatchToRoom, got {other:?}"),
        }
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
