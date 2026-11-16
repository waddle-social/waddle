//! Destination classification for stanzas that a connected client sends to its
//! home server (RFC 6120 §10).
//!
//! Every client-originated IQ is classified by its `to` address before any
//! namespace handler sees it. A local handler therefore never answers on behalf
//! of a remote entity, and IQ results and errors are routed by address instead
//! of being consumed by whichever handler claims their payload namespace.
//!
//! Remote destinations are handed to a [`RemoteRoute`] through [`offer_iq`]
//! or [`offer_stanza`], which bound the wait and fail closed. Until
//! server-to-server transport is configured the route is
//! [`FederationDisabled`], which refuses every stanza; IQ requests then
//! receive the error from [`unroutable_iq_reply`], messages and presence the
//! error from [`unroutable_reply`], and IQ results, IQ errors and error
//! stanzas are dropped. With S2S enabled the route is the S2S router
//! (`crate::s2s::route::S2sRouter`).

use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, DomainPart, DomainRef, FullJid, Jid};
use waddle_xmpp::identity::{canonical_jid, CanonicalDomain, DomainKind};
use waddle_xmpp::protocol::handlers::jingle::credential_free_jingle_echo;
use waddle_xmpp::xep::xep0166::NS_JINGLE;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// A specialised local service with its own configured domain (RFC 6120 §10.5:
/// the server routes such stanzas to that service).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServedService {
    /// XEP-0045 multi-user chat.
    Muc,
    /// XEP-0503 community spaces (PubSub).
    Spaces,
    /// XEP-0363 HTTP upload.
    Upload,
    /// Community-wide PubSub features that are not space bookmarks.
    Community,
    /// Extension bots.
    Extensions,
    /// XEP-0357 push service.
    Push,
    /// The XEP-0272 call mixer component.
    CallsMixer,
}

/// The exact domains this deployment serves. Matching is exact: a subdomain
/// of a served domain is not served unless it is configured itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedDomains {
    local: DomainPart,
    services: Vec<(DomainPart, ServedService)>,
}

impl ServedDomains {
    /// The account domain of this server, with no services yet.
    pub fn new(local: DomainPart) -> Self {
        Self {
            local,
            services: Vec::new(),
        }
    }

    /// Add one configured service domain.
    #[must_use]
    pub fn with_service(mut self, service: ServedService, domain: DomainPart) -> Self {
        self.services.push((domain, service));
        self
    }

    /// The account domain of this server.
    pub fn local(&self) -> &DomainRef {
        &self.local
    }

    /// The service configured at exactly `domain`, if any.
    pub fn service(&self, domain: &DomainRef) -> Option<ServedService> {
        self.services
            .iter()
            .find(|(served, _)| **served == *domain)
            .map(|(_, service)| *service)
    }

    /// Whether `domain` is the account domain or a configured service domain.
    pub fn serves(&self, domain: &DomainRef) -> bool {
        *self.local == *domain || self.service(domain).is_some()
    }

    /// Classify the `to` address of a client-originated stanza.
    ///
    /// The address is first put in the identity profile's canonical form
    /// (see [`canonical_address`]), and every address carried by the result
    /// uses that form. An internationalized domain written with U-labels and
    /// the same domain written with A-labels (`xn--…`) therefore classify
    /// identically. The served domains must be canonical as well; startup
    /// refuses a configured domain that is not.
    pub fn classify(&self, to: Option<&Jid>) -> Destination {
        let Some(address) = to else {
            return Destination::Missing;
        };
        let address = &canonical_address(address);
        let domain = address.domain();
        if *self.local == *domain {
            return match (address.node().is_some(), address.clone().try_into_full()) {
                (false, Ok(full)) => Destination::ServerResource(full),
                (false, Err(_)) => Destination::Server,
                (true, Ok(full)) => Destination::LocalFull(full),
                (true, Err(bare)) => Destination::LocalBare(bare),
            };
        }
        if let Some(service) = self.service(domain) {
            return Destination::Service {
                service,
                address: address.clone(),
            };
        }
        match unroutable_domain(domain) {
            Some(reason) => Destination::Invalid {
                address: address.clone(),
                reason,
            },
            None => Destination::Remote(RemoteDestination {
                address: address.clone(),
            }),
        }
    }
}

