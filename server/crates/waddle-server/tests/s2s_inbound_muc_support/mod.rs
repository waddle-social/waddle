//! A deployment that hosts its rooms for occupants at another server, for
//! the inbound MUC suites (XEP-0045, XEP-0313, XEP-0030, XEP-0199 and
//! XEP-0410 across S2S).
//!
//! `example.com` is a WebSocket state that runs an S2S snapshot, with the
//! production room host, S2S router and connection manager wired as startup
//! wires them, its router tables and seats in the state's in-memory SQLite
//! database, and its own WebSocket endpoint for local clients. `home-b.test`
//! is a second connection manager in the same process that authenticates by
//! certificate over loopback TLS, sends what a case hands it and keeps
//! everything it receives. No PostgreSQL, SpiceDB or second process is
//! involved; rooms here are rooms no host-grant scope manages.

#[path = "../s2s_routing_support/mod.rs"]
pub mod routing;

use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid};
use minidom::Element;
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::s2s::config::S2sConfig;
use waddle_server::s2s::conn::{S2sService, ServiceOptions};
use waddle_server::s2s::route::{
    RouterLimits, RouterOptions, S2sRouter, Seat, SqlIqCorrelations, SqlRemoteRoomStore,
};
use waddle_server::test_support::{
    host_rooms_for_remote_occupants, make_deployment_owner, router, seed_local_account,
    websocket_state_running_s2s, WebSocketLocalDelivery, WebSocketState,
};
use waddle_ws_test_support::WsXmppClient;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

pub use routing::transport::{listener, Deployment, Pki, Spec, HOME_B};
pub use routing::{error_condition, full, jid, send, wait_for, CapturingSink, ARRIVES, QUIET};

/// The account domain the WebSocket test state serves.
pub const DOMAIN: &str = "example.com";
/// Its MUC service.
pub const MUC: &str = "muc.example.com";
/// A room on it.
pub const ROOM: &str = "lounge@muc.example.com";
/// The remote occupant of these suites.
pub const BOB: &str = "bob@home-b.test/phone";

/// Its occupant JID in [`ROOM`] under the nickname the suites give it.
pub const BOB_OCCUPANT: &str = "lounge@muc.example.com/bob";

pub const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

const PASSWORD: &str = "inbound-muc-account-password";

/// The S2S side of the two deployments before either runs: their
/// certificates, the snapshot `example.com` loads and the sockets both
/// listen on.
pub struct Wiring {
    pki: Pki,
    /// The S2S snapshot of `example.com`.
    pub config: Arc<S2sConfig>,
    peer: Spec,
    listener: tokio::net::TcpListener,
    peer_listener: tokio::net::TcpListener,
}

impl Wiring {
    /// `example.com` and its MUC service admitting `home-b.test` by
    /// certificate, with `adjust` applied to the specification of
    /// `example.com` (its trusted homes, for instance) before it is loaded.
    pub async fn new(adjust: impl FnOnce(&mut Spec)) -> Self {
        let pki = Pki::new();
        let (local_listener, address) = listener().await;
        let (peer_listener, peer_address) = listener().await;
        let mut spec = Spec::new(DOMAIN, HOME_B);
        spec.local.push(MUC);
        spec.endpoints = vec![(HOME_B, peer_address)];
        spec.tables.extend(federating_new_rooms());
        adjust(&mut spec);
        let mut peer = Spec::new(HOME_B, DOMAIN);
        peer.allow = spec.local.clone();
        peer.private = spec.local.clone();
        peer.endpoints = spec.local.iter().map(|local| (*local, address)).collect();
        let config = Arc::new(spec.load(&pki).await.expect("a valid configuration"));
        Self {
            pki,
            config,
            peer,
            listener: local_listener,
            peer_listener,
        }
    }

    /// A snapshot of `example.com` that does not admit `home-b.test`: what
    /// a replica runs once peer admission has dropped the domain, while a
    /// stream admitted before is still open.
    pub async fn without_peer(&self) -> S2sConfig {
        let mut spec = Spec::new(DOMAIN, "elsewhere.test");
        spec.local.push(MUC);
        spec.tables.extend(federating_new_rooms());
        spec.load(&self.pki).await.expect("a valid configuration")
    }
}

