//! Messages and presence a bound client addresses to a domain this
//! deployment does not serve (RFC 6120 §10.4).
//!
//! Such a stanza never enters local delivery, the ingress authority or a
//! roster: it is offered to the deployment's remote route, and a stanza the
//! route does not take is answered with a stanza error (RFC 6120 §10.4.3):
//! `remote-server-not-found` while S2S is disabled or the domain cannot
//! name a peer, and the router's own reason otherwise, which for a local
//! account's messages and presence is `service-unavailable` until account
//! traffic across S2S is reviewed (ADR-0020 D7), except a resource's
//! presence and messages for a remote room it joins, occupies or leaves.
//! An error stanza is never answered.
//!
//! With S2S enabled the route also takes a local account's direct messages
//! and presence for remote contacts (ADR-0020 D7, gate G18;
//! `s2s-account-traffic-2047.md`). Before such a stanza is offered, the
//! sender's own XEP-0191 list is checked, as for IQ requests, and a
//! subscription stanza changes the account's half of the subscription only
//! once the route took it. Once accepted, an eligible message is copied to
//! the sender's other Carbons-enabled resources as a `<sent/>` carbon
//! (XEP-0280 §6.1, §7); see `sent_carbon_copy`. With S2S off nothing here
//! reads a roster or a blocking list: the route refuses first.

use jid::{FullJid, Jid};
use tracing::debug;
use waddle_xmpp::protocol::{CarbonKind, OutboundEvent};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

use super::super::interpret_loop::build_interpret_deps;
use super::super::transport_xml::stanza_to_xml;
use super::super::WebSocketState;
use crate::destination::{
    offer_stanza, unroutable_reply, RemoteDestination, StanzaOffer, UnroutableReason,
    UnservedDestination, REMOTE_ROUTE_OFFER_TIMEOUT,
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

/// Route a bound client's message to an unserved domain, and copy it to
/// the sender's other Carbons-enabled resources once the route accepts it.
pub(crate) async fn route_unserved_message(
    mut message: Message,
    destination: UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> Vec<String> {
    message.from = Some(Jid::from(sender.clone()));
    if let Some(reply) = blocked_reply(
        &Stanza::Message(message.clone()),
        &destination,
        sender,
        state,
    )
    .await
    {
        return reply;
    }
    let carbon = match &destination {
        UnservedDestination::Remote(remote) => sent_carbon_copy(&message, remote),
        UnservedDestination::Invalid(_) => None,
    };
    let offered = offer(Stanza::Message(message), destination, sender, state).await;
    if offered.accepted {
        if let Some(carbon) = carbon {
            send_sent_carbons(carbon, sender, state).await;
        }
    }
    offered.replies
}

/// The message a `<sent/>` carbon forwards when `message`, addressed to
/// `remote`, is eligible for one, addressed as the route delivers it.
///
/// Eligibility is the server's XEP-0280 §6.1 rule set: a message of type
/// `chat`, a `normal` message with a body, or one with an instant-messaging
/// payload, which covers a private message to a room occupant; never a
/// `groupchat` message or one carrying `<private/>` or a full-JID
/// `<no-copy/>` hint. A message carrying a mediated invitation or a XEP-0249
/// direct invitation is eligible with or without a body.
///
/// The copy goes to every other Carbons-enabled resource of the sender,
/// not only to those sharing the sender's nickname in the room: §6.1 puts
/// that limit in a footnote as a SHOULD and asks clients that are not in
/// the room to ignore such a copy or offer to join first.
fn sent_carbon_copy(message: &Message, remote: &RemoteDestination) -> Option<Message> {
    if !waddle_xmpp::carbons::should_copy_message(message) {
        return None;
    }
    let mut copy = message.clone();
    copy.to = Some(remote.address().clone());
    Some(copy)
}

/// Copy `carbon`, a message `sender` sent that the remote route accepted,
/// to the sender's other Carbons-enabled resources, live or detached.
/// The interpreter applies each resource's current Carbons setting.
async fn send_sent_carbons(carbon: Message, sender: &FullJid, state: &WebSocketState) {
    let deps = build_interpret_deps(state, None);
    let event = OutboundEvent::SendCarbons {
        owner: sender.to_bare(),
        message: Box::new(carbon),
        kind: CarbonKind::Sent,
        exclude: vec![sender.clone()],
    };
    let outcome = crate::server::routes::interpret::interpret(vec![event], &deps).await;
    debug_assert!(
        outcome.frames.is_empty(),
        "carbon fan-out returns no frames"
    );
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
        Type::Subscribe
        | Type::Subscribed
        | Type::Unsubscribe
        | Type::Unsubscribed
        | Type::Probe => Jid::from(sender.to_bare()),
        _ => Jid::from(sender.clone()),
    });
    // A denial of a remote sender's recorded request is the account's own:
    // the record is forgotten even when the stanza is refused, such as to
    // an address the account blocks or with S2S off (gate G18). With S2S
    // on, the subscription path forgets it as it decides what to route.
    let denial = presence.type_ == Type::Unsubscribed;
    if let Some(reply) = blocked_reply(
        &Stanza::Presence(presence.clone()),
        &destination,
        sender,
        state,
    )
    .await
    {
        if denial {
            forget_denied_request(state, sender, &destination).await;
        }
        return reply;
    }
    let subscription = matches!(
        presence.type_,
        Type::Subscribe | Type::Subscribed | Type::Unsubscribe | Type::Unsubscribed
    );
    if let (true, true, UnservedDestination::Remote(remote)) = (
        subscription,
        crate::server::routes::websocket::s2s_accounts::s2s_enabled(state),
        &destination,
    ) {
        return super::presence::route_remote_subscription(state, sender, presence, remote.clone())
            .await;
    }
    if denial {
        forget_denied_request(state, sender, &destination).await;
    }
    offer(Stanza::Presence(presence), destination, sender, state)
        .await
        .replies
}

