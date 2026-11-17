//! The fixture of the suites for ending a retired account's sessions
//! (`account_retirement_sessions.rs`, `xep0198_account_retirement_ws.rs`):
//! the production `/ws` router over a loopback socket against the in-memory
//! test state, with accounts, rooms and a replica's lease and work driven
//! one poll at a time.

use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use uuid::Uuid;
use waddle_server::auth::retirement::retire_account;
use waddle_server::auth::session_endings::{request_session_ending, SessionEndingRequest};
use waddle_server::auth::{NativeUserStore, RegisterRequest};
use waddle_server::db::actor::DbQuery;
use waddle_server::db::{Database, Value};
use waddle_server::server::account_session_endings::{ReplicaLease, RequestedEndings};
use waddle_server::server::ResolvedXmppDatabaseUrl;
use waddle_server::test_support::{
    attach_pending_startup, make_deployment_owner, router, seed_local_account, websocket_state,
    websocket_state_with_sm_persistence, WebSocketState,
};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

/// The test state's account domain and MUC service.
pub const DOMAIN: &str = "example.com";
pub const MUC: &str = "muc.example.com";
pub const PASSWORD: &str = "retirement-sessions-password";
/// RFC 7395 framing.
pub const FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
/// How long a case waits for something that arrives.
pub const WAIT: Duration = Duration::from_secs(10);

/// The in-memory test state served on loopback.
pub struct Server {
    pub state: Arc<WebSocketState>,
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn start() -> Self {
        Self::serve(websocket_state().await).await
    }

    /// A server whose offline store is the one startup attaches: rows in the
    /// state's database, which also holds the ledger of retired JIDs and the
    /// durable stream-management authority's sessions and their ingress
    /// custody.
    pub async fn start_with_offline_store() -> Self {
        Self::with_offline_store(websocket_state().await).await
    }

    /// [`Self::start_with_offline_store`] whose stream-management session
    /// registry keeps its sessions and ingress custody in `persistence`.
    pub async fn start_with_offline_store_and_sm(
        persistence: Arc<dyn waddle_xmpp::stream_management::persistence::SmPersistenceStorage>,
    ) -> Self {
        Self::with_offline_store(websocket_state_with_sm_persistence(persistence).await).await
    }

    async fn with_offline_store(mut state: Arc<WebSocketState>) -> Self {
        let global = state.deps.app_state.db_pool.global().clone();
        let pending = attach_pending_startup(
            &ResolvedXmppDatabaseUrl::InheritedGlobal(global.database_url().to_owned()),
            &global,
        )
        .await
        .expect("the offline store in the test state's database");
        Arc::get_mut(&mut state)
            .expect("an unshared test state")
            .deps
            .protocol
            .pending_delivery_storage = Arc::new(pending);
        Self::serve(state).await
    }

