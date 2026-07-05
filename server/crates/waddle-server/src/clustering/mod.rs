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
use crate::db::DatabaseDriver;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "clustering")]
mod behaviour;
#[cfg(feature = "clustering")]
mod dns;
#[cfg(feature = "clustering")]
mod identity;
#[cfg(feature = "clustering")]
mod metrics;
#[cfg(feature = "clustering")]
mod swarm;

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
pub fn start_if_enabled(
    config: &ClusteringConfig,
    driver: DatabaseDriver,
    stop_token: &CancellationToken,
) -> Result<(), ClusteringError> {
    if !config.enabled {
        return Ok(());
    }

    // Root-cause-first: a binary built without the `clustering` feature can
    // never cluster regardless of driver, so that is the more fundamental
    // failure and is reported before the Postgres prerequisite.
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (driver, stop_token);
        Err(ClusteringError::FeatureNotCompiled)
    }

    #[cfg(feature = "clustering")]
    {
        if driver != DatabaseDriver::Postgres {
            return Err(ClusteringError::RequiresPostgres { driver });
        }
        // Slices 2–5 wire the keypair-slot lease, peer allowlist, remote codec,
        // and supervised relay actors on top of this swarm.
        let local_peer_id = swarm::spawn(config, stop_token.clone())?;
        tracing::info!(
            %local_peer_id,
            listen_addrs = ?config.listen_addrs,
            request_timeout_ms = config.messaging.request_timeout.as_millis() as u64,
            "ADR-0017 Phase 2: clustering swarm started (node discovery only)"
        );
        Ok(())
    }
}
