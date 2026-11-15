//! Messages and presence a bound client addresses to a domain this
//! deployment does not serve (RFC 6120 §10.4).
//!
//! Such a stanza never enters local delivery, the ingress authority or a
//! roster: it is offered to the deployment's remote route, and a stanza the
//! route does not take is answered with a stanza error (RFC 6120 §10.4.3):
//! `remote-server-not-found` while S2S is disabled or the domain cannot
//! name a peer, and the router's own reason otherwise, which for a local
//! account's messages and presence is `service-unavailable` until account
//! traffic across S2S is reviewed (ADR-0020 D7). An error stanza is never
//! answered.

use jid::{FullJid, Jid};
use tracing::debug;
use waddle_xmpp::Stanza;

use super::super::transport_xml::stanza_to_xml;
use super::super::WebSocketState;
use crate::destination::{
    offer_stanza, unroutable_reply, StanzaOffer, UnroutableReason, UnservedDestination,
    REMOTE_ROUTE_OFFER_TIMEOUT,
};

/// The unserved destination of a client stanza addressed to `to`, or `None`
/// when this deployment serves it (or the home domain is unusable, which
/// startup refuses).
///
/// Every message and presence a client sends with a `to` passes here, so a
/// domain spelled exactly as a configured one is recognised as served
/// without preparing the configured domains; any other spelling is
/// classified in full.
pub(crate) fn unserved_destination(
    to: Option<&Jid>,
    domain: &str,
    state: &WebSocketState,
) -> Option<UnservedDestination> {
    let to = to?;
    let services = &state.deps.service_domains;
    let written = to.domain().as_str();
    let configured = [
        domain,
        services.muc.as_str(),
        services.spaces.as_str(),
        services.upload.as_str(),
        services.extensions.as_str(),
        services.push.as_str(),
        services.community.as_str(),
    ];
    if configured.contains(&written) {
        return None;
    }
    services
        .served_domains(domain)?
        .classify(Some(to))
        .unserved()
}

/// Route a bound client's message to an unserved domain.
pub(crate) async fn route_unserved_message(
    mut message: xmpp_parsers::message::Message,
    destination: UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> Vec<String> {
    message.from = Some(Jid::from(sender.clone()));
    offer(Stanza::Message(message), destination, sender, state).await
}

/// Route a bound client's presence to an unserved domain. A subscription
/// request or response carries the account's bare JID (RFC 6121 §3.1.2),
/// any other presence the bound full JID. Nothing on the roster changes.
pub(crate) async fn route_unserved_presence(
    mut presence: xmpp_parsers::presence::Presence,
    destination: UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> Vec<String> {
    use xmpp_parsers::presence::Type;
    presence.from = Some(match presence.type_ {
        Type::Subscribe | Type::Subscribed | Type::Unsubscribe | Type::Unsubscribed => {
            Jid::from(sender.to_bare())
        }
        _ => Jid::from(sender.clone()),
    });
    offer(Stanza::Presence(presence), destination, sender, state).await
}

async fn offer(
    stanza: Stanza,
    destination: UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> Vec<String> {
    let reply = match destination {
        UnservedDestination::Invalid(reason) => {
            let reason = UnroutableReason::InvalidDestination(reason);
            debug!(?reason, "Refusing a stanza to an unserved domain");
            unroutable_reply(&stanza, &Jid::from(sender.clone()), sender.domain(), reason)
        }
        UnservedDestination::Remote(remote) => match offer_stanza(
            state.deps.remote_route.as_ref(),
            remote,
            stanza,
            sender.domain(),
            REMOTE_ROUTE_OFFER_TIMEOUT,
        )
        .await
        {
            StanzaOffer::Accepted | StanzaOffer::Unattributed => None,
            StanzaOffer::Refused { reason, reply } => {
                debug!(?reason, "Refusing a stanza to an unserved domain");
                reply.map(|reply| *reply)
            }
        },
    };
    reply
        .map(|reply| stanza_to_xml(&reply))
        .into_iter()
        .collect()
}
