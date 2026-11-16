//! A federated deployment for the account-traffic suites (ADR-0020 D7;
//! gate G18): the production `/ws` router over the in-memory test state,
//! served on loopback, whose app state runs an S2S snapshot, with the real
//! S2S router as its remote route and the stanza sink of its S2S service,
//! the WebSocket local delivery and the account side attached as startup
//! attaches them; and a peer deployment, `home-b.test`, that sends stanzas
//! through its own connection manager and keeps every stanza it receives.
//!
//! The local deployment serves `example.com`, the test state's pinned
//! account domain, so both configurations name `peer_scope = "public"`;
//! the listeners and endpoints are still loopback addresses. The router's
//! correlation table and remote room occupancies are in an in-memory
//! SQLite database of their own; the account side's stores are in the
//! test state's database. No PostgreSQL, SpiceDB or second process is
//! involved.

#[path = "../s2s_routing_support/mod.rs"]
pub mod routing;
#[path = "../ws_loopback_support/mod.rs"]
pub mod ws;

use std::sync::Arc;
use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use waddle_server::destination::RemoteRoute;
use waddle_server::s2s::conn::{S2sService, ServiceOptions};
use waddle_server::s2s::route::{
    RouterLimits, RouterOptions, S2sRouter, SqlIqCorrelations, SqlRemoteRoomStore,
};
use waddle_server::test_support::{
    websocket_state_with_federation, WebSocketLocalAccounts, WebSocketLocalDelivery,
};
use waddle_ws_test_support::iq::bound_jid;
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

pub use routing::transport::{Pki, Spec};
pub use routing::{error_condition, wait_for, ARRIVES, QUIET};

/// The local deployment's account domain.
pub const LOCAL: &str = ws::DOMAIN;
/// The peer deployment's domain.
pub const PEER: &str = "home-b.test";
/// A remote account at the peer.
pub const BOB: &str = "bob@home-b.test";
/// A resource of [`BOB`].
pub const BOB_PHONE: &str = "bob@home-b.test/phone";
/// Another remote account at the peer, never a contact of anybody here.
pub const MALLORY: &str = "mallory@home-b.test/desk";

/// How long a client waits for a stanza that should arrive.
pub const WAIT: Duration = Duration::from_secs(10);

/// The local deployment, its router and S2S service, and the peer.
pub struct Federated {
    pub loopback: ws::Loopback,
    pub router: S2sRouter,
    pub local: routing::Deployment,
    pub peer: routing::Deployment,
    pub captured: Arc<routing::CapturingSink>,
}

/// How a suite adjusts the fixture.
pub struct Adjust {
    pub limits: RouterLimits,
    /// Applied to the local deployment's S2S configuration, which both its
    /// router and its app state run unless `effect_snapshot` is set.
    pub local: fn(&mut Spec),
    /// When set, the app state runs this changed copy of the local
    /// configuration instead: the account side then decides admission
    /// (ADR-0020 D1) at the effect boundary from another snapshot than
    /// the one the stream was admitted under.
    pub effect_snapshot: Option<fn(&mut Spec)>,
    /// Further domains the peer serves, such as a PubSub service, each
    /// admitted by the local deployment and reached at the peer's address.
    pub peer_services: &'static [&'static str],
}

impl Default for Adjust {
    fn default() -> Self {
        Self {
            limits: RouterLimits::default(),
            local: |_| {},
            effect_snapshot: None,
            peer_services: &[],
        }
    }
}

fn unique(name: &str) -> String {
    [name, "-", uuid::Uuid::new_v4().to_string().as_str()].concat()
}

/// The federated deployment with the default fixture.
pub async fn federated(pki: &Pki) -> Federated {
    federated_with(pki, Adjust::default()).await
}