/// Forget the subscription request the remote `destination` sent the
/// sender's account, which the sender denies.
async fn forget_denied_request(
    state: &WebSocketState,
    sender: &FullJid,
    destination: &UnservedDestination,
) {
    if let UnservedDestination::Remote(remote) = destination {
        super::presence::forget_request(state, &sender.to_bare(), &remote.address().to_bare())
            .await;
    }
}

/// XEP-0191 §3.3: with S2S enabled, a message or presence to a remote
/// address on the sender's own blocking list is answered with
/// `not-acceptable` and `<blocked/>` and not routed. With S2S off the
/// route's own refusal answers first, as before.
async fn blocked_reply(
    stanza: &Stanza,
    destination: &UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> Option<Vec<String>> {
    let UnservedDestination::Remote(remote) = destination else {
        return None;
    };
    if !super::presence::sender_blocks(state, sender, remote.address()).await {
        return None;
    }
    debug!("Refusing a stanza to a remote address on the sender's blocking list");
    Some(
        super::presence::outgoing_block_reply(stanza, sender)
            .map(|reply| stanza_to_xml(&reply))
            .into_iter()
            .collect(),
    )
}

/// What became of a stanza offered to the remote route.
struct Offered {
    /// The route took the stanza.
    accepted: bool,
    /// The frames for the sender: the error answering a refused stanza.
    replies: Vec<String>,
}

async fn offer(
    stanza: Stanza,
    destination: UnservedDestination,
    sender: &FullJid,
    state: &WebSocketState,
) -> Offered {
    let (accepted, reply) = match destination {
        UnservedDestination::Invalid(reason) => {
            let reason = UnroutableReason::InvalidDestination(reason);
            debug!(?reason, "Refusing a stanza to an unserved domain");
            (
                false,
                unroutable_reply(&stanza, &Jid::from(sender.clone()), sender.domain(), reason),
            )
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
            StanzaOffer::Accepted => (true, None),
            StanzaOffer::Unattributed => (false, None),
            StanzaOffer::Refused { reason, reply } => {
                debug!(?reason, "Refusing a stanza to an unserved domain");
                (false, reply.map(|reply| *reply))
            }
        },
    };
    Offered {
        accepted,
        replies: reply
            .map(|reply| stanza_to_xml(&reply))
            .into_iter()
            .collect(),
    }
}
