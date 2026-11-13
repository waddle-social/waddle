//! Two-node characterization of the durable stream-management authority
//! and the account coordinator (`WADDLE_XMPP_DURABLE_SM=durable`,
//! `WADDLE_XMPP_ACCOUNT_AUTHORITY=coordinator`).
//!
//! Two `waddle-server` processes share one PostgreSQL database and are
//! meshed over libp2p. Each scenario seeds its own accounts, so the rows a
//! scenario observes are its own. The first bind of an account takes the
//! account's `UserActor` claim, so the node a scenario binds an account on
//! first owns that account.
//!
//! A test whose name starts with `gap_` pins behaviour the selectors do not
//! support on a cluster: it asserts what the servers answer and its comment
//! states the behaviour the scenario is meant to have. Closing the gap
//! replaces the pinned answer with the intended one.

#![cfg(feature = "clustering")]

pub mod cluster_support;

use std::future::Future;
use std::time::Duration;

use cluster_support::{
    activate_mailbox_epoch, cluster_e2e_serial_lock, fanout_message, free_tcp_port, generate_pool,
    join_fanout_room, open_control_db, reset_and_enroll, reset_node_lease_tables,
    spawn_cluster_node, wait_for_cross_node_stanza, wait_for_node_lease_lapse, wait_for_readiness,
    AccountAuthority, ClusterNode, EnrolledPool, NodeSelectors,
};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::db::Database;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const DOMAIN: &str = "localhost";
const PASSWORD: &str = "selector-cluster-password";
const CLIENT_NS: &str = "jabber:client";
const BIND_NS: &str = "urn:ietf:params:xml:ns:xmpp-bind";
const CARBONS_NS: &str = "urn:xmpp:carbons:2";
const STREAM_NS: &str = "http://etherx.jabber.org/streams";
const ACCEPTANCE_ENV: &str = "WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE";
const OWNER_LOCALPARTS_ENV: &str = "WADDLE_SERVER_OWNER_LOCALPARTS";

/// The accounts every scenario seeds, by the name a scenario calls them.
const USERS: [&str; 5] = ["alice", "bob", "carol", "probe-a", "probe-b"];

/// `sm_durable_sessions.state` values.
const SESSION_LIVE: i64 = 1;
const SESSION_DETACHED: i64 = 2;

/// `account_resource_bindings.state` values.
const BINDING_ACTIVE: i64 = 3;

/// A bind must answer within the server's bind-retirement budget.
const BIND_BUDGET: Duration = Duration::from_secs(20);
const DELIVERY_BUDGET: Duration = Duration::from_secs(30);
/// How long a client is watched for a stanza that must not arrive.
const QUIET: Duration = Duration::from_secs(2);

/// How a stanza for a resource on the other node travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// The ordered relay: the shared database stays below the mailbox
    /// protocol epoch.
    Relay,
    /// Direct requests and room requests in the durable mailbox: both
    /// nodes select it and the database is at the mailbox epoch.
    Mailbox,
}

struct Node {
    server: TestServer,
    node_id: String,
    selectors: NodeSelectors,
}

impl Node {
    fn coordinates_accounts(&self) -> bool {
        self.selectors.account_authority == AccountAuthority::Coordinator
    }
}

