//! Destination classification for stanzas that a connected client sends to its
//! home server (RFC 6120 §10).
//!
//! Every client-originated IQ is classified by its `to` address before any
//! namespace handler sees it. A local handler therefore never answers on behalf
//! of a remote entity, and IQ results and errors are routed by address instead
//! of being consumed by whichever handler claims their payload namespace.
//!
//! Remote destinations are handed to a [`RemoteRoute`]. Until server-to-server
//! transport is configured the route is [`FederationDisabled`], which refuses
//! every stanza; callers then answer IQ requests with
//! [`unroutable_iq_reply`] and drop IQ results and errors.

use std::net::{Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use jid::{BareJid, DomainPart, DomainRef, FullJid, Jid};
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
    pub fn classify(&self, to: Option<&Jid>) -> Destination {
        let Some(address) = to else {
            return Destination::Missing;
        };
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
/// RFC 7622 §3.2 permits both forms in an address but notes that IP-literal
/// domainparts might not be accepted for server-to-server communication and
/// that unqualified hostnames resolve only on a local network. A peer is
/// discovered through DNS SRV records and authenticated by a DNS reference
/// identity (RFC 6120 §13.7.2, RFC 9525), and neither form can satisfy both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvalidDestination {
    /// An IPv4 address or bracketed IPv6 address.
    IpLiteral,
    /// A single DNS label with no parent domain.
    UnqualifiedHost,
}

fn unroutable_domain(domain: &DomainRef) -> Option<InvalidDestination> {
    let raw = domain.as_str();
    let ipv6 = raw
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|inner| inner.parse::<Ipv6Addr>().is_ok());
    if ipv6 || raw.parse::<Ipv4Addr>().is_ok() {
        return Some(InvalidDestination::IpLiteral);
    }
    (!raw.contains('.')).then_some(InvalidDestination::UnqualifiedHost)
}

/// Why a stanza addressed to an unserved domain was not routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnroutableReason {
    /// Server-to-server transport is not enabled on this deployment.
    FederationDisabled,
    /// The destination cannot name a federation peer.
    InvalidDestination(InvalidDestination),
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
    /// "still joined"; `remote-server-timeout` implies an attempted stream.
    pub fn stanza_error(self, home: &DomainRef) -> StanzaError {
        let text = match self {
            Self::FederationDisabled => "Routing to remote domains is not enabled on this server.",
            Self::InvalidDestination(InvalidDestination::IpLiteral) => {
                "IP-literal domains cannot be reached from this server."
            }
            Self::InvalidDestination(InvalidDestination::UnqualifiedHost) => {
                "Unqualified domains cannot be reached from this server."
            }
        };
        let mut error = StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::RemoteServerNotFound,
            "en",
            text,
        );
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

/// A stanza from a local entity addressed to a remote domain.
#[derive(Debug)]
pub struct RemoteOutbound {
    /// The local sender; `stanza` carries it as `from`. For a client stanza
    /// this is the bound full JID (RFC 6120 §8.1.2.1).
    pub sender: Jid,
    /// Where the stanza is addressed.
    pub destination: RemoteDestination,
    /// The stanza to route.
    pub stanza: Stanza,
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
        /// The refused stanza.
        outbound: Box<RemoteOutbound>,
    },
}

/// Egress for client-originated stanzas addressed to remote domains.
///
/// The caller has already classified the destination and stamped the sender.
/// An implementation must either take ownership of the stanza or return it as
/// [`RemoteRouteOutcome::Unroutable`]; it never produces a local reply itself.
/// Request correlation and reply delivery belong to the implementation.
#[async_trait]
pub trait RemoteRoute: Send + Sync {
    /// Offer one stanza for remote delivery.
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome;
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
