//! Routed S2S deployments for the routing suites: the connection-manager
//! fixture (test CA, configuration files, loopback listeners and a
//! hand-written peer) with an S2S router as the stanza sink and remote
//! route of a deployment, its IQ correlation table in an in-memory SQLite
//! database, and a recording [`LocalDelivery`] in place of WebSocket
//! clients. A deployment without a router records what it receives with
//! [`CapturingSink`] and sends stanzas through its own connection manager.

#[path = "../s2s_connection_manager/support.rs"]
pub mod transport;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use jid::{DomainPart, FullJid, Jid};
use minidom::Element;
use tokio::net::TcpListener;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::destination::{
    offer_iq, Destination, IqOffer, RemoteDestination, ServedDomains, ServedService,
    REMOTE_ROUTE_OFFER_TIMEOUT,
};
use waddle_server::s2s::conn::{
    S2sService, ServiceOptions, SinkVerdict, StanzaSink, VerifiedPeerStream,
};
use waddle_server::s2s::route::{
    LocalDelivery, LocalDeliveryOutcome, LocalOrigin, RouterLimits, RouterOptions, S2sRouter,
    SqlIqCorrelations,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;

pub use transport::{domain, eventually, listener, Deployment, Pki, RawPeer, Spec, HOME_A, HOME_B};

pub fn jid(raw: &str) -> Jid {
    raw.parse().expect("a test JID")
}

pub fn full(raw: &str) -> FullJid {
    raw.parse().expect("a test full JID")
}

fn domain_part(raw: &str) -> DomainPart {
    raw.parse().expect("a test domain")
}

/// The service domains of a deployment serving `local`.
pub fn service(prefix: &str, local: &str) -> String {
    [prefix, ".", local].concat()
}

/// The destinations a deployment serving `local` serves: the account
/// domain and one domain per service, named `<service>.<local>`.
pub fn served(local: &str) -> ServedDomains {
    [
        (ServedService::Muc, "muc"),
        (ServedService::Spaces, "spaces"),
        (ServedService::Upload, "upload"),
        (ServedService::Community, "community"),
        (ServedService::Extensions, "extensions"),
        (ServedService::Push, "push"),
        (ServedService::CallsMixer, "mixer"),
    ]
    .into_iter()
    .fold(
        ServedDomains::new(domain_part(local)),
        |served, (kind, prefix)| served.with_service(kind, domain_part(&service(prefix, local))),
    )
}

/// The remote destination `address` names for a deployment serving
/// `served`.
pub fn remote(served: &ServedDomains, address: &str) -> RemoteDestination {
    match served.classify(Some(&jid(address))) {
        Destination::Remote(remote) => remote,
        other => panic!("{address} is not a remote destination: {other:?}"),
    }
}

/// A migrated in-memory database for one deployment's correlation table.
pub async fn correlation_database(name: &str) -> Database {
    let database = Database::in_memory(name)
        .await
        .expect("an in-memory database");
    MigrationRunner::single()
        .run(&database)
        .await
        .expect("the deployment migrations");
    database
}

/// What the router delivered to local resources, and who it says
/// generated each stanza.
#[derive(Debug, Default)]
pub struct RecordingDelivery {
    delivered: Mutex<Vec<(FullJid, Stanza, LocalOrigin)>>,
}

impl RecordingDelivery {
    fn recorded(&self) -> Vec<(FullJid, Stanza, LocalOrigin)> {
        match self.delivered.lock() {
            Ok(delivered) => delivered.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn delivered(&self) -> Vec<(FullJid, Stanza)> {
        self.recorded()
            .into_iter()
            .map(|(to, stanza, _)| (to, stanza))
            .collect()
    }

    /// The origin of each IQ delivered with `id`.
    pub fn origins(&self, id: &str) -> Vec<LocalOrigin> {
        self.recorded()
            .into_iter()
            .filter_map(|(_, stanza, origin)| match stanza {
                Stanza::Iq(iq) if iq.id() == id => Some(origin),
                _ => None,
            })
            .collect()
    }

    /// The IQs delivered with `id`.
    pub fn iqs(&self, id: &str) -> Vec<(FullJid, Iq)> {
        self.delivered()
            .into_iter()
            .filter_map(|(to, stanza)| match stanza {
                Stanza::Iq(iq) if iq.id() == id => Some((to, *iq)),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl LocalDelivery for RecordingDelivery {
    async fn deliver(
        &self,
        to: &FullJid,
        stanza: Stanza,
        origin: LocalOrigin,
    ) -> LocalDeliveryOutcome {
        match self.delivered.lock() {
            Ok(mut delivered) => delivered.push((to.clone(), stanza, origin)),
            Err(poisoned) => poisoned.into_inner().push((to.clone(), stanza, origin)),
        }
        LocalDeliveryOutcome::Delivered
    }
}

/// A sink that takes and keeps every stanza, malformed or not.
#[derive(Debug, Default)]
pub struct CapturingSink {
    stanzas: Mutex<Vec<Element>>,
}

impl CapturingSink {
    pub fn stanzas(&self) -> Vec<Element> {
        match self.stanzas.lock() {
            Ok(stanzas) => stanzas.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// The captured stanzas whose `id` is `id`.
    pub fn with_id(&self, id: &str) -> Vec<Element> {
        self.stanzas()
            .into_iter()
            .filter(|stanza| stanza.attr("id") == Some(id))
            .collect()
    }

    fn keep(&self, stanza: Element) {
        match self.stanzas.lock() {
            Ok(mut stanzas) => stanzas.push(stanza),
            Err(poisoned) => poisoned.into_inner().push(stanza),
        }
    }
}

impl StanzaSink for CapturingSink {
    fn offer(&self, _stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        self.keep(stanza);
        SinkVerdict::Accepted
    }

    fn offer_malformed(&self, _stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        self.keep(stanza);
        SinkVerdict::Accepted
    }
}

/// A deployment whose S2S router is its stanza sink and remote route.
pub struct Routed {
    pub deployment: Deployment,
    pub router: S2sRouter,
    pub delivery: Arc<RecordingDelivery>,
    pub served: ServedDomains,
}

impl Routed {
    /// Offer `iq` from a local resource to `address`, as the WebSocket
    /// path does.
    pub async fn offer_iq(&self, address: &str, iq: Iq) -> IqOffer {
        let local = domain_part(self.served.local().as_str());
        offer_iq(
            &self.router,
            remote(&self.served, address),
            iq,
            &local,
            REMOTE_ROUTE_OFFER_TIMEOUT,
        )
        .await
    }
}

/// Start a routed deployment from `spec` on `listener`, its correlation
/// table in `database`. The first local domain of the specification is
/// its account domain.
pub async fn start_routed(
    pki: &Pki,
    spec: &Spec,
    listener: TcpListener,
    database: Database,
    limits: RouterLimits,
) -> Routed {
    let config = Arc::new(spec.load(pki).await.expect("a valid configuration"));
    let served = served(spec.local[0]);
    let router = S2sRouter::new(RouterOptions {
        config: Arc::clone(&config),
        served: served.clone(),
        correlations: Arc::new(SqlIqCorrelations::new(database)),
        limits,
        stop: None,
    });
    let delivery = Arc::new(RecordingDelivery::default());
    assert!(router.attach_local(delivery.clone()));
    let service = S2sService::start(
        config,
        Some(listener),
        ServiceOptions {
            sink: Arc::new(router.clone()),
            ..Default::default()
        },
    );
    let handle = service.handle();
    let address = service.local_address().expect("a listening service");
    assert!(router.attach_transport(handle.clone()));
    Routed {
        deployment: Deployment {
            service,
            handle,
            address,
        },
        router,
        delivery,
        served,
    }
}

/// `home-a.test` and `home-b.test`, both routed, each admitting and
/// dialling the other.
pub async fn routed_pair(pki: &Pki, limits: RouterLimits) -> (Routed, Routed) {
    let (listener_a, address_a) = listener().await;
    let (listener_b, address_b) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.endpoints = vec![(HOME_A, address_a)];
    let a = start_routed(
        pki,
        &spec_a,
        listener_a,
        correlation_database("s2s-routing-home-a").await,
        limits,
    )
    .await;
    let b = start_routed(
        pki,
        &spec_b,
        listener_b,
        correlation_database("s2s-routing-home-b").await,
        limits,
    )
    .await;
    (a, b)
}

/// `home-a.test`, routed, with `adjust_a` applied to its specification,
/// and `home-b.test`, which keeps what it receives and admits and dials
/// every local domain of `home-a.test`.
pub async fn routed_and_capturing(
    pki: &Pki,
    limits: RouterLimits,
    adjust_a: impl FnOnce(&mut Spec),
) -> (Routed, Deployment, Arc<CapturingSink>) {
    let (listener_a, address_a) = listener().await;
    let (listener_b, address_b) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    adjust_a(&mut spec_a);
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.allow = spec_a.local.clone();
    spec_b.private = spec_a.local.clone();
    spec_b.endpoints = spec_a
        .local
        .iter()
        .map(|local| (*local, address_a))
        .collect();
    let a = start_routed(
        pki,
        &spec_a,
        listener_a,
        correlation_database("s2s-routing-routed").await,
        limits,
    )
    .await;
    let sink = Arc::new(CapturingSink::default());
    let b = transport::start_with(
        pki,
        &spec_b,
        listener_b,
        ServiceOptions {
            sink: sink.clone(),
            ..Default::default()
        },
    )
    .await;
    (a, b, sink)
}

/// Send `stanza` from `local` on `peer` to `remote` through the peer's own
/// connection manager.
pub async fn send(peer: &Deployment, local: &str, remote: &str, stanza: impl Into<Element>) {
    let stream = peer
        .handle
        .connect(&domain(local), &domain(remote))
        .await
        .expect("an outbound stream");
    peer.handle
        .transmit(&stream, stanza.into())
        .await
        .expect("the stanza is written");
}

/// Wait up to `wait` for `found` to return something.
pub async fn wait_for<T>(wait: Duration, found: impl Fn() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if let Some(value) = found() {
            return Some(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// How long a suite waits for something that should arrive.
pub const ARRIVES: Duration = Duration::from_secs(10);

/// How long a suite waits before concluding that nothing arrives.
pub const QUIET: Duration = Duration::from_secs(1);

/// The defined condition of an `<error/>` child of `stanza`, if any.
pub fn error_condition(stanza: &Element) -> Option<String> {
    let error = stanza.children().find(|child| child.name() == "error")?;
    error
        .children()
        .find(|child| child.ns() == "urn:ietf:params:xml:ns:xmpp-stanzas" && child.name() != "text")
        .map(|condition| condition.name().to_owned())
}