/// Two meshed nodes over one database.
struct Cluster {
    db: Database,
    a: Node,
    b: Option<Node>,
    /// Suffix of every account this scenario seeds.
    tag: String,
    _pool: EnrolledPool,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Cluster {
    /// Both nodes with `selectors`.
    async fn uniform(selectors: NodeSelectors, delivery: Delivery) -> Self {
        Self::start(selectors, selectors, delivery).await
    }

    async fn start(
        selectors_a: NodeSelectors,
        selectors_b: NodeSelectors,
        delivery: Delivery,
    ) -> Self {
        let postgres_url = waddle_ws_test_support::require_test_postgres_url()
            .expect("the selector cluster suite needs PostgreSQL: set WADDLE_TEST_POSTGRES_URL");
        let serial = cluster_e2e_serial_lock().lock().await;
        let db = open_control_db(&postgres_url).await;
        let pool = generate_pool();
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;

        let tag = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
        let localparts: Vec<String> = USERS.iter().map(|user| format!("{user}-{tag}")).collect();
        let accounts: Vec<(&str, &str)> = localparts
            .iter()
            .map(|localpart| (localpart.as_str(), PASSWORD))
            .collect();
        // Room creation is limited to server owners.
        let owners = format!("admin,{}", localparts.join(","));
        let mut envs = vec![(OWNER_LOCALPARTS_ENV, owners.as_str())];
        if delivery == Delivery::Mailbox {
            envs.push((ACCEPTANCE_ENV, "mailbox"));
        }

        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let (server_a, node_a, _) = spawn_cluster_node(ClusterNode {
            postgres_url: &postgres_url,
            pool_env: &pool.pool_env,
            swarm_port: port_a,
            bootstrap_ports: &[port_b],
            selectors: selectors_a,
            accounts: &accounts,
            extra_envs: &envs,
        })
        .await;
        let (server_b, node_b, _) = spawn_cluster_node(ClusterNode {
            postgres_url: &postgres_url,
            pool_env: &pool.pool_env,
            swarm_port: port_b,
            bootstrap_ports: &[port_a],
            selectors: selectors_b,
            accounts: &accounts,
            extra_envs: &envs,
        })
        .await;
        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
        let a = Node {
            server: server_a,
            node_id: node_a,
            selectors: selectors_a,
        };
        let b = Node {
            server: server_b,
            node_id: node_b,
            selectors: selectors_b,
        };
        wait_for_cross_node_stanza(
            &a.server,
            &b.server,
            (&format!("probe-a-{tag}"), PASSWORD),
            (&format!("probe-b-{tag}"), PASSWORD),
        )
        .await;
        if delivery == Delivery::Mailbox {
            activate_mailbox_epoch(&db).await;
        }
        Self {
            db,
            a,
            b: Some(b),
            tag,
            _pool: pool,
            _serial: serial,
        }
    }

    fn b(&self) -> &Node {
        self.b.as_ref().expect("node B is running")
    }

    /// The bare JID of the scenario's account `user`.
    fn bare(&self, user: &str) -> String {
        format!("{user}-{}@{DOMAIN}", self.tag)
    }

    fn full(&self, user: &str, resource: &str) -> String {
        format!("{}/{resource}", self.bare(user))
    }

    /// An authenticated connection of `user` to `node` that has bound no
    /// resource.
    async fn connect(&self, node: &Node, user: &str) -> WsXmppClient {
        let mut client = WsXmppClient::connect(&node.server.ws_url())
            .await
            .expect("connect");
        client
            .authenticate(DOMAIN, &format!("{user}-{}", self.tag), PASSWORD)
            .await
            .expect("authenticate");
        client
    }

    /// A connection of `user` to `node` bound to `resource`. The first
    /// node an account is bound on owns the account.
    async fn bound(&self, node: &Node, user: &str, resource: &str) -> WsXmppClient {
        let mut client = self.connect(node, user).await;
        let answer = bind(&mut client, resource).await;
        assert_eq!(
            answer,
            Answer::Bound(self.full(user, resource)),
            "{user} binds {resource}"
        );
        client
    }

    /// [`Self::bound`] with resumable stream management; also returns the
    /// resume token.
    async fn resumable(&self, node: &Node, user: &str, resource: &str) -> (WsXmppClient, String) {
        let mut client = self.bound(node, user, resource).await;
        let token = enable(&mut client).await;
        (client, token)
    }

    /// Send available presence from the resource `full_jid`, bound on
    /// `node`, and wait until the server has processed it: it has answered
    /// a ping sent after the presence, and a coordinator node has recorded
    /// the presence, which is when a bare-JID stanza starts to select the
    /// resource.
    async fn go_available(&self, node: &Node, client: &mut WsXmppClient, full_jid: &str) {
        send(client, &Element::builder("presence", CLIENT_NS).build()).await;
        let ping = Element::builder("iq", CLIENT_NS)
            .attr(xml_ncname!("type").to_owned(), "get")
            .attr(xml_ncname!("id").to_owned(), "after-presence")
            .append(Element::builder("ping", "urn:xmpp:ping").build())
            .build();
        send(client, &ping).await;
        recv_element(client, "ping result", |element| {
            element.is("iq", CLIENT_NS) && element.attr("id") == Some("after-presence")
        })
        .await;
        if node.coordinates_accounts() {
            eventually("the binding records available presence", || async {
                binding(&self.db, full_jid)
                    .await
                    .filter(|row| row.presence_available)
            })
            .await;
        }
    }
}

/// What a server answered to a bind or a resume.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// The bind result, with the bound full JID.
    Bound(String),
    /// `<resumed/>`, with its `h`.
    Resumed(String),
    /// `<failed/>` with the named stanza error condition.
    Failed(String),
    /// A stream error with the named condition.
    StreamError(String),
    /// A stanza error on the request, with the named condition.
    StanzaError(String),
    /// The stream ended, or nothing arrived, before any of the above.
    Nothing(String),
}

fn first_child_name(element: &Element) -> String {
    element
        .children()
        .next()
        .map_or_else(|| "none".to_owned(), |child| child.name().to_owned())
}

/// Read frames until one answers a bind or resume request.
async fn answer(client: &mut WsXmppClient, budget: Duration) -> Answer {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = match client.recv_timeout(remaining).await {
            Ok(frame) => frame,
            Err(error) => return Answer::Nothing(error),
        };
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if element.is("error", STREAM_NS) {
            return Answer::StreamError(first_child_name(&element));
        }
        if element.is("resumed", SM_NS) {
            return Answer::Resumed(element.attr("h").unwrap_or_default().to_owned());
        }
        if element.is("failed", SM_NS) {
            return Answer::Failed(first_child_name(&element));
        }
        if element.is("iq", CLIENT_NS) {
            if let Some(jid) = element
                .get_child("bind", BIND_NS)
                .and_then(|bind| bind.get_child("jid", BIND_NS))
            {
                return Answer::Bound(jid.text());
            }
            if let Some(error) = element.get_child("error", CLIENT_NS) {
                return Answer::StanzaError(first_child_name(error));
            }
        }
    }
}

async fn send(client: &mut WsXmppClient, element: &Element) {
    client
        .send(&String::from(element))
        .await
        .expect("send a stanza");
}

async fn bind(client: &mut WsXmppClient, resource: &str) -> Answer {
    let request = Element::builder("iq", CLIENT_NS)
        .attr(xml_ncname!("type").to_owned(), "set")
        .attr(xml_ncname!("id").to_owned(), "bind")
        .append(
            Element::builder("bind", BIND_NS)
                .append(
                    Element::builder("resource", BIND_NS)
                        .append(resource)
                        .build(),
                )
                .build(),
        )
        .build();
    send(client, &request).await;
    answer(client, BIND_BUDGET).await
}

/// Enable resumable stream management and return the resume token.
async fn enable(client: &mut WsXmppClient) -> String {
    let request = Element::builder("enable", SM_NS)
        .attr(xml_ncname!("resume").to_owned(), "true")
        .build();
    send(client, &request).await;
    recv_element(client, "<enabled/>", |element| element.is("enabled", SM_NS))
        .await
        .attr("id")
        .expect("resume token")
        .to_owned()
}

async fn resume(client: &mut WsXmppClient, token: &str, handled: u32) -> Answer {
    let request = Element::builder("resume", SM_NS)
        .attr(xml_ncname!("previd").to_owned(), token)
        .attr(xml_ncname!("h").to_owned(), handled.to_string())
        .build();
    send(client, &request).await;
    answer(client, BIND_BUDGET).await
}

