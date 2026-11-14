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
use xmpp_parsers::iq::Iq;

use super::implementation::revoke_credential_minted_into_undeliverable_iq;
use super::Deps;
use crate::destination::{
    unroutable_iq_reply, FederationDisabled, RemoteOutbound, RemoteRoute, RemoteRouteOutcome,
    UnroutableReason, UnservedDestination,
};

/// Classify `target` for an interpreted IQ. `None` keeps local delivery: the
/// target is served here, or the interpreter has no WebSocket state and so
/// does not know the configured service domains.
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

/// Offer the IQ to the remote route. A refused request is answered with
/// `remote-server-not-found` addressed to its `from`; a refused result or
/// error, or a request without `from`, is dropped.
pub(in crate::server::routes::interpret::route_to_connection) async fn route_unserved_iq(
    deps: &Deps<'_>,
    target: UnservedDestination,
    stanza: Stanza,
    call_setup: Option<PendingCallSetupRoute>,
) -> Vec<Stanza> {
    let Some(sender) = stanza_sender(&stanza) else {
        debug!("Dropping interpreted IQ to an unserved domain without a sender");
        if let Some(ticket) = call_setup {
            ticket.undeliverable();
        }
        return Vec::new();
    };
    let (reason, stanza) = match target {
        UnservedDestination::Invalid(reason) => {
            (UnroutableReason::InvalidDestination(reason), stanza)
        }
        UnservedDestination::Remote(destination) => {
            let outbound = RemoteOutbound {
                sender: sender.clone(),
                destination,
                stanza,
            };
            let outcome = match deps.web_socket_state {
                Some(state) => state.deps.remote_route.route(outbound).await,
                None => FederationDisabled.route(outbound).await,
            };
            match outcome {
                RemoteRouteOutcome::Accepted => {
                    if let Some(ticket) = call_setup {
                        ticket.delivered();
                    }
                    return Vec::new();
                }
                RemoteRouteOutcome::Unroutable { reason, outbound } => (reason, outbound.stanza),
            }
        }
    };
    if let Some(ticket) = call_setup {
        ticket.undeliverable();
    }
    revoke_credential_minted_into_undeliverable_iq(&stanza, deps.sfu);
    let Stanza::Iq(iq) = &stanza else {
        return Vec::new();
    };
    let Ok(home) = DomainPart::from_str(deps.local_domain) else {
        return Vec::new();
    };
    debug!(id = %iq.id(), ?reason, "Refusing interpreted IQ to an unserved domain");
    unroutable_iq_reply(iq, &sender, &home, reason)
        .map(sanitize_jingle_echo)
        .map(|reply| vec![Stanza::Iq(Box::new(reply))])
        .unwrap_or_default()
}

fn stanza_sender(stanza: &Stanza) -> Option<Jid> {
    match stanza {
        Stanza::Iq(iq) => iq.from().cloned(),
        Stanza::Message(message) => message.from.clone(),
        Stanza::Presence(presence) => presence.from.clone(),
    }
}

/// RFC 6120 §8.3.1 lets an error echo the request. A forwarded Jingle request
/// can carry a server-minted media credential for the addressee, so its echo
/// omits that transport (#1444).
fn sanitize_jingle_echo(mut reply: Iq) -> Iq {
    if let Iq::Error {
        payload: Some(payload),
        ..
    } = &mut reply
    {
        if payload.is("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE) {
            *payload =
                waddle_xmpp::protocol::handlers::jingle::credential_free_jingle_echo(payload);
        }
    }
    reply
}
