//! Room handler chain dispatcher.
//!
//! Mirrors [`super::super::dispatch::StanzaDispatcher`]'s message pipeline
//! shape (registration order, halt-or-continue stepping) but for the
//! groupchat-locality chain. Run from the
//! [`super::super::event::OutboundEvent::DispatchToRoom`] interpreter arm.

use super::context::RoomContext;
use super::traits::{RoomHandler, RoomHandlerOutcome};
use crate::protocol::event::OutboundEvent;
use std::sync::Arc;
use xmpp_parsers::message::Message;

/// Result of running the room chain once.
#[derive(Debug)]
pub struct RoomDispatchOutcome {
    /// Events emitted by handlers in registration order.
    pub events: Vec<OutboundEvent>,
    /// True when a handler halted the chain (e.g. occupancy validation
    /// failed and emitted a typed error reply). Diagnostic only — the
    /// interpreter pumps `events` regardless.
    pub halted: bool,
}

/// Ordered chain of [`RoomHandler`] stages.
///
/// Cheap to clone (handlers sit behind `Arc`) so the same dispatcher
/// can be shared between sender-pass dispatches and tests.
#[derive(Clone, Default)]
pub struct RoomDispatcher {
    handlers: Vec<Arc<dyn RoomHandler>>,
}

impl RoomDispatcher {
    /// Create an empty dispatcher.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Append a handler to the chain. Handlers run in registration
    /// order.
    pub fn register(&mut self, handler: Arc<dyn RoomHandler>) {
        self.handlers.push(handler);
    }

    /// Number of registered handlers — used by tests to assert the
    /// chain shape.
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Run the chain against `message` with `ctx`.
    pub fn dispatch(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomDispatchOutcome {
        let mut events = Vec::new();
        for handler in &self.handlers {
            match handler.handle(message, ctx) {
                RoomHandlerOutcome::Continue(more) => events.extend(more),
                RoomHandlerOutcome::Halt(more) => {
                    events.extend(more);
                    return RoomDispatchOutcome {
                        events,
                        halted: true,
                    };
                }
            }
        }
        RoomDispatchOutcome {
            events,
            halted: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::context::{OccupantSnapshot, RoomContext};
    use super::super::traits::{RoomHandler, RoomHandlerOutcome};
    use super::*;
    use crate::protocol::id_gen::FixedIdGenerator;
    use crate::types::{Affiliation, Role};
    use jid::{BareJid, FullJid};
    use xmpp_parsers::message::{Message, MessageType};

    struct CountHandler {
        name: &'static str,
        halt: bool,
    }

    impl RoomHandler for CountHandler {
        fn name(&self) -> &'static str {
            self.name
        }
        fn handle(&self, _message: &mut Message, _ctx: &RoomContext<'_>) -> RoomHandlerOutcome {
            if self.halt {
                RoomHandlerOutcome::Halt(Vec::new())
            } else {
                RoomHandlerOutcome::Continue(Vec::new())
            }
        }
    }

    fn fixture_ctx<'a>(
        room: &'a BareJid,
        sender_full: &'a FullJid,
        occupants: &'a [OccupantSnapshot],
        gen: &'a FixedIdGenerator,
    ) -> RoomContext<'a> {
        RoomContext {
            room,
            sender_full,
            occupants,
            managed_room_forbidden: false,
            id_gen: gen,
            occupant_id_secret: b"test-secret",
        }
    }

    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }
    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }

    #[test]
    fn dispatch_runs_handlers_in_order_until_halt() {
        let room = bare("room@conf.example.com");
        let sender = full("alice@example.com/web");
        let occupants = vec![OccupantSnapshot {
            full_jid: sender.clone(),
            nick: "alice".to_string(),
            affiliation: Affiliation::Member,
            role: Role::Participant,
        }];
        let id_gen = FixedIdGenerator("fresh-id".to_string());
        let ctx = fixture_ctx(&room, &sender, &occupants, &id_gen);

        let mut chain = RoomDispatcher::new();
        chain.register(Arc::new(CountHandler {
            name: "first",
            halt: false,
        }));
        chain.register(Arc::new(CountHandler {
            name: "halter",
            halt: true,
        }));
        chain.register(Arc::new(CountHandler {
            name: "after-halt",
            halt: false,
        }));

        let mut msg = Message::new(Some(jid::Jid::from(room.clone())));
        msg.from = Some(jid::Jid::from(sender.clone()));
        msg.type_ = MessageType::Groupchat;

        let outcome = chain.dispatch(&mut msg, &ctx);
        assert!(outcome.halted, "chain halted at the second handler");
    }
}
