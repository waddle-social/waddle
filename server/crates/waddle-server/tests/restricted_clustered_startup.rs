//! Two clustered servers under the restricted PostgreSQL runtime role.
//!
//! The owner phase prepares a database with a clustering plan. Two real
//! server processes then start under the runtime login, which holds no DDL
//! privilege, each through its own recording relay in front of PostgreSQL,
//! and form a cluster. Clients on both nodes exchange direct messages, share
//! a room owned by one node, and move an XEP-0198 stream from one node to the
//! other. One node then shuts down gracefully while the other keeps serving,
//! takes the room over and serves its archive.
//!
//! Neither relay may record an `insufficient_privilege` error or a statement
//! that changes the catalog, and the catalog must be what the owner phase
//! left.
//!
//! PostgreSQL is optional in local development, so the test skips unless
//! `WADDLE_TEST_POSTGRES_URL` is configured. Publish-gating lanes require
//! PostgreSQL through `WADDLE_TEST_REQUIRE_POSTGRES=1` and fail if it is missing.
#![cfg(all(feature = "test-support", feature = "clustering"))]

#[path = "restricted_runtime_support/mod.rs"]
mod support;

use std::time::{Duration, Instant};

use minidom::Element;
use uuid::Uuid;
use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};
use xmpp_parsers::message::MessageType;

use support::{
    assert_catalog_unchanged, catalog_entries, chat_message, collect_until, contains_text,
    exercise_stream_management, has_descendant, keypair_pool, mam_query, muc_presence, probe,
    provision_clustered, send_element, KeypairPool, Prepared, SocketEndpoint, Wire, DOMAIN, SCHEMA,
};

/// One server process of the cluster and the relay its database
/// connections cross.
struct Node {
    server: TestServer,
    endpoint: SocketEndpoint,
}

fn free_tcp_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local address").port()
}

/// Start one clustered server under the runtime login. The server connects
/// through its own relay, and the operator handoff names that endpoint.
/// Returns once the node has published its node identity, which it does
/// after leasing a keypair slot and registering its node lease.
async fn start_node(
    prepared: &Prepared,
    url: &str,
    keypairs: &KeypairPool,
    swarm_port: u16,
    bootstrap_ports: &[u16],
) -> Node {
    let endpoint = SocketEndpoint::for_url(url).await;
    let runtime_url = format!(
        "postgresql:///{}?host={}&port={}&user={}&options=-c%20search_path%3D{SCHEMA}",
        prepared.database,
        endpoint.directory.path().display(),
        endpoint.port,
        prepared.roles.runtime
    );
    let handoff = prepared.handoff_for(&runtime_url).await;
    let node_id_file =
        std::env::temp_dir().join(format!("waddle-restricted-cluster-node-{}", Uuid::new_v4()));
    let mut env = prepared.runtime_env_for(&runtime_url, &handoff);
    env.extend([
        // Both accounts join rooms on a node that does not own them, which
        // the room registry routes to the owner only for an account allowed
        // to create rooms.
        (
            "WADDLE_SERVER_OWNER_LOCALPARTS",
            "admin,alice,bob".to_owned(),
        ),
        ("WADDLE_CLUSTERING_ENABLED", "true".to_owned()),
        (
            "WADDLE_CLUSTERING_LISTEN_ADDRS",
            format!("/ip4/127.0.0.1/tcp/{swarm_port}"),
        ),
        ("WADDLE_CLUSTERING_KEYPAIR_POOL", keypairs.seeds.join(",")),
        (
            "WADDLE_CLUSTERING_NODE_ID_FILE",
            node_id_file.display().to_string(),
        ),
        // Short intervals so membership, node leases and the orphan reaper
        // settle within the test's budgets.
        ("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS", "1000".to_owned()),
        ("WADDLE_CLUSTERING_DIAL_INTERVAL_MS", "1000".to_owned()),
        ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "1000".to_owned()),
        ("WADDLE_CLUSTERING_LEASE_TTL_MS", "10000".to_owned()),
        (
            "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS",
            "300".to_owned(),
        ),
        ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "1200".to_owned()),
        ("WADDLE_CLUSTERING_ISOLATION_INTERVALS", "2".to_owned()),
        (
            "WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS",
            "200".to_owned(),
        ),
        (
            "WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS",
            "1000".to_owned(),
        ),
        (
            "WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS",
            "1000".to_owned(),
        ),
        (
            "WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS",
            "300".to_owned(),
        ),
        (
            "WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS",
            "500".to_owned(),
        ),
    ]);
    if !bootstrap_ports.is_empty() {
        env.push((
            "WADDLE_CLUSTERING_BOOTSTRAP_PEERS",
            bootstrap_ports
                .iter()
                .map(|port| format!("localhost:{port}"))
                .collect::<Vec<_>>()
                .join(","),
        ));
    }
    let server = tokio::task::spawn_blocking(move || {
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let server = TestServer::start_with_extra_envs(
            &[("alice", "alice-password"), ("bob", "bob-password")],
            &env,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if std::fs::read_to_string(&node_id_file)
                .is_ok_and(|contents| contents.trim().split(' ').count() == 2)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the clustered restricted server did not publish its node identity"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_file(&node_id_file);
        server
    })
    .await
    .expect("clustered restricted server starts");
    Node { server, endpoint }
}