/// Where a client-originated stanza is addressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// No `to`: the server handles the stanza on behalf of the sending
    /// account (RFC 6120 §10.3).
    Missing,
    /// The bare server domain (RFC 6120 §10.5.1).
    Server,
    /// The server domain with a resourcepart (RFC 6120 §10.5.2). No client
    /// can bind such an address.
    ServerResource(FullJid),
    /// A bare JID at the account domain (RFC 6120 §10.5.3).
    LocalBare(BareJid),
    /// A full JID at the account domain (RFC 6120 §10.5.4).
    LocalFull(FullJid),
    /// An address at a configured local service domain.
    Service {
        /// The service that owns the domain.
        service: ServedService,
        /// The complete destination address.
        address: Jid,
    },
    /// An unserved domain, reachable only through server-to-server routing
    /// (RFC 6120 §10.4).
    Remote(RemoteDestination),
    /// An unserved domainpart that no federation route can reach.
    Invalid {
        /// The complete destination address.
        address: Jid,
        /// Why no route can exist.
        reason: InvalidDestination,
    },
}

impl Destination {
    /// The destination when this deployment does not serve its domain, or
    /// `None` when it is handled locally.
    pub fn unserved(&self) -> Option<UnservedDestination> {
        match self {
            Self::Remote(remote) => Some(UnservedDestination::Remote(remote.clone())),
            Self::Invalid { reason, .. } => Some(UnservedDestination::Invalid(*reason)),
            Self::Missing
            | Self::Server
            | Self::ServerResource(_)
            | Self::LocalBare(_)
            | Self::LocalFull(_)
            | Self::Service { .. } => None,
        }
    }
}

/// A destination whose domain this deployment does not serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnservedDestination {
    /// Reachable only through the remote route.
    Remote(RemoteDestination),
    /// Unreachable by any route.
    Invalid(InvalidDestination),
}

/// A destination at an unserved domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDestination {
    address: Jid,
}

impl RemoteDestination {
    /// `address` as a remote destination without classifying it, so that a
    /// suite can offer a route a destination the classifier would have
    /// refused and observe the route's own refusal.
    #[cfg(feature = "test-support")]
    pub fn unclassified(address: Jid) -> Self {
        Self { address }
    }

    /// The complete destination address.
    pub fn address(&self) -> &Jid {
        &self.address
    }

    /// The remote domain a server-to-server route must reach.
    pub fn domain(&self) -> &DomainRef {
        self.address.domain()
    }
}

/// Why an unserved domainpart cannot name a federation peer.
///
/// The reasons follow the identity profile's domain classification
/// ([`DomainKind`]); only a fully qualified DNS name can name a peer
/// (ADR-0020 D3). RFC 7622 §3.2 permits IP literals and unqualified hostnames
/// in an address but notes that IP-literal domainparts might not be accepted
/// for server-to-server communication and that unqualified hostnames resolve
/// only on a local network. A peer is discovered through DNS SRV records and
/// authenticated by a DNS reference identity (RFC 6120 §13.7.2, RFC 9525),
/// and none of these forms can satisfy both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvalidDestination {
    /// An IPv4 address or bracketed IPv6 address.
    IpLiteral,
    /// A name a resolver or URL parser reads as an IPv4 address, such as
    /// `2130706433`, `0x7f.1` or `127.1` ([`DomainKind::NumericIpv4Form`]).
    NumericIpv4Form,
    /// A single DNS label with no parent domain.
    UnqualifiedHost,
    /// A domainpart the identity profile cannot prepare (IDNA2008 with STD3
    /// rules), although the `jid` parser accepted it, such as one containing
    /// `_`. Client stanzas never carry one, because the frame parser refuses
    /// it first.
    UnpreparableDomain,
}

