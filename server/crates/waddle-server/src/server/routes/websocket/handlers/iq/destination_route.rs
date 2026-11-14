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
    not_authorized_iq_error, route_full_jid_iq, stanza_to_xml, ConnectionPhase, ResponseBatch,
    WebSocketState,
};
use crate::destination::{
    offer_iq, unroutable_iq_reply, Destination, IqOffer, RemoteDestination, UnroutableReason,
    UnservedDestination, REMOTE_ROUTE_OFFER_TIMEOUT,
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
            if let IqOffer::Refused { reason, .. } = offer_remote(state, sender, remote, iq).await {
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
    let reply = match destination {
        UnservedDestination::Remote(remote) => {
            match offer_remote(state, sender, remote, iq).await {
                IqOffer::Accepted | IqOffer::Unattributed => None,
                IqOffer::Refused { reason, reply } => {
                    debug!(?reason, "Refusing IQ request to an unserved domain");
                    reply
                }
            }
        }
        UnservedDestination::Invalid(reason) => {
            let reason = UnroutableReason::InvalidDestination(reason);
            debug!(id = %iq.id(), ?reason, "Refusing IQ request to an unserved domain");
            unroutable_iq_reply(&iq, &Jid::from(sender.clone()), sender.domain(), reason)
                .map(Box::new)
        }
    };
    reply
        .map(|reply| vec![stanza_to_xml(&Stanza::Iq(reply))])
        .unwrap_or_default()
        .into()
}

/// Refuse an IQ get/set to an unserved domain from a stream that has not bound
/// a resource.
///
/// RFC 6120 §7.1 forbids the server to process a stanza that a client sends,
/// before resource binding, to any entity other than the server or its own
/// account, and names `<not-authorized/>` as the condition. The request is
/// neither routed (there is no bound address to stamp as `from`) nor handed
/// to a namespace handler (which could answer for the remote entity). The
/// condition is applied as a stanza error rather than by closing the stream,
/// like the other errors this server returns before binding. The reply has no
/// `to` and no `by` (optional under §8.3.2): the stream has no bound address,
/// and no routing attribution exists for a stanza that was never routed.
pub(super) fn refuse_unbound_unserved_request(
    iq: &Iq,
    response_from: Option<&str>,
) -> ResponseBatch {
    debug!(id = %iq.id(), "Refusing IQ request to an unserved domain before resource binding");
    vec![build_iq_error_xml_typed(
        iq.id(),
        response_from,
        None,
        not_authorized_iq_error("Bind a resource before addressing other domains."),
    )]
    .into()
}

/// Offer `iq` to the remote route with `from` stamped to the bound sender
/// (RFC 6120 §8.1.2.1), bounded by [`REMOTE_ROUTE_OFFER_TIMEOUT`].
async fn offer_remote(
    state: &WebSocketState,
    sender: &FullJid,
    destination: RemoteDestination,
    mut iq: Iq,
) -> IqOffer {
    *iq.from_mut() = Some(Jid::from(sender.clone()));
    offer_iq(
        state.deps.remote_route.as_ref(),
        destination,
        iq,
        sender.domain(),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await
}
