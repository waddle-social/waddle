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
//! cleanly otherwise).
//!
//! ADR-0017 Phase 3 Slice 11 (harness-maturity capstone) added:
//! `lone_survivor_and_isolation_fencing` (Slice 2 primitives, formally
//! activated here); `whole_node_isolation_fences_then_self_heals_without_operator_intervention`
//! (renamed from the plan's original scaffold name,
//! `partial_partition_degrades_without_fencing`, per the Slice 11 corrigenda
//! — a disconnected node self-fences while its uninvolved peers stay ready,
//! then self-heals — see that test's own doc comment, deviations 107/108,
//! for exactly what is and is not provable given this harness's
//! connectivity primitives); the Slice 5 multi-process kill-one
//! claim-scoped hydration capstone
//! (`orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions`,
//! upgraded in Phase 4 Slice 1a to exercise the real stale-node watchdog);
//! and the Phase 4 MUC foreign-owned-room proxy wire shape
//! (`muc_join_routes_to_foreign_room_owner`).
//!
//! Deferred as a manual go/no-go measurement (dominated by kademlia's
//! hardcoded 1h record TTL): the dead publisher's record-visibility window.

#![cfg(feature = "clustering")]

use base64::Engine;
use libp2p::identity::ed25519;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use waddle_server::clustering::allowlist::{AllowlistStore, PostgresAllowlistStore};
use waddle_server::clustering::claims::{NodeLeaseStore, PostgresClaimStore};
use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::lease::{KeypairSlotLease, PostgresKeypairSlotLease};
use waddle_server::clustering::ordered_relay::{
    OrderedRelayAck, OrderedRelayChannel, OrderedRelayClaim, OrderedRelayEnvelopeClaims,
    OrderedRelayOrigin, OrderedRelayOriginProof, OrderedRelayPayload, OrderedRelayRecipient,
    OrderedRelayReply, OrderedRelaySenderState, OrderedRelaySequence, OriginInboundSequence,
    RemoteStanzaEnvelope,
};
use waddle_server::clustering::relay::{RelayAskError, RelayHandle, RelaySendFailure};
use waddle_server::clustering::swarm;
use waddle_server::clustering::NodeId;
use waddle_server::config::{ClusteringBootstrapConfig, ClusteringConfig, ClusteringLeaseConfig};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_ws_test_support::{extract_attr_after, TestServer, WsXmppClient};
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
use waddle_xmpp::Stanza;

const POOL_SIZE: usize = 4;
const CLUSTER_PEER_USERNAME: &str = "cluster-peer";
const CLUSTER_PEER_PASSWORD: &str = "cluster-peer-password";

struct EnrolledPool {
    /// base64-encoded 32-byte ed25519 seeds (the WADDLE_CLUSTERING_KEYPAIR_POOL value).
    pool_env: String,
    /// PeerIds derived from every pool slot (all enrolled).
    peer_ids: Vec<libp2p::PeerId>,
    seeds: Vec<Vec<u8>>,
}

fn generate_pool() -> EnrolledPool {
    let mut encoded_seeds = Vec::new();
    let mut seeds = Vec::new();
    let mut peer_ids = Vec::new();
    for _ in 0..POOL_SIZE {
        let keypair = ed25519::Keypair::generate();
        let seed = keypair.secret().as_ref().to_vec();
        seeds.push(seed.clone());
        peer_ids.push(
            libp2p::identity::Keypair::from(keypair)
                .public()
                .to_peer_id(),
        );
        encoded_seeds.push(base64::engine::general_purpose::STANDARD.encode(seed));
    }
    EnrolledPool {
        pool_env: encoded_seeds.join(","),
        peer_ids,
        seeds,
    }
}

fn keypair_for_peer(pool: &EnrolledPool, peer_id: &libp2p::PeerId) -> libp2p::identity::Keypair {
    for seed in &pool.seeds {
        let mut seed = seed.clone();
        let secret =
            ed25519::SecretKey::try_from_bytes(&mut seed).expect("valid enrolled seed bytes");
        let keypair = libp2p::identity::Keypair::from(ed25519::Keypair::from(secret));
        if keypair.public().to_peer_id() == *peer_id {
            return keypair;
        }
    }
    panic!("test-process peer id was not derived from enrolled pool");
}

fn sign_ordered_envelope(envelope: &mut RemoteStanzaEnvelope, keypair: &libp2p::identity::Keypair) {
    let signing_bytes = envelope.signing_bytes().expect("ordered signing bytes");
    envelope.origin_proof = Some(OrderedRelayOriginProof {
        public_key: keypair.public().encode_protobuf(),
        signature: keypair.sign(&signing_bytes).expect("sign envelope"),
    });
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
    use waddle_xmpp::ownership::ClaimStore as _;

    // Provision the schema through the production `ensure_schema` path — an
    // inline DDL copy here could silently diverge as the schema evolves.
    PostgresClaimStore::new(db.clone())
        .ensure_schema()
        .await
        .expect("claims schema");
    PostgresKeypairSlotLease::new(db.clone())
        .ensure_schema()
        .await
        .expect("lease schema");
    PostgresAllowlistStore::new(db.clone())
        .ensure_schema()
        .await
        .expect("allowlist schema");
    let conn = db.guard().await.expect("guard");
    conn.execute("DELETE FROM clustering_claims", ())
        .await
        .expect("clean claims");
    conn.execute("DELETE FROM clustering_nodes", ())
        .await
        .expect("clean nodes");
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

async fn reset_fixed_pair_roster(db: &Database) {
    waddle_server::db::MigrationRunner::single()
        .run(db)
        .await
        .expect("global schema migrations for fixed-pair roster reset");
    let conn = db.guard().await.expect("guard");
    conn.execute(
        "DELETE FROM roster_items \
         WHERE user_jid IN ('admin@localhost', 'cluster-peer@localhost') \
            OR contact_jid IN ('admin@localhost', 'cluster-peer@localhost')",
        (),
    )
    .await
    .expect("clean fixed-pair roster items");
    conn.execute(
        "DELETE FROM roster_versions \
         WHERE user_jid IN ('admin@localhost', 'cluster-peer@localhost')",
        (),
    )
    .await
    .expect("clean fixed-pair roster versions");
}

async fn fixed_pair_roster_snapshot(db: &Database) -> Vec<String> {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT user_jid, contact_jid, subscription, ask, approved \
             FROM roster_items \
             WHERE user_jid IN ('admin@localhost', 'cluster-peer@localhost') \
                OR contact_jid IN ('admin@localhost', 'cluster-peer@localhost') \
             ORDER BY user_jid, contact_jid",
            (),
        )
        .await
        .expect("fixed-pair roster snapshot");
    let mut snapshot = Vec::new();
    while let Some(row) = rows.next().await.expect("fixed-pair roster row") {
        let user: String = row.get(0).expect("user_jid");
        let contact: String = row.get(1).expect("contact_jid");
        let subscription: String = row.get(2).expect("subscription");
        let ask: Option<String> = row.get(3).expect("ask");
        let approved: bool = row.get(4).expect("approved");
        snapshot.push(format!(
            "{user}->{contact}: subscription={subscription}, ask={ask:?}, approved={approved}"
        ));
    }
    snapshot
}

fn stanza_xml(stanza: Stanza) -> String {
    let mut buf = Vec::new();
    stanza
        .to_element()
        .write_to(&mut buf)
        .expect("write stanza");
    String::from_utf8(buf).expect("utf8 stanza")
}

fn presence_xml(
    presence_type: xmpp_parsers::presence::Type,
    to: Option<&str>,
    status: Option<&str>,
) -> String {
    let mut presence = xmpp_parsers::presence::Presence::new(presence_type);
    presence.to = to
        .map(str::parse)
        .transpose()
        .expect("valid presence target");
    if let Some(status) = status {
        presence
            .statuses
            .insert(xmpp_parsers::message::Lang::new(), status.to_string());
    }
    stanza_xml(Stanza::Presence(presence))
}

fn frame_has_attr(frame: &str, name: &str, value: &str) -> bool {
    frame.contains(&format!("{name}='{value}'")) || frame.contains(&format!("{name}=\"{value}\""))
}

fn frame_attr_starts_with(frame: &str, name: &str, prefix: &str) -> bool {
    frame.contains(&format!("{name}='{prefix}")) || frame.contains(&format!("{name}=\"{prefix}"))
}

async fn send_roster_get(client: &mut WsXmppClient, id: &str) -> String {
    let iq = xmpp_parsers::iq::Iq::Get {
        from: None,
        to: None,
        id: id.to_string(),
        payload: minidom::Element::builder("query", "jabber:iq:roster").build(),
    };
    client
        .send(&stanza_xml(Stanza::Iq(Box::new(iq))))
        .await
        .expect("send roster get");
    client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("roster get result")
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
    spawn_cluster_server_with_envs(postgres_url, pool_env, swarm_port, bootstrap_ports, &[]).await
}

/// [`spawn_cluster_server`] plus caller-supplied extra envs (e.g. the
/// `LIVEKIT_*` set for the Muji route-to-owner test — the JWT mint is
/// pure, so no real LiveKit is needed).
async fn spawn_cluster_server_with_envs(
    postgres_url: &str,
    pool_env: &str,
    swarm_port: u16,
    bootstrap_ports: &[u16],
    extra_envs: &[(&'static str, &'static str)],
) -> (TestServer, String, String) {
    let postgres_url = postgres_url.to_string();
    let pool_env = pool_env.to_string();
    let bootstrap_ports = bootstrap_ports.to_vec();
    let extra_envs = extra_envs.to_vec();
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
            // ADR-0017 Phase 3 Slice 4/6 co-location invariant: clustered SM
            // persistence MUST live in the same Postgres database as the
            // clustering claims tables, or `open_for_cluster_mode` fails
            // startup with `ClusterColocationMismatch`. `TestServer`'s own
            // default (`sqlite::memory:`) is a non-Postgres URL, which
            // `open_for_cluster_mode`'s `postgres://`/`postgresql://` filter
            // silently treats as "not set" — falling back to the portable,
            // per-process, NON-shared SM store, defeating cross-node resume
            // entirely (Slice 6's `cross_node_resume_live_steal_handshake`
            // needs both nodes reading/writing the SAME persisted rows).
            ("WADDLE_XMPP_SM_DATABASE_URL", &postgres_url),
            // #1652: clustering rejects ephemeral (in-memory) durable stores
            // at readiness, so the harness runs every store on the shared
            // Postgres — the same shape as a production clustered node.
            ("WADDLE_XMPP_MAM_DATABASE_URL", &postgres_url),
            ("WADDLE_XMPP_INBOX_DATABASE_URL", &postgres_url),
            ("WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL", &postgres_url),
            ("WADDLE_XMPP_PUBSUB_DATABASE_URL", &postgres_url),
            // NB: the fixed test account stays enabled (TestServer's default) —
            // disabling it flips the permission backend onto the SpiceDB path and
            // fails startup. Its seeding is delete-then-recreate, safe for the
            // sequential startups this harness performs.
            // The secondary fixed account is also owner-capable so foreign-owned
            // MUC join tests reach the RoomRegistry ownership check instead of
            // being denied by the local instant-room creation guard first.
            ("WADDLE_SERVER_OWNER_LOCALPARTS", "admin,cluster-peer"),
            ("WADDLE_CLUSTERING_ENABLED", "true"),
            // #1652: clustered nodes refuse a row-less database unless the
            // rollout carries the enroll action; enroll is idempotent, so
            // every node of the harness may carry it. One fixed deployment
            // UUID = one deployment sharing the Postgres, matching prod.
            (
                "WADDLE_DEPLOYMENT_UUID",
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a9c1e2",
            ),
            ("WADDLE_DB_LINEAGE_ACTION", "enroll"),
            ("WADDLE_CLUSTERING_LISTEN_ADDRS", &listen),
            ("WADDLE_CLUSTERING_KEYPAIR_POOL", &pool_env),
            ("WADDLE_CLUSTERING_NODE_ID_FILE", &node_id_file_str),
            ("WADDLE_CLUSTERING_FAULT_INJECTION", "true"),
            // Tight intervals so revocation/re-dial assertions run fast.
            ("WADDLE_CLUSTERING_ALLOWLIST_REFRESH_MS", "1000"),
            ("WADDLE_CLUSTERING_DIAL_INTERVAL_MS", "1000"),
            ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "1000"),
            ("WADDLE_CLUSTERING_LEASE_TTL_MS", "10000"),
            // ADR-0017 Phase 3 Slice 2 (FIX 4): equally tight node-lease/
            // self-fence timing, exercised by `lone_survivor_and_isolation_fencing`
            // below — every server this harness spawns is a real production
            // subprocess that runs `clustering::start_if_enabled`'s node-lease/
            // self-fence loop unconditionally, so these envs simply make that
            // already-running production loop observable on a modest wall
            // clock instead of introducing a second harness-only code path.
            ("WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS", "300"),
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "1200"),
            ("WADDLE_CLUSTERING_ISOLATION_INTERVALS", "2"),
            ("WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS", "200"),
            ("WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS", "1000"),
            // ADR-0017 Phase 3 Slice 6: must be <= WADDLE_CLUSTERING_NODE_LEASE_TTL_MS
            // (config validation enforces this — the held-response window is
            // an upper bound on, never longer than, a fresh node-lease TTL).
            ("WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS", "1000"),
            // ADR-0017 Phase 3 Slice 10: node_lease_ttl (1200) must be >= 3x
            // the claim-release budget. The default budget is 5s, which would
            // reject this harness's fast-timer config at startup — size it
            // down to match (1200 >= 3 * 300).
            ("WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS", "300"),
            // ADR-0017 Phase 3 Slice 11 corrigenda (deviation 111, FIX C):
            // the orphan reaper's cadence has no other reason to run at its
            // 120s production default in this harness — every real
            // subprocess spawned here already runs the unmodified
            // `spawn_orphan_reaper_janitor` loop, so tightening only its
            // interval (not its logic) lets
            // `orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions`
            // observe a sweep in seconds instead of waiting out ~120s of
            // real wall-clock time per attempt.
            ("WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS", "500"),
        ];
        if !bootstrap_peers.is_empty() {
            envs.push(("WADDLE_CLUSTERING_BOOTSTRAP_PEERS", &bootstrap_peers));
        }
        envs.extend(extra_envs.iter().copied());

        let server = TestServer::start_with_extra_envs(
            &[(CLUSTER_PEER_USERNAME, CLUSTER_PEER_PASSWORD)],
            &envs,
        );

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

