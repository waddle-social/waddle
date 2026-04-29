//! Handler trait for the MUC room handler chain.
//!
//! Per #229 Q1, every handler is single-purpose and synchronous. Room
//! handlers receive a [`super::context::RoomContext`] (the frozen-at-
//! dispatch-start occupancy + sender + room state) plus the in-flight
//! `<message type='groupchat'>` and emit
//! [`super::super::event::OutboundEvent`]s.
//!
//! Handlers may mutate the in-flight message — XEP-0421 occupant-id
//! stamping, XEP-0359 stanza-id stamping, `from='room/nick'` rewriting
//! all rewrite the message so subsequent handlers see the canonicalized
//! form. The mutation is per-dispatch only; the interpreter discards
//! the rewritten message after the chain completes.
//!
//! The room chain is run *to completion* — there is no `AwaitCallback`
//! variant for room handlers because every concern they own
//! (canonicalization, archive eligibility, fan-out) is synchronous.
//! [`HandlerOutcome`] mirrors the user-side handler outcome shape so a
//! `Halt` cleanly stops the chain after a typed error reply.

use super::super::event::OutboundEvent;
use super::context::RoomContext;
use xmpp_parsers::message::Message;

/// Outcome of one room-handler invocation.
///
/// Mirrors [`super::super::traits::HandlerOutcome`] without the
/// `AwaitCallback` variant — room dispatch is fully synchronous.
#[derive(Debug)]
pub enum RoomHandlerOutcome {
    /// The handler emits zero or more events and the chain continues to
    /// the next handler.
    Continue(Vec<OutboundEvent>),
    /// The handler emits zero or more events and the chain terminates.
    /// Used for the XEP-0045 §7.4 sender-occupancy `<not-acceptable/>`
    /// reply and the managed-room `<forbidden/>` reply.
    Halt(Vec<OutboundEvent>),
}

impl RoomHandlerOutcome {
    /// Convenience: a `Continue` with no events.
    pub fn noop() -> Self {
        RoomHandlerOutcome::Continue(Vec::new())
    }
}

/// One stage of the MUC room handler chain.
///
/// Implementations must be pure: no I/O, no blocking, no actor sends.
/// The chain is registered into a [`super::dispatch::RoomDispatcher`]
/// in the locked Q7 order:
/// `OccupancyValidation → MucCanonicalize → MucArchive → Reflector`.
pub trait RoomHandler: Send + Sync {
    /// Human-readable identifier, used in logs and tests.
    fn name(&self) -> &'static str;

    /// Process a groupchat message stanza.
    ///
    /// `message` is `&mut Message` because canonicalization handlers
    /// (XEP-0359 stanza-id, XEP-0421 occupant-id, `from='room/nick'`
    /// rewrite) mutate the in-flight message so downstream handlers
    /// see the rewritten form.
    fn handle(&self, message: &mut Message, ctx: &RoomContext<'_>) -> RoomHandlerOutcome;
}