fn chat(to: &str, id: &str) -> Element {
    Element::builder("message", CLIENT_NS)
        .attr(xml_ncname!("to").to_owned(), to)
        .attr(xml_ncname!("type").to_owned(), "chat")
        .attr(xml_ncname!("id").to_owned(), id)
        .append(Element::builder("body", CLIENT_NS).append(id).build())
        .build()
}

fn is_message(element: &Element, id: &str) -> bool {
    element.is("message", CLIENT_NS) && element.attr("id") == Some(id)
}

async fn try_recv_element<F: Fn(&Element) -> bool>(
    client: &mut WsXmppClient,
    budget: Duration,
    accept: F,
) -> Result<Element, String> {
    client
        .recv_matching_within(budget, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| accept(&element))
        })
        .await
        .map(|frame| frame.parse().expect("server XML"))
}

async fn recv_element<F: Fn(&Element) -> bool>(
    client: &mut WsXmppClient,
    what: &str,
    accept: F,
) -> Element {
    try_recv_element(client, DELIVERY_BUDGET, accept)
        .await
        .unwrap_or_else(|error| panic!("{what} did not arrive: {error}"))
}

/// The message `id` arrives, and no second copy of it follows.
async fn recv_message_once(client: &mut WsXmppClient, id: &str) -> Element {
    let message = recv_element(client, id, |element| is_message(element, id)).await;
    let again = try_recv_element(client, QUIET, |element| is_message(element, id)).await;
    assert!(again.is_err(), "message {id} arrived twice: {again:?}");
    message
}

async fn expect_no_message(client: &mut WsXmppClient, id: &str) {
    let arrived = try_recv_element(client, QUIET, |element| is_message(element, id)).await;
    assert!(
        arrived.is_err(),
        "message {id} must not arrive: {arrived:?}"
    );
}

/// Poll `probe` until it yields a value.
async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: not reached within 20 s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Clone)]
struct SessionRow {
    session_id: String,
    state: i64,
    terminal_reason: Option<i64>,
    generation: i64,
    owner_node_id: Option<String>,
    outbound_appended: i64,
}

/// The durable sessions of `full_jid`, oldest change first.
async fn sessions(db: &Database, full_jid: &str) -> Vec<SessionRow> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT session_id, state::bigint, terminal_reason::bigint, generation, \
             owner_node_id, outbound_appended FROM sm_durable_sessions \
             WHERE full_jid = ? ORDER BY updated_at_ms",
            waddle_server::db_params![full_jid.to_owned()],
        )
        .await
        .expect("read durable sessions");
    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await.expect("durable session row") {
        sessions.push(SessionRow {
            session_id: row.get(0).expect("session_id"),
            state: row.get(1).expect("state"),
            terminal_reason: row.get(2).expect("terminal_reason"),
            generation: row.get(3).expect("generation"),
            owner_node_id: row.get(4).expect("owner_node_id"),
            outbound_appended: row.get(5).expect("outbound_appended"),
        });
    }
    sessions
}

/// The only durable session of `full_jid`, once it is in `state`.
async fn session_in(db: &Database, full_jid: &str, state: i64) -> SessionRow {
    eventually("the durable session reaches the expected state", || async {
        let rows = sessions(db, full_jid).await;
        assert!(rows.len() <= 1, "one session per resource: {rows:?}");
        rows.into_iter().find(|row| row.state == state)
    })
    .await
}

#[derive(Debug, Clone)]
struct BindingRow {
    state: i64,
    binding_ref: Option<String>,
    sm_session_id: Option<String>,
    presence_available: bool,
    carbons_enabled: bool,
}

async fn binding(db: &Database, full_jid: &str) -> Option<BindingRow> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT state::bigint, binding_ref, sm_session_id, presence_available::bigint, \
             carbons_enabled::bigint FROM account_resource_bindings WHERE full_jid = ?",
            waddle_server::db_params![full_jid.to_owned()],
        )
        .await
        .expect("read account bindings");
    let row = rows.next().await.expect("account binding row")?;
    Some(BindingRow {
        state: row.get(0).expect("state"),
        binding_ref: row.get(1).expect("binding_ref"),
        sm_session_id: row.get(2).expect("sm_session_id"),
        presence_available: row.get::<i64>(3).expect("presence_available") == 1,
        carbons_enabled: row.get::<i64>(4).expect("carbons_enabled") == 1,
    })
}

/// The binding of `full_jid`, once it holds a binding in `state`.
async fn binding_in(db: &Database, full_jid: &str, state: i64) -> BindingRow {
    eventually("the account binding reaches the expected state", || async {
        binding(db, full_jid)
            .await
            .filter(|row| row.state == state && row.binding_ref.is_some())
    })
    .await
}

/// The node that holds the `UserActor` claim of `bare_jid`.
async fn account_owner(db: &Database, bare_jid: &str) -> Option<String> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT node_id FROM clustering_claims WHERE entity = ?",
            waddle_server::db_params![format!("user_actor:{bare_jid}")],
        )
        .await
        .expect("read the account claim");
    rows.next()
        .await
        .expect("account claim row")
        .map(|row| row.get(0).expect("node_id"))
}

fn is_counted_stanza(element: &Element) -> bool {
    matches!(element.name(), "message" | "presence" | "iq") && element.ns() == CLIENT_NS
}

/// Read frames until the message `id` arrives. Returns how many counted
/// stanzas (XEP-0198 `h` units) were read, the message included.
async fn recv_counting(client: &mut WsXmppClient, id: &str) -> u32 {
    let mut counted = 0;
    loop {
        let frame = client
            .recv_timeout(DELIVERY_BUDGET)
            .await
            .unwrap_or_else(|error| panic!("message {id} did not arrive: {error}"));
        let Ok(element) = frame.parse::<Element>() else {
            continue;
        };
        if is_counted_stanza(&element) {
            counted += 1;
        }
        if is_message(&element, id) {
            return counted;
        }
    }
}

