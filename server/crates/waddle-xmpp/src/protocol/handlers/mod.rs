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
//! - XEP-0280 carbons enable/disable — [`carbons::CarbonsHandler`]
//!   (ack only; per-connection toggle still pending)
//! - XEP-0092 software version — [`version::VersionHandler`]
//! - XEP-0202 entity time — [`time::TimeHandler`]
//!
//! ⏳ Staying in the legacy `websocket.rs` path until the two-phase async
//! callback machinery lands:
//! - XEP-0030 disco#info / disco#items (needs MUC registry + DB lookup)
//! - RFC 6121 roster (needs durable roster storage + push fanout)
//! - XEP-0363 HTTP upload (needs `create_upload_slot` async call)
//! - XEP-0313 MAM query (needs MamStorage async call)
//! - XEP-0045 muc#owner (needs MUC registry)
//! - XEP-0166 Jingle (needs SfuServiceActor)

pub mod archive;
pub mod blocking_filter;
pub mod canonicalize;
pub mod carbons;
pub mod carbons_message;
pub mod enrichment_dispatch;
pub mod errors;
pub mod inbox;
pub mod offline_delivery;
pub mod ping;
pub mod receipts;
pub mod rich_target_validation;
pub mod route;
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
    dispatcher.register_iq(Arc::new(carbons::CarbonsHandler));
    dispatcher.register_iq(Arc::new(version::VersionHandler));
    dispatcher.register_iq(Arc::new(time::TimeHandler));
}

/// Register the full message-pipeline handler chain in the order
/// locked by issue #229 Q2(a):
///
/// 1. [`blocking_filter::BlockingFilterHandler`] (XEP-0191) — drop
///    blocked stanzas before they reach later stages (privacy
///    invariant).
/// 2. [`rich_target_validation::RichTargetValidationHandler`]
///    (XEP-0308 / 0424 / 0461) — validate rich-message targets
///    against the archive before stamping or routing.
/// 3. [`canonicalize::CanonicalizeHandler`] (XEP-0359) — strip
///    foreign `<stanza-id>` siblings and stamp under the local
///    archive.
/// 4. [`enrichment_dispatch::EnrichmentDispatchHandler`] — request
///    embed enrichment via two-phase callback so later stages see the
///    rewritten message.
/// 5. [`archive::ArchiveHandler`] (XEP-0313) — persist to MAM under
///    the local user's archive (sender-side and recipient-side
///    distinct entries).
/// 6. [`carbons_message::CarbonsMessageHandler`] (XEP-0280) — fan
///    out sent / received carbons to other resources of the local
///    user.
/// 7. [`inbox::InboxHandler`] — project the conversation into the
///    Waddle inbox.
/// 8. [`route::RouteHandler`] — final stage for the content stanza:
///    route 1:1 to the destination connection, dispatch groupchat to
///    the room chain, or write to the local wire on the recipient pass.
/// 9. [`receipts::ReceiptsHandler`] (XEP-0184) — after the content
///    stanza is queued for delivery, emit a separate receipt route for
///    eligible direct messages that requested one.
pub fn register_default_message_handlers(dispatcher: &mut StanzaDispatcher) {
    dispatcher.register_message(Arc::new(blocking_filter::BlockingFilterHandler));
    dispatcher.register_message(Arc::new(
        rich_target_validation::RichTargetValidationHandler,
    ));
    dispatcher.register_message(Arc::new(canonicalize::CanonicalizeHandler));
    dispatcher.register_message(Arc::new(enrichment_dispatch::EnrichmentDispatchHandler));
    dispatcher.register_message(Arc::new(archive::ArchiveHandler));
    dispatcher.register_message(Arc::new(offline_delivery::OfflineDeliveryHandler));
    dispatcher.register_message(Arc::new(carbons_message::CarbonsMessageHandler));
    dispatcher.register_message(Arc::new(inbox::InboxHandler));
    dispatcher.register_message(Arc::new(route::RouteHandler));
    dispatcher.register_message(Arc::new(receipts::ReceiptsHandler));
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
