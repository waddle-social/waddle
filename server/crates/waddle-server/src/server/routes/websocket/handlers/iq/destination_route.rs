//! Address-based IQ routing that runs before namespace dispatch.
//!
//! Each client IQ is classified by [`ServedDomains::classify`]. IQ results and
//! errors are routed here by destination alone; requests to unserved domains
//! are offered to the deployment's [`RemoteRoute`] and, when it refuses them,
//! answered with a locally attributed stanza error. Everything else continues
//! to the namespace handlers.
//!
//! [`ServedDomains::classify`]: crate::destination::ServedDomains::classify
//! [`RemoteRoute`]: crate::destination::RemoteRoute

use jid::{FullJid, Jid};
use tracing::{debug, warn};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

use super::{
    build_iq_error_xml_typed, handle_caps_disco_info_result, internal_server_error_iq_error,
    route_full_jid_iq, stanza_to_xml, ConnectionPhase, ResponseBatch, WebSocketState,
};
use crate::destination::{
    unroutable_iq_reply, Destination, RemoteDestination, RemoteOutbound, RemoteRouteOutcome,
    UnroutableReason, UnservedDestination,
};
use crate::server::routes::interpret::OrderedRelayRouteOrigin;

/// Classify the IQ's `to` against this deployment's served domains.
///
/// `None` means the configured home domain is not a valid domainpart, so no
/// address can be classified.
pub(super) fn classify_iq_destination(
    iq: &Iq,
    domain: &str,
    state: &WebSocketState,
) -> Option<Destination> {
    let served = state.deps.service_domains.served_domains(domain);
    if served.is_none() {
        warn!("home domain is not a valid domainpart; IQ destination unclassifiable");
    }
    served.map(|served| served.classify(iq.to()))
}

/// Answer for an IQ whose destination could not be classified: requests fail
/// closed, results and errors are dropped (RFC 6120 §8.3.1 rule 8).
pub(super) fn unclassified_iq(iq: &Iq, response_from: Option<&str>) -> ResponseBatch {
    if !matches!(iq, Iq::Get { .. } | Iq::Set { .. }) {
        return ResponseBatch::default();
    }
    vec![build_iq_error_xml_typed(
        iq.id(),
        response_from,
        None,
        internal_server_error_iq_error("Internal server error."),
    )]
    .into()
}

/// Route an IQ result or error by destination. No namespace handler sees it.
///
/// - A local account full JID receives it through the peer route.
/// - A server-addressed reply (no `to`, or the bare server domain) can only
///   complete a request the server issued itself: the pending XEP-0115
///   disco#info query.
/// - A remote destination is offered to the remote route; a refusal is silent
///   because no error may answer a result or error.
/// - Every other destination is dropped: nothing local awaits it.
pub(super) async fn route_iq_response(
    iq: Iq,
    destination: Destination,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    ordered_relay_origin: Option<OrderedRelayRouteOrigin>,
    response_from: Option<&str>,
) -> ResponseBatch {
    let Some(sender) = phase.bound_jid() else {
        debug!(id = %iq.id(), "Dropping IQ result/error from an unbound stream");
        return ResponseBatch::default();
    };
    match destination {
        Destination::LocalFull(target) => route_full_jid_iq(
            iq,
            state,
            Some(sender),
            target,
            response_from,
            ordered_relay_origin,
        )
        .await
        .into(),
        Destination::Missing | Destination::Server => {
            handle_caps_disco_info_result(&iq, sender, state);
            ResponseBatch::default()
        }
        Destination::Remote(remote) => {
            if let Some((reason, _)) = offer_remote(state, sender, remote, iq).await {
                debug!(?reason, "Dropping unroutable remote IQ result/error");
            }
            ResponseBatch::default()
        }
        Destination::ServerResource(_)
        | Destination::LocalBare(_)
        | Destination::Service { .. }
        | Destination::Invalid { .. } => {
            debug!(id = %iq.id(), "Dropping IQ result/error with no local correlator");
            ResponseBatch::default()
        }
    }
}

/// Route an IQ get/set from a bound resource to an unserved domain: offer it
/// to the remote route, and answer a refusal with the bounded stanza error.
pub(super) async fn route_unserved_request(
    iq: Iq,
    destination: UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> ResponseBatch {
    let refused = match destination {
        UnservedDestination::Remote(remote) => offer_remote(state, sender, remote, iq).await,
        UnservedDestination::Invalid(reason) => {
            Some((UnroutableReason::InvalidDestination(reason), iq))
        }
    };
    let Some((reason, request)) = refused else {
        return ResponseBatch::default();
    };
    debug!(id = %request.id(), ?reason, "Refusing IQ request to an unserved domain");
    let sender_jid = Jid::from(sender.clone());
    unroutable_iq_reply(&request, &sender_jid, sender.domain(), reason)
        .map(|reply| vec![stanza_to_xml(&Stanza::Iq(Box::new(reply)))])
        .unwrap_or_default()
        .into()
}

/// Offer `iq` to the remote route with `from` stamped to the bound sender
/// (RFC 6120 §8.1.2.1). Returns the refused IQ and the reason, or `None` when
/// the route took ownership of it.
async fn offer_remote(
    state: &WebSocketState,
    sender: &FullJid,
    destination: RemoteDestination,
    mut iq: Iq,
) -> Option<(UnroutableReason, Iq)> {
    *iq.from_mut() = Some(Jid::from(sender.clone()));
    let outbound = RemoteOutbound {
        sender: Jid::from(sender.clone()),
        destination,
        stanza: Stanza::Iq(Box::new(iq)),
    };
    match state.deps.remote_route.route(outbound).await {
        RemoteRouteOutcome::Accepted => None,
        RemoteRouteOutcome::Unroutable { reason, outbound } => match outbound.stanza {
            Stanza::Iq(iq) => Some((reason, *iq)),
            Stanza::Message(_) | Stanza::Presence(_) => {
                warn!("remote route returned a different stanza kind than it was offered");
                None
            }
        },
    }
}