impl InvalidDestination {
    /// The reason a domain of `kind` cannot name a peer, or `None` for a
    /// fully qualified DNS name.
    pub fn for_kind(kind: DomainKind) -> Option<Self> {
        match kind {
            DomainKind::Dns => None,
            DomainKind::SingleLabel => Some(Self::UnqualifiedHost),
            DomainKind::NumericIpv4Form => Some(Self::NumericIpv4Form),
            DomainKind::Ipv4 | DomainKind::Ipv6 => Some(Self::IpLiteral),
        }
    }
}

/// `address` in the identity profile's canonical form
/// ([`waddle_xmpp::identity::canonical_jid`]): one trailing label separator
/// removed from its domainpart, as RFC 7622 §3.2 requires before an address
/// is compared or used, and an internationalized domain in A-label form.
/// Applying it twice is harmless.
///
/// [`ServedDomains::classify`] applies it before classifying. A caller that
/// hands the classified stanza on applies it to the stanza's `to` as well, so
/// that handlers compare and reply from the same address the classifier used:
/// `node@domain.` and `node@domain` are one entity, and an exact text
/// comparison against a served address must not tell them apart.
///
/// Client stanzas reach the classifier already canonical, because the frame
/// parser applies the profile to their decoded addresses and refuses
/// spellings it would fold. An address that cannot be put in canonical form
/// (a domain that IDNA2008 rejects although the `jid` parser accepted it) is
/// returned unchanged; it matches no served domain and classifies as
/// [`InvalidDestination::UnpreparableDomain`].
pub fn canonical_address(address: &Jid) -> Jid {
    canonical_jid(address).unwrap_or_else(|_| address.clone())
}

/// Classify an unserved domain with the identity profile's
/// [`CanonicalDomain::kind`], the one classification admission, routing and
/// discovery share.
fn unroutable_domain(domain: &DomainRef) -> Option<InvalidDestination> {
    match CanonicalDomain::from_domain(domain) {
        Ok(domain) => InvalidDestination::for_kind(domain.kind()),
        Err(_) => Some(InvalidDestination::UnpreparableDomain),
    }
}

/// Why a stanza addressed to an unserved domain was not routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnroutableReason {
    /// Server-to-server transport is not enabled on this deployment.
    FederationDisabled,
    /// The destination cannot name a federation peer.
    InvalidDestination(InvalidDestination),
    /// The remote route did not answer within [`REMOTE_ROUTE_OFFER_TIMEOUT`]
    /// and the offer was cancelled.
    RouteTimedOut,
    /// Peer admission (ADR-0020 D1) does not admit the remote domain.
    PeerNotAdmitted,
    /// The stanza is refused by the routing policy: a local account's
    /// account traffic to remote JIDs (ADR-0020 D7), meaning any message or
    /// presence that is not for a remote room the sending resource joins,
    /// occupies or leaves, or a sender that may not address remote
    /// entities.
    RefusedByPolicy,
    /// The egress queue for the remote domain, the router's bound on
    /// pending requests, or its bound on joined remote rooms, is full.
    EgressBusy,
    /// The request cannot be correlated: its id is empty, too long, or
    /// already pending for the same sender and recipient.
    InvalidRequest,
    /// A join presence for a remote JID whose service discovery recently
    /// answered that it is not a multi-user chat room (XEP-0045 §6.4).
    NotARoom,
}

