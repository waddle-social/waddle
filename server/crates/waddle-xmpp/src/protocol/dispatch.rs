//! Stanza dispatcher: O(1) lookup of IQ handlers by namespace, and
//! pipelined dispatch of message and presence handlers.
//!
//! This module is the inverse of the old `if frame.contains(ns) { … }`
//! chain. IQ handlers self-register under the namespace they own, and
//! dispatch is a single `HashMap::get`. Message and presence handlers
//! register into ordered pipelines: every registered handler sees every
//! stanza and independently decides whether to emit events.
//!
//! Exhaustiveness for IQ is enforced socially: if a new IQ namespace
//! arrives with no registered handler the dispatcher emits a `WARN` log
//! event, which surfaces in tests.

use super::event::{IqContext, OutboundEvent};
use super::traits::{IqHandler, MessageHandler, PresenceHandler};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Level;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

/// Routes stanzas to the handlers registered for them.
///
/// Instances are cheap to clone (the handlers sit behind `Arc`) so the
/// same dispatcher can be shared between the TCP and WebSocket transports.
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
    /// registration order.
    pub fn register_message(&mut self, handler: Arc<dyn MessageHandler>) {
        self.message_handlers.push(handler);
    }

    /// Append a presence handler to the pipeline. Handlers run in
    /// registration order.
    pub fn register_presence(&mut self, handler: Arc<dyn PresenceHandler>) {
        self.presence_handlers.push(handler);
    }

    /// Dispatch an IQ stanza.
    ///
    /// IQ `result` and `error` payloads arriving at the server are treated
    /// as client acknowledgements and silently consumed — they are not
    /// routed to a handler.
    pub fn dispatch_iq(&self, iq: &Iq, ctx: &IqContext<'_>) -> Vec<OutboundEvent> {
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

    /// Run every registered message handler against this stanza in
    /// registration order; concatenate their emitted events.
    pub fn dispatch_message(&self, message: &Message, ctx: &IqContext<'_>) -> Vec<OutboundEvent> {
        self.message_handlers
            .iter()
            .flat_map(|h| h.handle(message, ctx))
            .collect()
    }

    /// Run every registered presence handler against this stanza in
    /// registration order; concatenate their emitted events.
    pub fn dispatch_presence(
        &self,
        presence: &Presence,
        ctx: &IqContext<'_>,
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
