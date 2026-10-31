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
use std::sync::Arc;
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
/// Graceful per-entity claim drain + rollout-aware acquire placement
/// (ADR-0017 Phase 3 Slice 10). `pub(crate)` (not `pub`): every caller
/// (`self_fence::run_node_lease`, `server::session_janitors`'s orphan
/// reaper) lives inside this crate; the multi-process harness drives drain
/// behavior through the production shutdown path, not this module directly.
#[cfg(feature = "clustering")]
pub(crate) mod drain;
#[cfg(feature = "clustering")]
mod identity;
/// Keypair-slot lease store. Public for the same reason as [`allowlist`]:
/// the harness provisions schema via the production path.
#[cfg(feature = "clustering")]
pub mod lease;
/// Real `LocallyClaimedEntities` backed by the SM session registry
/// (ADR-0017 Phase 3 Slice 5, carried debt (b)). Public for the same reason
/// as [`self_fence`]: the harness wires/drives it directly in the
/// claim-scoped-hydration and deposed-owner scenarios.
#[cfg(feature = "clustering")]
pub mod local_claims;
/// `pub(crate)` (ADR-0017 Phase 3 Slice 10): `server::session_janitors`'s
/// Q6 SM-drain path records into the same `claims_released_on_drain`/
/// `claims_abandoned_on_drain` counters this module's own per-entity drain
/// uses, so both mechanisms feed one shared observability surface.
#[cfg(feature = "clustering")]
pub(crate) mod metrics;
/// Ordered relay channel substrate (ADR-0017 Phase 4 Slice 2). Public for the
/// same reason as [`relay`]: the multi-process harness drives it directly
/// before production DM/MUC/presence/IQ routing callers are attached.
#[cfg(feature = "clustering")]
pub mod ordered_relay;
/// Per-node supervised relay actor + client handle. Public: the multi-process
/// cluster harness drives `RelayHandle` cross-node, and Phase 4's routing
/// builds on the relay message set.
#[cfg(feature = "clustering")]
pub mod relay;
/// The `ResourcePresenceAsker` seam and its `RelayHandle` implementation
/// (#1803) — the room host's side of the cross-node "does the claim owner
/// know this exact resource?" probe that gates ghost-occupant eviction.
#[cfg(feature = "clustering")]
pub mod resource_presence;
/// The `RemoteResumeAsker` implementation over `RelayHandle` (ADR-0017
/// Phase 3 Slice 6) — the resuming node's side of the cross-node XEP-0198
/// resume live-steal handshake. Public so `server/http.rs` can construct it.
#[cfg(feature = "clustering")]
pub mod resume_asker;
/// The `RelayActor`-side bridge to the live `ConnectionRegistry` (ADR-0017
/// Phase 3 Slice 6). Public for the same reason as [`local_claims`]: the
/// harness wires/drives it directly in the cross-node live-steal handshake
/// scenario.
pub mod resume_bridge;
#[cfg(feature = "clustering")]
pub mod route_bridge;
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
/// W3C trace context carried on relay messages so a cross-node delivery
/// forms one trace (#1485). Telemetry only — never read for relay
/// semantics.
#[cfg(feature = "clustering")]
pub mod trace_context;

/// Serializes tests that touch the shared `clustering_peer_allowlist` table
/// (the allowlist store tests and the swarm bring-up smoke test, which loads
/// the allowlist at startup) so a concurrently seeded row cannot leak between
/// them.
#[cfg(all(test, feature = "clustering"))]
pub(crate) fn allowlist_table_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Serializes tests that touch the shared `clustering_keypair_slots` table.
/// The keypair-slot lease tests and swarm bring-up smoke test run in the same
/// lib-test binary under the Nix Postgres gate, so they must share one lock.
#[cfg(all(test, feature = "clustering"))]
pub(crate) fn keypair_slot_table_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod readiness_tests;

#[cfg(all(test, feature = "clustering"))]
mod route_bridge_registration_tests;

#[cfg(all(test, feature = "clustering"))]
mod tests;

mod node;
#[cfg(feature = "clustering")]
use node::clustering_scope_token;
#[cfg(feature = "clustering")]
use node::prepare_node_lease;
#[cfg(feature = "clustering")]
use node::register_node_lease;
pub use node::start_if_enabled;
pub use node::ClusteringError;
pub use node::ClusteringHandles;
pub use node::ClusteringShutdown;
pub use node::CriticalNodeFailure;
pub use node::NodeAdmission;
pub use node::NodeAdmissionError;
use node::NodeAdmissionGeneration;
pub use node::NodeAdmissionPermit;
#[cfg(feature = "clustering")]
pub use node::NodeId;
pub use node::NodeLifecycle;
use node::NodeLifecycleState;
pub use node::ShutdownSmDrain;
pub use node::StartupServingTransition;