impl UnroutableReason {
    /// The stanza error returned to the sender of an unroutable request,
    /// attributed to the home server that generated it (RFC 6120 §8.3.1).
    ///
    /// RFC 6120 §10.4.3 requires an error when a stanza cannot be routed to the
    /// recipient's server and names `remote-server-not-found` when no route
    /// to that server can be resolved. Inter-domain communication is optional
    /// for a deployment (§10.4), so a disabled transport resolves no route.
    /// `service-unavailable` would describe the recipient rather than the
    /// route (§8.3.3.19) and, for an occupant JID, XEP-0410 §3.2 reads it as
    /// "still joined". `remote-server-timeout` (§8.3.3.17) is used only when a
    /// configured route was attempted and did not take the stanza in time; it
    /// has type `wait` because a later attempt may succeed.
    pub fn stanza_error(self, home: &DomainRef) -> StanzaError {
        let (type_, condition, text) = match self {
            Self::FederationDisabled => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "Routing to remote domains is not enabled on this server.",
            ),
            Self::InvalidDestination(InvalidDestination::IpLiteral) => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "IP-literal domains cannot be reached from this server.",
            ),
            Self::InvalidDestination(InvalidDestination::NumericIpv4Form) => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "Numeric IPv4 domains cannot be reached from this server.",
            ),
            Self::InvalidDestination(InvalidDestination::UnqualifiedHost) => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "Unqualified domains cannot be reached from this server.",
            ),
            Self::InvalidDestination(InvalidDestination::UnpreparableDomain) => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "The domain is not a valid domain name.",
            ),
            Self::RouteTimedOut => (
                ErrorType::Wait,
                DefinedCondition::RemoteServerTimeout,
                "The route to the remote domain did not respond in time.",
            ),
            // Admission is not disclosed: the domain cannot be reached, as
            // when no route to it resolves.
            Self::PeerNotAdmitted => (
                ErrorType::Cancel,
                DefinedCondition::RemoteServerNotFound,
                "The remote domain cannot be reached from this server.",
            ),
            // ADR-0020 D7 names `service-unavailable` for the refusal.
            Self::RefusedByPolicy => (
                ErrorType::Cancel,
                DefinedCondition::ServiceUnavailable,
                "This server does not route this traffic to remote domains.",
            ),
            // RFC 6120 §8.3.3.18: the server lacks the resources now.
            Self::EgressBusy => (
                ErrorType::Wait,
                DefinedCondition::ResourceConstraint,
                "The route to the remote domain is busy; retry later.",
            ),
            Self::InvalidRequest => (
                ErrorType::Modify,
                DefinedCondition::BadRequest,
                "The request id is empty, too long or already pending.",
            ),
            Self::NotARoom => (
                ErrorType::Cancel,
                DefinedCondition::ItemNotFound,
                "The address is not a multi-user chat room.",
            ),
        };
        let mut error = StanzaError::new(type_, condition, "en", text);
        error.by = Some(Jid::from(BareJid::from_parts(None, home)));
        error
    }
}

/// The error reply for an IQ request that could not be routed, or `None` for
/// an IQ result or error.
///
/// RFC 6120 §8.2.3 and §8.3.1 forbid answering a result or error with another
/// IQ response or error, so a refused response is dropped. A request receives
/// an error with the addresses swapped, the request ID and payload preserved,
/// `to` set to `sender` and the error attributed to `home`. For a client
/// request `sender` is the bound full JID, never a client-supplied `from`.
pub fn unroutable_iq_reply(
    iq: &Iq,
    sender: &Jid,
    home: &DomainRef,
    reason: UnroutableReason,
) -> Option<Iq> {
    let payload = match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload,
        Iq::Result { .. } | Iq::Error { .. } => return None,
    };
    Some(Iq::Error {
        from: iq.to().cloned(),
        to: Some(sender.clone()),
        id: iq.id().to_owned(),
        payload: Some(payload.clone()),
        error: reason.stanza_error(home),
    })
}

/// The error reply for a message or presence that could not be routed, or
/// `None` for an IQ (see [`unroutable_iq_reply`]) or an error stanza, which
/// is never answered (RFC 6120 §8.3.1).
///
/// The reply comes from the stanza's `to`, is addressed to `sender` and
/// carries the stanza's id; the payload is not echoed. For a client stanza
/// `sender` is the bound full JID, never a client-supplied `from`.
pub fn unroutable_reply(
    stanza: &Stanza,
    sender: &Jid,
    home: &DomainRef,
    reason: UnroutableReason,
) -> Option<Stanza> {
    let error = minidom::Element::from(reason.stanza_error(home));
    match stanza {
        Stanza::Iq(_) => None,
        Stanza::Message(message) => {
            if message.type_ == xmpp_parsers::message::MessageType::Error {
                return None;
            }
            let mut reply = xmpp_parsers::message::Message::error(Some(sender.clone()));
            reply.from = message.to.clone();
            reply.id = message.id.clone();
            reply.payloads.push(error);
            Some(Stanza::Message(reply))
        }
        Stanza::Presence(presence) => {
            if presence.type_ == xmpp_parsers::presence::Type::Error {
                return None;
            }
            let mut reply =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Error);
            reply.from = presence.to.clone();
            reply.to = Some(sender.clone());
            reply.id = presence.id.clone();
            reply.payloads.push(error);
            Some(Stanza::Presence(reply))
        }
    }
}