fn ordered_channel(
    id: &str,
    target: &jid::FullJid,
    target_epoch: ClaimEpoch,
) -> OrderedRelayChannel {
    OrderedRelayChannel {
        origin: OrderedRelayOrigin::SmSession(waddle_xmpp::pending_delivery::SmSessionId::new(id)),
        recipient: OrderedRelayRecipient::FullJid(target.clone()),
        target_epoch,
    }
}

fn ordered_origin_claim(id: &str, epoch: ClaimEpoch) -> OrderedRelayClaim {
    OrderedRelayClaim {
        entity: Entity::new(EntityType::SmSession, id),
        epoch,
    }
}

fn ordered_target_claim(target: &jid::FullJid, epoch: ClaimEpoch) -> OrderedRelayClaim {
    OrderedRelayClaim {
        entity: Entity::new(EntityType::UserActor, target.to_bare().to_string()),
        epoch,
    }
}

fn ordered_sender_full() -> jid::FullJid {
    "ordered-origin@localhost/test-process"
        .parse()
        .expect("ordered sender full jid")
}

fn ordered_sender_claim(epoch: ClaimEpoch) -> OrderedRelayClaim {
    OrderedRelayClaim {
        entity: Entity::new(
            EntityType::UserActor,
            ordered_sender_full().to_bare().to_string(),
        ),
        epoch,
    }
}

fn ordered_message_payload(
    id: &str,
    body_size: usize,
    target: &jid::FullJid,
) -> OrderedRelayPayload {
    let mut stanza = thread_message(id, body_size);
    if let waddle_xmpp::Stanza::Message(message) = &mut stanza.0 {
        message.from = Some(jid::Jid::from(ordered_sender_full()));
        message.to = Some(jid::Jid::from(target.clone()));
    }
    OrderedRelayPayload::Message {
        recipient: jid::Jid::from(target.clone()),
        stanza,
    }
}