/// The host-policy table of a deployment that creates its rooms open to
/// accounts at other servers. The suites' rooms are created by their first
/// occupant, so they are federated unless a case clears
/// [`Spec::tables`] to run the default, which is off.
pub fn federating_new_rooms() -> [String; 2] {
    [
        "[host_policy]".to_owned(),
        "federate_new_rooms = true".to_owned(),
    ]
}

/// `example.com` hosting its rooms for `home-b.test`.
pub struct Hosted {
    pub state: Arc<WebSocketState>,
    pub router: S2sRouter,
    /// The S2S service of `example.com`.
    pub service: Deployment,
    /// `home-b.test`, until a case shuts it down.
    peer: Option<Deployment>,
    /// What `home-b.test` received.
    pub captured: Arc<CapturingSink>,
    /// The WebSocket endpoint this fixture serves for `example.com`, when
    /// the state is its own.
    web: Option<(String, tokio::task::JoinHandle<()>)>,
    _pki: Pki,
}

impl Drop for Hosted {
    fn drop(&mut self) {
        if let Some((_, server)) = &self.web {
            server.abort();
        }
    }
}

impl Hosted {
    /// Start both deployments over a WebSocket state of their own, with
    /// `adjust` applied to the S2S specification of `example.com` before it
    /// is loaded.
    pub async fn start(limits: RouterLimits, adjust: impl FnOnce(&mut Spec)) -> Self {
        let wiring = Wiring::new(adjust).await;
        let state = websocket_state_running_s2s(&wiring.config).await;
        let mut hosted = Self::serve(wiring, Arc::clone(&state), limits).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let app = router(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        hosted.web = Some((format!("ws://{address}/ws"), server));
        hosted
    }

    /// Have `state`, which runs the snapshot of `wiring`, host its rooms
    /// for `home-b.test`: the production router and room host over the
    /// state's global database, the connection manager of `example.com`,
    /// and `home-b.test` beside it. The state's own WebSocket endpoint, if
    /// it has one, is its owner's.
    pub async fn serve(wiring: Wiring, state: Arc<WebSocketState>, limits: RouterLimits) -> Self {
        let Wiring {
            pki,
            config,
            peer,
            listener,
            peer_listener,
        } = wiring;
        let database = state.deps.app_state.db_pool.global().clone();
        let s2s_router = S2sRouter::new(RouterOptions {
            config: Arc::clone(&config),
            served: routing::served(DOMAIN),
            correlations: Arc::new(SqlIqCorrelations::new(database.clone())),
            rooms: Arc::new(SqlRemoteRoomStore::new(database.clone())),
            limits,
            stop: None,
        });
        assert!(s2s_router.attach_local(Arc::new(WebSocketLocalDelivery::new(&state))));
        host_rooms_for_remote_occupants(&state, &s2s_router, database);
        let service = S2sService::start(
            config,
            Some(listener),
            ServiceOptions {
                sink: Arc::new(s2s_router.clone()),
                ..Default::default()
            },
        );
        let handle = service.handle();
        let address = service.local_address().expect("a listening service");
        assert!(s2s_router.attach_transport(handle.clone()));

        let captured = Arc::new(CapturingSink::default());
        let peer = routing::transport::start_with(
            &pki,
            &peer,
            peer_listener,
            ServiceOptions {
                sink: captured.clone(),
                ..Default::default()
            },
        )
        .await;
        Self {
            state,
            router: s2s_router,
            service: Deployment {
                service,
                handle,
                address,
            },
            peer: Some(peer),
            captured,
            web: None,
            _pki: pki,
        }
    }

    /// An authenticated client of the native account `localpart`, created
    /// on first use, with `resource` bound.
    pub async fn client(&self, localpart: &str, resource: &str) -> WsXmppClient {
        seed_local_account(&self.state, localpart).await;
        // A second client of one account finds it registered.
        let _ = NativeUserStore::new(self.state.deps.app_state.db_pool.global_actor().clone())
            .register(RegisterRequest {
                username: localpart.to_owned(),
                domain: DOMAIN.to_owned(),
                password: PASSWORD.to_owned(),
                email: None,
            })
            .await;
        let (url, _) = self
            .web
            .as_ref()
            .expect("the fixture serves the state's WebSocket endpoint");
        WsXmppClient::connect_and_auth(url, DOMAIN, localpart, PASSWORD, resource)
            .await
            .expect("authenticated client")
    }

    /// An authenticated client of `localpart`, which may create rooms: an
    /// owner of the deployment.
    pub async fn room_creator(&self, localpart: &str, resource: &str) -> WsXmppClient {
        make_deployment_owner(&self.state, localpart).await;
        self.client(localpart, resource).await
    }

    /// Send `stanza` from `home-b.test` to the MUC service.
    pub async fn from_peer(&self, stanza: impl Into<Element>) {
        let peer = self.peer.as_ref().expect("the peer is running");
        send(peer, HOME_B, MUC, stanza).await;
    }

    /// Shut `home-b.test` down: its streams end with a stream error.
    pub async fn stop_peer(&mut self) {
        if let Some(peer) = self.peer.take() {
            let _ = peer.service.shutdown(Duration::from_secs(5)).await;
        }
    }

    /// The first stanza `home-b.test` received that `found` accepts, within
    /// [`ARRIVES`].
    pub async fn at_peer(&self, found: impl Fn(&Element) -> bool) -> Option<Element> {
        wait_for(ARRIVES, || {
            self.captured
                .stanzas()
                .into_iter()
                .find(|stanza| found(stanza))
        })
        .await
    }

    /// How many stanzas `home-b.test` has received: a mark for
    /// [`Self::at_peer_after`].
    pub fn received(&self) -> usize {
        self.captured.stanzas().len()
    }

    /// The first stanza `home-b.test` received after the first `mark` that
    /// `found` accepts, within [`ARRIVES`].
    pub async fn at_peer_after(
        &self,
        mark: usize,
        found: impl Fn(&Element) -> bool,
    ) -> Option<Element> {
        wait_for(ARRIVES, || {
            self.captured
                .stanzas()
                .into_iter()
                .skip(mark)
                .find(|stanza| found(stanza))
        })
        .await
    }

    /// The stanza with `id` that `home-b.test` received.
    pub async fn answered(&self, id: &str) -> Element {
        wait_for(ARRIVES, || self.captured.with_id(id).into_iter().next())
            .await
            .unwrap_or_else(|| panic!("no stanza with id {id} reached the peer"))
    }

    /// How many stanzas `home-b.test` received that `found` accepts.
    pub fn count_at_peer(&self, found: impl Fn(&Element) -> bool) -> usize {
        self.captured
            .stanzas()
            .iter()
            .filter(|stanza| found(stanza))
            .count()
    }

    /// How many rows of `table` name `account` in `column`: what this
    /// deployment stores for an account, in the state's global database.
    pub async fn rows_naming(&self, table: &str, column: &str, account: &str) -> i64 {
        let row = self
            .state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(waddle_server::db::actor::DbQueryOne {
                sql: format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?"),
                params: vec![account.into()],
            })
            .await
            .expect("count query")
            .expect("count row");
        match row.first() {
            Some(waddle_server::db::Value::Integer(count)) => *count,
            other => panic!("unexpected count {other:?}"),
        }
    }

    /// The seat `occupant` holds in `room` on `example.com`.
    pub fn seat(&self, room: &str, occupant: &str) -> Option<Seat> {
        let room: BareJid = room.parse().expect("a room JID");
        let occupant: FullJid = occupant.parse().expect("an occupant JID");
        self.router.seats().get(&room, &occupant)
    }

    /// Wait until `occupant` holds no seat in `room`.
    pub async fn unseated(&self, room: &str, occupant: &str) -> bool {
        wait_for(ARRIVES, || {
            self.seat(room, occupant).is_none().then_some(())
        })
        .await
        .is_some()
    }
}

/// A remote entity's join of `room` as `nick` (XEP-0045 §7.2.1).
pub fn remote_join(from: &str, room: &str, nick: &str, id: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(jid(from));
    presence.to = Some(jid(&[room, "/", nick].concat()));
    presence.id = Some(id.to_owned());
    presence
        .payloads
        .push(Element::builder("x", NS_MUC).build());
    presence
}

/// A remote occupant's leave of `room` (XEP-0045 §7.14).
pub fn remote_leave(from: &str, room: &str, nick: &str, id: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::Unavailable);
    presence.from = Some(jid(from));
    presence.to = Some(jid(&[room, "/", nick].concat()));
    presence.id = Some(id.to_owned());
    presence
}

