//! Two clustered nodes with the account coordinator and the durable
//! stream-management authority selected
//! (`WADDLE_XMPP_ACCOUNT_AUTHORITY=coordinator`,
//! `WADDLE_XMPP_DURABLE_SM=durable`) on one PostgreSQL.
//!
//! On a clustered node a coordinator acts only under the account's
//! `UserActor` claim. These tests observe that claim in
//! `clustering_claims` beside the `account_resource_bindings` rows: an
//! account nobody has bound has no claim and reads as an account without
//! resources, the first bind takes the claim for the node that hosts it,
//! and the claim is released with the account's last binding, a detached
//! one included.

#![cfg(all(feature = "clustering", feature = "test-support"))]

#[path = "cluster_support/mod.rs"]
pub mod cluster_support;

use std::time::Duration;

use cluster_support::{
    cluster_e2e_serial_lock, free_tcp_port, generate_pool, open_control_db, reset_and_enroll,
    reset_node_lease_tables, spawn_cluster_server_with_envs, wait_for_readiness, EnrolledPool,
    CLUSTER_PEER_PASSWORD, CLUSTER_PEER_USERNAME,
};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::db::Database;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;

const DOMAIN: &str = "localhost";
/// The account whose claim the tests observe.
const ACCOUNT: &str = "admin";
const CLIENT_NS: &str = "jabber:client";
const SELECTORS: &[(&str, &str)] = &[
    ("WADDLE_XMPP_ACCOUNT_AUTHORITY", "coordinator"),
    ("WADDLE_XMPP_DURABLE_SM", "durable"),
];

/// `account_resource_bindings.state` values.
const STATE_ACTIVE: i64 = 3;
const STATE_DETACHED: i64 = 4;

/// Two nodes that select the coordinator, and the database they share.
struct Scenario {
    db: Database,
    /// `None` once the scenario has killed node A.
    server_a: Option<TestServer>,
    node_a: String,
    server_b: TestServer,
    node_b: String,
    _pool: EnrolledPool,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Scenario {
    async fn start() -> Self {
        let postgres_url = waddle_ws_test_support::require_test_postgres_url()
            .expect("the coordinator cluster suite needs PostgreSQL: set WADDLE_TEST_POSTGRES_URL");
        let serial = cluster_e2e_serial_lock().lock().await;
        let db = open_control_db(&postgres_url).await;
        let pool = generate_pool();
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;
        forget_bindings(&db).await;
        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let (server_a, node_a, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_a,
            &[port_b],
            SELECTORS,
        )
        .await;
        let (server_b, node_b, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_b,
            &[port_a],
            SELECTORS,
        )
        .await;
        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
        Self {
            db,
            server_a: Some(server_a),
            node_a,
            server_b,
            node_b,
            _pool: pool,
            _serial: serial,
        }
    }

    /// A resource of the observed account bound on `server`.
    ///
    /// Every node seeds the account with its own generated password when it
    /// starts, so the stored credential is the one of the node that started
    /// last, node B.
    async fn account_on(&self, server: &TestServer, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &server.ws_url(),
            DOMAIN,
            ACCOUNT,
            self.server_b.fixed_account_password(),
            resource,
        )
        .await
        .unwrap_or_else(|error| panic!("{ACCOUNT}/{resource} binds: {error}"))
    }

    /// An authenticated stream of the observed account without a resource.
    async fn authenticated_on(&self, server: &TestServer) -> WsXmppClient {
        let mut client = WsXmppClient::connect(&server.ws_url())
            .await
            .expect("connect");
        client
            .authenticate(DOMAIN, ACCOUNT, self.server_b.fixed_account_password())
            .await
            .expect("authenticate");
        client
    }

    fn server_a(&self) -> &TestServer {
        self.server_a.as_ref().expect("node A")
    }

