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
//! A replica that hosts rooms for remote occupants (the inbound MUC
//! handlers are attached to its router) also lets the operations of the MUC
//! service reviewed for a remote principal reach those handlers
//! ([`RoomOperation`]); every other request to the MUC service is refused
//! as before. Which operations are open is an explicit input
//! ([`Reachable`]), so a router without the handlers refuses the whole
//! service.
//!
//! A replica that hosts its Spaces for remote principals (the Spaces host
//! is attached to its router) likewise lets the operations of the Spaces
//! PubSub service reviewed for a remote principal reach that host
//! ([`SpaceOperation`]; XEP-0503 over XEP-0060). Every one is an IQ
//! request: messages, presence and IQ responses addressed to the Spaces
//! service are refused, as is every request outside those operations.
//!
//! One decision depends on state and so is taken by the router before this
//! one: a message or presence from a remote room to the full JID of a
//! local resource that occupies, is joining or has just left that room is
//! delivered to that resource (D7's room exception; the router's `rooms`
//! module, against the shared occupancy table). Every other stanza to a
//! local account, including one from a room the resource never joined, is
//! decided here: an IQ request to the bare JID is answered on the
//! account's behalf, a response to a full JID completes that resource's
//! own request, and the rest is account traffic for the account side
//! (gate G18; `s2s-account-traffic-2047.md`).
//!
//! The inventory with each entry's status is recorded in
//! `server/docs/planning/s2s-routing-2047.md`; enabling a further service
//! or operation for remote senders is its own reviewed change to this
//! module, that inventory and the reachability suite.

use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::muc::{NS_MUC_ADMIN, NS_MUC_OWNER};
use waddle_xmpp::pubsub::{NS_PUBSUB, NS_PUBSUB_OWNER};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::xep::xep0425::NS_MESSAGE_MODERATE;
use waddle_xmpp::xep::xep0430::NS_MAM;
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

/// Which of the services D10 enables for remote senders have handlers
/// reviewed for a remote principal attached on this replica.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reachable {
    /// The inbound MUC handlers: remote occupants of local rooms.
    pub rooms: bool,
    /// The Spaces host: remote principals discovering, reading and
    /// subscribing to local Spaces.
    pub spaces: bool,
}

/// An operation of the Spaces PubSub service a remote principal reaches
/// (XEP-0503 over XEP-0060, with XEP-0030 and XEP-0199). Each is decided by
/// the Spaces host for that principal and the evidence of its stream;
/// reaching the host admits nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpaceOperation {
    /// XEP-0030 `disco#info` of the service or of a Space node.
    DiscoInfo,
    /// XEP-0030 `disco#items` of the service, the Spaces it hosts, or of a
    /// Space node, the rooms it lists.
    DiscoItems,
    /// XEP-0199 ping of the service.
    Ping,
    /// A XEP-0060 request in the `pubsub` namespace: item retrieval,
    /// `<subscribe/>` and `<unsubscribe/>`, and `<publish/>` and
    /// `<retract/>`.
    PubSub,
    /// A XEP-0060 request in the `pubsub#owner` namespace: a node's
    /// configuration, its deletion and purge, and its affiliations and
    /// subscriptions.
    Owner,
}

/// An operation of the MUC service a remote principal reaches (XEP-0045,
/// with XEP-0030, XEP-0199, XEP-0410, XEP-0313 and XEP-0425). Each is
/// decided by the room's host for that principal and the evidence of its
/// stream; reaching the handler admits nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomOperation {
    /// XEP-0030 `disco#info` of the service, a room or its nodes.
    DiscoInfo,
    /// XEP-0030 `disco#items` of the service, its rooms, or of a room.
    DiscoItems,
    /// XEP-0199 ping of the service or a room, and the XEP-0410 self-ping
    /// of an occupant JID.
    Ping,
    /// A XEP-0313 query of a room's archive.
    Archive,
    /// Presence to a room or an occupant JID: a join, a nickname or
    /// availability change, a leave, or an occupant's error.
    Presence,
    /// A message to a room or an occupant JID.
    Message,
    /// A XEP-0045 `muc#admin` request: the affiliation and role lists of a
    /// room and changes to them (kick, ban, role and affiliation).
    Admin,
    /// A XEP-0045 `muc#owner` request: a room's configuration and its
    /// destruction.
    Owner,
    /// A XEP-0425 moderation request: a moderator's retraction of another
    /// occupant's message.
    Moderation,
}

/// What the router does with a stanza from an S2S stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundReach {
    /// An operation of the MUC service, handed to the room's host.
    Room(RoomOperation),
    /// An operation of the Spaces PubSub service, handed to the Spaces
    /// host.
    Space(SpaceOperation),
    /// XEP-0030 `disco#info` of the server domain, answered by the router.
    ServerDiscoInfo,
    /// XEP-0030 `disco#items` of the server domain, answered by the router.
    ServerDiscoItems,
    /// XEP-0199 ping of the server domain, answered by the router.
    ServerPing,
    /// An IQ result or error for the server domain, delivered only when it
    /// completes a request this server sent: the `disco#info` that learns
    /// whether the domain of a remote room a local resource joins is a
    /// multi-user chat service.
    ServerResponse,
    /// An IQ result or error for a local resource, delivered only when it
    /// completes a request that resource sent (D7's first exception).
    CorrelatedResponse,
    /// A message or presence for a local account, or an IQ request for one
    /// of its resources: account traffic, decided by the account side when
    /// one is attached and refused as [`Refusal::AccountTraffic`]
    /// otherwise (gate G18).
    Account,
    /// Refused: an IQ request or a message receives `service-unavailable`;
    /// presence, IQ responses and errors are dropped without a reply.
    Refused(Refusal),
}