/// A remote occupant's message to `room`.
pub fn remote_groupchat(from: &str, room: &str, id: &str, body: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Groupchat, Some(jid(room)));
    message.from = Some(jid(from));
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang(String::new()), body.to_owned());
    message
}

/// A remote entity's IQ `get` to `to`.
pub fn remote_get(from: &str, to: &str, id: &str, payload: Element) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(to)),
        id: id.to_owned(),
        payload,
    }
}

/// The XEP-0045 status codes a presence carries.
pub fn statuses(presence: &Element) -> Vec<String> {
    presence
        .children()
        .filter(|child| child.is("x", NS_MUC_USER))
        .flat_map(Element::children)
        .filter(|child| child.name() == "status")
        .filter_map(|status| status.attr("code").map(str::to_owned))
        .collect()
}

/// The `role` and `affiliation` of a presence's `muc#user` item.
pub fn item(presence: &Element) -> Option<(String, String)> {
    let item = presence
        .children()
        .filter(|child| child.is("x", NS_MUC_USER))
        .flat_map(Element::children)
        .find(|child| child.name() == "item")?;
    Some((
        item.attr("role")?.to_owned(),
        item.attr("affiliation")?.to_owned(),
    ))
}

/// The `<error/>` of a refusal: its type, condition and text, without the
/// addresses and id of the stanza that carries it.
pub fn error_of(stanza: &Element) -> Option<Element> {
    stanza
        .children()
        .find(|child| child.name() == "error")
        .cloned()
}

