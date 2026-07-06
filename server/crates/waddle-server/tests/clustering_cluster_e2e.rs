//! ADR-0017 Phase 2 Slice 6 — multi-process cluster harness.
//!
//! kameo's `init_global` is a process singleton, so multi-node behaviour is
//! only testable across processes: this harness spawns two real
//! `waddle-server` processes (clustering enabled, shared Postgres control
//! plane) and joins the swarm from the test process as a third node, then
//! asserts the Phase 2 spike exit criteria over the real network:
//!
//! - cross-node relay ask round-trip (ping) and the bounded XML codec on the
//!   wire (thread-preserving stanza echo);
//! - integrity under concurrent large + small payloads (libp2p per-substream
//!   flow control interleaves them; per-pair *sequencing* is Phase 4);
//! - the receiver-applied ask budgets fail a slow handler with the typed
//!   `ReplyTimeout` classification inside the reply budget, and — with those
//!   budgets deliberately inflated past both the handler and the transport
//!   cap, so the receiver cannot proactively reply in time — the sender's
//!   libp2p `request_timeout` fails the ask with the typed `Transport`
//!   classification at ≈ the cap, proving the transport cap is the binding
//!   bound;
//! - relay crash → supervised respawn + same-name re-registration, recovered
//!   by the sender's stale-ref kademlia re-lookup;
//! - revoked-peer-with-live-connection: containment within the allowlist
//!   refresh interval, and recovery after re-enrollment;
//! - node churn: a sequential rolling restart of BOTH original bootstrap
//!   peers (at most one node down at any instant) — each replacement (fresh
//!   node_id + relay name) is re-discovered via kademlia, and both
//!   replacements stay reachable once no original peer survives.
//!
//! Gated on the `clustering` feature and `WADDLE_TEST_POSTGRES_URL` (skips
//! cleanly otherwise). Deferred to Phase 3 (needs heartbeat fencing + the
//! durable queue): lone-survivor at N=2 keeps serving; a single dead link of
//! three degrades to the durable fallback without fencing either endpoint.
//! Deferred as a manual go/no-go measurement (dominated by kademlia's
//! hardcoded 1h record TTL): the dead publisher's record-visibility window.

#![cfg(feature = "clustering")]

use base64::Engine;
use libp2p::identity::ed25519;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use waddle_server::clustering::allowlist::{AllowlistStore, PostgresAllowlistStore};
use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::lease::{KeypairSlotLease, PostgresKeypairSlotLease};
use waddle_server::clustering::relay::{RelayAskError, RelayHandle, RelaySendFailure};
use waddle_server::clustering::swarm;
use waddle_server::clustering::NodeId;
use waddle_server::config::{ClusteringBootstrapConfig, ClusteringConfig, ClusteringLeaseConfig};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_ws_test_support::TestServer;

const POOL_SIZE: usize = 4;

struct EnrolledPool {
    /// base64-encoded 32-byte ed25519 seeds (the WADDLE_CLUSTERING_KEYPAIR_POOL value).
    pool_env: String,
    /// PeerIds derived from every pool slot (all enrolled).
    peer_ids: Vec<libp2p::PeerId>,
}

fn generate_pool() -> EnrolledPool {
    let mut seeds = Vec::new();
    let mut peer_ids = Vec::new();
    for _ in 0..POOL_SIZE {
        let keypair = ed25519::Keypair::generate();
        let seed = keypair.secret().as_ref().to_vec();
        peer_ids.push(
            libp2p::identity::Keypair::from(keypair)
                .public()
                .to_peer_id(),
        );
        seeds.push(base64::engine::general_purpose::STANDARD.encode(seed));
    }
    EnrolledPool {
        pool_env: seeds.join(","),
        peer_ids,
    }
}

