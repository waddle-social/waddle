//! IQs routed by the interpreter to an unserved domain.
//!
//! Client IQs addressed to unserved domains are refused before namespace
//! dispatch. This arm covers IQs that a handler or the server itself routes
//! there, so that both paths share the deployment's remote route and the
//! same RFC 6120 §10.4.3 error.

use jid::{DomainPart, Jid};
use std::str::FromStr;
use tracing::debug;
use waddle_xmpp::telemetry::call::PendingCallSetupRoute;
use waddle_xmpp::Stanza;

use super::implementation::revoke_credential_minted_into_undeliverable_iq;
use super::Deps;
use crate::destination::{
    offer_iq, unroutable_iq_reply, without_server_minted_credentials, FederationDisabled, IqOffer,
    RemoteRoute, UnroutableReason, UnservedDestination, REMOTE_ROUTE_OFFER_TIMEOUT,
};

/// Classify `target` for an interpreted IQ. `None` keeps the local delivery
/// path when:
///
/// - the target is served here;
/// - the interpreter has no WebSocket state and so does not know the
///   configured service domains; or
/// - the effect sink is planning. A plan records effects for later execution
///   and must not perform the hand-off to the remote route itself, so a
///   planned IQ to an unserved domain follows the planned local path, which
///   has no resource at that domain and cannot deliver it there.
pub(in crate::server::routes::interpret::route_to_connection) fn unserved_iq_target(
    deps: &Deps<'_>,
    target: &Jid,
    stanza: &Stanza,
) -> Option<UnservedDestination> {
    if !matches!(stanza, Stanza::Iq(_)) || deps.effects.is_planning() {
        return None;
    }
    deps.web_socket_state?
        .deps
        .service_domains
        .served_domains(deps.local_domain)?
        .classify(Some(target))
        .unserved()
}

/// Offer the IQ to the remote route. A refused request is answered with the
/// bounded error addressed to its `from`; a refused result or error, or a
/// request without `from`, is dropped.
///
/// A remote recipient never receives a credential this server minted: any is
/// revoked and removed before the IQ is offered or refused.
pub(in crate::server::routes::interpret::route_to_connection) async fn route_unserved_iq(
    deps: &Deps<'_>,
    target: UnservedDestination,
    stanza: Stanza,
    call_setup: Option<PendingCallSetupRoute>,
) -> Vec<Stanza> {
    revoke_credential_minted_into_undeliverable_iq(&stanza, deps.sfu);
    let (Stanza::Iq(iq), Ok(home)) = (stanza, DomainPart::from_str(deps.local_domain)) else {
        if let Some(ticket) = call_setup {
            ticket.undeliverable();
        }
        return Vec::new();
    };
    let iq = without_server_minted_credentials(*iq);
    let offer = match target {
        UnservedDestination::Invalid(reason) => {
            let reason = UnroutableReason::InvalidDestination(reason);
            match iq.from() {
                Some(sender) => IqOffer::Refused {
                    reason,
                    reply: unroutable_iq_reply(&iq, sender, &home, reason).map(Box::new),
                },
                None => IqOffer::Unattributed,
            }
        }
        UnservedDestination::Remote(destination) => {
            let route: &dyn RemoteRoute = match deps.web_socket_state {
                Some(state) => state.deps.remote_route.as_ref(),
                None => &FederationDisabled,
            };
            offer_iq(route, destination, iq, &home, REMOTE_ROUTE_OFFER_TIMEOUT).await
        }
    };
    let reply = match offer {
        IqOffer::Accepted => {
            if let Some(ticket) = call_setup {
                ticket.delivered();
            }
            return Vec::new();
        }
        IqOffer::Refused { reason, reply } => {
            debug!(?reason, "Refusing interpreted IQ to an unserved domain");
            reply
        }
        IqOffer::Unattributed => {
            debug!("Dropping interpreted IQ to an unserved domain without a sender");
            None
        }
    };
    if let Some(ticket) = call_setup {
        ticket.undeliverable();
    }
    reply
        .map(|reply| vec![Stanza::Iq(reply)])
        .unwrap_or_default()
}