/// The selectors travel with each node: a node that selects the durable
/// authority and the coordinator commits their rows, and a node of the same
/// cluster that selects neither commits none.
#[tokio::test]
async fn each_node_serves_with_the_authorities_it_selects() {
    let cluster = Cluster::start(
        NodeSelectors::DURABLE_COORDINATOR,
        NodeSelectors::default(),
        Delivery::Relay,
    )
    .await;
    let alice = cluster.full("alice", "phone");
    let bob = cluster.full("bob", "desk");

    let (_alice, alice_token) = cluster.resumable(&cluster.a, "alice", "phone").await;
    let session = session_in(&cluster.db, &alice, SESSION_LIVE).await;
    assert_eq!(session.generation, 1);
    assert_eq!(
        session.owner_node_id.as_deref(),
        Some(cluster.a.node_id.as_str())
    );
    assert_ne!(
        session.session_id, alice_token,
        "the token is not the storage key"
    );
    let row = binding_in(&cluster.db, &alice, BINDING_ACTIVE).await;
    assert_eq!(
        row.sm_session_id.as_deref(),
        Some(session.session_id.as_str())
    );

    let (_bob, _) = cluster.resumable(cluster.b(), "bob", "desk").await;
    assert!(
        sessions(&cluster.db, &bob).await.is_empty(),
        "a legacy node commits no durable session"
    );
    assert!(
        binding(&cluster.db, &bob).await.is_none(),
        "a registry node commits no account binding"
    );
}

/// The stream error a node answers when session initialization fails.
fn internal_server_error() -> Answer {
    Answer::StreamError("internal-server-error".to_owned())
}