    /// Kill node A without letting it run any teardown, and wait until
    /// the cluster has recorded its node lease as expired. Until then a
    /// claim it holds counts as held by a live node.
    async fn kill_node_a(&mut self) {
        drop(self.server_a.take().expect("node A"));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let conn = self.db.guard().await.expect("database guard");
            let mut rows = conn
                .query(
                    "SELECT count(*) FROM clustering_nodes WHERE node_id = ? AND NOT expired",
                    waddle_server::db_params![self.node_a.clone()],
                )
                .await
                .expect("node lease query");
            let unexpired = rows
                .next()
                .await
                .expect("node lease row")
                .expect("one row")
                .get::<i64>(0)
                .expect("unexpired count");
            if unexpired == 0 {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "node A's lease was not recorded as expired"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn peer_on(&self, server: &TestServer, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &server.ws_url(),
            DOMAIN,
            CLUSTER_PEER_USERNAME,
            CLUSTER_PEER_PASSWORD,
            resource,
        )
        .await
        .expect("the peer account binds")
    }
}

/// Delete the binding rows earlier scenarios left in the shared database.
/// Their nodes are gone, and a row that names a binding keeps its account
/// bound. The relations do not exist before the first node has started.
///
/// The relations accept a write only from a transaction that proves the
/// live protocol epoch, so the proof and the deletes run as one statement.
async fn forget_bindings(db: &Database) {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT count(*) FROM information_schema.tables WHERE table_name IN \
             ('account_resource_operations', 'account_resource_bindings', \
             'ingress_protocol_epoch', 'pending_delivery')",
            (),
        )
        .await
        .expect("relation lookup");
    let relations = rows
        .next()
        .await
        .expect("relation row")
        .expect("one row")
        .get::<i64>(0)
        .expect("relation count");
    drop(rows);
    if relations < 4 {
        return;
    }
    // A stored stanza an earlier scenario delivered just before its nodes
    // were killed stays claimed by that delivery, which holds back the
    // account's later stored stanzas.
    conn.execute(
        "DELETE FROM pending_delivery WHERE recipient_jid = ?",
        waddle_server::db_params![account_jid()],
    )
    .await
    .expect("delete stored stanzas");
    conn.execute(
        "DO $$ BEGIN \
         PERFORM pg_catalog.set_config('waddle.protocol_epoch', \
             (SELECT epoch::text FROM ingress_protocol_epoch WHERE id = 1), true), \
           pg_catalog.set_config('waddle.protocol_epoch_xid', \
             pg_catalog.pg_current_xact_id()::text, true); \
         DELETE FROM account_resource_operations; \
         DELETE FROM account_resource_bindings; \
         END $$",
        (),
    )
    .await
    .expect("delete binding rows");
}

fn account_jid() -> String {
    format!("{ACCOUNT}@{DOMAIN}")
}

fn full_jid(resource: &str) -> String {
    format!("{ACCOUNT}@{DOMAIN}/{resource}")
}

/// The node and epoch of the observed account's `UserActor` claim.
async fn account_claim(db: &Database) -> Option<(String, i64)> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT node_id, claim_epoch FROM clustering_claims WHERE entity = ?",
            waddle_server::db_params![format!("user_actor:{}", account_jid())],
        )
        .await
        .expect("claim query");
    rows.next().await.expect("claim row").map(|row| {
        (
            row.get::<String>(0).expect("claim node"),
            row.get::<i64>(1).expect("claim epoch"),
        )
    })
}

async fn wait_for_claim<F: Fn(Option<&(String, i64)>) -> bool>(
    db: &Database,
    expectation: &str,
    accept: F,
) -> Option<(String, i64)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let claim = account_claim(db).await;
        if accept(claim.as_ref()) {
            return claim;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the account claim is {claim:?}, expected {expectation}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The lifecycle state of the binding a resource row holds, if it holds one.
async fn binding_state(db: &Database, resource: &str) -> Option<i64> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT state FROM account_resource_bindings \
             WHERE full_jid = ? AND binding_ref IS NOT NULL",
            waddle_server::db_params![full_jid(resource)],
        )
        .await
        .expect("binding query");
    rows.next()
        .await
        .expect("binding row")
        .map(|row| row.get::<i64>(0).expect("binding state"))
}