/// Serializes the heavy subprocess tests in this binary. They share the
/// mutable control-plane tables (`clustering_nodes`/`clustering_claims`/
/// `clustering_peer_allowlist`/`clustering_keypair_slots`) in one Postgres
/// and each starts by DELETE-resetting them — running concurrently (the
/// cargo test default) would wipe rows the other test's live subprocess
/// servers depend on mid-run.
fn cluster_e2e_serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
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
    let _serial = cluster_e2e_serial_lock().lock().await;

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
        // Irrelevant to this in-test-process swarm join (it never spawns
        // `spawn_orphan_reaper_janitor`, an HTTP-server-only janitor), but
        // the field is still required by the struct literal — kept tight
        // for consistency with every other timer above.
        orphan_reaper_interval: Duration::from_secs(1),
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
        node_lease: waddle_server::config::ClusteringNodeLeaseConfig {
            heartbeat_interval: Duration::from_secs(1),
            lease_ttl: Duration::from_secs(10),
            claim_release_budget: Duration::from_secs(5),
        },
        self_fence: waddle_server::config::ClusteringSelfFenceConfig::default(),
        steal_intent: waddle_server::config::ClusteringStealIntentConfig::default(),
        resume_handshake: waddle_server::config::ClusteringResumeHandshakeConfig {
            timeout: Duration::from_secs(2),
        },
        pod_template_hash: None,
    };
    let handle = swarm::spawn(
        &config,
        &db,
        stop.clone(),
        swarm::RelayBridges {
            resume_bridge: waddle_server::clustering::resume_bridge::ResumeStealBridge::new(),
            room_local_claims: waddle_server::clustering::local_claims::RoomLocalClaims::new(),
            ordered_relay_delivery_bridge:
                waddle_server::clustering::route_bridge::OrderedRelayDeliveryBridge::new(
                    stop.clone(),
                    &config.messaging,
                ),
        },
    )
    .await
    .expect("test-process swarm joins the mesh");
    assert!(!handle.node_id.as_str().is_empty());

    // --- Exit criterion: cross-node ask round-trip (real network, two other
    // processes), including discovery of B through the mesh (the test only
    // bootstraps toward A).
    // Exercise the configured receiver-side ask timeouts (ADR element 5)
    // through the handle's wiring, not just config validation.
    let mut relay_a = RelayHandle::new(NodeId::new(node_a.clone()), stop.clone())
        .with_ask_timeouts(
            config.messaging.mailbox_timeout,
            config.messaging.reply_timeout,
        );
    ping_until(&mut relay_a, &node_a, Duration::from_secs(30))
        .await
        .expect("cross-node ping to A");
    let mut relay_b = RelayHandle::new(NodeId::new(node_b.clone()), stop.clone());
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

    // --- Phase 4 Slice 3: ordered relay over the real cross-node relay.
    // The receiver validates origin proof + origin/target claims, delivers
    // to a live target resource on node A, ACKs an exact duplicate
    // idempotently, and NACKs a later gap.
    use waddle_xmpp::ownership::ClaimStore as _;

    let mut ordered_target_client = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        "localhost",
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        "ordered-device",
    )
    .await
    .expect("cluster-peer target connects to node A");
    let ordered_target_full: jid::FullJid = ordered_target_client
        .full_jid
        .as_ref()
        .expect("target bind full jid")
        .parse()
        .expect("target full jid");

    let claim_store = PostgresClaimStore::new(db.clone());
    let ordered_stream_id = "ordered-e2e-stream";
    let origin_identity = NodeIdentity::new(
        handle.node_id.as_str().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    claim_store
        .register_with_peer_id(
            &origin_identity,
            None,
            Some(handle.local_peer_id.to_string()),
        )
        .await
        .expect("register test-process origin node");
    let origin_entity = Entity::new(EntityType::SmSession, ordered_stream_id);
    let origin_epoch = claim_store
        .acquire(&origin_entity, &origin_identity)
        .await
        .expect("test-process origin SM claim");
    let sender_entity = Entity::new(
        EntityType::UserActor,
        ordered_sender_full().to_bare().to_string(),
    );
    let sender_epoch = claim_store
        .acquire(&sender_entity, &origin_identity)
        .await
        .expect("test-process sender UserActor claim");
    let target_entity = Entity::new(
        EntityType::UserActor,
        ordered_target_full.to_bare().to_string(),
    );
    let target_snapshot = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(snapshot) = claim_store
                .current_claim(&target_entity)
                .await
                .expect("target user claim lookup")
            {
                if snapshot.owner.node_id == node_a && snapshot.owner_lease_fresh {
                    break snapshot;
                }
            }
            assert!(
                Instant::now() < deadline,
                "node A did not publish target UserActor claim for {target_entity}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };

    let mut ordered_origin_client = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        "localhost",
        "admin",
        server_b.fixed_account_password(),
        "ordered-origin",
    )
    .await
    .expect("admin origin connects to node B");
    ordered_origin_client
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("origin enables SM");
    let origin_enabled = ordered_origin_client
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("origin receives SM enabled");
    let origin_stream_id = extract_attr_after(&origin_enabled, "<enabled", "id")
        .expect("origin enabled carries stream id");
    let origin_sm_entity = Entity::new(EntityType::SmSession, origin_stream_id.as_str());
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(snapshot) = claim_store
                .current_claim(&origin_sm_entity)
                .await
                .expect("origin SM claim lookup")
            {
                if snapshot.owner.node_id == node_b && snapshot.owner_lease_fresh {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "node B did not publish origin SM claim for {origin_sm_entity}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let mut cross_node_message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(ordered_target_full.clone())));
    cross_node_message.thread = Some(xmpp_parsers::message::Thread {
        id: "ordered-websocket-one".to_string(),
        parent: None,
    });
    cross_node_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "phase4 cross-node route".to_string(),
    );
    let cross_node_xml = waddle_xmpp::parser::message_to_string(&cross_node_message)
        .expect("serialize typed cross-node message");
    ordered_origin_client
        .send(&cross_node_xml)
        .await
        .expect("origin sends full-JID message to remote target");
    let delivered = ordered_target_client
        .recv_matching_within(Duration::from_secs(30), |frame| {
            frame.contains("ordered-websocket-one") && frame.contains("phase4 cross-node route")
        })
        .await
        .expect("target receives origin WebSocket cross-node message");
    assert!(
        delivered.contains(ordered_target_full.as_str()),
        "delivered frame should remain addressed to the target full JID: {delivered}"
    );
    ordered_origin_client
        .send(r#"<r xmlns="urn:xmpp:sm:3"/>"#)
        .await
        .expect("origin requests handled count after remote handoff");
    let origin_ack = ordered_origin_client
        .recv_matching_within(Duration::from_secs(30), |frame| {
            frame.contains("<a") && (frame.contains("h=\"1\"") || frame.contains("h='1'"))
        })
        .await
        .expect("origin SM ack advances after remote handoff completion");
    assert!(
        origin_ack.contains("h=\"1\"") || origin_ack.contains("h='1'"),
        "origin handled count should include the cross-node stanza: {origin_ack}"
    );

    // PR #1231 regression: a second resource for the SAME bare JID can land on
    // a non-owner node. The authoritative UserActor claim stays on node A, but
    // node A registers a remote resource whose outbound frames are relayed back
    // to node B's real WebSocket.
    let remote_resource = format!("ordered-remote-device-{}", uuid::Uuid::new_v4());
    let mut remote_target_client = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        "localhost",
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &remote_resource,
    )
    .await
    .expect("same-bare remote resource binds on node B without stealing node A's UserActor");
    let remote_target_full: jid::FullJid = remote_target_client
        .full_jid
        .as_ref()
        .expect("remote target bind full jid")
        .parse()
        .expect("remote target full jid");
    let after_remote_bind = claim_store
        .current_claim(&target_entity)
        .await
        .expect("target claim lookup after same-bare remote bind")
        .expect("target claim remains present after same-bare remote bind");
    assert_eq!(
        after_remote_bind.owner.node_id, node_a,
        "same-bare remote bind must not steal the authoritative UserActor claim"
    );
    assert_eq!(
        after_remote_bind.claim_epoch, target_snapshot.claim_epoch,
        "same-bare remote bind must not advance the UserActor claim epoch"
    );

    let mut same_bare_message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(remote_target_full.clone())));
    same_bare_message.thread = Some(xmpp_parsers::message::Thread {
        id: "ordered-remote-same-bare".to_string(),
        parent: None,
    });
    same_bare_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "phase4 same-bare remote resource".to_string(),
    );
    let same_bare_xml = waddle_xmpp::parser::message_to_string(&same_bare_message)
        .expect("serialize typed same-bare remote message");
    ordered_origin_client
        .send(&same_bare_xml)
        .await
        .expect("origin sends full-JID message to same-bare remote resource");
    let same_bare_delivered = remote_target_client
        .recv_matching_within(Duration::from_secs(30), |frame| {
            frame.contains("ordered-remote-same-bare")
                && frame.contains("phase4 same-bare remote resource")
        })
        .await
        .expect("same-bare remote resource receives owner-forwarded message");
    assert!(
        same_bare_delivered.contains(remote_target_full.as_str()),
        "same-bare remote frame should remain addressed to the node-B full JID: \
         {same_bare_delivered}"
    );

    let mut remote_origin_message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(ordered_target_full.clone())));
    remote_origin_message.thread = Some(xmpp_parsers::message::Thread {
        id: "ordered-remote-origin".to_string(),
        parent: None,
    });
    remote_origin_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "phase4 remote resource origin route".to_string(),
    );
    let remote_origin_xml = waddle_xmpp::parser::message_to_string(&remote_origin_message)
        .expect("serialize typed remote-origin message");
    remote_target_client
        .send(&remote_origin_xml)
        .await
        .expect("same-bare remote resource sends to owner-node sibling resource");
    let remote_origin_result = ordered_target_client
        .recv_matching_within(Duration::from_secs(30), |frame| {
            frame.contains("ordered-remote-origin")
                && frame.contains("phase4 remote resource origin route")
        })
        .await;
    let remote_origin_delivered = match remote_origin_result {
        Ok(frame) => frame,
        Err(error) => {
            // CI diagnostic for the repeatable zero-frame timeout on this
            // leg (issue #1627 / PR #1676): determine which link is dead
            // before panicking. Probe 1 exercises owner-node local
            // delivery to the same sibling connection; probe 2 exercises
            // node-B local echo back to the remote resource itself.
            let mut probe_local = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
                ordered_target_full.clone(),
            )));
            probe_local.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                "diag local-path probe".to_string(),
            );
            let probe_local_xml = waddle_xmpp::parser::message_to_string(&probe_local)
                .expect("serialize diag local probe");
            ordered_origin_client
                .send(&probe_local_xml)
                .await
                .expect("diag: origin sends local-path probe");
            let local_probe = ordered_target_client
                .recv_matching_within(Duration::from_secs(10), |frame| {
                    frame.contains("diag local-path probe")
                })
                .await;
            let mut probe_echo = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
                remote_target_full.clone(),
            )));
            probe_echo.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                "diag node-b echo probe".to_string(),
            );
            let probe_echo_xml = waddle_xmpp::parser::message_to_string(&probe_echo)
                .expect("serialize diag echo probe");
            remote_target_client
                .send(&probe_echo_xml)
                .await
                .expect("diag: remote resource sends self-echo probe");
            let echo_probe = remote_target_client
                .recv_matching_within(Duration::from_secs(10), |frame| {
                    frame.contains("diag node-b echo probe")
                })
                .await;
            panic!(
                "owner-node sibling never received the remote-origin message: {error}\n\
                 diag probe 1 (owner-node local delivery to the same sibling connection): {local_probe:?}\n\
                 diag probe 2 (node-B local self-echo from the remote resource): {echo_probe:?}"
            );
        }
    };
    assert!(
        remote_origin_delivered.contains(ordered_target_full.as_str()),
        "remote-origin frame should remain addressed to the owner-node full JID: \
         {remote_origin_delivered}"
    );
    let _ = remote_target_client.close().await;

    let ordered_origin_node = NodeId::new(handle.node_id.as_str().to_string());
    let ordered_channel = ordered_channel(
        ordered_stream_id,
        &ordered_target_full,
        target_snapshot.claim_epoch,
    );
    let origin_claim = ordered_origin_claim(ordered_stream_id, origin_epoch);
    let sender_claim = ordered_sender_claim(sender_epoch);
    let target_claim = ordered_target_claim(&ordered_target_full, target_snapshot.claim_epoch);
    let origin_keypair = keypair_for_peer(&pool, &handle.local_peer_id);
    let mut ordered_sender = OrderedRelaySenderState::default();
    let mut first_ordered = ordered_sender
        .next_envelope(
            ordered_origin_node.clone(),
            ordered_channel.clone(),
            OriginInboundSequence(1),
            OrderedRelayEnvelopeClaims::new(
                origin_claim.clone(),
                sender_claim.clone(),
                target_claim.clone(),
            ),
            ordered_message_payload("ordered-one", 32, &ordered_target_full),
        )
        .expect("first ordered envelope");
    sign_ordered_envelope(&mut first_ordered, &origin_keypair);
    let first_reply = relay_a
        .deliver_ordered(first_ordered.clone())
        .await
        .expect("ordered relay first envelope");
    assert!(matches!(
        first_reply,
        OrderedRelayReply::Ack(OrderedRelayAck {
            sequence: OrderedRelaySequence(1),
            duplicate: false,
            next_expected: OrderedRelaySequence(2),
            ..
        })
    ));
    ordered_target_client
        .recv_matching_within(Duration::from_secs(30), |frame| {
            frame.contains("ordered-one")
        })
        .await
        .expect("target receives direct ordered relay envelope");

    let duplicate_reply = relay_a
        .deliver_ordered(first_ordered)
        .await
        .expect("ordered relay duplicate envelope");
    assert!(matches!(
        duplicate_reply,
        OrderedRelayReply::Ack(OrderedRelayAck {
            sequence: OrderedRelaySequence(1),
            duplicate: true,
            next_expected: OrderedRelaySequence(2),
            ..
        })
    ));

    let mut gap = RemoteStanzaEnvelope {
        asserted_origin_node: ordered_origin_node,
        channel: ordered_channel,
        sequence: OrderedRelaySequence(3),
        origin_inbound_sequence: OriginInboundSequence(3),
        origin_claim,
        sender_claim,
        target_claim,
        payload: ordered_message_payload("ordered-gap", 32, &ordered_target_full),
        origin_proof: None,
    };
    sign_ordered_envelope(&mut gap, &origin_keypair);
    let gap_reply = relay_a
        .deliver_ordered(gap)
        .await
        .expect("ordered relay gap envelope");
    assert!(
        matches!(
            gap_reply,
            OrderedRelayReply::Nack(waddle_server::clustering::ordered_relay::OrderedRelayNack {
                reason: waddle_server::clustering::ordered_relay::OrderedRelayNackReason::Gap {
                    expected: OrderedRelaySequence(2)
                },
                ..
            })
        ),
        "expected ordered relay gap NACK, got {gap_reply:?}"
    );
    drop(ordered_target_client);

    // --- Exit criterion: integrity under concurrent large + small payloads.
    // (Per-(origin→recipient) sequencing is Phase 4; Phase 2 asserts that
    // interleaved large/small asks each come back intact.)
    let mut join_set = tokio::task::JoinSet::new();
    for index in 0..8u32 {
        let node = node_a.clone();
        let stop = stop.clone();
        let size = if index % 2 == 0 { 100 * 1024 } else { 16 };
        let thread_id = format!("mix-{index}");
        join_set.spawn(async move {
            let mut relay = RelayHandle::new(NodeId::new(node), stop);
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
    let mut relay_b_slow = RelayHandle::new(NodeId::new(node_b.clone()), stop.clone())
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
    let mut relay_b2 = RelayHandle::new(NodeId::new(node_b2.clone()), stop.clone());
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
    let mut relay_a2 = RelayHandle::new(NodeId::new(node_a2.clone()), stop.clone());
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

/// Wipe the `clustering_nodes`/`clustering_claims` tables (provisioning the
/// schema first via the production `ensure_schema` path, in case this is
/// the first test in the binary to touch it — mirrors `reset_and_enroll`'s
/// own "provision through the production path" convention). Residual rows
/// from an earlier scenario in the same shared Postgres instance are
/// otherwise invisible garbage that can pollute `count_other_live_nodes`:
/// `lone_survivor_and_isolation_fencing` needs a truthful "other live node"
/// count before any survivor janitor has had time to run the stale-node
/// watchdog.
async fn reset_node_lease_tables(db: &Database) {
    use waddle_xmpp::ownership::ClaimStore as _;
    let store = PostgresClaimStore::new(db.clone());
    store
        .ensure_schema()
        .await
        .expect("ensure node-lease schema");
    let conn = db.guard().await.expect("guard");
    conn.execute("DELETE FROM clustering_claims", ())
        .await
        .expect("clean claims");
    conn.execute("DELETE FROM clustering_nodes", ())
        .await
        .expect("clean nodes");
}

/// Poll a spawned cluster server's `/ready` endpoint once; `None` if the
/// request itself fails (e.g. the process hasn't bound its HTTP listener
/// yet), `Some(true)`/`Some(false)` for a 2xx/non-2xx response.
async fn readiness_status(server: &TestServer) -> Option<bool> {
    let url = format!("{}/ready", server.http_base_url());
    reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .ok()
        .map(|response| response.status().is_success())
}

/// Poll until a spawned cluster server's `/ready` endpoint reports
/// `want_ready`, or panic once `deadline` elapses.
async fn wait_for_readiness(server: &TestServer, want_ready: bool, deadline: Duration) {
    let until = Instant::now() + deadline;
    loop {
        if readiness_status(server).await == Some(want_ready) {
            return;
        }
        assert!(
            Instant::now() < until,
            "server did not report {} within {deadline:?}",
            if want_ready { "ready" } else { "not-ready" }
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Read `clustering_nodes.expired` for `node_id` directly (Slice 11 harness
/// assertions need to observe the exact same committed flag the
/// production CAS predicates read — see `session_janitors.rs`'s identical
/// single-process helper, `expired_flag`, which this mirrors for the
/// multi-process harness since that helper is private to its own crate).
async fn node_expired_flag(db: &Database, node_id: &str) -> Option<bool> {
    let conn = db.guard().await.expect("guard");
    let mut rows = conn
        .query(
            "SELECT expired FROM clustering_nodes WHERE node_id = ?",
            waddle_server::db_params![node_id.to_string()],
        )
        .await
        .expect("query expired flag");
    rows.next()
        .await
        .expect("row")
        .map(|row| row.get::<bool>(0).expect("expired column"))
}

async fn seed_detached_sm_session_row(
    db: &Database,
    stream_id: &str,
    user_id: &str,
    full_jid: &str,
) {
    let conn = db.guard().await.expect("guard");
    conn.execute(
        r#"
        INSERT INTO sm_sessions (
            stream_id, user_id, full_jid, inbound_count, outbound_count,
            last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
            carbons_enabled, roster_interested, blocklist_interested,
            presence_available, presence_priority
        ) VALUES (
            ?, ?, ?, 0, 0, 0, 60,
            CAST(EXTRACT(EPOCH FROM now()) * 1000 AS BIGINT),
            60000, 0, 0, 0, 1, 0
        )
        ON CONFLICT (stream_id) DO UPDATE SET
            user_id = EXCLUDED.user_id,
            full_jid = EXCLUDED.full_jid,
            detached_at_ms = EXCLUDED.detached_at_ms,
            max_resume_duration_ms = EXCLUDED.max_resume_duration_ms
        "#,
        waddle_server::db_params![
            stream_id.to_string(),
            user_id.to_string(),
            full_jid.to_string(),
        ],
    )
    .await
    .expect("seed detached sm_sessions row");
}

/// Read `(node_id, claim_epoch)` for a `clustering_claims` row keyed by its
/// already-encoded `entity` string (`"sm_session:<stream_id>"`, mirroring
/// `clustering::claims::entity_key`'s own encoding — see that function's
/// doc comment). `None` if the row does not exist.
async fn sm_session_claim_owner(db: &Database, stream_id: &str) -> Option<(String, i64)> {
    let conn = db.guard().await.expect("guard");
    let entity = format!("sm_session:{stream_id}");
    let mut rows = conn
        .query(
            "SELECT node_id, claim_epoch FROM clustering_claims WHERE entity = ?",
            waddle_server::db_params![entity],
        )
        .await
        .expect("query claim owner");
    match rows.next().await.expect("row") {
        Some(row) => {
            let node_id: String = row.get(0).expect("node_id column");
            let claim_epoch: i64 = row.get(1).expect("claim_epoch column");
            Some((node_id, claim_epoch))
        }
        None => None,
    }
}

/// ADR-0017 Phase 3 Slice 2: lone-survivor-at-N=2 keeps serving despite zero
/// reachable swarm peers (the N=2 carve-out — a single other live row is
/// never enough corroboration to blame isolation on); isolation-fencing DOES
/// trip once a node is swarm-isolated from >= 2 other live nodes for the
/// configured M consecutive intervals.
///
/// Exercises the REAL production node-lease/self-fence loop end-to-end, not
/// a harness-only reimplementation: every node spawned here
/// (`spawn_cluster_server`) is a full `waddle-server` subprocess that runs
/// `clustering::start_if_enabled`'s production bring-up unconditionally
/// (`server::start_with_config` always calls it — see `server/mod.rs`), so
/// `run_node_lease` is already driving each subprocess's readiness signal
/// with no harness-side wiring required. The one gap this fills: the
/// pre-existing harness never drove a *third* in-process node through this
/// path (the test-process node C in `cluster_exit_criteria_end_to_end` calls
/// `swarm::spawn` directly, bypassing `start_if_enabled`) — but this
/// scenario needs no in-process node at all, only real HTTP `/ready` polling
/// against subprocess servers, so it sidesteps that gap entirely rather than
/// needing to close it.
///
/// Wall clock: node-lease heartbeat/TTL/isolation-interval envs are tuned to
/// a few hundred milliseconds (see `spawn_cluster_server`), so both legs
/// below complete in single-digit seconds of real time.
#[tokio::test(flavor = "multi_thread")]
async fn lone_survivor_and_isolation_fencing() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    // --- Part 1: N=2 lone survivor. Kill one of two nodes; the survivor
    // sees at most one other "live" `clustering_nodes` row and zero
    // reachable swarm peers, but must never isolation-fence — element 4's
    // N=2 carve-out requires >= 2 other live rows before swarm
    // unreachability alone is trusted to mean isolation.
    {
        let pool = generate_pool();
        let db = open_control_db(&postgres_url).await;
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;

        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let (server_a, _node_a, _peer_a) =
            spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
        let (server_b, _node_b, _peer_b) =
            spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

        // Hard-kill B (SIGKILL via `Drop`) — an unclean death, exactly the
        // shape `cluster_exit_criteria_end_to_end`'s own rolling-restart leg
        // uses. A initially observes 1 other live row; once B's heartbeat
        // goes stale it stops counting under the freshness filter, and a
        // survivor janitor may also commit `expired = true`. Both states
        // are below the N=2 isolation carve-out, so the carve-out holds
        // across the whole window either way.
        drop(server_b);

        // Poll across several node-lease heartbeat/isolation-interval
        // windows: the survivor must stay ready the whole time.
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            assert_eq!(
                readiness_status(&server_a).await,
                Some(true),
                "lone survivor at N=2 must never isolation-fence, even with zero \
                 reachable swarm peers"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        drop(server_a);
    }

    // --- Part 2: N>=3 isolation DOES fence. Three live nodes; isolate one
    // of them (D) from the swarm by revoking its OWN peer-allowlist entry
    // (symmetric enforcement, per `cluster_exit_criteria_end_to_end`'s own
    // revocation leg: A and B's refreshes close their side of the
    // connection to D within one allowlist-refresh interval, which tears
    // down D's side too, and D's own re-dials are rejected by A/B's
    // accept-side check) while A and B stay genuinely alive and
    // heartbeating throughout, so `clustering_nodes` truthfully shows D
    // >= 2 other live rows for the whole window — this is swarm isolation
    // with Postgres fully reachable to D, not a Postgres partition.
    {
        let pool = generate_pool();
        let db = open_control_db(&postgres_url).await;
        reset_and_enroll(&db, &pool).await;
        reset_node_lease_tables(&db).await;

        let port_a = free_tcp_port();
        let port_b = free_tcp_port();
        let port_d = free_tcp_port();
        let (server_a, _node_a, _peer_a) =
            spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b, port_d]).await;
        let (server_b, _node_b, _peer_b) =
            spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a, port_d]).await;
        let (server_d, _node_d, peer_d) =
            spawn_cluster_server(&postgres_url, &pool.pool_env, port_d, &[port_a, port_b]).await;

        wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
        wait_for_readiness(&server_d, true, Duration::from_secs(15)).await;

        let conn = db.guard().await.expect("guard");
        conn.execute(
            "DELETE FROM clustering_peer_allowlist WHERE peer_id = ?",
            waddle_server::db_params![peer_d.clone()],
        )
        .await
        .expect("revoke D");

        // D must self-fence: readiness flips not-ready within a modest
        // deadline (a few allowlist-refresh + isolation-interval windows).
        wait_for_readiness(&server_d, false, Duration::from_secs(15)).await;

        // A and B were never isolated from each other — they must stay
        // ready throughout, proving this is a targeted fence of the
        // isolated node, not a cluster-wide wobble.
        assert_eq!(
            readiness_status(&server_a).await,
            Some(true),
            "uninvolved peer A must stay ready"
        );
        assert_eq!(
            readiness_status(&server_b).await,
            Some(true),
            "uninvolved peer B must stay ready"
        );

        drop(server_a);
        drop(server_b);
        drop(server_d);
    }
}

/// ADR-0017 Phase 3 Slice 6: the cross-node XEP-0198 resume live-steal
/// handshake, end to end, against two real `waddle-server` processes
/// sharing one Postgres control plane (co-located clustering claims + SM
/// persistence, per Slice 4's design). A client enables resumable stream
/// management on node A and STAYS CONNECTED (a genuinely live session, no
/// detach) while a second connection resumes the same `previd` against node
/// B. Node B has no local record and no persisted snapshot to read, so it
/// must ask node A over the real swarm relay (`RelayResumeSteal`) to
/// force-detach — proving the whole handshake: the relay message
/// round-trip, node A's own identity check gating the destructive close,
/// the `<conflict/>` stream error actually reaching client A's live socket,
/// and node B's subsequent claim-steal + hydrate succeeding against the
/// now-persisted snapshot.
#[tokio::test]
async fn cross_node_resume_live_steal_handshake() {
    let _serial = cluster_e2e_serial_lock().lock().await;
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping cross_node_resume_live_steal_handshake: WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };

    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, _node_a, _peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, _node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

    const DOMAIN: &str = "localhost";
    const USERNAME: &str = "admin";
    // The fixed test account is shared (same Postgres users table) but each
    // `TestServer` reseeds it delete-then-recreate on its own startup with
    // its own randomly generated password — whichever server started LAST
    // (server_b) is the one whose password is actually live in the shared
    // table by the time both are up.
    let password = server_b.fixed_account_password().to_string();

    // Client A connects to node A and enables resumable SM. It stays
    // connected for the whole test — this IS the "live, owned elsewhere"
    // branch (no detach, so no persisted snapshot exists until node A's
    // force-detach lands).
    let resource_a = format!("cross-node-resume-a-{}", uuid::Uuid::new_v4());
    let mut client_a = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &resource_a,
    )
    .await
    .expect("client A connects to node A");
    client_a
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("enable stream management");
    let enabled = client_a
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("stream management enabled");
    let previd = extract_attr_after(&enabled, "<enabled", "id")
        .unwrap_or_else(|| panic!("enabled missing id: {enabled}"));

    // Client B connects to node B (a DIFFERENT process) and resumes the
    // SAME previd while client A is still live on node A. This can only
    // succeed via the cross-node live-steal handshake: node B has no local
    // claim and nothing persisted to read until node A force-detaches.
    let mut client_b = WsXmppClient::connect(&server_b.ws_url())
        .await
        .expect("client B connects to node B");
    client_b
        .authenticate(DOMAIN, USERNAME, &password)
        .await
        .expect("client B authenticates against node B");
    client_b
        .send(&format!(
            r#"<resume xmlns="urn:xmpp:sm:3" previd="{previd}" h="0"/>"#
        ))
        .await
        .expect("send cross-node resume");

    // The handshake involves a real swarm round-trip (kademlia discovery +
    // relay ask), so allow a generous deadline; poll rather than a single
    // fixed-timeout recv so a slow CI runner cannot flake this.
    let resumption = client_b
        .recv_matching(|frame| frame.contains("<resumed") || frame.contains("<failed"))
        .await
        .expect("cross-node resume reply from node B");
    assert!(
        resumption.contains("<resumed"),
        "cross-node live-steal handshake must resume onto node B, got: {resumption}"
    );
    assert!(
        resumption.contains(&previd),
        "resumed previd must match: {resumption}"
    );

    // Client A's live socket must have been force-detached: XEP-0198
    // "Resumption"'s `<conflict/>` stream error, then the transport close.
    let conflict_or_close = client_a
        .recv_matching(|frame| frame.contains("conflict") || frame.contains("<close"))
        .await
        .expect("client A observes the force-detach close");
    assert!(
        conflict_or_close.contains("conflict") || conflict_or_close.contains("<close"),
        "client A must see the <conflict/> stream error (or the framing close that follows): {conflict_or_close}"
    );

    drop(server_a);
    drop(server_b);
}

/// ADR-0017 Phase 3 Slice 11 (binding FIX 6b, council-adjudicated): the
/// multi-process capstone of Slice 5's own single-process
/// `session_janitors::orphan_reaper_sweep_tests` — two real processes,
/// shared Postgres, kill one, and assert the survivor's REAL
/// `spawn_orphan_reaper_janitor` loop (not a harness reimplementation —
/// `run_orphan_reaper_sweep` is `pub(crate)`-private to `waddle-server`, so
/// this test cannot call it directly even if it wanted to; it can only wait
/// for the real subprocess's own janitor tick and observe its effects
/// through Postgres) steals and hydrates ONLY the dead node's orphaned
/// `sm_session` claim, never re-touching the survivor's own already-claimed
/// session.
///
/// Phase 4 Slice 1a closes the earlier carried risk directly: the survivor's
/// real orphan-reaper sweep first runs a bounded stale-node watchdog that
/// discovers heartbeat-stale, non-expired node rows and feeds each candidate
/// through `NodeLeaseStore::expire`. Only after that committed-expired flag
/// lands does `list_orphaned_sm_session_claims` surface the dead node's SM
/// claim for the ordinary `steal_stale(OwnerStale)` CAS. This test therefore
/// no longer seeds `expired = true`; it waits for B's production janitor to
/// flip A's row and reclaim A's claim. ADR-0017 Phase 3 Slice 11 corrigenda
/// (deviation 111, FIX C): the loop's *cadence* is now env-
/// overridable (`WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS`, wired through
/// `ClusteringConfig::orphan_reaper_interval`), and `spawn_cluster_server`
/// sets it to 500ms for every subprocess this harness spawns — a minimal,
/// explicitly-sanctioned production change (only the timer's period, never
/// its logic) made so this test observes a real sweep in seconds rather
/// than waiting out the 120s production default per attempt.
///
/// Wall clock: dominated by the harness's 500ms `WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS`
/// override, not the 120s production default.
#[tokio::test(flavor = "multi_thread")]
async fn orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping orphan_reaper_kills_one_node_and_hydrates_only_its_orphaned_sessions: \
             WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    const DOMAIN: &str = "localhost";
    const OWNER_USERNAME: &str = "admin";
    // The node-lease TTL this harness configures every subprocess with
    // (`spawn_cluster_server`'s `WADDLE_CLUSTERING_NODE_LEASE_TTL_MS`) —
    // reused below so the wait for heartbeat staleness lines up with the
    // real subprocess config, not an arbitrarily different value.
    const NODE_LEASE_TTL: Duration = Duration::from_millis(1200);

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, node_a, _peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

    // Shared fixed test account; whichever server started LAST (server_b)
    // is the one whose reseeded password is live in the shared Postgres
    // users table (same convention `cross_node_resume_live_steal_handshake`
    // documents and relies on). The B-side control client deliberately uses
    // a different bare JID: once UserActor claims are live, two concurrent
    // local UserActors for the same bare JID on different nodes are not a
    // valid harness shortcut.
    let owner_password = server_b.fixed_account_password().to_string();

    // Client A enables resumable SM on node A: `handle_sm_enable` acquires
    // the `sm_session` `ClaimStore` claim immediately (before any detach —
    // see `session_registry/claims.rs`'s `ensure_claimed` call site), so
    // node A genuinely, currently owns this claim in the SAME shared
    // Postgres `clustering_claims` table the orphan reaper reads.
    let resource_a = format!("orphan-a-{}", uuid::Uuid::new_v4());
    let mut client_a = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        OWNER_USERNAME,
        &owner_password,
        &resource_a,
    )
    .await
    .expect("client A connects to node A");
    client_a
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("client A enables resumable SM");
    let enabled_a = client_a
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("client A's SM enabled ack");
    let stream_a = extract_attr_after(&enabled_a, "<enabled", "id")
        .unwrap_or_else(|| panic!("enabled missing id: {enabled_a}"));

    // Client B enables resumable SM on node B — the CONTROL: this claim
    // must survive the whole test completely untouched (same owner, same
    // epoch), proving the orphan reaper's targeted scan never re-touches a
    // survivor's own already-claimed sessions.
    let resource_b = format!("orphan-b-{}", uuid::Uuid::new_v4());
    let mut client_b = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        DOMAIN,
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &resource_b,
    )
    .await
    .expect("client B connects to node B");
    client_b
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("client B enables resumable SM");
    let enabled_b = client_b
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("client B's SM enabled ack");
    let stream_b = extract_attr_after(&enabled_b, "<enabled", "id")
        .unwrap_or_else(|| panic!("enabled missing id: {enabled_b}"));

    // Precondition sanity: both claims exist, owned exactly as expected,
    // before anything is killed.
    let (owner_a_before, epoch_a_before) = sm_session_claim_owner(&db, &stream_a)
        .await
        .expect("A's sm_session claim must exist before the kill");
    assert_eq!(
        owner_a_before, node_a,
        "A must own its own freshly-enabled claim"
    );
    let (owner_b_before, epoch_b_before) = sm_session_claim_owner(&db, &stream_b)
        .await
        .expect("B's sm_session claim must exist before the kill");
    assert_eq!(
        owner_b_before, node_b,
        "B must own its own freshly-enabled claim"
    );

    assert_eq!(
        node_expired_flag(&db, &node_a).await,
        Some(false),
        "A's clustering_nodes row must start non-expired before the kill"
    );

    // Phase 4 Slice 1a hardening: the orphan reaper must only reclaim
    // hydratable detached SM sessions, not claim-only live sessions created by
    // `<enable resume='true'/>`. Seed the durable detached row explicitly so
    // this capstone still exercises the production reaper path that steals and
    // hydrates a real orphan, while the claim remains owned by node A until
    // node B's janitor wins the CAS.
    let full_jid_a = format!("{OWNER_USERNAME}@{DOMAIN}/{resource_a}");
    seed_detached_sm_session_row(&db, &stream_a, OWNER_USERNAME, &full_jid_a).await;

    // Hard-kill A (SIGKILL via `Drop`, exactly like `lone_survivor_and_
    // isolation_fencing`'s own unclean-death leg) — client A's socket goes
    // down with it, un-gracefully, with no self-expire.
    drop(server_a);

    // Let A's heartbeat genuinely lapse past the configured node-lease TTL
    // before waiting for B's real stale-node watchdog. The production
    // `expire()` CAS itself is gated on heartbeat staleness
    // (`heartbeat < now() - lease_ttl`), so this sleep is not cosmetic.
    tokio::time::sleep(NODE_LEASE_TTL * 2).await;

    // From here on, everything is B's real production
    // `spawn_orphan_reaper_janitor` loop: the stale-node watchdog flips A's
    // node row, then the committed-expired orphan scan steals A's claim.
    // Poll Postgres for its effect rather than driving it directly (the
    // sweep is crate-private; this harness cannot call it).
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some((owner, _epoch)) = sm_session_claim_owner(&db, &stream_a).await {
            if owner == node_b && node_expired_flag(&db, &node_a).await == Some(true) {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "node B's real orphan-reaper janitor never reclaimed A's orphaned sm_session \
             claim within {deadline:?} of A's death"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Targeted, exactly-once hydration, proven via Postgres alone (this
    // harness has no in-process view into B's own SM session registry —
    // the claims table is the shared, observable ground truth both
    // processes and this test agree on):
    let (owner_a_after, epoch_a_after) = sm_session_claim_owner(&db, &stream_a)
        .await
        .expect("A's claim must still exist, now under a new owner");
    assert_eq!(
        owner_a_after, node_b,
        "B must now own A's orphaned sm_session claim"
    );
    assert!(
        epoch_a_after > epoch_a_before,
        "the steal must have bumped the claim epoch (was {epoch_a_before}, now {epoch_a_after})"
    );

    // The control claim (B's own) must be COMPLETELY untouched: same
    // owner, same epoch, byte-for-byte — proving the reaper's targeted
    // scan never re-steals/re-hydrates a survivor's own already-claimed
    // session a second time.
    let (owner_b_after, epoch_b_after) = sm_session_claim_owner(&db, &stream_b)
        .await
        .expect("B's own claim must still exist");
    assert_eq!(
        owner_b_after, node_b,
        "B's own claim must still be owned by B"
    );
    assert_eq!(
        epoch_b_after, epoch_b_before,
        "B's own claim epoch must be byte-for-byte unchanged — never re-stolen/re-hydrated"
    );

    drop(server_b);
    drop(client_a);
    drop(client_b);
}

/// ADR-0017 Phase 3 Slice 7 (deviation 21/34, council-adjudicated): the
/// deposed-owner-with-live-socket harness scenario's `RoomActor` variant —
/// bound to this slice per the phase plan's own text ("the `RoomActor`
/// variant of this same scenario is NOT carried [to Phase 4] — it already
/// lands in Slice 7"). The `UserActor` variant remains deferred to
/// whichever Phase 4 slice first wires `UserActor` Postgres claims
/// (deviation 34) — no production `UserActor` claim-acquisition call site
/// exists in this phase.
///
/// Scenario: a genuinely wedged, genuinely-claimed `RoomActor` (this node
/// really holds its Postgres claim via `PostgresClaimStore`), contested via
/// the steal-intent veto path (Slice 3, element 4's "Unwedge" text) —
/// proving the veto scan's health-check-fails branch reaches a real
/// `RoomActor` through `RoomLocalClaims`, not just `self_fence.rs`'s own
/// `FakeLocalClaims` unit tests. "Genuinely wedged" is produced only after
/// the room has completed durable restore and been published: its next
/// config persist never resolves, so the actor's mailbox loop is genuinely,
/// durably stuck processing a production mutation ahead of the health check.
/// This preserves the "wedged, not merely slow" precondition without asking
/// the registry to publish an actor whose initial restore never completed.
///
/// No cross-node proxy/production steal-intent reporter exists this phase
/// (Slice 3's `report_steal_intent` has no production caller until a
/// future slice wires cross-node MUC routing), so — mirroring Slice 3's
/// own dedicated tests — this harness seeds the steal-intent row directly
/// via `NodeLeaseStore::report_steal_intent` rather than faking a
/// production reporter call site that does not exist yet.
///
/// Runs the REAL `self_fence::run_node_lease` loop (not a reimplementation)
/// against real Postgres, with a short heartbeat interval, and asserts the
/// wedged `RoomActor` is hard-killed within one heartbeat interval of the
/// intent being seeded — "reconciliation conflict-closes within one
/// heartbeat interval," the exact bound element 4's veto-scan text
/// promises.
#[tokio::test]
async fn deposed_owner_with_live_socket_room_actor_scenario() {
    use std::future::pending;
    use std::pin::Pin;
    use waddle_server::clustering::claims::NodeLeaseStore as _;
    use waddle_server::clustering::local_claims::RoomLocalClaims;
    use waddle_server::clustering::self_fence::{
        self, ConnectedPeerCount, LocallyClaimedEntities, NodeLeaseRunConfig,
    };
    use waddle_server::clustering::NodeLifecycle;
    use waddle_server::config::{ClusteringNodeLeaseConfig, ClusteringSelfFenceConfig};
    use waddle_xmpp::muc::durable::{
        DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    };
    use waddle_xmpp::muc::room_actor::{HealthCheck, UpdateConfig};
    use waddle_xmpp::muc::{RoomConfig, RoomRegistry};
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    let Some(url) = std::env::var("WADDLE_TEST_POSTGRES_URL").ok() else {
        return;
    };
    let _guard = cluster_e2e_serial_lock().lock().await;

    let db = open_control_db(&url).await;
    reset_node_lease_tables(&db).await;
    db.guard()
        .await
        .expect("guard")
        .execute("DELETE FROM clustering_steal_intents", ())
        .await
        .expect("clean steal intents");

    // Initial restore succeeds so the room can be published. Its first
    // durable config mutation is the deliberate post-publication wedge.
    struct HangingDurableStore {
        expected_fence: RoomClaimFenceContext,
    }

    impl HangingDurableStore {
        fn new(expected_fence: RoomClaimFenceContext) -> Self {
            Self { expected_fence }
        }

        fn validate_fence(
            &self,
            room_jid: &jid::BareJid,
            fence: &RoomClaimFenceContext,
        ) -> Result<(), waddle_xmpp::XmppError> {
            let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
            if fence.entity == expected_entity && fence == &self.expected_fence {
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::internal(
                    "test store received an unexpected exact room claim fence",
                ))
            }
        }
    }

    impl MucDurableStore for HangingDurableStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let validation = self.validate_fence(room_jid, fence);
            Box::pin(async move {
                validation?;
                Ok(None)
            })
        }

        fn save_config_fenced<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a RoomConfig,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            if let Err(error) = self.validate_fence(room_jid, fence) {
                return Box::pin(async move { Err(error) });
            }
            Box::pin(pending()) as Pin<Box<_>>
        }

        fn save_subject_fenced<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = self.validate_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn save_affiliation_fenced<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = self.validate_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn delete_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = self.validate_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let matches = self.validate_fence(room_jid, fence).is_ok();
            Box::pin(async move { Ok(matches) })
        }
    }

    let identity = NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let node_lease_store = PostgresClaimStore::new(db.clone());
    node_lease_store
        .register(&identity, None)
        .await
        .expect("register node lease");
    let claim_store: std::sync::Arc<dyn ClaimStore> =
        std::sync::Arc::new(PostgresClaimStore::new(db.clone()));
    let room_jid: jid::BareJid = format!("wedged-{}@muc.example.com", uuid::Uuid::new_v4())
        .parse()
        .expect("valid room JID");
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    let expected_epoch = claim_store
        .ensure_claimed(&entity, &identity)
        .await
        .expect("pre-acquire the exact room claim expected by the durable-store fake");
    let expected_fence =
        RoomClaimFenceContext::new(entity.clone(), identity.clone(), expected_epoch);

    let occupant_id_secret =
        OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
            .expect("valid test occupant-id secret");
    let room_registry =
        RoomRegistry::spawn("muc.example.com".to_string(), occupant_id_secret, None);
    room_registry
        .wire_clustering_claims(
            std::sync::Arc::clone(&claim_store),
            SharedNodeIdentity::new(identity.clone()),
            Some(std::sync::Arc::new(HangingDurableStore::new(
                expected_fence,
            ))),
            None,
        )
        .await;

    let room_local_claims = RoomLocalClaims::new();
    room_local_claims.wire(room_registry.clone());

    let actor_ref = room_registry
        .get_or_create_room(
            room_jid.clone(),
            "w-1".to_string(),
            "c-1".to_string(),
            RoomConfig::default(),
        )
        .await
        .expect("get_or_create_room genuinely claims and spawns the room")
        .actor_ref;

    actor_ref
        .tell(UpdateConfig {
            config: RoomConfig::default(),
        })
        .await
        .expect("enqueue post-publication mutation wedge");

    // Confirm genuinely wedged before proceeding: the config update is FIFO
    // ahead of this bounded health ask and cannot finish its durable persist.
    let health = actor_ref
        .ask(HealthCheck)
        .mailbox_timeout(Duration::from_millis(300))
        .reply_timeout(Duration::from_millis(300))
        .await;
    assert!(
        health.is_err(),
        "the room actor must be genuinely wedged for this scenario to be meaningful"
    );

    // Seed the steal-intent (Slice 3's veto path) from a distinct
    // "reporter" identity — mirroring Slice 3's own dedicated tests, since
    // no production reporter call site exists for RoomActor claims yet.
    let reporter = NodeIdentity::new(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    node_lease_store
        .report_steal_intent(&entity, &reporter)
        .await
        .expect("report steal intent");

    // Drive the REAL node-lease loop with a short heartbeat interval so
    // the veto scan's next tick is imminent.
    let heartbeat_interval = Duration::from_millis(200);
    let stop_token = CancellationToken::new();
    let local_claims: std::sync::Arc<dyn LocallyClaimedEntities> = room_local_claims;
    let live_identity = SharedNodeIdentity::new(identity.clone());
    tokio::spawn(self_fence::run_node_lease(
        node_lease_store,
        identity,
        stop_token.clone(),
        NodeLeaseRunConfig {
            pod_template_hash: None,
            lease_config: ClusteringNodeLeaseConfig {
                heartbeat_interval,
                lease_ttl: Duration::from_secs(30),
                claim_release_budget: Duration::from_secs(5),
            },
            self_fence_config: ClusteringSelfFenceConfig::default(),
            connected_peers: ConnectedPeerCount::new(),
            local_claims,
            readiness: NodeLifecycle::new(),
            live_identity,
            peer_id: None,
            claim_store,
            claim_release_budget: Duration::from_secs(5),
        },
    ));

    // The veto scan's next tick must observe the health-check failure and
    // proactively demote (hard-kill) the wedged room actor —
    // "reconciliation conflict-closes within one heartbeat interval" once
    // the health-ask itself resolves. `RoomLocalClaims::health_check`'s own
    // bounded ask (`ROOM_HEALTH_CHECK_TIMEOUT`, 5s in production) is the
    // dominant term, not the heartbeat interval itself (which only governs
    // how promptly the scan *starts*) — the deadline below accounts for
    // that bound plus one heartbeat interval of scheduling slack.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8) + heartbeat_interval;
    loop {
        if !actor_ref.is_alive() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the wedged, genuinely-claimed RoomActor was never hard-killed by the \
             veto scan within the expected window"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    stop_token.cancel();
}

/// ADR-0017 Phase 4: the MUC join proxy wire shape, over two real processes.
/// Phase 3 deliberately bounced this exact case because the room owner was
/// fresh on another node and no cross-node MUC routing existed yet. Phase 4
/// changes that contract: node B must preserve node A as the single
/// authoritative RoomActor writer and proxy the join over the ordered relay,
/// carrying the owner-built XEP-0045 replies back to client B.
///
/// Client A joins a fresh instant room on node A (a real production join:
/// `RoomRegistryActor::acquire_room_claim` genuinely acquires the Postgres
/// claim under node A's identity) and stays connected, keeping A's
/// node-lease fresh — `steal_from_dead_owner` only ever applies to a
/// stale/dead owner (see `owner_lease_fresh`), so a live owner's claim is
/// never contested. Client B then joins the SAME room JID against node B:
/// node B's own `RoomRegistryActor` sees a fresh foreign owner, proxies the
/// join, and must return the owner node's roster/self-presence/subject
/// sequence instead of a local retry error.
#[tokio::test]
async fn muc_join_routes_to_foreign_room_owner() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping muc_join_routes_to_foreign_room_owner: \
             WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    const DOMAIN: &str = "localhost";
    const OWNER_USERNAME: &str = "admin";
    const NS_MUC: &str = "http://jabber.org/protocol/muc";

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, _node_a, _peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, _node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

    let owner_password = server_b.fixed_account_password().to_string();

    let room = format!("slice11-foreign-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Client A joins first, against node A: an XEP-0045 instant-room join
    // (bare `<x xmlns='.../muc'/>`), which genuinely creates the room and
    // grants Owner — the real production `acquire_room_claim` path, not a
    // seeded/faked claim.
    let resource_a = format!("muc-owner-a-{}", uuid::Uuid::new_v4());
    let mut client_a = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        OWNER_USERNAME,
        &owner_password,
        &resource_a,
    )
    .await
    .expect("client A connects to node A");
    client_a
        .send(&format!(
            r#"<presence to="{room}/owner-a"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("client A sends join presence");
    client_a
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("client A's join completes (room created, A is Owner)");

    // Client B, a SEPARATE bare JID on node B (a different process),
    // attempts to join the SAME room while A is still live. Node B must not
    // steal the room claim or bounce locally; it proxies the join to node A
    // and writes node A's XEP-0045 join replies back to B's client.
    let resource_b = format!("muc-joiner-b-{}", uuid::Uuid::new_v4());
    let mut client_b = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        DOMAIN,
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &resource_b,
    )
    .await
    .expect("client B connects to node B");
    client_b
        .send(&format!(
            r#"<presence to="{room}/joiner-b"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("client B sends join presence against the foreign-owned room");

    let join_replies = client_b
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("client B's remote-owner join completes");

    assert!(
        join_replies.iter().all(|frame| {
            !(frame.contains("<presence") && frame.contains("type='error'"))
        }),
        "remote MUC join must not fall back to the Phase 3 resource-constraint bounce: {join_replies:?}"
    );
    assert!(
        join_replies.iter().any(|frame| {
            frame.contains("<presence")
                && (frame.contains("from='") || frame.contains("from=\""))
                && frame.contains("/joiner-b")
                && (frame.contains("status code='110'") || frame.contains("status code=\"110\""))
        }),
        "remote MUC join must include XEP-0045 self-presence status 110: {join_replies:?}"
    );

    client_b
        .close()
        .await
        .expect("client B closes after remote-owner join");
    let unavailable = client_a
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame.contains(&room)
                && frame.contains("/joiner-b")
                && (frame.contains("type='unavailable'") || frame.contains("type=\"unavailable\""))
        })
        .await
        .expect("client A receives remote MUC unavailable for client B");
    assert!(
        unavailable.contains(&room) && unavailable.contains("/joiner-b"),
        "remote MUC cleanup must relay occupant unavailable through the owner node: {unavailable}"
    );

    drop(server_a);
    drop(server_b);
    drop(client_a);
}

/// #1445: a Muji (XEP-0272) `session-initiate` whose signaling lands on
/// the replica that does NOT own the room actor must succeed — relayed
/// to the claim owner, minted there, replies written back through the
/// receiving node — instead of the historical `room_not_found`
/// `<forbidden/>` denial (~29% of production joins at 2 replicas).
///
/// Negative control: a room no node has ever created (no claim row
/// anywhere) still denies with `<forbidden/>` — the relay must not turn
/// genuinely-nonexistent rooms into retry loops.
#[tokio::test]
async fn muji_initiate_routes_to_foreign_room_owner() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping muji_initiate_routes_to_foreign_room_owner: \
             WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    const DOMAIN: &str = "localhost";
    const OWNER_USERNAME: &str = "admin";
    const NS_MUC: &str = "http://jabber.org/protocol/muc";
    // The api-secret must be >= 32 bytes; none of these reach a real
    // LiveKit — the mint is a pure local JWT signature.
    let livekit_envs: &[(&'static str, &'static str)] = &[
        ("LIVEKIT_API_KEY", "APItestkeycluster"),
        (
            "LIVEKIT_API_SECRET",
            "test-secret-with-at-least-32-bytes-of-payload",
        ),
        ("LIVEKIT_WS_URL", "wss://livekit.example.test"),
        ("LIVEKIT_TURN_HOST", "turn.example.test"),
        (
            "LIVEKIT_TURN_SHARED_SECRET",
            "turn-shared-secret-value-also-long-enough",
        ),
        (
            "LIVEKIT_WEBHOOK_SECRET",
            "test-webhook-secret-with-at-least-32-bytes",
        ),
    ];

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, _node_a, _peer_a) = spawn_cluster_server_with_envs(
        &postgres_url,
        &pool.pool_env,
        port_a,
        &[port_b],
        livekit_envs,
    )
    .await;
    let (server_b, _node_b, _peer_b) = spawn_cluster_server_with_envs(
        &postgres_url,
        &pool.pool_env,
        port_b,
        &[port_a],
        livekit_envs,
    )
    .await;

    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

    let owner_password = server_b.fixed_account_password().to_string();
    let room = format!("muji-foreign-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Client A creates and joins the room on node A — node A holds the
    // room claim and the occupancy.
    let resource_a = format!("muji-owner-a-{}", uuid::Uuid::new_v4());
    let mut client_a = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        OWNER_USERNAME,
        &owner_password,
        &resource_a,
    )
    .await
    .expect("client A connects to node A");
    client_a
        .send(&format!(
            r#"<presence to="{room}/owner-a"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("client A sends join presence");
    client_a
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("client A's join completes (room created, A is Owner)");

    // Client B joins the same room THROUGH NODE B (relayed join), so B
    // is an occupant whose occupancy lives on node A.
    let resource_b = format!("muji-joiner-b-{}", uuid::Uuid::new_v4());
    let mut client_b = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        DOMAIN,
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &resource_b,
    )
    .await
    .expect("client B connects to node B");
    client_b
        .send(&format!(
            r#"<presence to="{room}/joiner-b"><x xmlns="{NS_MUC}"/></presence>"#
        ))
        .await
        .expect("client B sends join presence");
    client_b
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("client B's relayed join completes");

    // Client B's Muji session-initiate hits node B, which has no room
    // actor. Pre-#1445 this denied with room_not_found; now it must be
    // relayed to node A, minted there, and answered with the IQ ack +
    // the focus's session-accept carrying a real token.
    client_b
        .send(&String::from(&muji_initiate_iq(&room, "cross-node-1")))
        .await
        .expect("client B sends Muji session-initiate against node B");
    let replies = client_b
        .recv_until(|frame| frame.contains("session-accept"))
        .await
        .expect("client B receives the focus's session-accept via the relay");
    assert!(
        replies.iter().all(|frame| !frame.contains("<forbidden")
            && !frame.contains("type='error'")
            && !frame.contains("type=\"error\"")),
        "cross-node Muji join must not be denied: {replies:?}"
    );
    assert!(
        replies.iter().any(|frame| {
            frame.contains("session-accept")
                && frame.contains("<token")
                && frame.contains("isfocus")
        }),
        "the relayed mint must produce a token-bearing session-accept: {replies:?}"
    );

    // Cross-node teardown (#1445): the terminate must reach the SAME
    // node the initiate registered on, or the owner keeps a phantom
    // in-call participant — which additionally suppresses DeleteRoom
    // for every other occupant. Here we assert only that the relayed
    // terminate is accepted rather than erroring; the owner-side
    // registry effect is pinned deterministically by
    // `jingle_muji_relay`'s unit tests, which can observe the SFU
    // registry directly instead of inferring it from the wire.
    client_b
        .send(&String::from(&muji_terminate_iq(&room, "cross-node-1")))
        .await
        .expect("client B sends Muji session-terminate against node B");
    let terminate_reply = client_b
        .recv_matching(|frame| frame.contains("mjt-cross-node-1"))
        .await
        .expect("the relayed terminate is answered");
    assert!(
        !terminate_reply.contains("<error"),
        "a cross-node hangup must never fail: {terminate_reply}"
    );

    // Negative control: a room with NO claim anywhere is genuinely
    // nonexistent — the terminal room_not_found denial survives.
    let ghost = format!("muji-ghost-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    client_b
        .send(&String::from(&muji_initiate_iq(&ghost, "ghost-1")))
        .await
        .expect("client B sends Muji session-initiate for an unclaimed room");
    let denial = client_b
        .recv_matching(|frame| frame.contains("mji-ghost-1") && frame.contains("<error"))
        .await
        .expect("unclaimed room still denies");
    assert!(
        denial.contains("<forbidden"),
        "unclaimed-room denial must keep the historical <forbidden/> shape: {denial}"
    );

    client_b.close().await.expect("client B closes");
    drop(server_a);
    drop(server_b);
    drop(client_a);
}

/// #1594: a `participant_joined` webhook that lands on the replica NOT
/// holding the room's claim must converge that participant's media
/// grants immediately — relayed to the claim owner, which re-derives
/// the occupant's XEP-0045 voice from its authoritative room actor and
/// pushes it to LiveKit. Observable as the owner's Twirp
/// `UpdateParticipant` call against the (mocked) LiveKit admin API,
/// where pre-#1594 the delivery was merely acknowledged and enforcement
/// waited for the owner's 60s reconciliation tick.
#[tokio::test]
async fn participant_joined_webhook_reasserts_grants_on_foreign_room_owner() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping participant_joined_webhook_reasserts_grants_on_foreign_room_owner: \
             WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    const DOMAIN: &str = "localhost";
    const OWNER_USERNAME: &str = "admin";
    const WEBHOOK_SECRET: &str = "test-webhook-secret-with-at-least-32-bytes";

    // Fake LiveKit admin origins, ONE PER NODE. `admin_base_url_from_ws`
    // derives the admin REST base by swapping the `LIVEKIT_WS_URL`
    // scheme (`ws://` → `http://`) on the same authority, so giving
    // each node its own mock attributes every admin call to the node
    // that made it — the assertion below is "the OWNER pushed", not
    // "someone pushed".
    async fn fake_livekit_admin() -> (wiremock::MockServer, &'static str) {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&mock)
            .await;
        let ws_url: &'static str =
            Box::leak(mock.uri().replacen("http://", "ws://", 1).into_boxed_str());
        (mock, ws_url)
    }
    let (livekit_admin_a, ws_url_a) = fake_livekit_admin().await;
    let (livekit_admin_b, ws_url_b) = fake_livekit_admin().await;

    fn livekit_envs(ws_url: &'static str) -> Vec<(&'static str, &'static str)> {
        vec![
            ("LIVEKIT_API_KEY", "APItestkeycluster"),
            (
                "LIVEKIT_API_SECRET",
                "test-secret-with-at-least-32-bytes-of-payload",
            ),
            ("LIVEKIT_WS_URL", ws_url),
            ("LIVEKIT_TURN_HOST", "turn.example.test"),
            (
                "LIVEKIT_TURN_SHARED_SECRET",
                "turn-shared-secret-value-also-long-enough",
            ),
            ("LIVEKIT_WEBHOOK_SECRET", WEBHOOK_SECRET),
        ]
    }

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, _node_a, _peer_a) = spawn_cluster_server_with_envs(
        &postgres_url,
        &pool.pool_env,
        port_a,
        &[port_b],
        &livekit_envs(ws_url_a),
    )
    .await;
    let (server_b, _node_b, _peer_b) = spawn_cluster_server_with_envs(
        &postgres_url,
        &pool.pool_env,
        port_b,
        &[port_a],
        &livekit_envs(ws_url_b),
    )
    .await;

    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

    let owner_password = server_b.fixed_account_password().to_string();
    let room = format!("grants-foreign-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    // Client A creates and joins the room on node A — node A holds the
    // room claim and the authoritative occupancy.
    let resource_a = format!("grants-owner-a-{}", uuid::Uuid::new_v4());
    let mut client_a = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        OWNER_USERNAME,
        &owner_password,
        &resource_a,
    )
    .await
    .expect("client A connects to node A");
    client_a
        .send(&String::from(&muc_join_presence(&room, "owner-a")))
        .await
        .expect("client A sends join presence");
    client_a
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("client A's join completes (room created, A is Owner)");

    // Client B joins the same room THROUGH NODE B (relayed join), so
    // B's occupancy — and therefore B's voice — lives on node A while
    // node B has no room actor.
    let resource_b = format!("grants-joiner-b-{}", uuid::Uuid::new_v4());
    let mut client_b = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        DOMAIN,
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &resource_b,
    )
    .await
    .expect("client B connects to node B");
    client_b
        .send(&String::from(&muc_join_presence(&room, "joiner-b")))
        .await
        .expect("client B sends join presence");
    client_b
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("client B's relayed join completes");

    // LiveKit reports B's SFU join to node B — the NON-owning replica.
    // Pre-#1594 this was acknowledged with grants unenforced until the
    // owner's reconcile tick; now node B must relay the re-assert to
    // node A synchronously with the delivery.
    let identity = format!("{CLUSTER_PEER_USERNAME}@{DOMAIN}/{resource_b}");
    let body = serde_json::to_vec(&serde_json::json!({
        "id": format!("EV_{}", uuid::Uuid::new_v4()),
        "event": "participant_joined",
        "room": { "name": room },
        "participant": { "identity": identity },
    }))
    .expect("webhook body");
    let response = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/livekit/webhook",
            server_b.http_base_url()
        ))
        .header("Authorization", livekit_webhook_auth(WEBHOOK_SECRET, &body))
        .body(body)
        .send()
        .await
        .expect("post LiveKit webhook to the non-owning node");
    assert!(
        response.status().is_success(),
        "a cross-node re-assertable join must be acknowledged, got {}",
        response.status()
    );

    // The OWNER (node A) pushes the re-derived grant to LiveKit. The
    // push is fire-and-forget after the relay reply, so poll briefly.
    // Each node has its own mock, so this attributes the push to node
    // A structurally, not by timing alone.
    let is_update_for_identity = |request: &wiremock::Request| {
        request.url.path() == "/twirp/livekit.RoomService/UpdateParticipant"
            && String::from_utf8_lossy(&request.body).contains(&identity)
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let requests = livekit_admin_a
            .received_requests()
            .await
            .unwrap_or_default();
        if requests.iter().any(is_update_for_identity) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the claim owner (node A) must push an UpdateParticipant for the \
             relayed re-assert; node A admin requests seen: {:?}",
            requests
                .iter()
                .map(|request| request.url.path().to_string())
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // And the non-owning webhook receiver must NOT have pushed it —
    // node B has no local actor, so an UpdateParticipant from B would
    // mean the relay was bypassed by some local fallback.
    let node_b_requests = livekit_admin_b
        .received_requests()
        .await
        .unwrap_or_default();
    assert!(
        !node_b_requests.iter().any(is_update_for_identity),
        "the non-owning node must not push grants for this join; node B \
         admin requests seen: {:?}",
        node_b_requests
            .iter()
            .map(|request| request.url.path().to_string())
            .collect::<Vec<_>>()
    );

    client_b.close().await.expect("client B closes");
    drop(server_a);
    drop(server_b);
    drop(client_a);
}