    async fn serve(state: Arc<WebSocketState>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let app = router(Arc::clone(&state));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            state,
            url: ["ws://", &address.to_string(), "/ws"].concat(),
            task,
        }
    }

    pub fn db(&self) -> &Database {
        self.state.deps.app_state.db_pool.global()
    }

    /// Create the native account `localpart@example.com`.
    pub async fn account(&self, localpart: &str) -> BareJid {
        seed_local_account(&self.state, localpart).await;
        NativeUserStore::new(self.state.deps.app_state.db_pool.global_actor().clone())
            .register(RegisterRequest {
                username: localpart.to_owned(),
                domain: DOMAIN.to_owned(),
                password: PASSWORD.to_owned(),
                email: None,
            })
            .await
            .expect("register native account");
        account(localpart)
    }

    /// A client of `localpart` that has authenticated and bound nothing.
    pub async fn authenticated(&self, localpart: &str) -> WsXmppClient {
        let mut client = WsXmppClient::connect(&self.url)
            .await
            .expect("connect a client");
        client
            .authenticate(DOMAIN, localpart, PASSWORD)
            .await
            .expect("authenticate");
        client
    }

    /// A client of `localpart` with `resource` bound.
    pub async fn client(&self, localpart: &str, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(&self.url, DOMAIN, localpart, PASSWORD, resource)
            .await
            .expect("authenticated client")
    }

    /// A new room on the test state's MUC service. `owner` is made the
    /// deployment owner, whose first entry creates the room as an instant
    /// room (XEP-0045 §10.1.3) that other accounts may then enter.
    pub async fn room(&self, owner: &str) -> BareJid {
        make_deployment_owner(&self.state, owner).await;
        ["lobby-", &Uuid::new_v4().simple().to_string(), "@", MUC]
            .concat()
            .parse()
            .expect("room JID")
    }

    /// Retire `account` and record the request to end its sessions, as the
    /// operator command does.
    pub async fn retire(&self, account: &BareJid) -> SessionEndingRequest {
        retire_account(self.db(), account)
            .await
            .expect("the account is retired");
        request_session_ending(self.db(), account)
            .await
            .expect("the request is recorded")
    }

    /// A replica of this server with its lease registered.
    pub async fn replica(&self) -> (ReplicaLease, RequestedEndings) {
        let mut lease = ReplicaLease::new(&self.state);
        lease.keep(self.db()).await.expect("the lease registers");
        let endings = RequestedEndings::new(lease.replica());
        (lease, endings)
    }

    /// The full JIDs of `account` the session registry holds as detached.
    pub async fn detached(&self, account: &BareJid) -> Vec<FullJid> {
        self.state
            .deps
            .protocol
            .sm_session_registry
            .detached_resources_for_user(account)
            .await
            .expect("the session registry is readable")
    }

    /// The durable stream-management sessions of `resource` that wait for a
    /// resumption.
    pub async fn durable_detached_sessions(&self, resource: &FullJid) -> Vec<String> {
        /// `sm_durable_sessions.state` of a session that waits for a
        /// resumption.
        const DURABLE_DETACHED: i64 = 2;
        self.state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .ask(DbQuery {
                sql: "SELECT session_id FROM sm_durable_sessions WHERE full_jid = ? AND state = ?"
                    .to_owned(),
                params: vec![resource.to_string().into(), DURABLE_DETACHED.into()],
            })
            .await
            .expect("the durable sessions are readable")
            .into_iter()
            .filter_map(|row| match row.first() {
                Some(Value::Text(session)) => Some(session.clone()),
                _ => None,
            })
            .collect()
    }

    /// How many frames the durable authority retains for the sessions
    /// `sessions` names, delivered to them and not acknowledged.
    pub async fn held_frames(&self, sessions: &[String]) -> usize {
        let actor = self.state.deps.app_state.db_pool.global_actor();
        let mut held = 0;
        for session in sessions {
            held += actor
                .ask(DbQuery {
                    sql: "SELECT sequence FROM sm_unacked WHERE stream_id = ?".to_owned(),
                    params: vec![session.clone().into()],
                })
                .await
                .expect("the retained frames are readable")
                .len();
        }
        held
    }

    /// How many messages `account`'s offline store holds.
    pub async fn offline_messages(&self, account: &BareJid) -> usize {
        use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
        self.state
            .deps
            .protocol
            .pending_delivery_storage
            .list(account)
            .await
            .expect("the offline store is readable")
            .len()
    }
}

pub fn account(localpart: &str) -> BareJid {
    [localpart, "@", DOMAIN]
        .concat()
        .parse()
        .expect("account JID")
}

pub async fn send(client: &mut WsXmppClient, stanza: impl Into<Element>) {
    let element: Element = stanza.into();
    client
        .send(&String::from(&element))
        .await
        .expect("send a stanza");
}

/// XEP-0045 §7.2: enter `room` as `nick`.
pub fn join(room: &BareJid, nick: &str) -> Presence {
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(Jid::from(
        room.with_resource_str(nick).expect("occupant JID"),
    ));
    presence
        .payloads
        .push(Element::builder("x", xmpp_parsers::ns::MUC).build());
    presence
}