/// The federated deployment, adjusted.
pub async fn federated_with(pki: &Pki, adjust: Adjust) -> Federated {
    let (listener_local, address_local) = routing::listener().await;
    let (listener_peer, address_peer) = routing::listener().await;
    let public = "peer_scope = \"public\"".to_owned();

    let mut spec_local = Spec::new(LOCAL, PEER);
    spec_local.top.push(public.clone());
    spec_local.endpoints = vec![(PEER, address_peer)];
    for service in adjust.peer_services {
        spec_local.allow.push(service);
        spec_local.private.push(service);
        spec_local.endpoints.push((service, address_peer));
    }
    (adjust.local)(&mut spec_local);
    let config = Arc::new(spec_local.load(pki).await.expect("a valid configuration"));
    let effect_config = match adjust.effect_snapshot {
        Some(change) => {
            change(&mut spec_local);
            Arc::new(
                spec_local
                    .load(pki)
                    .await
                    .expect("a valid effect-boundary configuration"),
            )
        }
        None => Arc::clone(&config),
    };

    let mut state = websocket_state_with_federation(&effect_config).await;
    let database = routing::correlation_database(&unique("s2s-account")).await;
    let router = S2sRouter::new(RouterOptions {
        config: Arc::clone(&config),
        served: routing::served(LOCAL),
        correlations: Arc::new(SqlIqCorrelations::new(database.clone())),
        rooms: Arc::new(SqlRemoteRoomStore::new(database)),
        limits: adjust.limits,
        stop: None,
    });
    // The remote route is set before anything holds the state weakly.
    let route: Arc<dyn RemoteRoute> = Arc::new(router.clone());
    Arc::get_mut(&mut state)
        .expect("an unshared test state")
        .deps
        .remote_route = route;
    let loopback = ws::Loopback::serve(state).await;
    assert!(router.attach_local(Arc::new(WebSocketLocalDelivery::new(&loopback.state))));
    assert!(router.attach_accounts(Arc::new(WebSocketLocalAccounts::new(&loopback.state))));
    let service = S2sService::start(
        Arc::clone(&config),
        Some(listener_local),
        ServiceOptions {
            sink: Arc::new(router.clone()),
            ..Default::default()
        },
    );
    let handle = service.handle();
    let address = service.local_address().expect("a listening service");
    assert!(router.attach_transport(handle.clone()));

    let mut spec_peer = Spec::new(PEER, LOCAL);
    spec_peer.local.extend(adjust.peer_services.iter().copied());
    spec_peer.top.push(public);
    spec_peer.endpoints = vec![(LOCAL, address_local)];
    let captured = Arc::new(routing::CapturingSink::default());
    let peer = routing::transport::start_with(
        pki,
        &spec_peer,
        listener_peer,
        ServiceOptions {
            sink: captured.clone(),
            ..Default::default()
        },
    )
    .await;
    Federated {
        loopback,
        router,
        local: routing::Deployment {
            service,
            handle,
            address,
        },
        peer,
        captured,
    }
}

impl Federated {
    /// Send `stanza` from the peer to the local deployment.
    pub async fn from_peer(&self, stanza: impl Into<Element>) {
        routing::send(&self.peer, PEER, LOCAL, stanza).await;
    }

    /// Send `stanza` from the peer's domain `service` to the local
    /// deployment, on that domain's own stream.
    pub async fn from_peer_service(&self, service: &str, stanza: impl Into<Element>) {
        routing::send(&self.peer, service, LOCAL, stanza).await;
    }

    /// The first stanza with `id` the peer receives, waiting for it.
    pub async fn peer_receives(&self, id: &str) -> Element {
        wait_for(ARRIVES, || self.captured.with_id(id).into_iter().next())
            .await
            .unwrap_or_else(|| panic!("the peer receives `{id}`"))
    }

    /// The stanzas the peer received with `id`, after a quiet period.
    pub async fn peer_received_after_quiet(&self, id: &str) -> Vec<Element> {
        tokio::time::sleep(QUIET).await;
        self.captured.with_id(id)
    }

    /// Every stanza the peer received so far that `matches`.
    pub fn peer_received(&self, matches: impl Fn(&Element) -> bool) -> Vec<Element> {
        self.captured
            .stanzas()
            .into_iter()
            .filter(|stanza| matches(stanza))
            .collect()
    }

    /// Create the local account `localpart@example.com`.
    pub async fn account(&self, localpart: &str) -> jid::BareJid {
        self.loopback.account(localpart).await
    }

    /// An authenticated client of `localpart` with `resource` bound, that
    /// has sent its initial presence with `priority`.
    pub async fn online(&self, localpart: &str, resource: &str, priority: i8) -> WsXmppClient {
        let mut client = self.loopback.client(localpart, resource).await;
        let mut presence = Presence::new(PresenceType::None).with_priority(priority);
        presence.id = Some(["initial-", resource].concat());
        send(&mut client, presence).await;
        client
    }
}

pub fn jid(raw: &str) -> Jid {
    raw.parse().expect("a test JID")
}

pub fn full_jid(client: &WsXmppClient) -> FullJid {
    bound_jid(client).try_into_full().expect("a bound full JID")
}

/// A message from `from` to `to`.
pub fn message(from: &str, to: &str, type_: MessageType, id: &str, body: &str) -> Message {
    let mut message = Message::new_with_type(type_, Some(jid(to)));
    message.from = Some(jid(from));
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), body.to_owned());
    message
}

/// Presence of `type_` from `from` to `to`.
pub fn presence(from: &str, to: &str, type_: PresenceType, id: &str) -> Presence {
    let mut presence = Presence::new(type_);
    presence.from = Some(jid(from));
    presence.to = Some(jid(to));
    presence.id = Some(id.to_owned());
    presence
}

/// An IQ `get` from `from` to `to` carrying `payload`.
pub fn get(from: &str, to: &str, id: &str, payload: Element) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(to)),
        id: id.to_owned(),
        payload,
    }
}

/// Send `stanza` on `client`'s stream.
pub async fn send(client: &mut WsXmppClient, stanza: impl Into<Element>) {
    let element: Element = stanza.into();
    client
        .send(&String::from(&element))
        .await
        .expect("send a stanza");
}