/// MUC join `<presence/>` built with the typed XML builder (repo
/// XML-generation rule: never construct XML with `format!`).
fn muc_join_presence(room: &str, nick: &str) -> xmpp_parsers::minidom::Element {
    use xmpp_parsers::minidom::Element;
    const NS_MUC: &str = "http://jabber.org/protocol/muc";
    Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            format!("{room}/{nick}"),
        )
        .append(Element::builder("x", NS_MUC).build())
        .build()
}

/// Signed `Authorization` header for a synthetic LiveKit webhook body,
/// matching LiveKit's scheme: a JWT over the body's SHA-256, signed
/// with the deployment's webhook secret.
fn livekit_webhook_auth(secret: &str, body: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(body);
    let claims = serde_json::json!({
        "sha256": base64::engine::general_purpose::STANDARD.encode(hasher.finalize()),
        "exp": (chrono::Utc::now() + chrono::Duration::seconds(60)).timestamp(),
        "iat": chrono::Utc::now().timestamp(),
    });
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("sign LiveKit webhook");
    format!("Bearer {token}")
}

/// Muji `session-terminate` IQ for `room` — the hangup counterpart of
/// [`muji_initiate_iq`]. XEP-0272 requires only the `<muji room='…'/>`
/// marker on terminate, no contents.
fn muji_terminate_iq(room: &str, sid: &str) -> xmpp_parsers::minidom::Element {
    use waddle_xmpp::xep::xep0166::NS_JINGLE;
    use waddle_xmpp::xep::xep0272::NS_MUJI;
    use xmpp_parsers::minidom::Element;

    let jingle = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-terminate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid)
        .append(
            Element::builder("muji", NS_MUJI)
                .attr(minidom::rxml::xml_ncname!("room").to_owned(), room)
                .build(),
        )
        .build();
    Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            format!("mjt-{sid}"),
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "calls.localhost",
        )
        .append(jingle)
        .build()
}

