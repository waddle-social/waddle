//! Stanza dispatcher: O(1) lookup of IQ handlers by namespace, and
//! pipelined dispatch of message and presence handlers.
//!
//! IQ handlers self-register under the namespace they own; dispatch is a
//! single `HashMap::get`. Message and presence handlers register into
//! ordered pipelines — each handler runs until it returns
//! [`HandlerOutcome::Halt`] / [`HandlerOutcome::AwaitCallback`]
//! (messages) or the pipeline ends (presence).
//!
//! Exhaustiveness for IQ is enforced socially: if a new IQ namespace
//! arrives with no registered handler the dispatcher emits a `WARN` log
//! event, which surfaces in tests.

use super::event::{OutboundEvent, StanzaContext};
use super::message_context::MessageContext;
use super::traits::{HandlerId, HandlerOutcome, IqHandler, MessageHandler, PresenceHandler};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Level;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

/// Why a message-pipeline run terminated.
///
/// Carried alongside the emitted events in [`MessageDispatchOutcome`] so
/// the state machine can decide whether to register a pending op (for
/// [`MessageDispatchTermination::Awaiting`]) or simply hand the events to
/// the interpreter.
#[derive(Debug, PartialEq, Eq)]
pub enum MessageDispatchTermination {
    /// Every registered message handler ran to completion without
    /// halting or pausing.
    Completed,
    /// A handler returned [`HandlerOutcome::Halt`]; later handlers did
    /// not see the message.
    Halted { halted_at: HandlerId },
    /// A handler returned [`HandlerOutcome::AwaitCallback`]; the state
    /// machine is expected to register a pending op and resume dispatch
    /// at the handler immediately after `resume_after` when the matching
    /// callback arrives.
    Awaiting { resume_after: HandlerId },
}

/// Result of running the message pipeline once.
#[derive(Debug)]
pub struct MessageDispatchOutcome {
    /// Events emitted by handlers, in registration order. Includes events
    /// emitted by the halting / pausing handler itself.
    pub events: Vec<OutboundEvent>,
    /// Why the pipeline stopped.
    pub termination: MessageDispatchTermination,
}

/// Routes stanzas to the handlers registered for them.
///
/// Instances are cheap to clone (the handlers sit behind `Arc`) so the
/// same dispatcher can be shared between WebSocket C2S sessions, tests,
/// and other callers that feed typed stanzas into the machine.
#[derive(Clone, Default)]
pub struct StanzaDispatcher {
    iq_handlers: HashMap<&'static str, Arc<dyn IqHandler>>,
    message_handlers: Vec<Arc<dyn MessageHandler>>,
    presence_handlers: Vec<Arc<dyn PresenceHandler>>,
}

impl StanzaDispatcher {
    /// Create an empty dispatcher.
    pub fn new() -> Self {
        Self {
            iq_handlers: HashMap::new(),
            message_handlers: Vec::new(),
            presence_handlers: Vec::new(),
        }
    }

    /// Register an IQ handler. If a handler is already registered for the
    /// same namespace it is replaced.
    pub fn register_iq(&mut self, handler: Arc<dyn IqHandler>) {
        self.iq_handlers.insert(handler.namespace(), handler);
    }

    /// Append a message handler to the pipeline. Handlers run in
    /// registration order. Returns the [`HandlerId`] assigned to this
    /// handler — useful for tests asserting on `resume_after` semantics.
    pub fn register_message(&mut self, handler: Arc<dyn MessageHandler>) -> HandlerId {
        let id = HandlerId(self.message_handlers.len());
        self.message_handlers.push(handler);
        id
    }

    /// Append a presence handler to the pipeline. Handlers run in
    /// registration order.
    pub fn register_presence(&mut self, handler: Arc<dyn PresenceHandler>) {
        self.presence_handlers.push(handler);
    }

    /// Number of registered message handlers.
    pub fn message_handler_count(&self) -> usize {
        self.message_handlers.len()
    }

    /// Dispatch an IQ stanza.
    ///
    /// IQ `result` and `error` payloads arriving at the server are treated
    /// as client acknowledgements and silently consumed — they are not
    /// routed to a handler.
    pub fn dispatch_iq(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let element = match &iq.payload {
            IqType::Get(e) | IqType::Set(e) => e,
            IqType::Result(_) | IqType::Error(_) => return Vec::new(),
        };

        let ns = element.ns();
        match self.iq_handlers.get(ns.as_str()) {
            Some(handler) => handler.handle(iq, ctx),
            None => vec![OutboundEvent::Log {
                level: Level::WARN,
                message: format!(
                    "No handler registered for IQ namespace '{}' (id='{}')",
                    ns, iq.id
                ),
            }],
        }
    }

