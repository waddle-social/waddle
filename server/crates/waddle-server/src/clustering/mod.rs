//! ADR-0017 Phase 2 — owned libp2p swarm subsystem (node discovery only).
//!
//! Strictly additive and flag-gated. With `clustering.enabled` false (the
//! default) none of this runs and server behaviour is byte-for-byte identical
//! to the single-replica path. The subsystem is additionally gated behind the
//! `clustering` Cargo feature, which pulls in `kameo/remote` + libp2p; a
//! default build links no libp2p at all.
//!
//! This phase is **discovery only**: kademlia resolves peers, but no stanza is
//! routed cross-node (that is Phase 4). `replicaCount > 1` and
//! `clustering.enabled` stay hard-locked in the Helm chart until Phase 4, so
//! the subsystem is exercised only by the multi-process test harness.

use crate::config::ClusteringConfig;
use crate::db::{Database, DatabaseDriver};
use tokio_util::sync::CancellationToken;

/// Peer-allowlist store. Public: the multi-process cluster harness
/// provisions the control-plane schema through the same `ensure_schema`
/// path production uses, never a diverging inline DDL copy.
#[cfg(feature = "clustering")]
pub mod allowlist;
#[cfg(feature = "clustering")]
mod behaviour;
/// Postgres-authoritative entity ownership claims (ADR-0017 Phase 3 Slice
/// 1). Public for the same reason as [`allowlist`]/[`lease`]: the harness
/// provisions schema via the production path.
#[cfg(feature = "clustering")]
pub mod claims;
/// Remote XML-text serde codec for stanzas/elements crossing the kameo
/// boundary. Public: the Slice 5 relay actors' message types embed these
/// wrappers, and downstream phases (cross-node routing) build on them.
#[cfg(feature = "clustering")]
pub mod codec;
#[cfg(feature = "clustering")]
mod dns;
#[cfg(feature = "clustering")]
mod identity;
/// Keypair-slot lease store. Public for the same reason as [`allowlist`]:
/// the harness provisions schema via the production path.
#[cfg(feature = "clustering")]
pub mod lease;
#[cfg(feature = "clustering")]
mod metrics;
/// Per-node supervised relay actor + client handle. Public: the multi-process
/// cluster harness drives `RelayHandle` cross-node, and Phase 4's routing
/// builds on the relay message set.
#[cfg(feature = "clustering")]
pub mod relay;
/// Self-fencing, isolation detection, and re-registration hysteresis
/// (ADR-0017 Phase 3 Slice 2). Public for the same reason as
/// [`claims`]/[`lease`]: the harness drives the node-lease loop directly.
#[cfg(feature = "clustering")]
pub mod self_fence;
/// Owned swarm bring-up. Public so the multi-process cluster harness can run
/// a swarm inside the test process (kameo's `init_global` is a process
/// singleton, so multi-node behaviour is only testable across processes —
/// the harness process joins the swarm as a real node).
#[cfg(feature = "clustering")]
pub mod swarm;

/// A node's per-process clustering identity: freshly generated on every
/// start, never reused across restarts. Names the node's keypair-slot lease
/// and its single kademlia relay registration, and travels on remote actor
/// message types (typed per the typed-payloads rule — never a bare `String`).
#[cfg(feature = "clustering")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