/// The reference of the binding a resource row holds.
async fn binding_ref(db: &Database, resource: &str) -> Option<String> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT binding_ref FROM account_resource_bindings WHERE full_jid = ?",
            waddle_server::db_params![full_jid(resource)],
        )
        .await
        .expect("binding query");
    rows.next()
        .await
        .expect("binding row")
        .and_then(|row| row.get::<Option<String>>(0).expect("binding reference"))
}

/// Wait until the resource's row names a stream-management session.
async fn wait_for_session_reference(db: &Database, resource: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let conn = db.guard().await.expect("database guard");
        let mut rows = conn
            .query(
                "SELECT count(*) FROM account_resource_bindings \
                 WHERE full_jid = ? AND sm_session_id IS NOT NULL",
                waddle_server::db_params![full_jid(resource)],
            )
            .await
            .expect("session reference query");
        let referenced = rows
            .next()
            .await
            .expect("session reference row")
            .expect("one row")
            .get::<i64>(0)
            .expect("session reference count");
        if referenced == 1 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{resource} did not record its stream-management session"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_binding(db: &Database, resource: &str, expected: Option<i64>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let state = binding_state(db, resource).await;
        if state == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the binding of {resource} is in state {state:?}, expected {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn send_element(client: &mut WsXmppClient, element: Element) {
    client
        .send(&String::from(&element))
        .await
        .expect("send a stanza");
}

fn chat(to: &str, id: &str, body: &str) -> Element {
    Element::builder("message", CLIENT_NS)
        .attr(xml_ncname!("to").to_owned(), to)
        .attr(xml_ncname!("type").to_owned(), "chat")
        .attr(xml_ncname!("id").to_owned(), id)
        .append(Element::builder("body", CLIENT_NS).append(body).build())
        .build()
}

fn available() -> Element {
    Element::builder("presence", CLIENT_NS)
        .append(Element::builder("priority", CLIENT_NS).append("0").build())
        .build()
}

fn ping(id: &str) -> Element {
    Element::builder("iq", CLIENT_NS)
        .attr(xml_ncname!("type").to_owned(), "get")
        .attr(xml_ncname!("id").to_owned(), id)
        .append(Element::builder("ping", "urn:xmpp:ping").build())
        .build()
}

async fn recv_element<F: Fn(&Element) -> bool>(
    client: &mut WsXmppClient,
    what: &str,
    accept: F,
) -> Element {
    client
        .recv_matching_within(Duration::from_secs(15), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| accept(&element))
        })
        .await
        .unwrap_or_else(|error| panic!("{what} did not arrive: {error}"))
        .parse()
        .expect("server XML")
}

/// The first bind of an account takes the account's claim for the node that
/// hosts it, a second resource shares the claim, and the claim is released
/// when the last resource closes. The other node can then host the account.
#[tokio::test]
async fn the_first_bind_takes_the_account_claim_and_the_last_unbind_releases_it() {
    let scenario = Scenario::start().await;
    assert_eq!(account_claim(&scenario.db).await, None);

    let desk = scenario.account_on(scenario.server_a(), "desk").await;
    wait_for_binding(&scenario.db, "desk", Some(STATE_ACTIVE)).await;
    let taken = wait_for_claim(&scenario.db, "held by node A", |claim| {
        claim.is_some_and(|(node, _)| node == &scenario.node_a)
    })
    .await;

    let phone = scenario.account_on(scenario.server_a(), "phone").await;
    wait_for_binding(&scenario.db, "phone", Some(STATE_ACTIVE)).await;

    desk.close().await.expect("close the first resource");
    wait_for_binding(&scenario.db, "desk", None).await;
    assert_eq!(
        account_claim(&scenario.db).await,
        taken,
        "the claim stays while the account has a resource"
    );

    phone.close().await.expect("close the last resource");
    wait_for_binding(&scenario.db, "phone", None).await;
    wait_for_claim(&scenario.db, "released", |claim| claim.is_none()).await;

    let _tablet = scenario.account_on(&scenario.server_b, "tablet").await;
    wait_for_binding(&scenario.db, "tablet", Some(STATE_ACTIVE)).await;
    wait_for_claim(&scenario.db, "held by node B", |claim| {
        claim.is_some_and(|(node, _)| node == &scenario.node_b)
    })
    .await;
}