/// A stanza from a local entity addressed to a remote domain.
///
/// The sender is the stanza's own `from`, so the route can never be told one
/// sender while the stanza names another. For a client stanza `from` is the
/// bound full JID stamped by the caller (RFC 6120 §8.1.2.1).
#[derive(Debug)]
pub struct RemoteOutbound {
    sender: Jid,
    destination: RemoteDestination,
    stanza: Stanza,
}

impl RemoteOutbound {
    /// Wrap `stanza` for `destination`. A stanza without `from` has no sender
    /// a remote peer could answer and is handed back, unchanged, as the
    /// error.
    ///
    /// The stanza's `to` is set to the destination's address, which is in
    /// canonical form, so the stanza a route delivers always names the
    /// destination the route was given.
    pub fn new(destination: RemoteDestination, mut stanza: Stanza) -> Result<Self, Box<Stanza>> {
        let sender = match &stanza {
            Stanza::Iq(iq) => iq.from().cloned(),
            Stanza::Message(message) => message.from.clone(),
            Stanza::Presence(presence) => presence.from.clone(),
        };
        if sender.is_some() {
            let to = Some(destination.address().clone());
            match &mut stanza {
                Stanza::Iq(iq) => *iq.to_mut() = to,
                Stanza::Message(message) => message.to = to,
                Stanza::Presence(presence) => presence.to = to,
            }
        }
        match sender {
            Some(sender) => Ok(Self {
                sender,
                destination,
                stanza,
            }),
            None => Err(Box::new(stanza)),
        }
    }

    /// The local sender: the stanza's `from`.
    pub fn sender(&self) -> &Jid {
        &self.sender
    }

    /// Where the stanza is addressed.
    pub fn destination(&self) -> &RemoteDestination {
        &self.destination
    }

    /// The stanza to route.
    pub fn stanza(&self) -> &Stanza {
        &self.stanza
    }

    /// The destination and the stanza, for an implementation that takes
    /// ownership of the stanza.
    pub fn into_parts(self) -> (RemoteDestination, Stanza) {
        (self.destination, self.stanza)
    }
}

/// The result of offering a stanza to the remote route.
#[derive(Debug)]
pub enum RemoteRouteOutcome {
    /// The route took the stanza. Any reply arrives later through the route,
    /// so the caller sends nothing now.
    Accepted,
    /// The route did not take the stanza and returns it unchanged.
    Unroutable {
        /// Why it was refused.
        reason: UnroutableReason,
        /// The refused stanza, exactly as offered.
        outbound: Box<RemoteOutbound>,
    },
}

/// How long [`offer_iq`] waits for [`RemoteRoute::route`] before it cancels
/// the call and fails closed. It bounds the hand-off to the route, not
/// delivery to the peer, and stays well below the per-connection handler
/// backstop so a slow route is reported as a routing failure.
pub const REMOTE_ROUTE_OFFER_TIMEOUT: Duration = Duration::from_secs(2);