/// The deployment default for new rooms: not open to accounts at other
/// servers. It undoes what [`Wiring::new`] sets.
pub fn not_federating(spec: &mut Spec) {
    spec.tables.clear();
}

/// An owner's configuration submission (XEP-0045 §10.2) that sets the
/// room's `urn:waddle:roomconfig:federated` field.
pub fn federated_form(federated: bool) -> Element {
    owner_form(&[(
        "urn:waddle:roomconfig:federated",
        if federated { "1" } else { "0" },
    )])
}

/// An owner's configuration submission (XEP-0045 §10.2) of `fields`, each
/// a form variable and its value.
pub fn owner_form(fields: &[(&str, &str)]) -> Element {
    let field = |var: &str, type_: Option<&str>, value: &str| {
        let mut field = Element::builder("field", "jabber:x:data")
            .attr(minidom::rxml::xml_ncname!("var").to_owned(), var);
        if let Some(type_) = type_ {
            field = field.attr(minidom::rxml::xml_ncname!("type").to_owned(), type_);
        }
        field
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(value)
                    .build(),
            )
            .build()
    };
    let mut form = Element::builder("x", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(field(
            "FORM_TYPE",
            Some("hidden"),
            "http://jabber.org/protocol/muc#roomconfig",
        ));
    for (var, value) in fields {
        form = form.append(field(var, None, value));
    }
    Element::builder("query", "http://jabber.org/protocol/muc#owner")
        .append(form.build())
        .build()
}

/// The identifier `by` gave a message (XEP-0359 `stanza-id`).
pub fn stanza_id(message: &Element, by: &str) -> Option<String> {
    message
        .children()
        .find(|child| child.is("stanza-id", "urn:xmpp:sid:0") && child.attr("by") == Some(by))
        .and_then(|child| child.attr("id").map(str::to_owned))
}