async fn wait_ready(node: &Node) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let url = format!("{}/readyz", node.server.http_base_url());
    while probe(&url).await != reqwest::StatusCode::OK {
        assert!(Instant::now() < deadline, "the node did not report ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn connect(node: &Node, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&node.server.ws_url(), DOMAIN, user, password, resource)
        .await
        .unwrap_or_else(|error| panic!("{user} authenticates and binds: {error}"))
}

fn joined(stanzas: &[Element]) -> bool {
    stanzas.iter().any(|stanza| {
        stanza.name() == "message"
            && has_descendant(stanza, "subject", xmpp_parsers::ns::JABBER_CLIENT)
    })
}

/// Send direct messages until one arrives. The mesh forms after both nodes
/// report ready, so the first messages can precede the route between them.
async fn deliver_direct_message(
    from: &mut WsXmppClient,
    to: &mut WsXmppClient,
    recipient: &str,
    marker: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut attempt = 0;
    loop {
        attempt += 1;
        send_element(
            from,
            chat_message(
                recipient,
                MessageType::Chat,
                &format!("{marker}-{attempt}"),
                &format!("{marker}-{attempt}"),
            ),
        )
        .await;
        let arrival = tokio::time::Instant::now() + Duration::from_secs(2);
        while let Ok(frame) = to
            .recv_timeout(arrival.saturating_duration_since(tokio::time::Instant::now()))
            .await
        {
            if frame
                .parse::<Element>()
                .is_ok_and(|stanza| contains_text(&stanza, marker))
            {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no direct message crossed the cluster to {recipient}"
        );
    }
}

fn assert_wire_clean(wire: &Wire, node: &str) {
    let denied: Vec<_> = wire
        .errors
        .iter()
        .filter(|(sqlstate, _)| sqlstate == "42501")
        .collect();
    assert!(
        denied.is_empty(),
        "{node} was refused a privilege: {denied:#?}"
    );
    assert!(
        wire.catalog_statements.is_empty(),
        "{node} sent catalog-changing statements: {:#?}",
        wire.catalog_statements
    );
}

/// Two servers under the runtime role form a cluster and serve clients on
/// both nodes without DDL and without being refused a privilege.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_restricted_nodes_form_a_cluster_and_serve_without_ddl() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let keypairs = keypair_pool(2);
    let prepared =
        Prepared::from_provisioned(provision_clustered(&url, &keypairs).await, &url).await;
    let before = catalog_entries(&prepared.admin).await;

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let mut node_a = start_node(&prepared, &url, &keypairs, port_a, &[port_b]).await;
    let node_b = start_node(&prepared, &url, &keypairs, port_b, &[port_a]).await;
    wait_ready(&node_a).await;
    wait_ready(&node_b).await;
    // Each start reseeds the fixed administrator account, so the password of
    // the node started last is the one in the shared database.
    let admin_password = node_b.server.fixed_account_password().to_owned();

    // Direct messages in both directions between clients on different nodes.
    let mut alice = connect(&node_a, "alice", "alice-password", "laptop").await;
    let mut bob = connect(&node_b, "bob", "bob-password", "phone").await;
    for client in [&mut alice, &mut bob] {
        send_element(
            client,
            Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT).build(),
        )
        .await;
    }
    let to_bob = format!("to-bob-{}", Uuid::new_v4());
    deliver_direct_message(&mut alice, &mut bob, "bob@localhost", &to_bob).await;
    let to_alice = format!("to-alice-{}", Uuid::new_v4());
    deliver_direct_message(&mut bob, &mut alice, "alice@localhost", &to_alice).await;

    // A room created on node A, joined and written to from node B: the join
    // and the message are routed to the owning node, and the reflection and
    // the archive come back through it.
    let mut admin = connect(&node_a, "admin", &admin_password, "console").await;
    let room = format!(
        "restricted-cluster-{}@muc.localhost",
        Uuid::new_v4().simple()
    );
    send_element(&mut admin, muc_presence(&format!("{room}/owner"), false)).await;
    collect_until(&mut admin, "room creation on node A", joined).await;
    send_element(&mut bob, muc_presence(&format!("{room}/bob"), false)).await;
    collect_until(&mut bob, "room join from node B", joined).await;

    let from_owner = format!("room-from-owner-{}", Uuid::new_v4());
    send_element(
        &mut admin,
        chat_message(&room, MessageType::Groupchat, "from-owner", &from_owner),
    )
    .await;
    collect_until(&mut bob, "room message on node B", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| contains_text(stanza, &from_owner))
    })
    .await;
    let from_bob = format!("room-from-bob-{}", Uuid::new_v4());
    send_element(
        &mut bob,
        chat_message(&room, MessageType::Groupchat, "from-bob", &from_bob),
    )
    .await;
    collect_until(&mut bob, "reflection on node B", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| contains_text(stanza, &from_bob))
    })
    .await;
    collect_until(&mut admin, "room message on node A", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| contains_text(stanza, &from_bob))
    })
    .await;
    // An instant room's archive is served only where its room actor is
    // live, so it is read on the owning node here and on node B once node B
    // owns the room.
    let archived = mam_query(&mut admin, "room-archive", Some(&room)).await;
    for body in [&from_owner, &from_bob] {
        assert!(
            archived.iter().any(|stanza| contains_text(stanza, body)),
            "the room archive on the owning node must hold {body}: {archived:?}"
        );
    }

    // A stream enabled on node A resumes on node B.
    exercise_stream_management(&node_a.server, &node_b.server).await;

    // Node A shuts down gracefully. Node B keeps serving, and the room is
    // served from node B once node A has released it.
    drop(admin);
    drop(alice);
    node_a.server.send_sigterm();
    assert!(
        node_a.server.wait_for_exit(Duration::from_secs(60)).await,
        "node A did not exit after SIGTERM"
    );
    wait_ready(&node_b).await;
    let mut alice = connect(&node_b, "alice", "alice-password", "desktop").await;
    send_element(
        &mut alice,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT).build(),
    )
    .await;
    let after_shutdown = format!("after-shutdown-{}", Uuid::new_v4());
    deliver_direct_message(&mut bob, &mut alice, "alice@localhost", &after_shutdown).await;
    send_element(&mut alice, muc_presence(&format!("{room}/alice"), false)).await;
    collect_until(&mut alice, "room join after node A left", joined).await;
    let from_alice = format!("room-from-alice-{}", Uuid::new_v4());
    send_element(
        &mut alice,
        chat_message(&room, MessageType::Groupchat, "from-alice", &from_alice),
    )
    .await;
    collect_until(&mut alice, "reflection after node A left", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| contains_text(stanza, &from_alice))
    })
    .await;
    let archived = mam_query(&mut alice, "room-archive-after", Some(&room)).await;
    for body in [&from_owner, &from_bob, &from_alice] {
        assert!(
            archived.iter().any(|stanza| contains_text(stanza, body)),
            "the room archive on the surviving node must hold {body}: {archived:?}"
        );
    }
    drop(alice);
    drop(bob);

    assert_wire_clean(&node_a.endpoint.take_wire(), "node A");
    assert_wire_clean(&node_b.endpoint.take_wire(), "node B");
    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "clustered restricted serving changed the catalog",
    )
    .await;
    drop(node_b);
    drop(node_a);

    // The database the cluster served on is still one the owner phase
    // accepts.
    prepared
        .rerun_owner_phase()
        .await
        .expect("owner phase reruns after clustered serving");
    prepared.cleanup().await;
}
