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
        let _ = (db, stop_token, driver);
        Err(ClusteringError::FeatureNotCompiled)
    }

    #[cfg(feature = "clustering")]
    {
        if driver != DatabaseDriver::Postgres {
            return Err(ClusteringError::RequiresPostgres { driver });
        }
        // The swarm leases its keypair-pool slot from `db` (Postgres control
        // plane) before binding, enforces the peer allowlist at the behaviour
        // layer, and registers its supervised relay actor in kademlia.
        let handle = swarm::spawn(config, db, stop_token.clone()).await?;
        tracing::info!(
            local_peer_id = %handle.local_peer_id,
            node_id = %handle.node_id,
            listen_addrs = ?config.listen_addrs,
            request_timeout_ms = config.messaging.request_timeout.as_millis() as u64,
            "ADR-0017 Phase 2: clustering swarm started (node discovery only)"
        );
        Ok(())
    }
}