/// A remote occupant's message to `room` that carries `payload` and no
/// text.
pub fn remote_payload(from: &str, room: &str, id: &str, payload: Element) -> Message {
    let mut message = Message::new_with_type(MessageType::Groupchat, Some(jid(room)));
    message.from = Some(jid(from));
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message.payloads.push(payload);
    message
}

/// The next message `client` receives from the occupant `from` that has a
/// child `name` in `ns`.
pub async fn local_receives_payload(
    client: &mut WsXmppClient,
    from: &str,
    name: &str,
    ns: &str,
) -> Option<Element> {
    local_receives(client, ARRIVES, |stanza| {
        stanza.name() == "message"
            && stanza.attr("from") == Some(from)
            && stanza.children().any(|child| child.is(name, ns))
    })
    .await
}

/// A payload element `name` in `ns` with the attribute `id`.
pub fn naming(name: &str, ns: &str, id: &str) -> Element {
    Element::builder(name, ns)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .build()
}

/// An IQ `set` from a local client to `to`, and its answer.
pub async fn local_set(client: &mut WsXmppClient, to: &str, id: &str, payload: Element) -> Element {
    let request = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(payload)
        .build();
    client.send(&String::from(&request)).await.expect("send");
    local_receives(client, ARRIVES, |stanza| {
        stanza.name() == "iq" && stanza.attr("id") == Some(id)
    })
    .await
    .expect("the answer to the request")
}

/// Whether `stanza` is a presence from `from` to `to`.
pub fn presence_between(stanza: &Element, from: &str, to: &str) -> bool {
    stanza.name() == "presence"
        && stanza.attr("from") == Some(from)
        && stanza.attr("to") == Some(to)
}

/// The body of a message, if it has one.
pub fn body(message: &Element) -> Option<String> {
    message
        .children()
        .find(|child| child.name() == "body")
        .map(Element::text)
}

/// A local client's join of `room` as `nick`: the first presence the room
/// sends from that occupant address.
pub async fn local_join(client: &mut WsXmppClient, room: &str, nick: &str) -> Element {
    let occupant = [room, "/", nick].concat();
    let join = Element::builder("presence", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            occupant.clone(),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build();
    client.send(&String::from(&join)).await.expect("send join");
    let frame = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "presence" && element.attr("from") == Some(occupant.as_str())
            })
        })
        .await
        .expect("join presence");
    frame.parse().expect("presence XML")
}

/// A local client's groupchat message to `room`.
pub async fn local_groupchat(client: &mut WsXmppClient, room: &str, id: &str, text: &str) {
    let message = Element::builder("message", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "groupchat")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("body", "jabber:client")
                .append(text)
                .build(),
        )
        .build();
    client
        .send(&String::from(&message))
        .await
        .expect("send groupchat");
}

/// The next stanza `client` receives within `budget` that `found` accepts.
pub async fn local_receives(
    client: &mut WsXmppClient,
    budget: Duration,
    found: impl Fn(&Element) -> bool,
) -> Option<Element> {
    client
        .recv_matching_within(budget, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| found(&element))
        })
        .await
        .ok()
        .and_then(|frame| frame.parse().ok())
}

/// `alice@example.com/desk` in [`ROOM`], which she created, and
/// `bob@home-b.test/phone` joined to it as `bob`: the deployment, alice's
/// client and the room's own presence for bob as `home-b.test` received it.
pub async fn room_with_remote_occupant(
    limits: RouterLimits,
    adjust: impl FnOnce(&mut Spec),
) -> (Hosted, WsXmppClient, Element) {
    let hosted = Hosted::start(limits, adjust).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    let created = local_join(&mut alice, ROOM, "alice").await;
    assert_eq!(created.attr("type"), None, "{created:?}");
    hosted
        .from_peer(remote_join(BOB, ROOM, "bob", "join-1"))
        .await;
    let occupant = [ROOM, "/bob"].concat();
    let own = hosted
        .at_peer(|stanza| {
            presence_between(stanza, &occupant, BOB) && statuses(stanza).contains(&"110".to_owned())
        })
        .await
        .expect("the room's own presence for the remote occupant");
    (hosted, alice, own)
}
