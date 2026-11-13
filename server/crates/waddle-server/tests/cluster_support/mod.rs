//! Multi-process cluster fixtures shared by the clustering test binaries.
//!
//! Each binary that needs real `waddle-server` processes meshed over libp2p
//! against one PostgreSQL control plane declares this module with
//! `pub mod cluster_support;`. The fixtures reset the shared control-plane
//! tables, so every binary using them belongs in nextest's `postgres` group
//! and serializes its own tests through [`cluster_e2e_serial_lock`].

use base64::Engine;
use libp2p::identity::ed25519;
use std::time::{Duration, Instant};
use waddle_server::clustering::allowlist::{AllowlistStore, PostgresAllowlistStore};
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::clustering::lease::{KeypairSlotLease, PostgresKeypairSlotLease};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_ws_test_support::{TestServer, WsXmppClient};

pub const POOL_SIZE: usize = 4;

pub const CLUSTER_PEER_USERNAME: &str = "cluster-peer";

pub const CLUSTER_PEER_PASSWORD: &str = "cluster-peer-password";

pub struct EnrolledPool {
    /// base64-encoded 32-byte ed25519 seeds (the WADDLE_CLUSTERING_KEYPAIR_POOL value).
    pub pool_env: String,
    /// PeerIds derived from every pool slot (all enrolled).
    pub peer_ids: Vec<libp2p::PeerId>,
    pub seeds: Vec<Vec<u8>>,
}

pub fn generate_pool() -> EnrolledPool {
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

pub async fn open_control_db(url: &str) -> Database {
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
pub async fn reset_and_enroll(db: &Database, pool: &EnrolledPool) {
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

/// Reserve an ephemeral TCP port by binding and immediately releasing it.
///
/// Inherently best-effort (bind/release TOCTOU): another process could grab
/// the port before the spawned server binds it. The window is unavoidable
/// here — the mesh needs pre-agreed ports (A's seed list names B's port and
/// vice versa), so neither server can pick its own port at bind time. The
/// kernel avoids immediately reissuing a just-released ephemeral port, and a
/// lost race fails loudly (the server's swarm listen bind errors and the
/// node-id file never appears) rather than corrupting the test.
pub fn free_tcp_port() -> u16 {
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
pub async fn spawn_cluster_server(
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
pub async fn spawn_cluster_server_with_envs(
    postgres_url: &str,
    pool_env: &str,
    swarm_port: u16,
    bootstrap_ports: &[u16],
    extra_envs: &[(&str, &str)],
) -> (TestServer, String, String) {
    spawn_node(
        postgres_url,
        pool_env,
        swarm_port,
        bootstrap_ports,
        &[],
        extra_envs,
    )
    .await
}

/// The stream-management authority a node selects
/// (`WADDLE_XMPP_DURABLE_SM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamManagementAuthority {
    /// The in-memory session registry with detach-time persistence.
    #[default]
    Legacy,
    /// The durable session authority.
    Durable,
}

impl StreamManagementAuthority {
    /// The value the server parses from its environment.
    pub fn env_value(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Durable => "durable",
        }
    }
}

/// The account-resource authority a node selects
/// (`WADDLE_XMPP_ACCOUNT_AUTHORITY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountAuthority {
    /// The connection registry and `UserActor`.
    #[default]
    Registry,
    /// The account coordinator.
    Coordinator,
}

impl AccountAuthority {
    /// The value the server parses from its environment.
    pub fn env_value(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Coordinator => "coordinator",
        }
    }
}

/// The authorities one node selects at startup. The default is what a
/// server selects when neither variable is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeSelectors {
    pub stream_management: StreamManagementAuthority,
    pub account_authority: AccountAuthority,
}

impl NodeSelectors {
    /// Durable stream management with the account coordinator. The
    /// coordinator refuses to start without durable stream management, so
    /// this is the only combination that selects it.
    pub const DURABLE_COORDINATOR: Self = Self {
        stream_management: StreamManagementAuthority::Durable,
        account_authority: AccountAuthority::Coordinator,
    };

    /// Durable stream management with the connection registry.
    pub const DURABLE_REGISTRY: Self = Self {
        stream_management: StreamManagementAuthority::Durable,
        account_authority: AccountAuthority::Registry,
    };

    /// The selector variables of a node's environment. Both are always
    /// stated, so a node started through [`spawn_cluster_node`] never
    /// depends on the server's defaults.
    pub fn envs(self) -> [(&'static str, &'static str); 2] {
        [
            (
                waddle_server::sm_authority::DURABLE_SM_MODE_ENV,
                self.stream_management.env_value(),
            ),
            (
                waddle_server::account_coordinator::ACCOUNT_AUTHORITY_MODE_ENV,
                self.account_authority.env_value(),
            ),
        ]
    }
}