    /// Run the message pipeline against `message`.
    ///
    /// Handlers run in registration order until one returns
    /// [`HandlerOutcome::Halt`] or [`HandlerOutcome::AwaitCallback`]. The
    /// returned [`MessageDispatchOutcome`] carries every emitted event in
    /// order plus a discriminator for *why* the pipeline ended, so the
    /// state machine can wire pause-resume on awaiting outcomes.
    ///
    /// `message` is taken as `&mut` so XEP-0359-stamping and other
    /// rewriting handlers can mutate it for downstream handlers in the
    /// same pass. Read-only handlers simply don't write.
    ///
    /// `resume_after` is filled in by the dispatcher from the iteration
    /// index — handlers do not supply it themselves, so it cannot be
    /// out of range. The complementary bounds check on caller-supplied
    /// `resume_after` lives in [`StanzaDispatcher::resume_message`].
    pub fn dispatch_message(
        &self,
        message: &mut Message,
        ctx: &MessageContext<'_>,
    ) -> MessageDispatchOutcome {
        let mut events = Vec::new();
        for (idx, handler) in self.message_handlers.iter().enumerate() {
            let id = HandlerId(idx);
            match handler.handle(message, ctx) {
                HandlerOutcome::Continue(more) => events.extend(more),
                HandlerOutcome::Halt(more) => {
                    events.extend(more);
                    return MessageDispatchOutcome {
                        events,
                        termination: MessageDispatchTermination::Halted { halted_at: id },
                    };
                }
                HandlerOutcome::AwaitCallback(more) => {
                    events.extend(more);
                    return MessageDispatchOutcome {
                        events,
                        termination: MessageDispatchTermination::Awaiting { resume_after: id },
                    };
                }
            }
        }
        MessageDispatchOutcome {
            events,
            termination: MessageDispatchTermination::Completed,
        }
    }

    /// Resume a paused message pipeline at the handler immediately after
    /// `resume_after`.
    ///
    /// Used by the state machine when an
    /// [`super::event::InboundEvent`] callback arrives that matches a
    /// previously-emitted [`HandlerOutcome::AwaitCallback`]. The handlers
    /// up to and including `resume_after` do **not** re-run.
    ///
    /// `resume_after` is **bounds-checked**: an out-of-range id (e.g. a
    /// stale callback against a dispatcher whose handler set has changed,
    /// or a state-machine bug) returns a `Halted` outcome with an
    /// `ERROR`-level diagnostic event, never silently no-ops or skips
    /// handlers. This protects security-critical handlers (XEP-0191
    /// blocking, XEP-0359 stamping) from being bypassed by a malformed
    /// resume.
    pub fn resume_message(
        &self,
        message: &mut Message,
        ctx: &MessageContext<'_>,
        resume_after: HandlerId,
    ) -> MessageDispatchOutcome {
        let handler_count = self.message_handlers.len();
        if resume_after.0 >= handler_count {
            return MessageDispatchOutcome {
                events: vec![OutboundEvent::Log {
                    level: Level::ERROR,
                    message: format!(
                        "resume_message called with resume_after={:?} but only \
                         {handler_count} handler(s) are registered; halting to avoid \
                         silently dropping the pipeline",
                        resume_after
                    ),
                }],
                termination: MessageDispatchTermination::Halted {
                    halted_at: resume_after,
                },
            };
        }
        let mut events = Vec::new();
        let start = resume_after.0.saturating_add(1);
        for (idx, handler) in self.message_handlers.iter().enumerate().skip(start) {
            let id = HandlerId(idx);
            match handler.handle(message, ctx) {
                HandlerOutcome::Continue(more) => events.extend(more),
                HandlerOutcome::Halt(more) => {
                    events.extend(more);
                    return MessageDispatchOutcome {
                        events,
                        termination: MessageDispatchTermination::Halted { halted_at: id },
                    };
                }
                HandlerOutcome::AwaitCallback(more) => {
                    events.extend(more);
                    return MessageDispatchOutcome {
                        events,
                        termination: MessageDispatchTermination::Awaiting { resume_after: id },
                    };
                }
            }
        }
        MessageDispatchOutcome {
            events,
            termination: MessageDispatchTermination::Completed,
        }
    }

    /// Run every registered presence handler against this stanza in
    /// registration order; concatenate their emitted events.
    pub fn dispatch_presence(
        &self,
        presence: &Presence,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        self.presence_handlers
            .iter()
            .flat_map(|h| h.handle(presence, ctx))
            .collect()
    }

    /// True when there is at least one registered IQ handler for this
    /// namespace. Used by the transport adapter to decide whether to route
    /// a stanza through the state machine or the legacy path.
    pub fn has_iq_handler(&self, namespace: &str) -> bool {
        self.iq_handlers.contains_key(namespace)
    }
}
