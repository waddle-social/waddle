//! Two-node suite for delivery to a user whose account another node owns,
//! carried as direct requests in the durable mailbox (#2173).
//!
//! Node A hosts the sender and node B hosts the recipient and holds the
//! recipient account's `UserActor` claim. Both select the mailbox and
//! durable stream management, and the shared database is at the mailbox
//! protocol epoch, so a chat message from A to a resource on B travels as a
//! `Direct` entry: the origin commits the request, the account owner hands
//! the stanza to the recipient's session path and decides, and the origin
//! consumes the decision.
//!
//! The owner-side seams of `clustering::acceptance_seams` stop the account
//! owner before its hand-over and decision.

#![cfg(all(feature = "clustering", feature = "test-support"))]

#[path = "cluster_support/mod.rs"]
pub mod cluster_support;

use std::path::PathBuf;
use std::time::Duration;

use cluster_support::{
    cluster_e2e_serial_lock, free_tcp_port, generate_pool, open_control_db, reset_and_enroll,
    reset_node_lease_tables, spawn_cluster_server_with_envs, wait_for_cross_node_registration,
    wait_for_readiness, EnrolledPool, CLUSTER_PEER_PASSWORD, CLUSTER_PEER_USERNAME,
};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::db::Database;
use waddle_ws_test_support::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const RECIPIENT: &str = "admin";
const SEAM_DIRECTORY_ENV: &str = "WADDLE_TEST_SEAM_REMOTE_ACCEPTANCE_DIRECTORY";
const ACCEPTANCE_ENV: &str = "WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE";
const DURABLE_SM_ENV: &str = "WADDLE_XMPP_DURABLE_SM";
const HOLD_BEFORE_COMMIT: &str = "hold-before-commit";

// Each query takes one parameter, a `LIKE` pattern for the recipient full
// JIDs of one scenario: the ingress relations are shared by every scenario
// of the database and cannot be emptied between them.
const REQUESTS: &str = "SELECT count(*) FROM ingress_direct_requests WHERE recipient_jid LIKE ?";
const SETTLED_ACCEPTED: &str = "SELECT count(*) FROM ingress_direct_requests \
     WHERE recipient_jid LIKE ? AND owner_decision = 1 AND settled_at IS NOT NULL";
const SETTLED_REJECTED: &str = "SELECT count(*) FROM ingress_direct_requests \
     WHERE recipient_jid LIKE ? AND owner_decision = 2 AND refusal = 1 AND settled_at IS NOT NULL";
const UNDECIDED: &str = "SELECT count(*) FROM ingress_direct_requests \
     WHERE recipient_jid LIKE ? AND owner_decision = 0";
const CONSUMED_ENTRIES: &str = "SELECT count(*) FROM ingress_mailbox_entries e \
     JOIN ingress_direct_requests r ON r.request_key = e.direct_key \
     WHERE r.recipient_jid LIKE ? AND e.operation_kind = 6 AND e.disposition >= 3";
const LANES: &str = "SELECT count(DISTINCT e.lane_id) FROM ingress_mailbox_entries e \
     JOIN ingress_direct_requests r ON r.request_key = e.direct_key \
     WHERE r.recipient_jid LIKE ? AND e.operation_kind = 6";

/// Advance the shared database to the mailbox protocol epoch.
async fn activate_mailbox_epoch(db: &Database) {
    let conn = db.guard().await.expect("epoch guard");
    for _ in 0..2 {
        conn.execute(
            "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = now(), \
             lineage_uuid = (SELECT lineage_uuid::uuid FROM _lineage WHERE id = 1) \
             WHERE id = 1 AND epoch < 2",
            (),
        )
        .await
        .expect("activate the mailbox epoch");
    }
}

async fn scalar(db: &Database, recipients: &str, sql: &str) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(sql, waddle_server::db_params![recipients.to_owned()])
        .await
        .expect("scalar query");
    rows.next()
        .await
        .expect("scalar row")
        .expect("one row")
        .get::<i64>(0)
        .expect("scalar value")
}