/// The first stanza named `name` with `id` that reaches `client`.
pub async fn receive(client: &mut WsXmppClient, name: &str, id: &str) -> Element {
    let frame = client
        .recv_matching_within(WAIT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == name && element.attr("id") == Some(id))
        })
        .await
        .unwrap_or_else(|error| panic!("`{id}` reaches the client: {error}"));
    frame.parse::<Element>().expect("stanza XML")
}

/// The first stanza that reaches `client` and `matches`.
pub async fn receive_matching(
    client: &mut WsXmppClient,
    matches: impl Fn(&Element) -> bool,
) -> Element {
    let frame = client
        .recv_matching_within(WAIT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| matches(&element))
        })
        .await
        .unwrap_or_else(|error| panic!("a matching stanza reaches the client: {error}"));
    frame.parse::<Element>().expect("stanza XML")
}

/// Whether a stanza named `name` with `id` reaches `client` within
/// [`QUIET`].
pub async fn arrives_quietly(client: &mut WsXmppClient, name: &str, id: &str) -> bool {
    client
        .recv_matching_within(QUIET, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == name && element.attr("id") == Some(id))
        })
        .await
        .is_ok()
}

/// Make `contact` a contact of `account` with `subscription` (`none`,
/// `to`, `from` or `both`), as a completed subscription exchange leaves
/// the account's roster.
pub async fn contact(federated: &Federated, account: &str, contact: &str, subscription: &str) {
    let storage = waddle_server::db::roster::DatabaseRosterStorage::new(
        federated
            .loopback
            .state
            .deps
            .app_state
            .db_pool
            .global()
            .clone(),
    );
    let account: jid::BareJid = account.parse().expect("an account JID");
    storage
        .apply_roster_change(
            &account,
            waddle_server::db::roster::RosterRowChange::Upsert(
                waddle_server::db::roster::RosterItemRow {
                    contact_jid: contact.to_owned(),
                    name: None,
                    subscription: subscription.to_owned(),
                    ask: None,
                    approved: false,
                    groups: Vec::new(),
                },
            ),
        )
        .await
        .expect("a roster item");
}

/// The subscription state of `account`'s roster item for `contact`, and
/// whether it carries a pending request.
pub async fn roster_item(
    federated: &Federated,
    account: &str,
    contact: &str,
) -> Option<(String, Option<String>, bool)> {
    let storage = waddle_server::db::roster::DatabaseRosterStorage::new(
        federated
            .loopback
            .state
            .deps
            .app_state
            .db_pool
            .global()
            .clone(),
    );
    let account: jid::BareJid = account.parse().expect("an account JID");
    let contact: jid::BareJid = contact.parse().expect("a contact JID");
    storage
        .get_roster_item(&account, &contact)
        .await
        .expect("the roster is readable")
        .map(|row| (row.subscription, row.ask, row.approved))
}

/// Block `blocked` on `account`'s XEP-0191 list.
pub async fn block(federated: &Federated, account: &str, blocked: &str) {
    let storage = waddle_server::db::blocking::DatabaseBlockingStorage::new(
        federated
            .loopback
            .state
            .deps
            .app_state
            .db_pool
            .global()
            .clone(),
    );
    let account: jid::BareJid = account.parse().expect("an account JID");
    storage
        .add_blocks(&account, &[jid(blocked)])
        .await
        .expect("a block");
}

/// Whether `stanza` is a presence of `type_` (`None` for available).
pub fn is_presence(stanza: &Element, type_: Option<&str>) -> bool {
    stanza.name() == "presence" && stanza.attr("type") == type_
}

/// Whether `stanza` is a message of type `error`.
pub fn is_message_error(stanza: &Element) -> bool {
    stanza.name() == "message" && stanza.attr("type") == Some("error")
}

/// Whether the `<body/>` of `stanza` is absent.
pub fn has_no_body(stanza: &Element) -> bool {
    stanza.children().all(|child| child.name() != "body")
}

pub use routing::send as peer_send;

/// The XEP-0199 ping payload.
pub fn ping() -> Element {
    Element::builder("ping", waddle_xmpp::xep::xep0199::NS_PING).build()
}

/// A typed message parsed from `stanza`.
pub fn typed_message(stanza: Element) -> Message {
    Message::try_from(stanza).expect("a typed message")
}

/// The message type of a received message element.
pub fn message_type(stanza: &Element) -> MessageType {
    typed_message(stanza.clone()).type_
}

/// Wait for `account` to hold a roster item for `contact`, and return it.
pub async fn wait_for_item(
    federated: &Federated,
    account: &str,
    contact: &str,
) -> (String, Option<String>, bool) {
    routing::wait_for_async(ARRIVES, || roster_item(federated, account, contact))
        .await
        .unwrap_or_else(|| panic!("{account} holds a roster item for {contact}"))
}