/// A message to the bare JID of an account nobody holds a claim on finds an
/// account without resources: it is stored, the sender gets no error, and
/// the account's next resource receives it. The read takes no claim.
#[tokio::test]
async fn a_message_to_an_account_without_a_claim_is_stored_for_its_next_resource() {
    let scenario = Scenario::start().await;
    let mut sender = scenario.peer_on(scenario.server_a(), "sender").await;
    assert_eq!(account_claim(&scenario.db).await, None);

    send_element(
        &mut sender,
        chat(&account_jid(), "unclaimed-1", "for later"),
    )
    .await;
    // The sender's next round trip orders the test after the message.
    send_element(&mut sender, ping("after-unclaimed-1")).await;
    let reply = recv_element(&mut sender, "the ping result or an error", |element| {
        element.attr("id") == Some("after-unclaimed-1") || element.attr("id") == Some("unclaimed-1")
    })
    .await;
    assert_eq!(
        (reply.name(), reply.attr("id"), reply.attr("type")),
        ("iq", Some("after-unclaimed-1"), Some("result")),
        "the message is not answered with an error: {reply:?}"
    );
    assert_eq!(
        account_claim(&scenario.db).await,
        None,
        "reading an account without a claim takes none"
    );

    let mut desk = scenario.account_on(scenario.server_a(), "desk").await;
    send_element(&mut desk, available()).await;
    let delivered = recv_element(&mut desk, "the stored message", |element| {
        element.is("message", CLIENT_NS) && element.attr("id") == Some("unclaimed-1")
    })
    .await;
    assert_eq!(
        delivered
            .get_child("body", CLIENT_NS)
            .map(Element::text)
            .as_deref(),
        Some("for later")
    );
}