async fn wait_for_scalar(
    db: &Database,
    recipients: &str,
    sql: &str,
    expected: i64,
    budget: Duration,
) {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let value = scalar(db, recipients, sql).await;
        if value == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{sql} stayed at {value}, expected {expected}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn chat(to: &str, id: &str, body: &str) -> Element {
    Element::builder("message", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(xml_ncname!("to").to_owned(), to)
        .attr(xml_ncname!("type").to_owned(), "chat")
        .attr(xml_ncname!("id").to_owned(), id)
        .append(
            Element::builder("body", waddle_xmpp::ns::JABBER_CLIENT)
                .append(body)
                .build(),
        )
        .build()
}

fn bodies(frames: &[String]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| frame.parse::<Element>().ok())
        .filter(|frame| frame.is("message", waddle_xmpp::ns::JABBER_CLIENT))
        .filter_map(|frame| {
            frame
                .get_child("body", waddle_xmpp::ns::JABBER_CLIENT)
                .map(Element::text)
        })
        .collect()
}

async fn drain(client: &mut WsXmppClient, budget: Duration) -> Vec<String> {
    let mut frames = Vec::new();
    while let Ok(frame) = client.recv_timeout(budget).await {
        frames.push(frame);
    }
    frames
}

/// Two meshed nodes at the mailbox epoch with durable stream management.
struct Scenario {
    db: Database,
    server_a: TestServer,
    server_b: Option<TestServer>,
    seams: PathBuf,
    /// Suffix of every recipient resource this scenario binds or addresses.
    tag: String,
    _pool: EnrolledPool,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Scenario {
    async fn start() -> Self {
        let postgres_url = waddle_ws_test_support::require_test_postgres_url()
            .expect("the direct lane suite needs PostgreSQL: set WADDLE_TEST_POSTGRES_URL");
        let serial = cluster_e2e_serial_lock().lock().await;
        let db = open_control_db(&postgres_url).await;
        let pool = generate_pool();
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;
        let seams =
            std::env::temp_dir().join(format!("waddle-direct-seams-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&seams).expect("create seam directory");
        let seam_directory = seams.display().to_string();
        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let (server_a, _, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_a,
            &[port_b],
            &[(ACCEPTANCE_ENV, "mailbox"), (DURABLE_SM_ENV, "durable")],
        )
        .await;
        let (server_b, _, _) = spawn_cluster_server_with_envs(
            &postgres_url,
            &pool.pool_env,
            port_b,
            &[port_a],
            &[
                (SEAM_DIRECTORY_ENV, seam_directory.as_str()),
                (ACCEPTANCE_ENV, "mailbox"),
                (DURABLE_SM_ENV, "durable"),
            ],
        )
        .await;
        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
        // The origin carries a stanza as a direct request only for a
        // resource it knows another node hosts, which needs the mesh.
        wait_for_cross_node_registration(&server_a, &server_b).await;
        activate_mailbox_epoch(&db).await;
        Self {
            db,
            server_a,
            server_b: Some(server_b),
            seams,
            tag: uuid::Uuid::new_v4().simple().to_string(),
            _pool: pool,
            _serial: serial,
        }
    }

    async fn sender(&self) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &self.server_a.ws_url(),
            DOMAIN,
            CLUSTER_PEER_USERNAME,
            CLUSTER_PEER_PASSWORD,
            &format!("sender-{}", uuid::Uuid::new_v4()),
        )
        .await
        .expect("sender binds on node A")
    }

    /// A resource of the recipient account bound on `server`.
    ///
    /// Every node seeds the shared recipient account with its own generated
    /// password when it starts, so the stored credential is the one of the
    /// node that started last, node B.
    async fn recipient_on(&self, server: &TestServer, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &server.ws_url(),
            DOMAIN,
            RECIPIENT,
            self.server_b().fixed_account_password(),
            &self.resource(resource),
        )
        .await
        .expect("recipient binds")
    }

    /// The resource name `name` stands for in this scenario.
    fn resource(&self, name: &str) -> String {
        format!("{name}-{}", self.tag)
    }

    /// The full JID of the recipient's resource `name`.
    fn recipient(&self, name: &str) -> String {
        format!("{RECIPIENT}@{DOMAIN}/{}", self.resource(name))
    }

    /// `LIKE` pattern matching the recipient full JIDs of this scenario.
    fn recipients(&self) -> String {
        format!("%-{}", self.tag)
    }

    fn server_b(&self) -> &TestServer {
        self.server_b.as_ref().expect("node B")
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.seams);
    }
}