/// Why a stanza from an S2S stream is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Traffic to a local account refused as if the account did not exist
    /// (RFC 6121 §8.5.1): an IQ request to the bare JID, which the server
    /// answers on the account's behalf for no namespace (gate G18), a
    /// response to the bare JID, and, while no account side is attached,
    /// all account traffic (D7). Traffic from a remote room the account's
    /// resource occupies never reaches this decision.
    AccountTraffic,
    /// A request, message or presence for the server domain other than
    /// service discovery and ping (D10).
    ServerRequest,
    /// MUC or the Spaces PubSub service: enabled for remote senders by D10,
    /// but this replica has no handler reviewed for a remote principal
    /// attached for it: the inbound MUC handlers for the MUC service, the
    /// Spaces host for the Spaces service.
    AwaitingReview(ServedService),
    /// A stanza for the Spaces PubSub service outside the operations
    /// reviewed for a remote principal: any other IQ request, every
    /// message and presence, and every IQ result or error, since the
    /// service sends no request to a remote entity.
    SpaceRequest,
    /// A request to the MUC service outside the operations reviewed for a
    /// remote principal: registration, vCards, pins, call signalling and
    /// any other IQ request; and every IQ result or error, since no room
    /// sends a request to a remote entity.
    RoomRequest,
    /// A service D10 does not enable for remote senders: HTTP upload,
    /// push, the community and extension services and the call mixer.
    NotEnabled(ServedService),
}

/// What `stanza`, addressed to `target`, reaches on a replica where
/// `reachable` is attached.
pub fn reach(target: LocalTarget, stanza: &Stanza, reachable: Reachable) -> InboundReach {
    match (target, stanza) {
        (LocalTarget::Service(ServedService::Muc), stanza) if reachable.rooms => room(stanza),
        (LocalTarget::Service(ServedService::Spaces), stanza) if reachable.spaces => space(stanza),
        (LocalTarget::ServerDomain, Stanza::Iq(iq)) => server_iq(iq),
        (LocalTarget::ServerDomain | LocalTarget::ServerResource, _) => {
            InboundReach::Refused(Refusal::ServerRequest)
        }
        (LocalTarget::AccountResource, Stanza::Iq(iq))
            if matches!(iq.as_ref(), Iq::Result { .. } | Iq::Error { .. }) =>
        {
            InboundReach::CorrelatedResponse
        }
        (LocalTarget::AccountBare, Stanza::Iq(_)) => InboundReach::Refused(Refusal::AccountTraffic),
        (LocalTarget::AccountBare | LocalTarget::AccountResource, _) => InboundReach::Account,
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

/// A stanza addressed to the MUC service, a room or an occupant JID, on a
/// replica that hosts rooms for remote occupants.
fn room(stanza: &Stanza) -> InboundReach {
    let operation = match stanza {
        Stanza::Presence(_) => RoomOperation::Presence,
        Stanza::Message(_) => RoomOperation::Message,
        Stanza::Iq(iq) => match iq.as_ref() {
            Iq::Get { payload, .. } if payload.is("query", DISCO_INFO_NS) => {
                RoomOperation::DiscoInfo
            }
            Iq::Get { payload, .. } if payload.is("query", DISCO_ITEMS_NS) => {
                RoomOperation::DiscoItems
            }
            Iq::Get { payload, .. } if payload.is("ping", NS_PING) => RoomOperation::Ping,
            Iq::Get { payload, .. } | Iq::Set { payload, .. } if payload.is("query", NS_MAM) => {
                RoomOperation::Archive
            }
            Iq::Get { payload, .. } | Iq::Set { payload, .. }
                if payload.is("query", NS_MUC_ADMIN) =>
            {
                RoomOperation::Admin
            }
            Iq::Get { payload, .. } | Iq::Set { payload, .. }
                if payload.is("query", NS_MUC_OWNER) =>
            {
                RoomOperation::Owner
            }
            Iq::Set { payload, .. } if payload.is("moderate", NS_MESSAGE_MODERATE) => {
                RoomOperation::Moderation
            }
            Iq::Get { .. } | Iq::Set { .. } | Iq::Result { .. } | Iq::Error { .. } => {
                return InboundReach::Refused(Refusal::RoomRequest);
            }
        },
    };
    InboundReach::Room(operation)
}

/// A stanza addressed to the Spaces PubSub service, on a replica that hosts
/// its Spaces for remote principals.
fn space(stanza: &Stanza) -> InboundReach {
    let Stanza::Iq(iq) = stanza else {
        return InboundReach::Refused(Refusal::SpaceRequest);
    };
    let operation = match iq.as_ref() {
        Iq::Get { payload, .. } if payload.is("query", DISCO_INFO_NS) => SpaceOperation::DiscoInfo,
        Iq::Get { payload, .. } if payload.is("query", DISCO_ITEMS_NS) => {
            SpaceOperation::DiscoItems
        }
        Iq::Get { payload, .. } if payload.is("ping", NS_PING) => SpaceOperation::Ping,
        Iq::Get { payload, .. } | Iq::Set { payload, .. } if payload.is("pubsub", NS_PUBSUB) => {
            SpaceOperation::PubSub
        }
        Iq::Get { payload, .. } | Iq::Set { payload, .. }
            if payload.is("pubsub", NS_PUBSUB_OWNER) =>
        {
            SpaceOperation::Owner
        }
        Iq::Get { .. } | Iq::Set { .. } | Iq::Result { .. } | Iq::Error { .. } => {
            return InboundReach::Refused(Refusal::SpaceRequest);
        }
    };
    InboundReach::Space(operation)
}