/// One node of a harness cluster: where it listens, whom it dials, which
/// authorities it selects and which accounts it seeds.
///
/// Every node is its own `waddle-server` process, so the selectors travel
/// in that process's environment and two nodes of one test may select
/// different authorities.
#[derive(Debug, Clone, Copy)]
pub struct ClusterNode<'a> {
    pub postgres_url: &'a str,
    pub pool_env: &'a str,
    pub swarm_port: u16,
    pub bootstrap_ports: &'a [u16],
    pub selectors: NodeSelectors,
    /// Accounts seeded beside the fixed account and
    /// [`CLUSTER_PEER_USERNAME`], as `(localpart, password)`. Nodes that
    /// name the same account must name the same password: each node
    /// reseeds the account in the shared database when it starts.
    pub accounts: &'a [(&'a str, &'a str)],
    /// Further variables, applied after the harness's own and after the
    /// selectors.
    pub extra_envs: &'a [(&'a str, &'a str)],
}

/// Spawn the node `node` describes. Returns the server handle plus its
/// published (node_id, peer_id), like [`spawn_cluster_server`].
pub async fn spawn_cluster_node(node: ClusterNode<'_>) -> (TestServer, String, String) {
    let mut envs: Vec<(&str, &str)> = node.selectors.envs().to_vec();
    envs.extend_from_slice(node.extra_envs);
    spawn_node(
        node.postgres_url,
        node.pool_env,
        node.swarm_port,
        node.bootstrap_ports,
        node.accounts,
        &envs,
    )
    .await
}