/// Muji `session-initiate` IQ for `room`, built with minidom builders
/// per the repo's XML-generation rule (id = `mji-{sid}`).
fn muji_initiate_iq(room: &str, sid: &str) -> xmpp_parsers::minidom::Element {
    use waddle_xmpp::xep::xep0166::NS_JINGLE;
    use waddle_xmpp::xep::xep0167::NS_JINGLE_RTP;
    use waddle_xmpp::xep::xep0272::NS_MUJI;
    use waddle_xmpp::xep::xep_waddle_livekit_transport::NS_WADDLE_LIVEKIT_TRANSPORT;
    use xmpp_parsers::minidom::Element;

    let payload_type = Element::builder("payload-type", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "111")
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "opus")
        .attr(minidom::rxml::xml_ncname!("clockrate").to_owned(), "48000")
        .attr(minidom::rxml::xml_ncname!("channels").to_owned(), "2")
        .build();
    let description = Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), "audio")
        .append(payload_type)
        .build();
    let content = Element::builder("content", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("creator").to_owned(),
            "initiator",
        )
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "audio")
        .append(description)
        .append(Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build())
        .build();
    let jingle = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid)
        .append(content)
        .append(
            Element::builder("muji", NS_MUJI)
                .attr(minidom::rxml::xml_ncname!("room").to_owned(), room)
                .build(),
        )
        .build();
    Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            format!("mji-{sid}"),
        )
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            "calls.localhost",
        )
        .append(jingle)
        .build()
}

