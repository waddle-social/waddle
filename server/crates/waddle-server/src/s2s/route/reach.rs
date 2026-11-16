//! What a stanza from an S2S stream may reach (ADR-0020 D10 and D7; gate
//! G23).
//!
//! Every stanza that passes an inbound stream's address checks is
//! addressed to a domain this deployment serves; this module decides, from
//! its destination and kind alone, which local service or handler it
//! reaches. The router answers the server domain's service discovery and
//! ping itself, completes a local resource's own pending request, or the
//! server's own room verification, with a correlated response, and
//! refuses everything else. No namespace handler written for client
//! sessions sees a stanza from an S2S stream.
//!
//! One decision depends on state and so is taken by the router before this
//! one: a message or presence from a remote room to the full JID of a
//! local resource that occupies, is joining or has just left that room is
//! delivered to that resource (D7's room exception; the router's `rooms`
//! module, against the shared occupancy table). Every other stanza to a
//! local account, including one from a room the resource never joined, is
//! decided here.
//!
//! The inventory with each entry's status is recorded in
//! `server/docs/planning/s2s-routing-2047.md`; enabling a further service
//! or operation for remote senders is its own reviewed change to this
//! module, that inventory and the reachability suite.

use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

use crate::destination::{Destination, ServedService};

/// A destination this deployment serves, as an inbound S2S stanza names
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalTarget {
    /// The bare server domain (RFC 6120 §10.5.1).
    ServerDomain,
    /// The server domain with a resourcepart, which no entity binds.
    ServerResource,
    /// A local account's bare JID.
    AccountBare,
    /// A local account's full JID.
    AccountResource,
    /// Any address at a configured service domain.
    Service(ServedService),
}

impl LocalTarget {
    /// The served destination `destination` names, or `None` for one this
    /// deployment does not serve.
    pub fn of(destination: &Destination) -> Option<Self> {
        match destination {
            Destination::Server => Some(Self::ServerDomain),
            Destination::ServerResource(_) => Some(Self::ServerResource),
            Destination::LocalBare(_) => Some(Self::AccountBare),
            Destination::LocalFull(_) => Some(Self::AccountResource),
            Destination::Service { service, .. } => Some(Self::Service(*service)),
            Destination::Missing | Destination::Remote(_) | Destination::Invalid { .. } => None,
        }
    }
}

/// What the router does with a stanza from an S2S stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundReach {
    /// XEP-0030 `disco#info` of the server domain, answered by the router.
    ServerDiscoInfo,
    /// XEP-0030 `disco#items` of the server domain, answered by the router.
    ServerDiscoItems,
    /// XEP-0199 ping of the server domain, answered by the router.
    ServerPing,
    /// An IQ result or error for the server domain, delivered only when it
    /// completes a request this server sent: the `disco#info` that learns
    /// whether a remote JID a local resource joins is a room.
    ServerResponse,
    /// An IQ result or error for a local resource, delivered only when it
    /// completes a request that resource sent (D7's first exception).
    CorrelatedResponse,
    /// Refused: an IQ request or a message receives `service-unavailable`;
    /// presence, IQ responses and errors are dropped without a reply.
    Refused(Refusal),
}

/// Why a stanza from an S2S stream is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Traffic to a local account, refused as if the account did not
    /// exist until account traffic is reviewed (D7, gate G18). Traffic
    /// from a remote room the account's resource occupies never reaches
    /// this decision.
    AccountTraffic,
    /// A request, message or presence for the server domain other than
    /// service discovery and ping (D10).
    ServerRequest,
    /// MUC or the Spaces PubSub service: enabled for remote senders by D10,
    /// but no handler has yet been reviewed for a remote principal, so
    /// they are refused until the slices that enable them land.
    AwaitingReview(ServedService),
    /// A service D10 does not enable for remote senders: HTTP upload,
    /// push, the community and extension services and the call mixer.
    NotEnabled(ServedService),
}

/// What `stanza`, addressed to `target`, reaches.
pub fn reach(target: LocalTarget, stanza: &Stanza) -> InboundReach {
    match (target, stanza) {
        (LocalTarget::ServerDomain, Stanza::Iq(iq)) => server_iq(iq),
        (LocalTarget::ServerDomain | LocalTarget::ServerResource, _) => {
            InboundReach::Refused(Refusal::ServerRequest)
        }
        (LocalTarget::AccountResource, Stanza::Iq(iq))
            if matches!(iq.as_ref(), Iq::Result { .. } | Iq::Error { .. }) =>
        {
            InboundReach::CorrelatedResponse
        }
        (LocalTarget::AccountBare | LocalTarget::AccountResource, _) => {
            InboundReach::Refused(Refusal::AccountTraffic)
        }
        (LocalTarget::Service(service @ (ServedService::Muc | ServedService::Spaces)), _) => {
            InboundReach::Refused(Refusal::AwaitingReview(service))
        }
        (LocalTarget::Service(service), _) => InboundReach::Refused(Refusal::NotEnabled(service)),
    }
}

/// An IQ addressed to the bare server domain. The only request this server
/// sends of its own is the `disco#info` that verifies a remote room, so a
/// response completes one of those or nothing.
fn server_iq(iq: &Iq) -> InboundReach {
    match iq {
        Iq::Get { payload, .. } if payload.is("query", DISCO_INFO_NS) => {
            InboundReach::ServerDiscoInfo
        }
        Iq::Get { payload, .. } if payload.is("query", DISCO_ITEMS_NS) => {
            InboundReach::ServerDiscoItems
        }
        Iq::Get { payload, .. } if payload.is("ping", NS_PING) => InboundReach::ServerPing,
        Iq::Result { .. } | Iq::Error { .. } => InboundReach::ServerResponse,
        Iq::Get { .. } | Iq::Set { .. } => InboundReach::Refused(Refusal::ServerRequest),
    }
}