/// A resource bound on the node that owns its account detaches when its
/// transport is lost and resumes on that node with the unhandled stanza
/// replayed.
async fn same_node_bind_detach_and_resume(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let node = &cluster.a;
    let alice_jid = cluster.full("alice", "phone");
    let (mut alice, token) = cluster.resumable(node, "alice", "phone").await;
    assert_eq!(
        account_owner(&cluster.db, &cluster.bare("alice")).await,
        Some(node.node_id.clone())
    );
    let mut bob = cluster.bound(node, "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "first")).await;
    let handled = recv_counting(&mut alice, "first").await;
    send(&mut bob, &chat(&alice_jid, "second")).await;
    recv_counting(&mut alice, "second").await;

    drop(alice);
    let detached = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;
    assert_eq!(detached.generation, 1);

    let mut alice = cluster.connect(node, "alice").await;
    let answer = resume(&mut alice, &token, handled).await;
    assert!(matches!(answer, Answer::Resumed(_)), "{answer:?}");
    recv_message_once(&mut alice, "second").await;
    let live = session_in(&cluster.db, &alice_jid, SESSION_LIVE).await;
    assert_eq!(live.session_id, detached.session_id);
    assert_eq!(live.generation, 2);
    assert_eq!(live.owner_node_id.as_deref(), Some(node.node_id.as_str()));

    send(&mut bob, &chat(&alice_jid, "third")).await;
    recv_message_once(&mut alice, "third").await;
}

/// Binding a full JID that a live connection of the same node holds
/// closes that connection with a conflict and gives the resource to the
/// new one.
async fn same_node_bind_replaces_a_live_binding(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let node = &cluster.a;
    let alice_jid = cluster.full("alice", "laptop");
    let mut old = cluster.bound(node, "alice", "laptop").await;

    let mut new = cluster.connect(node, "alice").await;
    let answer = bind(&mut new, "laptop").await;
    let conflict = recv_element(&mut old, "conflict", |element| {
        element.is("error", STREAM_NS)
    })
    .await;
    assert_eq!(first_child_name(&conflict), "conflict");
    assert_eq!(answer, Answer::Bound(alice_jid.clone()));

    let mut bob = cluster.bound(node, "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "after-replace")).await;
    recv_message_once(&mut new, "after-replace").await;
}

/// Binding a resource again while its durable session is detached.
///
/// Intended: the bind succeeds, the detached session becomes terminal with
/// the reason "replaced", and its resume token names no session.
async fn bind_over_a_detached_session(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let node = &cluster.a;
    let alice_jid = cluster.full("alice", "phone");
    let (alice, token) = cluster.resumable(node, "alice", "phone").await;
    drop(alice);
    let detached = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;

    let mut again = cluster.connect(node, "alice").await;
    let answer = bind(&mut again, "phone").await;
    if node.coordinates_accounts() {
        // Pinned gap: the detached binding holds the resource's slot at
        // the coordinator. The bind waits for that binding to retire,
        // which nothing does while its session stays detached, and ends
        // in a stream error when the wait runs out.
        assert_eq!(answer, internal_server_error());
        return;
    }
    // Pinned gap: a clustered durable store refuses to end a resource's
    // sessions for a bind. The bind succeeds and the detached session
    // stays detached. A resume with its token is refused, because the
    // resource is bound, and the session is still not terminal.
    assert_eq!(answer, Answer::Bound(alice_jid.clone()));
    let unreplaced = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;
    assert_eq!(unreplaced.session_id, detached.session_id);
    assert_eq!(unreplaced.terminal_reason, None);
    let mut late = cluster.connect(node, "alice").await;
    assert_eq!(
        resume(&mut late, &token, 0).await,
        Answer::Failed("item-not-found".to_owned())
    );
    let after = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;
    assert_eq!(after.session_id, detached.session_id);
    let mut bob = cluster.bound(node, "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "after-rebind")).await;
    recv_message_once(&mut again, "after-rebind").await;
}

/// Chat messages between accounts whose resources are on different nodes
/// arrive once in each direction, to a full JID and to a bare JID.
async fn accounts_on_different_nodes_exchange_messages(
    selectors: NodeSelectors,
    delivery: Delivery,
) {
    let cluster = Cluster::uniform(selectors, delivery).await;
    let alice_jid = cluster.full("alice", "phone");
    let bob_jid = cluster.full("bob", "desk");
    let (mut alice, _) = cluster.resumable(&cluster.a, "alice", "phone").await;
    let (mut bob, _) = cluster.resumable(cluster.b(), "bob", "desk").await;
    cluster
        .go_available(&cluster.a, &mut alice, &alice_jid)
        .await;
    cluster.go_available(cluster.b(), &mut bob, &bob_jid).await;

    send(&mut bob, &chat(&alice_jid, "b-to-a-full")).await;
    let delivered = recv_message_once(&mut alice, "b-to-a-full").await;
    assert_eq!(delivered.attr("from"), Some(bob_jid.as_str()));

    send(&mut alice, &chat(&bob_jid, "a-to-b-full")).await;
    let delivered = recv_message_once(&mut bob, "a-to-b-full").await;
    assert_eq!(delivered.attr("from"), Some(alice_jid.as_str()));

    send(&mut bob, &chat(&cluster.bare("alice"), "b-to-a-bare")).await;
    recv_message_once(&mut alice, "b-to-a-bare").await;

    send(&mut alice, &chat(&cluster.bare("bob"), "a-to-b-bare")).await;
    recv_message_once(&mut bob, "a-to-b-bare").await;

    // Each recipient's copies were taken into its durable session's custody
    // on the node that hosts it.
    let alice_session = session_in(&cluster.db, &alice_jid, SESSION_LIVE).await;
    assert_eq!(
        alice_session.owner_node_id.as_deref(),
        Some(cluster.a.node_id.as_str())
    );
    assert!(alice_session.outbound_appended >= 2, "{alice_session:?}");
    let bob_session = session_in(&cluster.db, &bob_jid, SESSION_LIVE).await;
    assert_eq!(
        bob_session.owner_node_id.as_deref(),
        Some(cluster.b().node_id.as_str())
    );
    assert!(bob_session.outbound_appended >= 2, "{bob_session:?}");
}

async fn messages_through_the_mailbox(selectors: NodeSelectors) {
    accounts_on_different_nodes_exchange_messages(selectors, Delivery::Mailbox).await;
}

async fn messages_over_the_relay(selectors: NodeSelectors) {
    accounts_on_different_nodes_exchange_messages(selectors, Delivery::Relay).await;
}

fn is_offline_copy(element: &Element, id: &str) -> bool {
    is_message(element, id) && element.has_child("delay", "urn:xmpp:delay")
}

/// A chat message for the bare JID of an account that has no connection on
/// either node is stored, and the account's next available resource
/// receives it once as a delayed message.
async fn message_to_an_account_without_a_connection(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let mut bob = cluster.bound(cluster.b(), "bob", "desk").await;
    send(&mut bob, &chat(&cluster.bare("carol"), "for-later")).await;
    // The sender's next round trip orders the test after the message.
    send(&mut bob, &chat(&cluster.full("bob", "desk"), "marker")).await;
    let marker = try_recv_element(&mut bob, DELIVERY_BUDGET, |element| {
        is_message(element, "marker") || element.is("error", STREAM_NS)
    })
    .await
    .expect("the sender's stream answers");
    assert!(is_message(&marker, "marker"), "{marker:?}");

    let mut carol = cluster.bound(&cluster.a, "carol", "phone").await;
    send(&mut carol, &Element::builder("presence", CLIENT_NS).build()).await;
    let stored = recv_message_once(&mut carol, "for-later").await;
    assert!(is_offline_copy(&stored, "for-later"), "{stored:?}");
    assert_eq!(stored.attr("to"), Some(cluster.bare("carol").as_str()));
}

/// Enable carbons on the resource `full_jid`, bound on `node`.
async fn enable_carbons(cluster: &Cluster, node: &Node, client: &mut WsXmppClient, full_jid: &str) {
    let request = Element::builder("iq", CLIENT_NS)
        .attr(xml_ncname!("type").to_owned(), "set")
        .attr(xml_ncname!("id").to_owned(), "carbons")
        .append(Element::builder("enable", CARBONS_NS).build())
        .build();
    send(client, &request).await;
    let result = recv_element(client, "carbons result", |element| {
        element.is("iq", CLIENT_NS) && element.attr("id") == Some("carbons")
    })
    .await;
    assert_eq!(result.attr("type"), Some("result"));
    if node.coordinates_accounts() {
        eventually("the binding records carbons", || async {
            binding(&cluster.db, full_jid)
                .await
                .filter(|row| row.carbons_enabled)
        })
        .await;
    }
}

fn is_carbon(element: &Element, direction: &str, id: &str) -> bool {
    element.is("message", CLIENT_NS)
        && element
            .get_child(direction, CARBONS_NS)
            .and_then(|carbon| carbon.get_child("forwarded", "urn:xmpp:forward:0"))
            .and_then(|forwarded| forwarded.get_child("message", CLIENT_NS))
            .is_some_and(|message| message.attr("id") == Some(id))
}

/// Two resources of one account on the node that owns it, and a
/// correspondent on the other node: the resource that enabled carbons
/// receives a copy of the message its sibling receives from the other node
/// and of the message its sibling sends there.
async fn carbons_reach_a_sibling_on_the_owning_node(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let laptop_jid = cluster.full("alice", "laptop");
    let phone_jid = cluster.full("alice", "phone");
    let bob_jid = cluster.full("bob", "desk");
    let mut laptop = cluster.bound(&cluster.a, "alice", "laptop").await;
    let mut phone = cluster.bound(&cluster.a, "alice", "phone").await;
    cluster
        .go_available(&cluster.a, &mut laptop, &laptop_jid)
        .await;
    cluster
        .go_available(&cluster.a, &mut phone, &phone_jid)
        .await;
    enable_carbons(&cluster, &cluster.a, &mut phone, &phone_jid).await;
    let mut bob = cluster.bound(cluster.b(), "bob", "desk").await;
    cluster.go_available(cluster.b(), &mut bob, &bob_jid).await;

    send(&mut bob, &chat(&laptop_jid, "inbound")).await;
    recv_message_once(&mut laptop, "inbound").await;
    recv_element(&mut phone, "received carbon", |element| {
        is_carbon(element, "received", "inbound")
    })
    .await;

    send(&mut laptop, &chat(&bob_jid, "outbound")).await;
    recv_message_once(&mut bob, "outbound").await;
    recv_element(&mut phone, "sent carbon", |element| {
        is_carbon(element, "sent", "outbound")
    })
    .await;
}

/// A second resource of an account, bound on the node that does not own
/// the account.
///
/// Intended: the bind succeeds, the account stays with the node that owns
/// it, and the resource takes part in the account like one on the owning
/// node: with carbons enabled it receives a copy of a message a
/// correspondent sends to its sibling on the other node.
async fn second_resource_on_the_other_node(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let laptop_jid = cluster.full("alice", "laptop");
    let phone_jid = cluster.full("alice", "phone");
    let mut laptop = cluster.bound(&cluster.a, "alice", "laptop").await;
    cluster
        .go_available(&cluster.a, &mut laptop, &laptop_jid)
        .await;

    let mut phone = cluster.connect(cluster.b(), "alice").await;
    let answer = bind(&mut phone, "phone").await;
    assert_eq!(
        account_owner(&cluster.db, &cluster.bare("alice")).await,
        Some(cluster.a.node_id.clone()),
        "the account stays with the node that bound it first"
    );
    if cluster.b().coordinates_accounts() {
        // Pinned gap: a node starts a coordinator only for an account
        // whose claim it holds, so it cannot record the binding.
        assert_eq!(answer, internal_server_error());
        assert!(binding(&cluster.db, &phone_jid).await.is_none());
        return;
    }
    assert_eq!(answer, Answer::Bound(phone_jid.clone()));
    cluster
        .go_available(cluster.b(), &mut phone, &phone_jid)
        .await;
    enable_carbons(&cluster, cluster.b(), &mut phone, &phone_jid).await;
    let mut bob = cluster.bound(cluster.b(), "bob", "desk").await;
    send(&mut bob, &chat(&laptop_jid, "to-the-laptop")).await;
    recv_message_once(&mut laptop, "to-the-laptop").await;
    recv_element(&mut phone, "received carbon", |element| {
        is_carbon(element, "received", "to-the-laptop")
    })
    .await;
}

fn is_groupchat(element: &Element, from: &str, id: &str) -> bool {
    is_message(element, id)
        && element.attr("type") == Some("groupchat")
        && element.attr("from") == Some(from)
}

/// Occupants of one room on different nodes each receive the other's
/// groupchat message and the reflection of their own.
async fn groupchat_reaches_occupants_on_both_nodes(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let room: jid::BareJid = format!("selectors-{}@muc.{DOMAIN}", cluster.tag)
        .parse()
        .expect("room JID");
    let (mut alice, _) = cluster.resumable(&cluster.a, "alice", "phone").await;
    let (mut bob, _) = cluster.resumable(cluster.b(), "bob", "desk").await;
    join_fanout_room(&mut alice, &room, "alice").await;
    join_fanout_room(&mut bob, &room, "bob").await;

    send(
        &mut alice,
        &fanout_message(&room, "from-alice", Some("from alice")),
    )
    .await;
    let from_alice = format!("{room}/alice");
    for client in [&mut bob, &mut alice] {
        recv_element(client, "alice's groupchat message", |element| {
            is_groupchat(element, &from_alice, "from-alice")
        })
        .await;
    }

    send(
        &mut bob,
        &fanout_message(&room, "from-bob", Some("from bob")),
    )
    .await;
    let from_bob = format!("{room}/bob");
    for client in [&mut alice, &mut bob] {
        recv_element(client, "bob's groupchat message", |element| {
            is_groupchat(element, &from_bob, "from-bob")
        })
        .await;
    }
}

/// The refusal of a resume whose session claim another node holds.
fn held_elsewhere() -> Answer {
    Answer::Failed("resource-constraint".to_owned())
}

/// A session detached on the node that owns it, resumed on the other node.
///
/// Intended: the other node takes the session, answers `<resumed/>` and
/// replays the stanza the client did not handle.
///
/// Pinned gap: a node does not take over a session claim another node
/// holds. The resume is refused and the session stays detached on its
/// node.
async fn detached_session_resumes_on_the_other_node(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let alice_jid = cluster.full("alice", "phone");
    let (mut alice, token) = cluster.resumable(&cluster.a, "alice", "phone").await;
    let mut bob = cluster.bound(&cluster.a, "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "held")).await;
    let handled = recv_counting(&mut alice, "held").await;
    drop(alice);
    let detached = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;

    let mut alice = cluster.connect(cluster.b(), "alice").await;
    assert_eq!(
        resume(&mut alice, &token, handled - 1).await,
        held_elsewhere()
    );
    let after = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;
    assert_eq!(after.session_id, detached.session_id);
    assert_eq!(after.generation, 1);
    assert_eq!(
        after.owner_node_id.as_deref(),
        Some(cluster.a.node_id.as_str())
    );
}

/// A session that is live on the node that owns it, resumed on the other
/// node.
///
/// Intended: the other node takes the session and answers `<resumed/>`,
/// and the connection that held it is closed with a conflict.
///
/// Pinned gap: the resume is refused like the resume of a detached
/// session, and the session stays live on its node.
async fn live_session_resumes_on_the_other_node(selectors: NodeSelectors) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let alice_jid = cluster.full("alice", "phone");
    let (mut old, token) = cluster.resumable(&cluster.a, "alice", "phone").await;
    session_in(&cluster.db, &alice_jid, SESSION_LIVE).await;

    let mut alice = cluster.connect(cluster.b(), "alice").await;
    assert_eq!(resume(&mut alice, &token, 0).await, held_elsewhere());
    let live = session_in(&cluster.db, &alice_jid, SESSION_LIVE).await;
    assert_eq!(live.generation, 1);
    assert_eq!(
        live.owner_node_id.as_deref(),
        Some(cluster.a.node_id.as_str())
    );
    let mut bob = cluster.bound(&cluster.a, "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "still-live")).await;
    recv_message_once(&mut old, "still-live").await;
}

/// A chat message for the full JID of a detached session, sent by a
/// correspondent on the session's node or on the other node.
///
/// Intended: the stanza enters the detached session's custody and the
/// resume replays it once.
async fn stanza_to_a_detached_session(selectors: NodeSelectors, sender_on_session_node: bool) {
    let cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let alice_jid = cluster.full("alice", "phone");
    let (alice, token) = cluster.resumable(&cluster.a, "alice", "phone").await;
    let sender_node = if sender_on_session_node {
        &cluster.a
    } else {
        cluster.b()
    };
    let mut bob = cluster.bound(sender_node, "bob", "desk").await;
    drop(alice);
    let detached = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;

    send(&mut bob, &chat(&alice_jid, "while-detached")).await;
    // The sender's next round trip orders the test after the message.
    send(&mut bob, &chat(&cluster.full("bob", "desk"), "marker")).await;
    let marker = try_recv_element(&mut bob, DELIVERY_BUDGET, |element| {
        is_message(element, "marker") || element.is("error", STREAM_NS)
    })
    .await
    .expect("the sender's stream answers");
    assert!(is_message(&marker, "marker"), "{marker:?}");

    let mut alice = cluster.connect(&cluster.a, "alice").await;
    let answer = resume(&mut alice, &token, 0).await;
    // Pinned gap: the stanza does not enter the detached session's
    // custody. It is stored for offline delivery, the resume replays
    // nothing, and the account's next resource to become available
    // receives it as a delayed message.
    assert!(matches!(answer, Answer::Resumed(_)), "{answer:?}");
    expect_no_message(&mut alice, "while-detached").await;
    let resumed = session_in(&cluster.db, &alice_jid, SESSION_LIVE).await;
    assert_eq!(resumed.session_id, detached.session_id);
    assert_eq!(resumed.outbound_appended, 0);
    let mut tablet = cluster.bound(&cluster.a, "alice", "tablet").await;
    send(
        &mut tablet,
        &Element::builder("presence", CLIENT_NS).build(),
    )
    .await;
    let stored = recv_message_once(&mut tablet, "while-detached").await;
    assert!(is_offline_copy(&stored, "while-detached"), "{stored:?}");
}

async fn stanza_from_the_same_node_to_a_detached_session(selectors: NodeSelectors) {
    stanza_to_a_detached_session(selectors, true).await;
}

async fn stanza_from_the_other_node_to_a_detached_session(selectors: NodeSelectors) {
    stanza_to_a_detached_session(selectors, false).await;
}

/// Kill node B and wait until its node lease has lapsed and node A serves.
async fn kill_node_b(cluster: &mut Cluster) {
    let node = cluster.b.take().expect("node B is running");
    let node_id = node.node_id.clone();
    drop(node);
    wait_for_node_lease_lapse(&cluster.db, &node_id).await;
    wait_for_readiness(&cluster.a.server, true, Duration::from_secs(30)).await;
}

/// The node that holds the claim of the stream-management session
/// `session_id`.
async fn session_claim_owner(db: &Database, session_id: &str) -> Option<String> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT node_id FROM clustering_claims WHERE entity = ?",
            waddle_server::db_params![format!("sm_session:{session_id}")],
        )
        .await
        .expect("read the session claim");
    rows.next()
        .await
        .expect("session claim row")
        .map(|row| row.get(0).expect("node_id"))
}

/// The node that hosts a live resumable session is killed, and the client
/// resumes on the surviving node.
///
/// Intended: once the dead node's lease has lapsed the survivor takes the
/// session, answers `<resumed/>` and replays the unhandled stanza.
///
/// Pinned gap: once the survivor has released the dead node's claim on
/// the session, the resume commits the session's next generation on the
/// survivor, is answered `<failed/>` with item-not-found and leaves the
/// session detached. A registry node then binds the resource afresh; a
/// coordinator node refuses the bind, because the account's claim still
/// names the dead node.
async fn session_of_a_killed_node_resumes_on_the_survivor(selectors: NodeSelectors) {
    let mut cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let alice_jid = cluster.full("alice", "phone");
    let (mut alice, token) = cluster.resumable(cluster.b(), "alice", "phone").await;
    let mut bob = cluster.bound(cluster.b(), "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "held")).await;
    let handled = recv_counting(&mut alice, "held").await;
    let session = session_in(&cluster.db, &alice_jid, SESSION_LIVE).await;
    kill_node_b(&mut cluster).await;
    drop(alice);
    drop(bob);
    eventually("the dead node's session claim is released", || async {
        session_claim_owner(&cluster.db, &session.session_id)
            .await
            .is_none()
            .then_some(())
    })
    .await;

    let mut alice = cluster.connect(&cluster.a, "alice").await;
    assert_eq!(
        resume(&mut alice, &token, handled - 1).await,
        Answer::Failed("item-not-found".to_owned())
    );
    let after = session_in(&cluster.db, &alice_jid, SESSION_DETACHED).await;
    assert_eq!(after.session_id, session.session_id);
    assert_eq!(after.generation, 2);
    assert_eq!(
        after.owner_node_id.as_deref(),
        Some(cluster.a.node_id.as_str())
    );
    let fresh = bind(&mut alice, "phone").await;
    if cluster.a.coordinates_accounts() {
        assert_eq!(fresh, internal_server_error());
    } else {
        assert_eq!(fresh, Answer::Bound(alice_jid));
    }
}

/// The node that owns an account and hosts its resource is killed, and
/// the client binds the same resource on the surviving node.
///
/// Intended: once the dead node's lease has lapsed the bind succeeds, the
/// survivor owns the account, and the resource receives messages for its
/// full JID and its bare JID.
async fn resource_of_a_killed_node_binds_on_the_survivor(selectors: NodeSelectors) {
    let mut cluster = Cluster::uniform(selectors, Delivery::Mailbox).await;
    let alice_jid = cluster.full("alice", "phone");
    let (alice, _) = cluster.resumable(cluster.b(), "alice", "phone").await;
    let dead = cluster.b().node_id.clone();
    kill_node_b(&mut cluster).await;
    drop(alice);

    let mut alice = cluster.connect(&cluster.a, "alice").await;
    let answer = bind(&mut alice, "phone").await;
    if cluster.a.coordinates_accounts() {
        // Pinned gap: the account's claim still names the dead node, and
        // the coordinator does not take over a claim another node's lapsed
        // lease holds, so it does not start for the account.
        assert_eq!(answer, internal_server_error());
        assert_eq!(
            account_owner(&cluster.db, &cluster.bare("alice")).await,
            Some(dead)
        );
        return;
    }
    assert_eq!(answer, Answer::Bound(alice_jid.clone()));
    cluster
        .go_available(&cluster.a, &mut alice, &alice_jid)
        .await;
    assert_eq!(
        account_owner(&cluster.db, &cluster.bare("alice")).await,
        Some(cluster.a.node_id.clone()),
        "the survivor owns the account"
    );
    let mut bob = cluster.bound(&cluster.a, "bob", "desk").await;
    send(&mut bob, &chat(&alice_jid, "to-full")).await;
    recv_message_once(&mut alice, "to-full").await;
    send(&mut bob, &chat(&cluster.bare("alice"), "to-bare")).await;
    recv_message_once(&mut alice, "to-bare").await;
    expect_no_message(&mut bob, "to-bare").await;
}

/// One test per scenario for a selector profile.
macro_rules! profile {
    ($profile:ident, $selectors:expr, { $($test:ident => $scenario:ident,)* }) => {
        mod $profile {
            use super::NodeSelectors;

            $(
                #[tokio::test]
                async fn $test() {
                    super::$scenario($selectors).await;
                }
            )*
        }
    };
}

profile!(durable_registry, NodeSelectors::DURABLE_REGISTRY, {
    same_node_bind_detach_and_resume => same_node_bind_detach_and_resume,
    same_node_bind_replaces_a_live_binding => same_node_bind_replaces_a_live_binding,
    gap_bind_over_a_detached_session => bind_over_a_detached_session,
    messages_through_the_mailbox => messages_through_the_mailbox,
    messages_over_the_relay => messages_over_the_relay,
    message_to_an_account_without_a_connection => message_to_an_account_without_a_connection,
    carbons_reach_a_sibling_on_the_owning_node => carbons_reach_a_sibling_on_the_owning_node,
    second_resource_on_the_other_node => second_resource_on_the_other_node,
    groupchat_reaches_occupants_on_both_nodes => groupchat_reaches_occupants_on_both_nodes,
    gap_detached_session_resumes_on_the_other_node => detached_session_resumes_on_the_other_node,
    gap_live_session_resumes_on_the_other_node => live_session_resumes_on_the_other_node,
    gap_stanza_from_the_same_node_to_a_detached_session => stanza_from_the_same_node_to_a_detached_session,
    gap_stanza_from_the_other_node_to_a_detached_session => stanza_from_the_other_node_to_a_detached_session,
    gap_session_of_a_killed_node_resumes_on_the_survivor => session_of_a_killed_node_resumes_on_the_survivor,
    resource_of_a_killed_node_binds_on_the_survivor => resource_of_a_killed_node_binds_on_the_survivor,
});

profile!(durable_coordinator, NodeSelectors::DURABLE_COORDINATOR, {
    same_node_bind_detach_and_resume => same_node_bind_detach_and_resume,
    same_node_bind_replaces_a_live_binding => same_node_bind_replaces_a_live_binding,
    gap_bind_over_a_detached_session => bind_over_a_detached_session,
    messages_through_the_mailbox => messages_through_the_mailbox,
    messages_over_the_relay => messages_over_the_relay,
    message_to_an_account_without_a_connection => message_to_an_account_without_a_connection,
    carbons_reach_a_sibling_on_the_owning_node => carbons_reach_a_sibling_on_the_owning_node,
    gap_second_resource_on_the_other_node => second_resource_on_the_other_node,
    groupchat_reaches_occupants_on_both_nodes => groupchat_reaches_occupants_on_both_nodes,
    gap_detached_session_resumes_on_the_other_node => detached_session_resumes_on_the_other_node,
    gap_live_session_resumes_on_the_other_node => live_session_resumes_on_the_other_node,
    gap_stanza_from_the_same_node_to_a_detached_session => stanza_from_the_same_node_to_a_detached_session,
    gap_stanza_from_the_other_node_to_a_detached_session => stanza_from_the_other_node_to_a_detached_session,
    gap_session_of_a_killed_node_resumes_on_the_survivor => session_of_a_killed_node_resumes_on_the_survivor,
    gap_resource_of_a_killed_node_binds_on_the_survivor => resource_of_a_killed_node_binds_on_the_survivor,
});