async fn open_control_db(url: &str) -> Database {
    // The test-process node (C) leases a keypair-pool slot and heartbeats it
    // exactly like a production pod, so it needs the same dedicated
    // control-plane pool `swarm::spawn`'s `run_heartbeat` renews through
    // (ADR-0017 element 4/12, Phase 3 Slice 0) — otherwise every renewal
    // would error and self-fence this node almost immediately.
    Database::from_config(
        "clustering-e2e-harness",
        &DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string())
            .with_control_plane_pool(waddle_server::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("open harness postgres")
}

/// Reset the clustering control-plane tables and enroll every pool PeerId.
async fn reset_and_enroll(db: &Database, pool: &EnrolledPool) {
    // Provision the schema through the production `ensure_schema` path — an
    // inline DDL copy here could silently diverge as the schema evolves.
    PostgresKeypairSlotLease::new(db.clone())
        .ensure_schema()
        .await
        .expect("lease schema");
    PostgresAllowlistStore::new(db.clone())
        .ensure_schema()
        .await
        .expect("allowlist schema");
    let conn = db.guard().await.expect("guard");
    conn.execute("DELETE FROM clustering_keypair_slots", ())
        .await
        .expect("clean slots");
    conn.execute("DELETE FROM clustering_peer_allowlist", ())
        .await
        .expect("clean allowlist");
    for peer in &pool.peer_ids {
        conn.execute(
            "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
            waddle_server::db_params![peer.to_string()],
        )
        .await
        .expect("enroll peer");
    }
}

/// Reserve an ephemeral TCP port by binding and immediately releasing it.
///
/// Inherently best-effort (bind/release TOCTOU): another process could grab
/// the port before the spawned server binds it. The window is unavoidable
/// here — the mesh needs pre-agreed ports (A's seed list names B's port and
/// vice versa), so neither server can pick its own port at bind time. The
/// kernel avoids immediately reissuing a just-released ephemeral port, and a
/// lost race fails loudly (the server's swarm listen bind errors and the
/// node-id file never appears) rather than corrupting the test.
fn free_tcp_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// Spawn a clustering-enabled waddle-server on a fixed swarm port, wired to
/// the shared Postgres and seeded with one loopback bootstrap entry per
/// `bootstrap_ports` element (the harness expression of the headless
/// Service resolving to every pod). Returns the server handle plus its
/// published (node_id, peer_id).
///
/// `TestServer::start_with_extra_envs` and the node-id-file poll both block
/// (process spawn + sleep polling), so the whole bring-up runs on the tokio
/// blocking pool instead of stalling a worker thread.
async fn spawn_cluster_server(
    postgres_url: &str,
    pool_env: &str,
    swarm_port: u16,
    bootstrap_ports: &[u16],
) -> (TestServer, String, String) {
    let postgres_url = postgres_url.to_string();
    let pool_env = pool_env.to_string();
    let bootstrap_ports = bootstrap_ports.to_vec();
    tokio::task::spawn_blocking(move || {
        let node_id_file = std::env::temp_dir().join(format!(
            "waddle-clustering-e2e-node-{}",
            uuid::Uuid::new_v4()
        ));
        let listen = format!("/ip4/127.0.0.1/tcp/{swarm_port}");
        let node_id_file_str = node_id_file.display().to_string();
        let bootstrap_peers = bootstrap_ports
            .iter()
            .map(|port| format!("localhost:{port}"))
            .collect::<Vec<_>>()
            .join(",");

        let mut envs: Vec<(&str, &str)> = vec![
            ("WADDLE_DB_DRIVER", "postgres"),
            ("WADDLE_DATABASE_URL", &postgres_url),
            // NB: the fixed test account stays enabled (TestServer's default) —
            // disabling it flips the permission backend onto the SpiceDB path and
            // fails startup. Its seeding is delete-then-recreate, safe for the
            // sequential startups this harness performs.
            ("WADDLE_CLUSTERING_ENABLED", "true"),
            ("WADDLE_CLUSTERING_LISTEN_ADDRS", &listen),
            ("WADDLE_CLUSTERING_KEYPAIR_POOL", &pool_env),
            ("WADDLE_CLUSTERING_NODE_ID_FILE", &node_id_file_str),
            ("WADDLE_CLUSTERING_FAULT_INJECTION", "true"),
            // Tight intervals so revocation/re-dial assertions run fast.
            ("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS", "1000"),
            ("WADDLE_CLUSTERING_DIAL_INTERVAL_MS", "1000"),
            ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "1000"),
            ("WADDLE_CLUSTERING_LEASE_TTL_MS", "10000"),
        ];
        if !bootstrap_peers.is_empty() {
            envs.push(("WADDLE_CLUSTERING_BOOTSTRAP_PEERS", &bootstrap_peers));
        }

        let server = TestServer::start_with_extra_envs(&[], &envs);

        // The node-id file is written during clustering bring-up, which
        // precedes HTTP readiness — but poll defensively anyway.
        let deadline = Instant::now() + Duration::from_secs(30);
        let contents = loop {
            if let Ok(contents) = std::fs::read_to_string(&node_id_file) {
                if contents.trim().split(' ').count() == 2 {
                    break contents;
                }
            }
            assert!(
                Instant::now() < deadline,
                "server did not publish its node-id file"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let mut parts = contents.trim().split(' ');
        let node_id = parts.next().expect("node id").to_string();
        let peer_id = parts.next().expect("peer id").to_string();
        let _ = std::fs::remove_file(&node_id_file);
        (server, node_id, peer_id)
    })
    .await
    .unwrap_or_else(|join_error| match join_error.try_into_panic() {
        // Re-raise the inner panic so assertion messages surface verbatim.
        Ok(payload) => std::panic::resume_unwind(payload),
        Err(join_error) => panic!("cluster server bring-up task failed: {join_error}"),
    })
}

/// Ping a relay with retries until `deadline` (discovery through kademlia can
/// take a few dial/refresh rounds).
async fn ping_until(
    handle: &mut RelayHandle,
    expected_node: &str,
    deadline: Duration,
) -> Result<(), String> {
    let until = Instant::now() + deadline;
    let mut last_error = String::new();
    while Instant::now() < until {
        match handle.ping().await {
            Ok(pong) if pong.node_id.as_str() == expected_node => return Ok(()),
            Ok(pong) => return Err(format!("wrong node answered: {}", pong.node_id)),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!("relay ping never succeeded: {last_error}"))
}

fn thread_message(id: &str, body_size: usize) -> RemoteStanza {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.thread = Some(xmpp_parsers::message::Thread {
        id: id.to_string(),
        parent: None,
    });
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "x".repeat(body_size));
    RemoteStanza(waddle_xmpp::Stanza::Message(message))
}

/// The whole Phase 2 exit-criteria suite runs as ONE test: the test process
/// hosts a single swarm (kameo `init_global` is a process singleton), and the
/// scenario steps build on each other.
#[tokio::test(flavor = "multi_thread")]
async fn cluster_exit_criteria_end_to_end() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };

    // Surface the test-process swarm's own tracing (lookups, connections,
    // relay registration) when debugging with --nocapture.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("waddle_server::clustering=debug,libp2p_kad=debug")
        .with_test_writer()
        .try_init();

    let pool = generate_pool();
    let db = open_control_db(&postgres_url).await;
    reset_and_enroll(&db, &pool).await;

    // --- Bring up the mesh: A (seed), B (bootstraps to A). Sequential so
    // concurrent first-boot migrations cannot race on the shared database.
    // Production shape: the headless Service resolves to every pod, so every
    // node dials every peer directly. The harness expresses that as one
    // loopback seed per node.
    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (mut server_a, node_a, peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

    // --- The test process joins as node C.
    let stop = CancellationToken::new();
    let config = ClusteringConfig {
        enabled: true,
        listen_addrs: vec!["/ip4/127.0.0.1/tcp/0".to_string()],
        bootstrap_peers: vec![
            ClusteringBootstrapConfig {
                dns_name: "localhost".to_string(),
                port: port_a,
            },
            ClusteringBootstrapConfig {
                dns_name: "localhost".to_string(),
                port: port_b,
            },
        ],
        keypair_pool: pool.pool_env.split(',').map(str::to_string).collect(),
        lease: ClusteringLeaseConfig {
            heartbeat_interval: Duration::from_secs(1),
            lease_ttl: Duration::from_secs(10),
        },
        allowlist_refresh_interval: Duration::from_secs(1),
        dial_interval: Duration::from_secs(1),
        // The binding transport cap for the timeout criterion below. This
        // literal bypasses env parsing, so it must uphold the same element-5
        // invariant the parser enforces: mailbox + reply <= request
        // (0.5s + 1.5s <= 2s).
        messaging: waddle_server::config::ClusteringMessagingConfig {
            request_timeout: Duration::from_secs(2),
            reply_timeout: Duration::from_millis(1_500),
            mailbox_timeout: Duration::from_millis(500),
            ..waddle_server::config::ClusteringMessagingConfig::default()
        },
        fault_injection: false,
        node_id_file: None,
    };
    let handle = swarm::spawn(&config, &db, stop.clone())
        .await
        .expect("test-process swarm joins the mesh");
    assert!(!handle.node_id.as_str().is_empty());

    // --- Exit criterion: cross-node ask round-trip (real network, two other
    // processes), including discovery of B through the mesh (the test only
    // bootstraps toward A).
    // Exercise the configured receiver-side ask timeouts (ADR element 5)
    // through the handle's wiring, not just config validation.
    let mut relay_a = RelayHandle::new(NodeId::new(node_a.clone())).with_ask_timeouts(
        config.messaging.mailbox_timeout,
        config.messaging.reply_timeout,
    );
    ping_until(&mut relay_a, &node_a, Duration::from_secs(30))
        .await
        .expect("cross-node ping to A");
    let mut relay_b = RelayHandle::new(NodeId::new(node_b.clone()));
    ping_until(&mut relay_b, &node_b, Duration::from_secs(30))
        .await
        .expect("cross-node ping to B");

    // --- Exit criterion: the bounded XML codec on the wire — a
    // thread-carrying stanza survives the round-trip to another process.
    let echoed = relay_a
        .echo_stanza(thread_message("e2e-thread", 64))
        .await
        .expect("cross-node stanza echo");
    match echoed.stanza.0 {
        waddle_xmpp::Stanza::Message(message) => {
            assert_eq!(message.thread.expect("thread survives").id, "e2e-thread");
        }
        other => panic!("expected message, got {}", other.name()),
    }

    // --- Exit criterion: integrity under concurrent large + small payloads.
    // (Per-(origin→recipient) sequencing is Phase 4; Phase 2 asserts that
    // interleaved large/small asks each come back intact.)
    let mut join_set = tokio::task::JoinSet::new();
    for index in 0..8u32 {
        let node = node_a.clone();
        let size = if index % 2 == 0 { 100 * 1024 } else { 16 };
        let thread_id = format!("mix-{index}");
        join_set.spawn(async move {
            let mut relay = RelayHandle::new(NodeId::new(node));
            let reply = relay
                .echo_stanza(thread_message(&thread_id, size))
                .await
                .expect("concurrent echo");
            match reply.stanza.0 {
                waddle_xmpp::Stanza::Message(message) => {
                    assert_eq!(message.thread.expect("thread").id, thread_id);
                    let body_len = message.bodies.values().next().map(String::len).unwrap_or(0);
                    assert_eq!(body_len, size);
                }
                other => panic!("expected message, got {}", other.name()),
            }
        });
    }
    while let Some(result) = join_set.join_next().await {
        result.expect("concurrent echo task");
    }

    // --- Exit criterion (part 1): the receiver-applied ask budgets. A 5s
    // handler exceeds relay_a's receiver-side reply budget (1.5s): the
    // receiver's local ask times out first and sends a ReplyTimeout error
    // frame back over the wire — the sender observes the typed ReplyTimeout
    // classification within the reply budget, never by waiting out the
    // handler. (With the validated config invariant mailbox + reply <=
    // request, the receiver ALWAYS replies before the transport cap, so this
    // path can never exercise `request_timeout` — that is part 2 below.)
    let started = Instant::now();
    let result = relay_a.sleep(5_000).await;
    let elapsed = started.elapsed();
    let error = match result {
        Ok(_) => panic!("5s receiver handler must fail the ask, but it succeeded"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            RelayAskError::Send {
                failure: RelaySendFailure::ReplyTimeout,
                ..
            }
        ),
        "expected the typed ReplyTimeout classification, got: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "ReplyTimeout must fire within the ~1.5s reply budget, not after the 5s handler (took {elapsed:?})"
    );

    // --- Exit criterion (part 2): the transport cap is the binding bound.
    // A dedicated handle grants the receiver ask budgets (10s mailbox/reply)
    // above BOTH the 5s handler and the sender's 2s transport
    // `request_timeout`, so the receiver cannot proactively reply — neither a
    // ReplyTimeout frame (budget outlives the handler) nor the success reply
    // (handler outlives the transport window) can arrive in time. The only
    // bound that can fire is the sender-side libp2p request_response
    // `request_timeout`, surfacing as the typed Transport classification at
    // ≈ the cap. Production config validation forbids this inversion
    // (mailbox + reply <= request), which is exactly why part 1 alone could
    // never prove the transport cap; the per-ask builder lets the harness
    // construct it deliberately. Aimed at B so A's relay is idle for the
    // crash scenario next.
    let mut relay_b_slow = RelayHandle::new(NodeId::new(node_b.clone()))
        .with_ask_timeouts(Duration::from_secs(10), Duration::from_secs(10));
    ping_until(&mut relay_b_slow, &node_b, Duration::from_secs(30))
        .await
        .expect("warm-up ping resolves B before timing the transport cap");
    let started = Instant::now();
    let result = relay_b_slow.sleep(5_000).await;
    let elapsed = started.elapsed();
    let error = match result {
        Ok(_) => panic!(
            "5s receiver handler must fail sender-side at the 2s transport cap, but it succeeded"
        ),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            RelayAskError::Send {
                failure: RelaySendFailure::Transport,
                ..
            }
        ),
        "expected the typed Transport classification (sender-side request_timeout), got: {error}"
    );
    assert!(
        elapsed >= Duration::from_millis(1_500),
        "transport-cap failure fired suspiciously early — not the 2s request_timeout (took {elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "transport-cap failure must fire at ~2s, not after the 5s handler (took {elapsed:?})"
    );

    // --- Exit criterion: relay crash → supervised respawn + same-name
    // re-registration; the sender's cached ref goes stale (new ActorId) and
    // recovers via the bounded-backoff kademlia re-lookup.
    relay_a.crash().await.expect("crash request goes out");
    ping_until(&mut relay_a, &node_a, Duration::from_secs(30))
        .await
        .expect("relay respawned, re-registered under the same name, and re-resolved");

    // --- Exit criterion: revoked-peer-with-live-connection. Revoke A
    // cluster-wide (delete its allowlist row, using the peer id A published
    // in its node-id file): every node's refresh closes live connections to A
    // within ~1 refresh interval, and re-dials are denied. Observed from the
    // test process: pings to A start failing.
    let conn = db.guard().await.expect("guard");
    conn.execute(
        "DELETE FROM clustering_peer_allowlist WHERE peer_id = ?",
        waddle_server::db_params![peer_a.clone()],
    )
    .await
    .expect("revoke A");
    let revoked_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if relay_a.ping().await.is_err() {
            break; // revocation landed: A unreachable from the test node
        }
        assert!(
            Instant::now() < revoked_deadline,
            "revoked peer A still reachable after the containment window"
        );
    }
    // Re-enroll A: connectivity must recover via the periodic re-dial.
    conn.execute(
        "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
        waddle_server::db_params![peer_a.clone()],
    )
    .await
    .expect("re-enroll A");
    ping_until(&mut relay_a, &node_a, Duration::from_secs(30))
        .await
        .expect("re-enrolled peer reachable again");

    // --- Exit criterion: re-discovery through a rolling restart of BOTH
    // bootstrap peers, sequential, at most one node down at any instant.
    //
    // Leg 1: hard-kill B (SIGKILL — an unclean pod death) and replace it with
    // a fresh process on the SAME swarm port (exactly what a replacement pod
    // behind the same headless Service looks like): new node_id, new relay
    // name, new leased slot. The mesh's periodic re-dial reconnects and the
    // newcomer's periodic re-registration makes its relay resolvable.
    drop(server_b);
    let (server_b2, node_b2, _peer_b2) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;
    assert_ne!(node_b2, node_b, "restarted node must mint a fresh node_id");
    let mut relay_b2 = RelayHandle::new(NodeId::new(node_b2.clone()));
    ping_until(&mut relay_b2, &node_b2, Duration::from_secs(45))
        .await
        .expect("restarted node re-discovered via kademlia");

    // Leg 2: with B's replacement verified live, cycle A too — the graceful
    // path this time (SIGTERM is what a real rolling restart sends), which
    // drains: the keypair slot is released for the replacement and the relay
    // proactively unregisters. After this NO original bootstrap peer
    // survives, so the test process must reach both replacements via the
    // periodic re-dial + kademlia re-discovery alone.
    server_a.send_sigterm();
    assert!(
        server_a.wait_for_exit(Duration::from_secs(30)).await,
        "node A did not exit after SIGTERM"
    );
    drop(server_a);
    let (server_a2, node_a2, _peer_a2) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    assert_ne!(node_a2, node_a, "restarted node must mint a fresh node_id");
    let mut relay_a2 = RelayHandle::new(NodeId::new(node_a2.clone()));
    ping_until(&mut relay_a2, &node_a2, Duration::from_secs(45))
        .await
        .expect("A's replacement re-discovered via kademlia after the full rolling restart");
    // Full cross-node discovery holds with zero original peers left: B's
    // replacement must still answer alongside A's.
    ping_until(&mut relay_b2, &node_b2, Duration::from_secs(30))
        .await
        .expect("B's replacement still reachable after the full rolling restart");

    // --- Shutdown.
    stop.cancel();
    drop(server_b2);
    drop(server_a2);
}

/// DEFERRED (Phase 3): lone-survivor at N=2 keeps serving while a node
/// isolated from all live swarm peers (Postgres reachable) fences — requires
/// the `nodes` heartbeat fencing that lands with the ownership control plane.
#[tokio::test]
#[ignore = "Phase 3: requires nodes-table heartbeat fencing"]
async fn lone_survivor_and_isolation_fencing() {}

/// DEFERRED (Phase 3): a single dead link between two of three nodes degrades
/// routing to the durable fallback without fencing either endpoint — requires
/// the durable `pending_delivery` cross-node fallback.
#[tokio::test]
#[ignore = "Phase 3: requires the durable-queue fallback"]
async fn partial_partition_degrades_without_fencing() {}

/// DEFERRED (manual go/no-go measurement): the visibility window of a
/// hard-killed publisher's kademlia provider+metadata records is dominated by
/// kameo's hardcoded 1h record TTL / 30min republish — measure against the
/// acceptance threshold out-of-band; graceful stops proactively unregister.
#[tokio::test]
#[ignore = "manual measurement: dominated by kademlia's hardcoded 1h record TTL"]
async fn dead_publisher_record_visibility_window() {}