/// ADR-0017 Phase 4: subscription presence and probes use the same ordered
/// relay as direct messages, while preserving RFC 6121 sender-local side
/// effects on the node that owns the originating resource.
#[tokio::test]
async fn subscription_presence_and_probe_route_to_foreign_user_owner() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping subscription_presence_and_probe_route_to_foreign_user_owner: \
             WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    let db = open_control_db(&postgres_url).await;
    let pool = generate_pool();
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;
    reset_fixed_pair_roster(&db).await;

    const DOMAIN: &str = "localhost";
    const ADMIN_BARE: &str = "admin@localhost";
    const PEER_BARE: &str = "cluster-peer@localhost";

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let (server_a, node_a, _peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b]).await;
    let (server_b, node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a]).await;

    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;

    // Both subprocesses share the same Postgres `users` table, and
    // `TestServer` reseeds the fixed admin account on startup. Server B is
    // started last, so its generated password is the live credential for
    // admin on both nodes.
    let admin_password = server_b.fixed_account_password().to_string();
    let admin_resource = format!("sub-owner-a-{}", uuid::Uuid::new_v4());
    let peer_resource = format!("sub-peer-b-{}", uuid::Uuid::new_v4());
    let mut admin = WsXmppClient::connect_and_auth(
        &server_a.ws_url(),
        DOMAIN,
        "admin",
        &admin_password,
        &admin_resource,
    )
    .await
    .expect("admin connects to node A");
    let mut peer = WsXmppClient::connect_and_auth(
        &server_b.ws_url(),
        DOMAIN,
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &peer_resource,
    )
    .await
    .expect("cluster peer connects to node B");

    let _ = send_roster_get(&mut admin, "cluster-admin-roster-init").await;
    let _ = send_roster_get(&mut peer, "cluster-peer-roster-init").await;
    admin
        .send(&presence_xml(
            xmpp_parsers::presence::Type::None,
            None,
            Some("cluster-ready"),
        ))
        .await
        .expect("admin sends available presence");
    peer.send(&presence_xml(
        xmpp_parsers::presence::Type::None,
        None,
        None,
    ))
    .await
    .expect("peer sends available presence");

    peer.send(&presence_xml(
        xmpp_parsers::presence::Type::Subscribe,
        Some(ADMIN_BARE),
        None,
    ))
    .await
    .expect("peer sends cross-node subscribe");
    let peer_pending_push = peer
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains(ADMIN_BARE)
                && frame_has_attr(frame, "ask", "subscribe")
        })
        .await
        .expect("peer receives sender-local pending subscribe roster push");
    assert!(
        peer_pending_push.contains(ADMIN_BARE),
        "sender-local subscribe roster push must name admin contact: {peer_pending_push}"
    );
    let admin_subscribe = admin
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame_has_attr(frame, "type", "subscribe")
                && frame_attr_starts_with(frame, "from", PEER_BARE)
        })
        .await
        .expect("admin receives subscribe through ordered relay");
    assert!(
        frame_has_attr(&admin_subscribe, "type", "subscribe"),
        "remote subscribe must retain RFC 6121 type: {admin_subscribe}"
    );
    use waddle_xmpp::ownership::ClaimStore as _;
    let claim_store = PostgresClaimStore::new(db.clone());
    let admin_claim = claim_store
        .current_claim(&Entity::new(EntityType::UserActor, ADMIN_BARE.to_string()))
        .await
        .expect("admin UserActor claim lookup")
        .expect("admin UserActor claim exists");
    assert_eq!(
        admin_claim.owner.node_id, node_a,
        "admin UserActor claim must stay on node A before approval"
    );
    let peer_claim = claim_store
        .current_claim(&Entity::new(EntityType::UserActor, PEER_BARE.to_string()))
        .await
        .expect("peer UserActor claim lookup")
        .expect("peer UserActor claim exists");
    assert_eq!(
        peer_claim.owner.node_id, node_b,
        "peer UserActor claim must stay on node B before approval"
    );

    admin
        .send(&presence_xml(
            xmpp_parsers::presence::Type::Subscribed,
            Some(PEER_BARE),
            None,
        ))
        .await
        .expect("admin approves peer subscription");
    let admin_approval_push = admin
        .recv_matching(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains(PEER_BARE)
                && frame_has_attr(frame, "subscription", "from")
        })
        .await
        .expect("admin receives sender-local approval roster push");
    assert!(
        admin_approval_push.contains(PEER_BARE),
        "approval roster catch-up must name peer contact: {admin_approval_push}"
    );
    let mut approval_frames = Vec::new();
    let approval_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < approval_deadline {
        let frame = peer
            .recv_timeout(Duration::from_millis(500))
            .await
            .unwrap_or_default();
        if !frame.is_empty() {
            approval_frames.push(frame);
        }
        let has_roster_push = approval_frames.iter().any(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains(ADMIN_BARE)
                && frame_has_attr(frame, "subscription", "to")
        });
        let has_subscribed = approval_frames.iter().any(|frame| {
            frame.contains("<presence")
                && frame_has_attr(frame, "type", "subscribed")
                && frame_attr_starts_with(frame, "from", ADMIN_BARE)
        });
        let has_current_presence = approval_frames.iter().any(|frame| {
            frame.contains("<presence")
                && !frame_has_attr(frame, "type", "subscribed")
                && frame_attr_starts_with(frame, "from", &format!("{ADMIN_BARE}/"))
                && frame.contains("cluster-ready")
        });
        if has_roster_push && has_subscribed && has_current_presence {
            break;
        }
    }
    let roster_after_approval = fixed_pair_roster_snapshot(&db).await;
    assert!(
        approval_frames.iter().any(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains(ADMIN_BARE)
                && frame_has_attr(frame, "subscription", "to")
        }),
        "peer approval roster push must name admin contact with subscription='to': frames={approval_frames:?}, roster={roster_after_approval:?}"
    );
    assert!(
        approval_frames.iter().any(|frame| {
            frame.contains("<presence")
                && frame_has_attr(frame, "type", "subscribed")
                && frame_attr_starts_with(frame, "from", ADMIN_BARE)
        }),
        "approval must stay an RFC 6121 subscribed presence: {approval_frames:?}"
    );
    let current_presence = approval_frames
        .iter()
        .find(|frame| {
            frame.contains("<presence")
                && !frame_has_attr(frame, "type", "subscribed")
                && frame_attr_starts_with(frame, "from", &format!("{ADMIN_BARE}/"))
                && frame.contains("cluster-ready")
        })
        .unwrap_or_else(|| {
            panic!("peer receives admin current presence after approval: {approval_frames:?}")
        });
    assert!(
        current_presence.contains("cluster-ready"),
        "approval catch-up must relay current presence from admin: {current_presence}"
    );

    peer.send(&presence_xml(
        xmpp_parsers::presence::Type::Probe,
        Some(ADMIN_BARE),
        None,
    ))
    .await
    .expect("peer sends cross-node probe");
    let probe_reply = peer
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame_attr_starts_with(frame, "from", &format!("{ADMIN_BARE}/"))
                && frame.contains("cluster-ready")
        })
        .await
        .expect("peer receives probe reply from foreign user owner");
    assert!(
        !frame_has_attr(&probe_reply, "type", "unsubscribed"),
        "authorized cross-node probe must return current presence, not unsubscribed: {probe_reply}"
    );

    let _ = admin.close().await;
    let _ = peer.close().await;
    drop(server_a);
    drop(server_b);
}