/// A transport loss that leaves a resumable session detaches the binding
/// and keeps the account's claim: the detached binding is still the
/// account's, and its resume commits under the same claim. Closing the
/// resumed stream retires the binding and releases the claim.
#[tokio::test]
async fn a_detached_binding_keeps_the_account_claim_until_it_is_retired() {
    let scenario = Scenario::start().await;
    let mut desk = scenario.account_on(scenario.server_a(), "desk").await;
    send_element(
        &mut desk,
        Element::builder("enable", SM_NS)
            .attr(xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = recv_element(&mut desk, "enabled", |element| element.is("enabled", SM_NS)).await;
    let previd = enabled.attr("id").expect("resume token").to_owned();
    send_element(&mut desk, available()).await;
    wait_for_binding(&scenario.db, "desk", Some(STATE_ACTIVE)).await;
    let taken = wait_for_claim(&scenario.db, "held by node A", |claim| {
        claim.is_some_and(|(node, _)| node == &scenario.node_a)
    })
    .await;

    drop(desk);
    wait_for_binding(&scenario.db, "desk", Some(STATE_DETACHED)).await;
    // The registry entry of the lost connection is removed before the
    // binding detaches; give a release that followed it time to show.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        account_claim(&scenario.db).await,
        taken,
        "a detached binding keeps the account's claim"
    );

    let mut resumed = scenario.authenticated_on(scenario.server_a()).await;
    send_element(
        &mut resumed,
        Element::builder("resume", SM_NS)
            .attr(xml_ncname!("previd").to_owned(), previd)
            .attr(xml_ncname!("h").to_owned(), "0")
            .build(),
    )
    .await;
    recv_element(&mut resumed, "resumed", |element| {
        element.is("resumed", SM_NS)
    })
    .await;
    wait_for_binding(&scenario.db, "desk", Some(STATE_ACTIVE)).await;
    assert_eq!(
        account_claim(&scenario.db).await,
        taken,
        "the resume commits under the claim the session was detached under"
    );

    resumed.close().await.expect("close the resumed stream");
    wait_for_binding(&scenario.db, "desk", None).await;
    wait_for_claim(&scenario.db, "released", |claim| claim.is_none()).await;
}

/// Enable stream management with resumption and announce availability.
async fn enable_resumable(client: &mut WsXmppClient) {
    send_element(
        client,
        Element::builder("enable", SM_NS)
            .attr(xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    recv_element(client, "enabled", |element| element.is("enabled", SM_NS)).await;
    send_element(client, available()).await;
}

/// The owner node dies with the account's bindings on it. The survivor
/// takes the account over on the next bind: the binding of the same full
/// JID is retired and replaced promptly, a binding without a session is
/// retired, and a binding whose session can resume is kept as detached
/// until a bind of its resource replaces it, also promptly.
#[tokio::test]
async fn a_dead_owners_bindings_are_settled_by_the_node_that_takes_the_account_over() {
    let mut scenario = Scenario::start().await;
    let _desk = scenario.account_on(scenario.server_a(), "desk").await;
    let _phone = scenario.account_on(scenario.server_a(), "phone").await;
    let mut tablet = scenario.account_on(scenario.server_a(), "tablet").await;
    enable_resumable(&mut tablet).await;
    for resource in ["desk", "phone", "tablet"] {
        wait_for_binding(&scenario.db, resource, Some(STATE_ACTIVE)).await;
    }
    let before = binding_ref(&scenario.db, "desk").await;
    wait_for_session_reference(&scenario.db, "tablet").await;

    scenario.kill_node_a().await;

    let started = tokio::time::Instant::now();
    let _desk_again = scenario.account_on(&scenario.server_b, "desk").await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the rebind does not wait out the replacement budget: {:?}",
        started.elapsed()
    );
    wait_for_binding(&scenario.db, "desk", Some(STATE_ACTIVE)).await;
    assert_ne!(binding_ref(&scenario.db, "desk").await, before);
    wait_for_claim(&scenario.db, "held by node B", |claim| {
        claim.is_some_and(|(node, _)| node == &scenario.node_b)
    })
    .await;
    wait_for_binding(&scenario.db, "phone", None).await;
    wait_for_binding(&scenario.db, "tablet", Some(STATE_DETACHED)).await;

    // A bind of the detached resource replaces it without waiting: its
    // connection is gone with the dead node.
    let detached = binding_ref(&scenario.db, "tablet").await;
    let started = tokio::time::Instant::now();
    let _tablet_again = scenario.account_on(&scenario.server_b, "tablet").await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the rebind of a detached resource does not wait out the replacement budget: {:?}",
        started.elapsed()
    );
    wait_for_binding(&scenario.db, "tablet", Some(STATE_ACTIVE)).await;
    assert_ne!(binding_ref(&scenario.db, "tablet").await, detached);
}

/// A stanza for an account whose owner node died is not an error: the
/// node that reads the account takes it over, retires the dead node's
/// bindings and stores the stanza. Nothing is left to hold the claim for,
/// so it is released, and the account's next resource gets the stanza.
#[tokio::test]
async fn a_message_to_an_account_whose_owner_died_is_stored_for_its_next_resource() {
    let mut scenario = Scenario::start().await;
    let _desk = scenario.account_on(scenario.server_a(), "desk").await;
    wait_for_binding(&scenario.db, "desk", Some(STATE_ACTIVE)).await;
    let mut sender = scenario.peer_on(&scenario.server_b, "sender").await;

    scenario.kill_node_a().await;

    send_element(&mut sender, chat(&account_jid(), "orphaned-1", "for later")).await;
    send_element(&mut sender, ping("after-orphaned-1")).await;
    let reply = recv_element(&mut sender, "the ping result or an error", |element| {
        element.attr("id") == Some("after-orphaned-1") || element.attr("id") == Some("orphaned-1")
    })
    .await;
    assert_eq!(
        (reply.name(), reply.attr("id"), reply.attr("type")),
        ("iq", Some("after-orphaned-1"), Some("result")),
        "the message is not answered with an error: {reply:?}"
    );
    wait_for_binding(&scenario.db, "desk", None).await;
    wait_for_claim(&scenario.db, "released", |claim| claim.is_none()).await;

    let mut desk = scenario.account_on(&scenario.server_b, "desk").await;
    send_element(&mut desk, available()).await;
    recv_element(&mut desk, "the stored message", |element| {
        element.is("message", CLIENT_NS) && element.attr("id") == Some("orphaned-1")
    })
    .await;
}