/// Egress for stanzas from local entities addressed to remote domains.
///
/// The caller has already classified the destination and stamped the sender.
/// An implementation must either take ownership of the stanza or return it as
/// [`RemoteRouteOutcome::Unroutable`]; it never produces a local reply itself.
/// Request correlation and reply delivery belong to the implementation.
///
/// # Contract
///
/// - **Enqueue only.** `route` hands the stanza to a bounded per-peer queue
///   and returns promptly. It must not resolve DNS, dial, negotiate a stream
///   or wait for a peer. Callers wrap it in [`REMOTE_ROUTE_OFFER_TIMEOUT`] and
///   drop the future when it expires, so the hand-off must be cancel-safe: a
///   cancelled call has not queued the stanza. The sender of a cancelled
///   request already holds a `remote-server-timeout` error.
/// - **Return what was offered.** `Unroutable` carries the offered
///   [`RemoteOutbound`] unchanged. Callers do not trust anything else: a
///   different stanza kind or IQ ID is discarded and the sender of a request
///   receives the error without the echoed payload. A request is never left
///   without a reply.
/// - **Correlate responses.** An IQ result or error offered here answers a
///   request that reached a local entity from a remote peer. It may be sent
///   only when it matches a pending inbound server-to-server request by ID,
///   sender, recipient, type and expiry, and completes that request at most
///   once (ADR-0020); anything else is dropped, never forwarded.
/// - **No server-minted credentials.** Nothing minted by this server for a
///   local recipient, such as the LiveKit join token in the Waddle Jingle
///   transport, may leave over a server-to-server stream. Callers remove the
///   credentials they mint before offering (see
///   [`without_server_minted_credentials`]); an implementation must still
///   refuse or strip any it finds.
#[async_trait]
pub trait RemoteRoute: Send + Sync {
    /// Offer one stanza for remote delivery.
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome;

    /// The local resource `resource` ended for good: no session holds its
    /// full JID and none will resume it. A route that let it join remote
    /// rooms leaves them for it (XEP-0045 §7.14). Like [`Self::route`] it
    /// only queues and must not wait for a peer. By default nothing
    /// happens.
    async fn resource_ended(&self, _resource: &FullJid) {}

    /// The local resource `resource` resumed its session on this replica,
    /// which may not be the one that held it before. A route that keeps
    /// state for it, such as the remote rooms it joined, records that this
    /// replica now holds it. By default nothing happens.
    async fn resource_resumed(&self, _resource: &FullJid) {}
}

/// The remote route used while server-to-server transport is not enabled:
/// every stanza is refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct FederationDisabled;

#[async_trait]
impl RemoteRoute for FederationDisabled {
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        RemoteRouteOutcome::Unroutable {
            reason: UnroutableReason::FederationDisabled,
            outbound: Box::new(outbound),
        }
    }
}

/// The result of [`offer_iq`].
#[derive(Debug)]
pub enum IqOffer {
    /// The route took the IQ.
    Accepted,
    /// The IQ was not routed. `reply` is the error for the sender of a
    /// request; it is `None` for a result or error, which is never answered
    /// (RFC 6120 §8.3.1).
    Refused {
        /// Why it was not routed.
        reason: UnroutableReason,
        /// The error to send to the request's sender.
        reply: Option<Box<Iq>>,
    },
    /// The IQ has no `from`, so there is neither a sender to route for nor an
    /// address to answer. It was not offered and is dropped.
    Unattributed,
}

/// Offer an IQ from a local entity to `route`, waiting at most `deadline`,
/// and fail closed.
///
/// The sender is the IQ's `from`, and its `to` is set to the destination's
/// canonical address before it is offered. A request that is not routed
/// always yields an error reply from that address, addressed to the sender
/// and attributed to `home`: with the echoed payload when the route returned
/// the IQ it was offered, and without it when the route timed out or returned
/// anything else.
pub async fn offer_iq(
    route: &dyn RemoteRoute,
    destination: RemoteDestination,
    mut iq: Iq,
    home: &DomainRef,
    deadline: Duration,
) -> IqOffer {
    *iq.to_mut() = Some(destination.address().clone());
    let offered = OfferedIq::capture(&iq);
    let outbound = match RemoteOutbound::new(destination, Stanza::Iq(Box::new(iq))) {
        Ok(outbound) => outbound,
        Err(_) => return IqOffer::Unattributed,
    };
    let sender = outbound.sender().clone();
    let (reason, returned) = match tokio::time::timeout(deadline, route.route(outbound)).await {
        Ok(RemoteRouteOutcome::Accepted) => return IqOffer::Accepted,
        Ok(RemoteRouteOutcome::Unroutable { reason, outbound }) => {
            (reason, Some(outbound.into_parts().1))
        }
        Err(_) => (UnroutableReason::RouteTimedOut, None),
    };
    IqOffer::Refused {
        reason,
        reply: offered.reply(returned, &sender, home, reason).map(Box::new),
    }
}

