//! XEP-specific handler implementations.
//!
//! Each submodule owns exactly one XEP concern (ping, session, …) and
//! implements the [`super::traits::IqHandler`] trait. A single
//! [`register_default_handlers`] builder wires the complete set into a
//! [`super::dispatch::StanzaDispatcher`].
//!
//! # Migration status
//!
//! ✅ Migrated to this module:
//! - XEP-0199 ping — [`ping::PingHandler`]
//! - RFC 3921 session — [`session::SessionHandler`]
//! - RFC 6121 roster — [`roster::RosterHandler`] (stateless empty-roster ack)
//! - XEP-0280 carbons enable/disable — [`carbons::CarbonsHandler`]
//!   (ack only; per-connection toggle still pending)
//! - XEP-0092 software version — [`version::VersionHandler`]
//! - XEP-0202 entity time — [`time::TimeHandler`]
//!
//! ⏳ Staying in the legacy `websocket.rs` path until the two-phase async
//! callback machinery lands:
//! - XEP-0030 disco#info / disco#items (needs MUC registry + DB lookup)
//! - XEP-0363 HTTP upload (needs `create_upload_slot` async call)
//! - XEP-0313 MAM query (needs MamStorage async call)
//! - XEP-0045 muc#owner (needs MUC registry)
//! - XEP-0166 Jingle (needs SfuServiceActor)

pub mod carbons;
pub mod ping;
pub mod roster;
pub mod session;
pub mod time;
pub mod version;

use super::dispatch::StanzaDispatcher;
use std::sync::Arc;
use xmpp_parsers::iq::{Iq, IqType};

/// Register every sync IQ handler that has been migrated so far.
///
/// As more handlers cross the sans-I/O boundary they will be added here.
/// Handlers requiring async I/O emit [`super::event::OutboundEvent`]
/// callback variants instead of doing the work inline, so they remain
/// synchronous from the state machine's point of view.
pub fn register_default_handlers(dispatcher: &mut StanzaDispatcher) {
    dispatcher.register_iq(Arc::new(ping::PingHandler));
    dispatcher.register_iq(Arc::new(session::SessionHandler));
    dispatcher.register_iq(Arc::new(roster::RosterHandler));
    dispatcher.register_iq(Arc::new(carbons::CarbonsHandler));
    dispatcher.register_iq(Arc::new(version::VersionHandler));
    dispatcher.register_iq(Arc::new(time::TimeHandler));
}

/// Build an empty `type="result"` IQ with `from`/`to` swapped relative to
/// the original request.
///
/// Used by handlers that need to acknowledge an IQ-set with no payload
/// (XEP-0199 ping, RFC 3921 session establishment, and several others).
pub(crate) fn empty_iq_result(original: &Iq) -> Iq {
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(None),
    }
}