/// A chat message to a full JID bound on the other node is delivered once,
/// and its request is accepted, consumed and settled.
#[tokio::test]
async fn message_to_a_remote_full_jid_is_delivered_once() {
    let scenario = Scenario::start().await;
    let mut recipient = scenario.recipient_on(scenario.server_b(), "desk").await;
    let mut sender = scenario.sender().await;
    sender
        .send(&String::from(&chat(
            &scenario.recipient("desk"),
            "direct-1",
            "to the desk",
        )))
        .await
        .expect("send chat");
    let received = tokio::time::timeout(
        Duration::from_secs(30),
        recipient.recv_until(|frame| frame.contains("to the desk")),
    )
    .await
    .expect("bounded delivery")
    .expect("delivery");
    let mut frames = received;
    frames.extend(drain(&mut recipient, Duration::from_secs(2)).await);
    assert_eq!(
        bodies(&frames)
            .iter()
            .filter(|body| body.as_str() == "to the desk")
            .count(),
        1,
        "the copy is delivered exactly once: {frames:?}"
    );
    wait_for_scalar(
        &scenario.db,
        &scenario.recipients(),
        SETTLED_ACCEPTED,
        1,
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(
        scalar(&scenario.db, &scenario.recipients(), REQUESTS).await,
        1
    );
    assert_eq!(
        scalar(&scenario.db, &scenario.recipients(), CONSUMED_ENTRIES).await,
        1
    );
}

/// Messages from one sender to one remote resource arrive in the order
/// they were sent: they share a lane and its sequence.
#[tokio::test]
async fn messages_on_one_lane_arrive_in_order() {
    let scenario = Scenario::start().await;
    let mut recipient = scenario.recipient_on(scenario.server_b(), "desk").await;
    let mut sender = scenario.sender().await;
    for index in 0..5 {
        sender
            .send(&String::from(&chat(
                &scenario.recipient("desk"),
                &format!("ordered-{index}"),
                &format!("ordered {index}"),
            )))
            .await
            .expect("send chat");
    }
    let frames = tokio::time::timeout(
        Duration::from_secs(30),
        recipient.recv_until(|frame| frame.contains("ordered 4")),
    )
    .await
    .expect("bounded delivery")
    .expect("delivery");
    let ordered: Vec<String> = bodies(&frames)
        .into_iter()
        .filter(|body| body.starts_with("ordered "))
        .collect();
    assert_eq!(
        ordered,
        (0..5)
            .map(|index| format!("ordered {index}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(scalar(&scenario.db, &scenario.recipients(), LANES).await, 1);
}

/// A chat message to a bare JID whose account has a resource on each node
/// reaches both resources once.
#[tokio::test]
async fn message_to_a_bare_jid_reaches_resources_on_both_nodes_once() {
    let scenario = Scenario::start().await;
    // The first bind takes the account's claim, so node B owns the account.
    let mut remote = scenario.recipient_on(scenario.server_b(), "desk").await;
    let mut local = scenario.recipient_on(&scenario.server_a, "phone").await;
    let mut sender = scenario.sender().await;
    sender
        .send(&String::from(&chat(
            &format!("{RECIPIENT}@{DOMAIN}"),
            "bare-1",
            "to the account",
        )))
        .await
        .expect("send chat");
    for client in [&mut remote, &mut local] {
        let mut frames = tokio::time::timeout(
            Duration::from_secs(30),
            client.recv_until(|frame| frame.contains("to the account")),
        )
        .await
        .expect("bounded delivery")
        .expect("delivery");
        frames.extend(drain(client, Duration::from_secs(2)).await);
        assert_eq!(
            bodies(&frames)
                .iter()
                .filter(|body| body.as_str() == "to the account")
                .count(),
            1,
            "each resource receives the message once: {frames:?}"
        );
    }
}

/// A full JID that is no longer bound when the account owner works its
/// request is rejected by the owner, the request records that nothing took
/// the stanza, and no entry stays pending.
///
/// The resource is bound when the origin commits the request and ends its
/// connection while the owner is held before its hand-over: a chat message
/// to a full JID that is already unbound is never a request for that JID,
/// because the origin routes it to the account's available resources
/// (RFC 6121 section 8.5.3.2.1).
#[tokio::test]
async fn absent_resource_is_rejected_and_left_to_the_origins_fallback() {
    let scenario = Scenario::start().await;
    // Keeps the account's claim on node B.
    let _present = scenario.recipient_on(scenario.server_b(), "desk").await;
    let leaving = scenario.recipient_on(scenario.server_b(), "absent").await;
    std::fs::write(scenario.seams.join(HOLD_BEFORE_COMMIT), b"").expect("arm seam");
    let mut sender = scenario.sender().await;
    sender
        .send(&String::from(&chat(
            &scenario.recipient("absent"),
            "absent-1",
            "to nobody",
        )))
        .await
        .expect("send chat");
    wait_for_scalar(
        &scenario.db,
        &scenario.recipients(),
        REQUESTS,
        1,
        Duration::from_secs(20),
    )
    .await;
    leaving
        .close()
        .await
        .expect("the addressed resource leaves");
    std::fs::remove_file(scenario.seams.join(HOLD_BEFORE_COMMIT)).expect("release seam");
    wait_for_scalar(
        &scenario.db,
        &scenario.recipients(),
        UNDECIDED,
        0,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        scalar(&scenario.db, &scenario.recipients(), SETTLED_ACCEPTED).await,
        0
    );
    let _ = drain(&mut sender, Duration::from_secs(2)).await;
    assert_eq!(
        scalar(&scenario.db, &scenario.recipients(), SETTLED_REJECTED).await,
        scalar(&scenario.db, &scenario.recipients(), REQUESTS).await,
        "every request for the absent resource is rejected as not delivered"
    );
}

/// The account owner is killed while it holds a request before its
/// hand-over and decision. The request stays undecided, the origin sends
/// the stanza no other way, and the recipient receives nothing twice.
#[tokio::test]
async fn owner_killed_before_its_decision_leaves_one_undecided_request() {
    let mut scenario = Scenario::start().await;
    let mut recipient = scenario.recipient_on(scenario.server_b(), "desk").await;
    std::fs::write(scenario.seams.join(HOLD_BEFORE_COMMIT), b"").expect("arm seam");
    let mut sender = scenario.sender().await;
    sender
        .send(&String::from(&chat(
            &scenario.recipient("desk"),
            "held-1",
            "held at the owner",
        )))
        .await
        .expect("send chat");
    wait_for_scalar(
        &scenario.db,
        &scenario.recipients(),
        REQUESTS,
        1,
        Duration::from_secs(20),
    )
    .await;
    let before = drain(&mut recipient, Duration::from_secs(2)).await;
    assert!(
        !bodies(&before)
            .iter()
            .any(|body| body == "held at the owner"),
        "nothing is handed over while the owner is held: {before:?}"
    );
    drop(recipient);
    drop(scenario.server_b.take());
    // The origin's wait ends without a decision; it reports the delivery as
    // uncertain and commits nothing further for the stanza.
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert_eq!(
        scalar(&scenario.db, &scenario.recipients(), REQUESTS).await,
        1
    );
    assert_eq!(
        scalar(&scenario.db, &scenario.recipients(), UNDECIDED).await,
        1
    );
}