/// The result of [`offer_stanza`].
#[derive(Debug)]
pub enum StanzaOffer {
    /// The route took the stanza.
    Accepted,
    /// The stanza was not routed. `reply` is the error for its sender, or
    /// `None` for an error stanza, which is never answered.
    Refused {
        /// Why it was not routed.
        reason: UnroutableReason,
        /// The error to send to the stanza's sender.
        reply: Option<Box<Stanza>>,
    },
    /// The stanza has no `from` and was not offered.
    Unattributed,
}

/// Offer a message or presence from a local entity to `route`, waiting at
/// most `deadline`, and fail closed: a stanza that is not routed is
/// answered with the error from [`unroutable_reply`] (RFC 6120 §10.4.3).
///
/// The sender is the stanza's `from`, and its `to` is set to the
/// destination's canonical address before it is offered. An IQ is refused
/// here; IQs go through [`offer_iq`].
pub async fn offer_stanza(
    route: &dyn RemoteRoute,
    destination: RemoteDestination,
    stanza: Stanza,
    home: &DomainRef,
    deadline: Duration,
) -> StanzaOffer {
    if matches!(stanza, Stanza::Iq(_)) {
        return StanzaOffer::Unattributed;
    }
    let outbound = match RemoteOutbound::new(destination, stanza) {
        Ok(outbound) => outbound,
        Err(_) => return StanzaOffer::Unattributed,
    };
    let sender = outbound.sender().clone();
    let offered = outbound.stanza().clone();
    let reason = match tokio::time::timeout(deadline, route.route(outbound)).await {
        Ok(RemoteRouteOutcome::Accepted) => return StanzaOffer::Accepted,
        Ok(RemoteRouteOutcome::Unroutable { reason, .. }) => reason,
        Err(_) => UnroutableReason::RouteTimedOut,
    };
    StanzaOffer::Refused {
        reason,
        reply: unroutable_reply(&offered, &sender, home, reason).map(Box::new),
    }
}

/// The addressing of an offered IQ, kept so that a request can be answered
/// even when the route does not hand the IQ back.
struct OfferedIq {
    id: String,
    to: Option<Jid>,
    request: bool,
}

impl OfferedIq {
    fn capture(iq: &Iq) -> Self {
        Self {
            id: iq.id().to_owned(),
            to: iq.to().cloned(),
            request: matches!(iq, Iq::Get { .. } | Iq::Set { .. }),
        }
    }

    fn reply(
        self,
        returned: Option<Stanza>,
        sender: &Jid,
        home: &DomainRef,
        reason: UnroutableReason,
    ) -> Option<Iq> {
        if !self.request {
            return None;
        }
        // Echo the payload only from the IQ that was offered (RFC 6120
        // §8.3.1 permits the echo; it is never required).
        let payload = match returned {
            Some(Stanza::Iq(iq)) if iq.id() == self.id => match *iq {
                Iq::Get { payload, .. } | Iq::Set { payload, .. } => Some(payload),
                Iq::Result { .. } | Iq::Error { .. } => None,
            },
            _ => None,
        };
        Some(Iq::Error {
            from: self.to,
            to: Some(sender.clone()),
            id: self.id,
            payload,
            error: reason.stanza_error(home),
        })
    }
}

/// `iq` with every credential this server minted for a local recipient
/// removed: the Waddle LiveKit transport in a Jingle payload. Revoke the
/// credential before calling this; the stripped IQ no longer names it.
pub fn without_server_minted_credentials(mut iq: Iq) -> Iq {
    let payload = match &mut iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => Some(payload),
        Iq::Result { payload, .. } | Iq::Error { payload, .. } => payload.as_mut(),
    };
    if let Some(payload) = payload {
        if payload.is("jingle", NS_JINGLE) {
            *payload = credential_free_jingle_echo(payload);
        }
    }
    iq
}