/// ADR-0017 Phase 3 Slice 11 (this slice's own harness-fencing scaffold,
/// deferred from Phase 2, activated here per the phase plan's own Slice 11
/// text). Originally named `partial_partition_degrades_without_fencing`
/// (the plan's own scaffold name) — renamed here, per the Slice 11
/// corrigenda, to describe what this test actually proves: a fully,
/// symmetrically isolated node self-fences and then, unattended, self-heals
/// once connectivity returns. Exit criterion 3 (phase-level) as originally
/// scoped reads "a single dead link among three nodes degrades to the
/// durable queue without fencing either endpoint" — deviations 107/108
/// below explain precisely which half of that criterion this test can and
/// cannot prove; see also the phase-level exit-criteria section's own
/// caveat for criterion 3.
///
/// **Deviation 107 (harness limitation, documented not fabricated) — no
/// pairwise link-severing primitive exists.** This harness's only
/// connectivity-affecting primitive reachable from a NEW test is
/// `clustering_peer_allowlist` revocation (`clustering/allowlist.rs`): a
/// single global `HashSet<PeerId>`, enforced identically and symmetrically
/// by every node's own refresh — revoking a peer_id cuts it off from the
/// WHOLE mesh, never from one specific counterpart. There is no per-pair
/// column, gate, or config knob. The relay actor's own fault-injection
/// messages (`RelayCrash`/`RelaySleep`, `clustering/relay.rs`) are the only
/// other candidate, but triggering either from THIS test would require a
/// `RelayHandle`, which requires this test process itself to join the
/// swarm via `swarm::spawn` — and this file's own module doc states
/// plainly that "kameo's `init_global` is a process singleton," so only ONE
/// test in this whole binary may ever do that; `cluster_exit_criteria_end_to_end`
/// already claims that slot. Building a genuine single-edge partition
/// primitive is squarely a production-code change (e.g. a per-peer
/// connection-gating config knob) — out of scope for a harness-only slice
/// per this slice's own HARD RULES, flagged here rather than added
/// speculatively. Closest faithful substitute implemented below: full,
/// symmetric isolation of ONE of three nodes (the same primitive
/// `lone_survivor_and_isolation_fencing`'s Part 2 uses), extended with an
/// assertion Part 2 does NOT make — genuine, unattended SELF-RECOVERY
/// (re-registration under a fresh identity, `self_fence.rs`'s
/// `readiness.serve()` re-arm path) once connectivity returns, with
/// no process restart. This is the most faithful available proof that a
/// connectivity degradation "degrades... without [permanently] fencing"
/// rather than requiring manual/operator intervention to recover.
///
/// **Deviation 108 (precise restatement, per the Slice 11 corrigenda) —
/// the durable-`pending_delivery`-fallback half of this criterion would be
/// NON-DIAGNOSTIC if asserted inside a partition test, not unobservable.**
/// `OfflineDeliveryHandler`/`queue_offline_delivery`
/// (`server/routes/interpret/offline_delivery.rs`) fires on the purely-LOCAL
/// headless-recipient pass (`route_to_connection.rs`'s
/// `select_bare_jid_live_targets`, which consults only the in-process
/// `UserRegistryActor` — no clustering/claims lookup anywhere in that
/// path), and `clustering/relay.rs`'s own module doc states plainly:
/// discovery/handshake only, "not wired into the stanza delivery path
/// (Phase 4)." That write path is real and harness-observable — a bare-JID
/// message to a recipient whose only live session is on ANOTHER node gets
/// written to `pending_delivery` on the sending node regardless of whether
/// the two nodes' link is healthy or severed, because cross-node liveness
/// is never consulted at all (Phase 4 scope). Asserting that write inside
/// THIS test would therefore re-prove deviation 14's already-recorded
/// Non-goal ("No cross-node janitor-flush test... requires the GA
/// cross-node stanza routing this phase explicitly excludes... deferred to
/// Phase 4") under a partition label it does not need — the write happens
/// identically in a fully healthy two-node cluster, so it demonstrates
/// nothing about partition-triggered fallback behavior specifically. Not
/// re-litigated here; this test proves only what a partition scenario can
/// actually distinguish (targeted isolation-fencing plus genuine
/// self-healing recovery), consistent with that already-settled scope
/// boundary.
///
/// Wall clock: dominated by the isolation-fencing window plus the
/// re-registration/re-dial recovery window (a few seconds each, per this
/// harness's fast-timer subprocess config) — single-digit-to-low-tens of
/// seconds total, reported at the end of this test run.
#[tokio::test(flavor = "multi_thread")]
async fn whole_node_isolation_fences_then_self_heals_without_operator_intervention() {
    let Ok(postgres_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping whole_node_isolation_fences_then_self_heals_without_operator_intervention: \
             WADDLE_TEST_POSTGRES_URL not set"
        );
        return;
    };
    let _serial = cluster_e2e_serial_lock().lock().await;

    let pool = generate_pool();
    let db = open_control_db(&postgres_url).await;
    reset_and_enroll(&db, &pool).await;
    reset_node_lease_tables(&db).await;

    let port_a = free_tcp_port();
    let port_b = free_tcp_port();
    let port_c = free_tcp_port();
    let (server_a, _node_a, _peer_a) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_a, &[port_b, port_c]).await;
    let (server_b, _node_b, _peer_b) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_b, &[port_a, port_c]).await;
    let (server_c, node_c, peer_c) =
        spawn_cluster_server(&postgres_url, &pool.pool_env, port_c, &[port_a, port_b]).await;

    wait_for_readiness(&server_a, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_b, true, Duration::from_secs(15)).await;
    wait_for_readiness(&server_c, true, Duration::from_secs(15)).await;

    // --- Induce (closest available primitive, see deviation 107): revoke
    // C's peer_id cluster-wide.
    let conn = db.guard().await.expect("guard");
    conn.execute(
        "DELETE FROM clustering_peer_allowlist WHERE peer_id = ?",
        waddle_server::db_params![peer_c.clone()],
    )
    .await
    .expect("revoke C");

    // C must self-fence: readiness flips not-ready within a modest
    // deadline (a few allowlist-refresh + isolation-interval windows) —
    // same shape `lone_survivor_and_isolation_fencing`'s Part 2 proves.
    wait_for_readiness(&server_c, false, Duration::from_secs(15)).await;

    // A and B were never isolated from EACH OTHER (only from C) — across
    // the whole isolation window, both must stay ready throughout, proving
    // a targeted fence of the disconnected node, never a cluster-wide
    // wobble that also touches the two nodes that kept their own link
    // alive.
    let hold_deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < hold_deadline {
        assert_eq!(
            readiness_status(&server_a).await,
            Some(true),
            "uninvolved peer A must stay ready throughout C's isolation window"
        );
        assert_eq!(
            readiness_status(&server_b).await,
            Some(true),
            "uninvolved peer B must stay ready throughout C's isolation window"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // --- Degrades WITHOUT [permanently] fencing: re-enroll C and assert
    // genuine, unattended SELF-RECOVERY — no process restart, a fresh
    // internal identity re-registers and readiness re-arms once swarm
    // connectivity actually returns (`self_fence.rs`'s
    // `can_reacquire_claims`/`readiness.serve()` path).
    conn.execute(
        "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
        waddle_server::db_params![peer_c.clone()],
    )
    .await
    .expect("re-enroll C");

    wait_for_readiness(&server_c, true, Duration::from_secs(30)).await;

    // Prove the recovery is REAL re-registration (a fresh `NodeIdentity`),
    // not a stale readiness flag: C's ORIGINAL `clustering_nodes` row must
    // now be committed-expired (`self_fence.rs`'s successful-recovery path
    // explicitly calls `expire_bounded` on the just-superseded identity),
    // and exactly three live (not expired, not draining) rows must exist
    // cluster-wide — A, B, and C's fresh post-recovery identity.
    assert_eq!(
        node_expired_flag(&db, &node_c).await,
        Some(true),
        "C's ORIGINAL clustering_nodes row must be committed-expired after self-healing \
         re-registration under a fresh identity"
    );
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM clustering_nodes WHERE NOT expired AND NOT draining",
            (),
        )
        .await
        .expect("count live nodes");
    let live_count: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("row present")
        .get(0)
        .expect("count column");
    assert_eq!(
        live_count, 3,
        "exactly three live node rows must exist post-recovery: A, B, and C's fresh \
         re-registered identity (C's original row is expired, not deleted)"
    );

    // A and B must still be ready too — full-mesh recovery, not just C's
    // own local view of itself.
    assert_eq!(readiness_status(&server_a).await, Some(true));
    assert_eq!(readiness_status(&server_b).await, Some(true));

    drop(server_a);
    drop(server_b);
    drop(server_c);
}

/// DEFERRED (manual go/no-go measurement): the visibility window of a
/// hard-killed publisher's kademlia provider+metadata records is dominated by
/// kameo's hardcoded 1h record TTL / 30min republish — measure against the
/// acceptance threshold out-of-band; graceful stops proactively unregister.
#[tokio::test]
#[ignore = "manual measurement: dominated by kademlia's hardcoded 1h record TTL"]
async fn dead_publisher_record_visibility_window() {}