#[cfg(feature = "clustering")]
impl NodeId {
    /// Mint a fresh per-process node id.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Wrap an id received out-of-band (e.g. a harness node-id file).
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(feature = "clustering")]
impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Serializes tests that touch the shared `clustering_peer_allowlist` table
/// (the allowlist store tests and the swarm bring-up smoke test, which loads
/// the allowlist at startup) so a concurrently seeded row cannot leak between
/// them.
#[cfg(all(test, feature = "clustering"))]
pub(crate) fn allowlist_table_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Shared client-facing HTTP readiness signal for the clustering control
/// plane (ADR-0017 element 4, Phase 3 Slice 2): flipped to not-ready the
/// instant a node self-fences (node-lease heartbeat CAS returns zero rows,
/// or Postgres is unreachable past the lease deadline) and back to ready
/// only once the node has re-registered under a fresh `node_id`/
/// `node_epoch` and satisfied the re-acquisition hysteresis gate. Cloning
/// shares the same underlying flag (cheap `Arc` clone).
///
/// Unconditionally compiled (no `clustering` feature gate) so `AppState`
/// and the `/ready`/`/readyz` handlers can hold one field regardless of
/// build: a non-clustering deployment (or a `clustering`-feature build with
/// `clustering.enabled = false`) never flips it, so it stays ready forever
/// — today's behavior, unchanged.
#[derive(Clone)]
pub struct ClusteringReadiness(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl ClusteringReadiness {
    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))
    }

    pub fn is_ready(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set_ready(&self, ready: bool) {
        self.0.store(ready, std::sync::atomic::Ordering::Release);
    }
}

impl Default for ClusteringReadiness {
    fn default() -> Self {
        Self::new()
    }
}

/// Startup failures for the clustering subsystem. The human-facing `Display`
/// text is surfaced as the server-startup diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum ClusteringError {
    /// Clustering was requested on a non-Postgres deployment. The cross-node
    /// control plane (leases, allowlist, ownership claims) is Postgres-only;
    /// SQLite gets single-writer exclusivity for free and never clusters.
    #[error(
        "clustering.enabled=true requires the Postgres database driver \
         (the cross-node control plane has no SQLite equivalent); found {driver:?}"
    )]
    RequiresPostgres { driver: DatabaseDriver },

    /// Clustering was requested at runtime but the binary was built without the
    /// `clustering` Cargo feature, so the swarm subsystem is not compiled in.
    #[error(
        "clustering.enabled=true but this binary was built without the \
         `clustering` cargo feature; rebuild with `--features clustering`"
    )]
    FeatureNotCompiled,

    /// The owned libp2p swarm failed to build or start.
    #[cfg(feature = "clustering")]
    #[error(transparent)]
    Swarm(#[from] swarm::SwarmError),

    /// The node-lease's initial registration (ADR-0017 Phase 3 Slice 2)
    /// failed. Fails startup rather than starting a node-lease loop that can
    /// never renew.
    #[cfg(feature = "clustering")]
    #[error("clustering node-lease registration failed: {0}")]
    NodeLease(waddle_xmpp::ownership::ClaimError),
}

/// Derive the clustering subsystem's own cancellation scope from the
/// process-wide shutdown token.
///
/// `CancellationToken::child_token` gives us both directions we need: parent
/// cancellation (process shutdown) still propagates into the child, so
/// server-wide shutdown always tears clustering down; but cancelling the
/// child does NOT cancel the parent, so a clustering-internal self-fence
/// (keypair-slot fencing loss, or a lease-renewal deadline blown while
/// partitioned from Postgres — see `swarm::run_heartbeat`) only stops
/// clustering. It must never drain axum, WebSocket admission, or ACME.
#[cfg(feature = "clustering")]
fn clustering_scope_token(parent: &CancellationToken) -> CancellationToken {
    parent.child_token()
}