async fn spawn_node(
    postgres_url: &str,
    pool_env: &str,
    swarm_port: u16,
    bootstrap_ports: &[u16],
    accounts: &[(&str, &str)],
    extra_envs: &[(&str, &str)],
) -> (TestServer, String, String) {
    let accounts: Vec<(String, String)> =
        std::iter::once(&(CLUSTER_PEER_USERNAME, CLUSTER_PEER_PASSWORD))
            .chain(accounts)
            .map(|(username, password)| ((*username).to_owned(), (*password).to_owned()))
            .collect();
    let postgres_url = postgres_url.to_string();
    let pool_env = pool_env.to_string();
    let bootstrap_ports = bootstrap_ports.to_vec();
    let extra_envs: Vec<(String, String)> = extra_envs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
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
            // ADR-0017: inherit the global PostgreSQL pool for every durable
            // store. This keeps clustered SM alongside the claims tables for
            // cross-node resume and avoids ephemeral stores rejected by
            // readiness (#1652). Shared-owner startup rejects per-store URLs.
            ("WADDLE_DATABASE_URL", &postgres_url),
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
        envs.extend(
            extra_envs
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );

        let accounts: Vec<(&str, &str)> = accounts
            .iter()
            .map(|(username, password)| (username.as_str(), password.as_str()))
            .collect();
        let server = TestServer::start_with_extra_envs(&accounts, &envs);

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

/// Serializes the heavy subprocess tests in this binary. They share the
/// mutable control-plane tables (`clustering_nodes`/`clustering_claims`/
/// `clustering_peer_allowlist`/`clustering_keypair_slots`) in one Postgres
/// and each starts by DELETE-resetting them — running concurrently (the
/// cargo test default) would wipe rows the other test's live subprocess
/// servers depend on mid-run.
pub fn cluster_e2e_serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
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
pub async fn reset_node_lease_tables(db: &Database) {
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
pub async fn readiness_status(server: &TestServer) -> Option<bool> {
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
pub async fn wait_for_readiness(server: &TestServer, want_ready: bool, deadline: Duration) {
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

/// Wait until `node_id`'s node lease no longer counts as live.
///
/// A node that was SIGKILLed keeps a live `clustering_nodes` row until its
/// last heartbeat is older than the node-lease TTL this harness configures.
/// A replacement started inside that window is a second live row beside the
/// dead one, and a survivor that has not dialled the replacement yet then
/// sees two live nodes and no reachable peer, which is the isolation rule
/// under which it fences itself and closes its client sessions. Scenarios
/// that replace a killed node and expect the survivor to keep serving wait
/// here before they start the replacement.
pub async fn wait_for_node_lease_lapse(db: &Database, node_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT count(*) FROM clustering_nodes WHERE node_id = ? AND NOT expired \
                 AND NOT draining AND heartbeat >= now() - interval '1200 milliseconds'",
                waddle_server::db_params![node_id.to_owned()],
            )
            .await
            .expect("node lease query");
        let live = rows
            .next()
            .await
            .expect("node lease row")
            .expect("one row")
            .get::<i64>(0)
            .expect("live count");
        if live == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "node {node_id} still holds a live node lease"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait until `second` can register a resource of an account whose claim
/// `first` holds, which it does through `first`'s relay: the two nodes are
/// then connected. A node reports ready before the mesh has formed, and
/// until it has, neither node knows the resources the other hosts.
pub async fn wait_for_cross_node_registration(first: &TestServer, second: &TestServer) {
    let holder = WsXmppClient::connect_and_auth(
        &first.ws_url(),
        "localhost",
        CLUSTER_PEER_USERNAME,
        CLUSTER_PEER_PASSWORD,
        &format!("mesh-probe-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("mesh probe binds on the first node");
    let resource = format!("mesh-probe-{}", uuid::Uuid::new_v4());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match WsXmppClient::connect_and_auth(
            &second.ws_url(),
            "localhost",
            CLUSTER_PEER_USERNAME,
            CLUSTER_PEER_PASSWORD,
            &resource,
        )
        .await
        {
            Ok(probe) => {
                probe.close().await.expect("close the remote mesh probe");
                break;
            }
            Err(error) => assert!(
                Instant::now() < deadline,
                "the nodes did not connect: {error}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    holder.close().await.expect("close the local mesh probe");
}

/// Wait until a stanza `first` accepts for a resource `second` hosts
/// reaches that resource: the two nodes are then connected.
///
/// Unlike [`wait_for_cross_node_registration`] this binds each probe on
/// the node that takes its account's claim, so it also holds for nodes
/// that refuse to bind a resource of an account another node owns. The
/// probe stanza is an IQ, which is answered with an error and never stored
/// while the recipient's node is unknown to the sender's.
pub async fn wait_for_cross_node_stanza(
    first: &TestServer,
    second: &TestServer,
    sender: (&str, &str),
    recipient: (&str, &str),
) {
    use minidom::rxml::xml_ncname;
    use minidom::Element;

    let mut sender_probe =
        WsXmppClient::connect_and_auth(&first.ws_url(), "localhost", sender.0, sender.1, "mesh")
            .await
            .expect("mesh probe binds on the first node");
    let mut recipient_probe = WsXmppClient::connect_and_auth(
        &second.ws_url(),
        "localhost",
        recipient.0,
        recipient.1,
        "mesh",
    )
    .await
    .expect("mesh probe binds on the second node");
    let to = format!("{}@localhost/mesh", recipient.0);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut attempt = 0_u32;
    'connected: loop {
        attempt += 1;
        let id = format!("mesh-{attempt}");
        let ping = Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(xml_ncname!("type").to_owned(), "get")
            .attr(xml_ncname!("id").to_owned(), id.as_str())
            .attr(xml_ncname!("to").to_owned(), to.as_str())
            .append(Element::builder("ping", "urn:xmpp:ping").build())
            .build();
        sender_probe
            .send(&String::from(&ping))
            .await
            .expect("send the mesh probe");
        let listen_until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < listen_until {
            if let Ok(frame) = recipient_probe
                .recv_timeout(Duration::from_millis(100))
                .await
            {
                if frame.contains("urn:xmpp:ping") {
                    break 'connected;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "no stanza crossed between the nodes within 30 s"
        );
    }
    sender_probe
        .close()
        .await
        .expect("close the sending mesh probe");
    recipient_probe
        .close()
        .await
        .expect("close the receiving mesh probe");
}

/// Advance the shared database to the mailbox protocol epoch, at which
/// nodes that select the mailbox carry remote room requests and direct
/// stanzas as mailbox entries instead of ordered-relay messages.
pub async fn activate_mailbox_epoch(db: &Database) {
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

pub async fn join_fanout_room(client: &mut WsXmppClient, room: &jid::BareJid, nick: &str) {
    client
        .send(&String::from(&muc_join_presence(&room.to_string(), nick)))
        .await
        .expect("send MUC join");
    let replies = tokio::time::timeout(
        Duration::from_secs(15),
        client.recv_until(|frame| frame.contains("<subject")),
    )
    .await
    .expect("bounded MUC join")
    .expect("MUC join replies");
    let occupant = room
        .with_resource_str(nick)
        .expect("occupant JID")
        .to_string();
    assert!(
        replies.iter().any(|frame| {
            frame.parse::<minidom::Element>().is_ok_and(|element| {
                element.is("presence", waddle_xmpp::ns::JABBER_CLIENT)
                    && element.attr("from") == Some(occupant.as_str())
                    && element.attr("type").is_none()
                    && element
                        .get_child("x", "http://jabber.org/protocol/muc#user")
                        .is_some_and(|x| {
                            x.children().any(|child| {
                                child.is("status", "http://jabber.org/protocol/muc#user")
                                    && child.attr("code") == Some("110")
                            })
                        })
            })
        }),
        "{occupant} must receive self-presence 110: {replies:?}"
    );
}

pub fn fanout_message(room: &jid::BareJid, id: &str, body: Option<&str>) -> minidom::Element {
    use minidom::rxml::xml_ncname;
    use minidom::Element;
    let payload = match body {
        Some(body) => Element::builder("body", waddle_xmpp::ns::JABBER_CLIENT)
            .append(body)
            .build(),
        None => Element::builder("composing", waddle_xmpp::xep::xep0085::NS_CHATSTATES).build(),
    };
    Element::builder("message", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(xml_ncname!("to").to_owned(), room.to_string())
        .attr(xml_ncname!("type").to_owned(), "groupchat")
        .attr(xml_ncname!("id").to_owned(), id)
        .append(payload)
        .build()
}

/// MUC join `<presence/>` built with the typed XML builder (repo
/// XML-generation rule: never construct XML with `format!`).
pub fn muc_join_presence(room: &str, nick: &str) -> xmpp_parsers::minidom::Element {
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