/// The first frame that reaches `client` and `matches`.
pub async fn receive(client: &mut WsXmppClient, matches: impl Fn(&Element) -> bool) -> Element {
    let frame = client
        .recv_matching_within(WAIT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| matches(&element))
        })
        .await
        .unwrap_or_else(|error| panic!("a matching frame reaches the client: {error}"));
    frame.parse().expect("frame XML")
}

/// Enter `room` as `nick` and wait for the room's presence of that occupant.
pub async fn enter(client: &mut WsXmppClient, room: &BareJid, nick: &str) {
    send(client, join(room, nick)).await;
    let occupant = [room.as_str(), "/", nick].concat();
    let presence = receive(client, |frame| {
        frame.name() == "presence" && frame.attr("from") == Some(occupant.as_str())
    })
    .await;
    assert_ne!(presence.attr("type"), Some("error"), "{presence:?}");
}

/// Every frame `client` receives until its stream ends, parsed.
pub async fn frames_until_closed(client: &mut WsXmppClient) -> Vec<Element> {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut frames = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the stream did not end; received {frames:?}"
        );
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if let Ok(element) = frame.parse::<Element>() {
                    frames.push(element);
                }
            }
            // A WebSocket close frame, or the end of the stream.
            Err(error) if !error.starts_with("Timeout") => return frames,
            Err(error) => panic!("the stream did not end: {error}; received {frames:?}"),
        }
    }
}

/// RFC 6120 §4.9.3.12.
pub fn is_not_authorized(frame: &Element) -> bool {
    frame.is("error", waddle_xmpp::ns::STREAM)
        && frame.has_child("not-authorized", waddle_xmpp::ns::STREAMS)
}

/// The stream errors and framing close among `frames` mean the stream
/// ended with `<not-authorized/>`, and nothing else ended it.
pub fn ended_not_authorized(frames: &[Element]) {
    let errors: Vec<&Element> = frames
        .iter()
        .filter(|frame| frame.is("error", waddle_xmpp::ns::STREAM))
        .collect();
    assert_eq!(errors.len(), 1, "one stream error: {frames:?}");
    assert!(is_not_authorized(errors[0]), "{frames:?}");
    assert!(
        frames.iter().any(|frame| frame.is("close", FRAMING)),
        "the stream is closed: {frames:?}"
    );
}

/// Whether `frame` is the room's presence of the occupant `nick`, of
/// `type_` (`None` for available).
pub fn occupant_presence(frame: &Element, room: &BareJid, nick: &str, type_: Option<&str>) -> bool {
    let occupant = [room.as_str(), "/", nick].concat();
    frame.name() == "presence"
        && frame.attr("from") == Some(occupant.as_str())
        && frame.attr("type") == type_
}

/// Bob, the deployment owner, creates a room and Alice enters it. Returns
/// Alice's account, the room, Bob's client (which has seen Alice enter) and
/// Alice's.
pub async fn occupied_room(server: &Server) -> (BareJid, BareJid, WsXmppClient, WsXmppClient) {
    let alice = server.account("alice").await;
    server.account("bob").await;
    let room = server.room("bob").await;
    let mut bobs = server.client("bob", "laptop").await;
    enter(&mut bobs, &room, "bob").await;
    let mut alices = server.client("alice", "phone").await;
    enter(&mut alices, &room, "alice").await;
    receive(&mut bobs, |frame| {
        occupant_presence(frame, &room, "alice", None)
    })
    .await;
    (alice, room, bobs, alices)
}

/// XEP-0198 §4 with resumption: the stream's session can be resumed.
/// Returns the id the session is resumed by.
pub async fn enable_resumption(client: &mut WsXmppClient) -> String {
    send(
        client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = receive(client, |frame| frame.is("enabled", SM_NS)).await;
    enabled
        .attr("id")
        .expect("a resumable session has an id")
        .to_owned()
}

/// Whether `client` still has a working stream: its server answers a ping.
pub async fn answers_ping(client: &mut WsXmppClient, id: &str) -> bool {
    matches!(
        round_trip(client, Iq::from_get(id, Ping)).await,
        Iq::Result { .. }
    )
}