/// Conditionally start the clustering swarm subsystem.
///
/// Returns `Ok(())` immediately, doing nothing, when clustering is disabled —
/// the default single-replica path, unchanged. When enabled it validates the
/// Postgres control-plane prerequisite and, on a `clustering`-feature build,
/// brings up the owned libp2p swarm (later slices).
pub async fn start_if_enabled(
    config: &ClusteringConfig,
    db: &Database,
    stop_token: &CancellationToken,
    readiness: ClusteringReadiness,
) -> Result<(), ClusteringError> {
    if !config.enabled {
        return Ok(());
    }

    let driver = db.driver();

    // Root-cause-first: a binary built without the `clustering` feature can
    // never cluster regardless of driver, so that is the more fundamental
    // failure and is reported before the Postgres prerequisite.
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (db, stop_token, driver, readiness);
        Err(ClusteringError::FeatureNotCompiled)
    }

    #[cfg(feature = "clustering")]
    {
        if driver != DatabaseDriver::Postgres {
            return Err(ClusteringError::RequiresPostgres { driver });
        }
        // Every clustering task (swarm event loop, lease heartbeat, relay
        // supervisor, allowlist/DNS timers, and — from Phase 3 Slice 2 — the
        // node-lease/self-fence loop) is driven off this child, never the
        // raw process-wide `stop_token` — see `clustering_scope_token`.
        let clustering_stop = clustering_scope_token(stop_token);
        // The swarm leases its keypair-pool slot from `db` (Postgres control
        // plane) before binding, enforces the peer allowlist at the behaviour
        // layer, and registers its supervised relay actor in kademlia.
        let handle = swarm::spawn(config, db, clustering_stop.clone()).await?;
        tracing::info!(
            local_peer_id = %handle.local_peer_id,
            node_id = %handle.node_id,
            listen_addrs = ?config.listen_addrs,
            request_timeout_ms = config.messaging.request_timeout.as_millis() as u64,
            "ADR-0017 Phase 2: clustering swarm started (node discovery only)"
        );

        // ADR-0017 Phase 3 Slice 2: the entity-ownership node lease is a
        // *different* lease from the keypair-slot lease `swarm::spawn` just
        // heartbeat-started above — this one guards this node's
        // `clustering_claims` ownership, not its libp2p identity (Q5's "no
        // coupling" precedent). Register once, up front (fail startup on a
        // genuine registration failure, exactly like the keypair-slot
        // acquire above), then hand the loop off to the background task.
        let node_lease = claims::PostgresClaimStore::new(db.clone());
        let node_identity = waddle_xmpp::ownership::NodeIdentity::new(
            handle.node_id.as_str().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        // FIX 6: read through the typed `ClusteringConfig::pod_template_hash`
        // (parsed once in `config.rs`'s `from_vars` pipeline, like every
        // sibling var) rather than a raw `std::env::var` at this call site.
        let pod_template_hash = config.pod_template_hash.clone();
        register_node_lease(&node_lease, &node_identity, pod_template_hash.clone()).await?;
        tokio::spawn(self_fence::run_node_lease(
            node_lease,
            node_identity,
            clustering_stop,
            self_fence::NodeLeaseRunConfig {
                pod_template_hash,
                lease_config: config.node_lease.clone(),
                self_fence_config: config.self_fence.clone(),
                connected_peers: handle.connected_peers.clone(),
                local_claims: std::sync::Arc::new(self_fence::NoLocallyClaimedEntities),
                readiness,
            },
        ));
        Ok(())
    }
}

/// Register this node's initial node-lease row, mapping the store's
/// `ClaimError` into [`ClusteringError`] so a genuine registration failure
/// fails server startup exactly like the keypair-slot acquire above, rather
/// than silently starting a node-lease loop that can never renew.
#[cfg(feature = "clustering")]
async fn register_node_lease(
    node_lease: &claims::PostgresClaimStore,
    identity: &waddle_xmpp::ownership::NodeIdentity,
    pod_template_hash: Option<String>,
) -> Result<(), ClusteringError> {
    use claims::NodeLeaseStore as _;
    node_lease
        .register(identity, pod_template_hash)
        .await
        .map_err(ClusteringError::NodeLease)
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;

    // Regression coverage for the cancellation-scope invariant: a
    // clustering self-fence must not take down the whole process, but
    // process shutdown must still take down clustering.
    #[test]
    fn clustering_scope_cancels_independently_of_parent() {
        let parent = CancellationToken::new();
        let child = clustering_scope_token(&parent);

        child.cancel();

        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn parent_cancellation_still_tears_down_clustering_scope() {
        let parent = CancellationToken::new();
        let child = clustering_scope_token(&parent);

        parent.cancel();

        assert!(child.is_cancelled());
    }
}
