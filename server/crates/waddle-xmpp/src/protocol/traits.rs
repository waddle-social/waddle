//! Handler traits.
//!
//! Every handler method is *synchronous*. Handlers emit
//! [`super::event::OutboundEvent`]s; they never perform I/O, make actor
//! calls, or block on database queries. Work that requires async is modelled
//! via the two-phase flow documented in the plan's *Design patterns*
//! section: a handler emits an outbound event carrying a
//! [`super::event::CallbackId`], the interpreter performs the work, and a
//! response arrives as a follow-up [`super::event::InboundEvent`].

use super::event::{IqContext, OutboundEvent};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

/// Handles a single IQ namespace (e.g. `urn:xmpp:ping`).
///
/// Dispatch is O(1) via [`super::dispatch::StanzaDispatcher`]'s `HashMap`
/// keyed on [`IqHandler::namespace`]. There is deliberately no
/// `matches(&self, iq)` method: dispatch is by namespace only, so there is
/// exactly one handler per IQ payload namespace. If a single XEP needs to
/// branch on element content (e.g. disco#info vs disco#items under different
/// `to` addresses) the handler does that branching internally.
pub trait IqHandler: Send + Sync {
    /// The XML namespace this handler owns.
    ///
    /// Must be constant for the lifetime of the handler — it is used as a
    /// `HashMap` key at registration time.
    fn namespace(&self) -> &'static str;

    /// Process an IQ whose inner-element namespace matched
    /// [`IqHandler::namespace`].
    ///
    /// Implementations must be pure: no I/O, no blocking, no actor sends.
    /// Returning an empty `Vec` is valid and means "silently consumed".
    fn handle(&self, iq: &Iq, ctx: &IqContext<'_>) -> Vec<OutboundEvent>;
}

/// Handles message stanzas.
///
/// Unlike [`IqHandler`], message handling is **pipelined**: every
/// registered message handler sees every message in the order they were
/// registered. Each handler decides independently whether to emit outbound
/// events (e.g. one handler archives, another broadcasts, a third requests
/// GitHub-link enrichment). This matches the shape of the existing
/// message-processing pipeline in `websocket.rs`.
///
/// Implementations must be pure — they never perform I/O directly. Work
/// that requires async (e.g. enrichment, MAM storage) is expressed as
/// [`super::event::OutboundEvent`] callback variants, with the eventual
/// result arriving as an [`super::event::InboundEvent`].
pub trait MessageHandler: Send + Sync {
    /// Human-readable identifier, used in logs and for debugging dispatch
    /// order. Not a routing key.
    fn name(&self) -> &'static str;

    /// Process a message stanza.
    ///
    /// Returning an empty `Vec` is valid and means "this handler had
    /// nothing to add for this message".
    fn handle(&self, message: &Message, ctx: &IqContext<'_>) -> Vec<OutboundEvent>;
}

/// Handles presence stanzas.
///
/// Presence is also pipelined, but a specific concern (e.g. MUC join/leave
/// vs. regular availability) can be expressed by having the handler return
/// early when the stanza doesn't match its shape.
pub trait PresenceHandler: Send + Sync {
    /// Human-readable identifier, used in logs and for debugging dispatch
    /// order.
    fn name(&self) -> &'static str;

    /// Process a presence stanza.
    ///
    /// Returning an empty `Vec` is valid — most presence handlers are
    /// only relevant for a specific subset of stanzas (e.g. the MUC-join
    /// handler only reacts when the stanza carries an
    /// `http://jabber.org/protocol/muc` child element).
    fn handle(&self, presence: &Presence, ctx: &IqContext<'_>) -> Vec<OutboundEvent>;
}
