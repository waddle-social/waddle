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
use std::time::Duration;
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
/// The `RemoteResumeAsker` implementation over `RelayHandle` (ADR-0017
/// Phase 3 Slice 6) — the resuming node's side of the cross-node XEP-0198
/// resume live-steal handshake. Public so `server/http.rs` can construct it.
#[cfg(feature = "clustering")]
pub mod resume_asker;
/// The `RelayActor`-side bridge to the live `ConnectionRegistry` (ADR-0017
/// Phase 3 Slice 6). Public for the same reason as [`local_claims`]: the
/// harness wires/drives it directly in the cross-node live-steal handshake
/// scenario.
#[cfg(feature = "clustering")]
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

/// Serializes tests that touch the shared `clustering_keypair_slots` table.
/// The keypair-slot lease tests and swarm bring-up smoke test run in the same
/// lib-test binary under the Nix Postgres gate, so they must share one lock.
#[cfg(all(test, feature = "clustering"))]
pub(crate) fn keypair_slot_table_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Critical local services whose unexpected death makes actor ownership and
/// socket admission unsafe in every runtime mode. These are typed operational
/// causes, not XMPP payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CriticalNodeFailure {
    RoomRegistryTerminated,
    UserRegistryTerminated,
}

/// The single authority for client admission on this node.
///
/// A physical WebSocket remains local, but it may be admitted only while the
/// node can safely own the logical work behind it. Keeping readiness and the
/// upgrade gate on this same state prevents a fenced node from becoming
/// `/ready`-healthy while still accepting a new socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeAdmission {
    Starting,
    Serving,
    Draining,
    FencedRecovering,
    Failed(CriticalNodeFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NodeAdmissionGeneration(u64);

impl NodeAdmissionGeneration {
    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[derive(Clone, Debug)]
struct NodeLifecycleState {
    admission: NodeAdmission,
    generation: NodeAdmissionGeneration,
    generation_revoked: CancellationToken,
}

/// A successful admission decision for one socket upgrade, bound to the
/// exact serving generation that issued it. Any lifecycle transition revokes
/// the permit, including a fence followed by recovery back to `Serving`.
#[derive(Debug)]
pub struct NodeAdmissionPermit {
    state: std::sync::Arc<std::sync::RwLock<NodeLifecycleState>>,
    generation: NodeAdmissionGeneration,
    generation_revoked: CancellationToken,
}

impl NodeAdmissionPermit {
    /// Revalidate at the last safe pre-upgrade boundary. This deliberately
    /// takes only a short read lock: fencing/readiness transitions must never
    /// wait for an HTTP/WebSocket handshake.
    pub fn revalidate(&self) -> Result<(), NodeAdmissionError> {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(state.admission, NodeAdmission::Serving) {
            return Err(NodeAdmissionError::NotServing(state.admission.clone()));
        }
        if state.generation != self.generation {
            return Err(NodeAdmissionError::Revoked);
        }
        Ok(())
    }

    /// Resolve as soon as the lifecycle leaves the exact serving generation
    /// that admitted this socket. The token stays cancelled after recovery,
    /// so an old transport can never become authoritative again.
    pub async fn revoked(&self) {
        self.generation_revoked.cancelled().await;
    }
}

/// Why an HTTP/XMPP connection was not admitted.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum NodeAdmissionError {
    #[error("node is not accepting connections: {0:?}")]
    NotServing(NodeAdmission),
    #[error("node admission permit was revoked by a lifecycle transition")]
    Revoked,
}

/// Result of the one startup-only promotion attempted after every critical
/// registry supervisor is armed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupServingTransition {
    Promoted,
    AlreadyServing,
    Blocked(NodeAdmission),
}

/// Cloneable lifecycle state shared by readiness, socket admission, and the
/// clustered fencing workers. `fatal_fence` is retained as the bridge to the
/// existing bounded claim-drain machinery; its terminal state is represented
/// here rather than by a separate readiness boolean.
#[derive(Clone)]
pub struct NodeLifecycle {
    state: std::sync::Arc<std::sync::RwLock<NodeLifecycleState>>,
    fatal_fence: CancellationToken,
}

impl NodeLifecycle {
    /// Existing single-node/test callers start in their historical serving
    /// state. Production startup uses [`Self::starting`] until all critical
    /// services and routes are constructed.
    pub fn new() -> Self {
        Self::with_admission(NodeAdmission::Serving)
    }

    pub fn starting() -> Self {
        Self::with_admission(NodeAdmission::Starting)
    }

    fn with_admission(admission: NodeAdmission) -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::RwLock::new(NodeLifecycleState {
                admission,
                generation: NodeAdmissionGeneration(0),
                generation_revoked: CancellationToken::new(),
            })),
            fatal_fence: CancellationToken::new(),
        }
    }

    pub fn admission(&self) -> NodeAdmission {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .admission
            .clone()
    }

    pub fn admit(&self) -> Result<NodeAdmissionPermit, NodeAdmissionError> {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match state.admission {
            NodeAdmission::Serving => Ok(NodeAdmissionPermit {
                state: std::sync::Arc::clone(&self.state),
                generation: state.generation,
                generation_revoked: state.generation_revoked.clone(),
            }),
            _ => Err(NodeAdmissionError::NotServing(state.admission.clone())),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.admission(), NodeAdmission::Serving)
    }

    pub fn serve(&self) {
        self.transition_nonterminal(NodeAdmission::Serving);
    }

    /// Complete startup only if no fence/drain/failure won the race while
    /// the HTTP graph was being constructed. Recovery is intentionally a
    /// separate explicit [`Self::serve`] call after the node re-registers.
    pub fn finish_startup(&self) -> StartupServingTransition {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match state.admission {
            NodeAdmission::Starting => {
                state.generation_revoked.cancel();
                state.admission = NodeAdmission::Serving;
                state.generation = state.generation.next();
                state.generation_revoked = CancellationToken::new();
                StartupServingTransition::Promoted
            }
            NodeAdmission::Serving => StartupServingTransition::AlreadyServing,
            _ => StartupServingTransition::Blocked(state.admission.clone()),
        }
    }

    pub fn begin_drain(&self) {
        self.transition_nonterminal(NodeAdmission::Draining);
    }

    pub fn begin_fenced_recovery(&self) {
        self.transition_nonterminal(NodeAdmission::FencedRecovering);
    }

    pub fn fail(&self, failure: CriticalNodeFailure) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = NodeAdmission::Failed(failure);
        if state.admission != next {
            state.generation_revoked.cancel();
            state.admission = next;
            state.generation = state.generation.next();
            state.generation_revoked = CancellationToken::new();
        }
        drop(state);
        self.fatal_fence.cancel();
    }

    pub fn critical_failure(&self) -> Option<CriticalNodeFailure> {
        match self.admission() {
            NodeAdmission::Failed(failure) => Some(failure),
            _ => None,
        }
    }

    fn transition_nonterminal(&self, next: NodeAdmission) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(state.admission, NodeAdmission::Failed(_)) && state.admission != next {
            state.generation_revoked.cancel();
            state.admission = next;
            state.generation = state.generation.next();
            state.generation_revoked = CancellationToken::new();
        }
    }

    #[cfg(any(test, feature = "clustering"))]
    pub(crate) fn fatal_fence_token(&self) -> CancellationToken {
        self.fatal_fence.clone()
    }
}

impl Default for NodeLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::{
        CriticalNodeFailure, NodeAdmission, NodeAdmissionError, NodeLifecycle,
        StartupServingTransition,
    };

    #[test]
    fn failed_latch_overrides_every_later_nonterminal_transition() {
        let lifecycle = NodeLifecycle::starting();
        lifecycle.fail(CriticalNodeFailure::RoomRegistryTerminated);
        lifecycle.serve();
        lifecycle.begin_drain();
        lifecycle.serve();
        assert_eq!(
            lifecycle.admission(),
            NodeAdmission::Failed(CriticalNodeFailure::RoomRegistryTerminated)
        );
        assert!(!lifecycle.is_ready());
    }

    #[test]
    fn only_serving_nodes_issue_admission_permits() {
        let lifecycle = NodeLifecycle::starting();
        assert!(lifecycle.admit().is_err());
        lifecycle.serve();
        let permit = lifecycle.admit().expect("serving permit");
        assert!(permit.revalidate().is_ok());
        lifecycle.begin_fenced_recovery();
        assert!(lifecycle.admit().is_err());
        assert_eq!(
            permit.revalidate(),
            Err(NodeAdmissionError::NotServing(
                NodeAdmission::FencedRecovering
            ))
        );

        lifecycle.serve();
        assert_eq!(permit.revalidate(), Err(NodeAdmissionError::Revoked));
        assert!(lifecycle
            .admit()
            .expect("recovered serving permit")
            .revalidate()
            .is_ok());
    }

    #[tokio::test]
    async fn old_generation_revocation_stays_latched_after_recovery() {
        let lifecycle = NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");

        lifecycle.begin_fenced_recovery();
        lifecycle.serve();

        tokio::time::timeout(std::time::Duration::from_millis(50), permit.revoked())
            .await
            .expect("old permit must remain revoked after recovery");
        assert_eq!(permit.revalidate(), Err(NodeAdmissionError::Revoked));
    }

    #[tokio::test]
    async fn lifecycle_revocation_wins_over_ready_socket_work() {
        let lifecycle = NodeLifecycle::new();
        let permit = lifecycle.admit().expect("serving permit");
        lifecycle.begin_fenced_recovery();
        let mut dispatched = false;

        tokio::select! {
            biased;
            _ = permit.revoked() => {}
            _ = std::future::ready(()) => dispatched = true,
        }

        assert!(
            !dispatched,
            "no stanza may dispatch after generation revocation"
        );
    }

    #[test]
    fn startup_promotion_cannot_overwrite_fencing_or_failure() {
        let lifecycle = NodeLifecycle::starting();
        assert_eq!(
            lifecycle.finish_startup(),
            StartupServingTransition::Promoted
        );
        assert_eq!(
            lifecycle.finish_startup(),
            StartupServingTransition::AlreadyServing
        );

        let fenced = NodeLifecycle::starting();
        fenced.begin_fenced_recovery();
        assert_eq!(
            fenced.finish_startup(),
            StartupServingTransition::Blocked(NodeAdmission::FencedRecovering)
        );
        assert_eq!(fenced.admission(), NodeAdmission::FencedRecovering);

        let failed = NodeLifecycle::starting();
        failed.fail(CriticalNodeFailure::UserRegistryTerminated);
        assert_eq!(
            failed.finish_startup(),
            StartupServingTransition::Blocked(NodeAdmission::Failed(
                CriticalNodeFailure::UserRegistryTerminated
            ))
        );
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

    /// The Postgres claim/node-lease control-plane schema failed to provision.
    /// This is fatal because every clustered node must register in
    /// `clustering_nodes` before it can safely claim or route work.
    #[cfg(feature = "clustering")]
    #[error("clustering claim-store schema initialization failed: {0}")]
    ClaimStoreSchema(waddle_xmpp::ownership::ClaimError),

    /// ADR-0017 Phase 3 Slice 7 FIX 8 (council-adjudicated): the durable MUC
    /// room store's own startup init (`ensure_schema`) failed. Fails
    /// clustering startup entirely rather than continuing with entity
    /// claims live but MUC room durability silently unprotected — a
    /// `RoomActor` claim can move between nodes without this store, which
    /// is exactly the unprotected-deposal hazard element 7 exists to close
    /// (config/affiliations/subject would silently reset to defaults on
    /// every steal), not a degraded-but-safe mode. Previously logged and
    /// treated as non-fatal; corrected here to match the co-location
    /// discipline every other clustering-durability store already enforces
    /// at startup (`sm_persistence::open_for_cluster_mode`,
    /// `pending_delivery::open_for_cluster_mode`).
    #[cfg(feature = "clustering")]
    #[error("clustering MUC durable store initialization failed: {0}")]
    MucDurableStoreInit(waddle_xmpp::XmppError),
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

/// Handles the clustering subsystem hands back to the caller of
/// [`start_if_enabled`] for use by code outside `clustering` itself
/// (ADR-0017 Phase 3 Slice 4 follow-up plumbing note).
///
/// Both fields are `None` whenever clustering is disabled, this binary
/// lacks the `clustering` Cargo feature, or (defensively) the subsystem
/// otherwise produced no live handles — callers must treat `None` as "no
/// Postgres-fenced claim machinery is available; use the portable,
/// single-node path instead," never as an error.
///
/// Unconditionally compiled (no `clustering` feature gate): both field
/// types ([`waddle_xmpp::ownership::ClaimStore`],
/// [`waddle_xmpp::ownership::SharedNodeIdentity`]) are themselves
/// unconditionally compiled, so ordinary non-clustering builds can still
/// name this type (e.g. on `AppState`) without conditional compilation
/// leaking into shared struct definitions.
#[derive(Clone, Default)]
pub struct ClusteringHandles {
    /// The same `ClaimStore` backing this node's entity-ownership claims —
    /// **not** a second, independent store. Constructed by cloning the
    /// same underlying `Database` the node-lease loop uses, so both views
    /// observe the same `clustering_claims` rows.
    pub claim_store: Option<Arc<dyn waddle_xmpp::ownership::ClaimStore>>,
    /// Live view of this node's current claim identity — the same shared
    /// handle `self_fence::run_node_lease` updates on every
    /// re-registration. See [`waddle_xmpp::ownership::SharedNodeIdentity`].
    pub node_identity: Option<waddle_xmpp::ownership::SharedNodeIdentity>,
    /// ADR-0017 Phase 3 Slice 5 (carried debt (b)): the real
    /// `LocallyClaimedEntities` handed to `run_node_lease`, constructed
    /// empty (the SM session registry does not exist yet at
    /// `start_if_enabled` time — it needs `ClusteringHandles` itself) and
    /// completed by `server/http.rs::create_sm_session_registry` calling
    /// [`local_claims::SmSessionLocalClaims::wire`] once the registry is
    /// built. The one field on this otherwise-unconditionally-compiled
    /// struct that IS feature-gated: its type
    /// (`local_claims::SmSessionLocalClaims`) only exists behind
    /// `clustering`, since it implements the also-feature-gated
    /// `self_fence::LocallyClaimedEntities` trait. `None` whenever
    /// clustering is disabled, mirroring `claim_store`/`node_identity`.
    #[cfg(feature = "clustering")]
    pub local_claims: Option<Arc<local_claims::SmSessionLocalClaims>>,
    /// ADR-0017 Phase 3 Slice 7: the `RoomActor`-backed
    /// `LocallyClaimedEntities` counterpart of `local_claims`, constructed
    /// empty here (the MUC room registry does not exist yet at this point
    /// in startup — it is spawned earlier, in `server/mod.rs`, mirroring
    /// `local_claims`'s own construction-order note) and wired later by
    /// `server/mod.rs` calling `RoomRegistry::wire_clustering_claims` /
    /// `RoomLocalClaims::wire` once the registry handle is available. Also
    /// the same `Arc` `RelayActor` answers `Demote` asks through (the
    /// two-part demotion protocol's part (a) receiving side). `None`
    /// under the same conditions `local_claims` is `None`.
    #[cfg(feature = "clustering")]
    pub room_local_claims: Option<Arc<local_claims::RoomLocalClaims>>,
    /// ADR-0017 Phase 4 Slice 1b: the `UserActor`-backed
    /// `LocallyClaimedEntities` counterpart of `local_claims`, constructed
    /// empty here because the user registry is spawned later while building
    /// `WebSocketState`, then wired by `server/http.rs` immediately after
    /// that registry is created. `None` under the same conditions as
    /// `local_claims`.
    #[cfg(feature = "clustering")]
    pub user_local_claims: Option<Arc<local_claims::UserLocalClaims>>,
    /// ADR-0017 Phase 3 Slice 7: the durable MUC room ownership store —
    /// the SAME `Arc` `server/mod.rs` hands to
    /// `RoomRegistry::wire_clustering_claims` (never a second, independent
    /// store) and the one `dispatch_to_room`'s fenced pre-fan-out backstop
    /// reads directly. `None` under the exact same conditions
    /// `local_claims` is `None` (clustering disabled, non-Postgres, a
    /// build without the `clustering` feature, or — defensively — this
    /// store's own `ensure_schema` failing at startup, logged but
    /// non-fatal to the rest of clustering).
    #[cfg(feature = "clustering")]
    pub muc_durable_store: Option<Arc<dyn waddle_xmpp::muc::MucDurableStore>>,
    /// ADR-0017 Phase 3 Slice 5: a `NodeLeaseStore` handle for the orphan
    /// reaper janitor (`server::session_janitors::spawn_orphan_reaper_janitor`),
    /// which runs on its own periodic cadence outside `run_node_lease`'s
    /// closure and therefore needs its own handle onto the same
    /// `clustering_nodes`/`clustering_claims` tables — wraps the same
    /// `Database` clone as `claim_store`/the node-lease loop's own store,
    /// never a second independent one. Feature-gated for the same reason
    /// as `local_claims`: `NodeLeaseStore` only exists behind `clustering`.
    #[cfg(feature = "clustering")]
    pub node_lease: Option<Arc<dyn claims::NodeLeaseStore>>,
    /// The configured node-lease TTL (ADR-0017 element 4/Q6) — the orphan
    /// reaper janitor needs this same value to bind into its `expire` calls
    /// and has no other route to `ClusteringNodeLeaseConfig` (it runs off
    /// `WebSocketState`, which does not otherwise carry `ServerConfig`).
    #[cfg(feature = "clustering")]
    pub lease_ttl: Option<std::time::Duration>,
    /// This node's own `pod_template_hash` (ADR-0017 Phase 3 Slice 10, Q5)
    /// — the same value passed to `NodeLeaseStore::register`. The orphan
    /// reaper janitor needs this alongside `node_lease` to compute the
    /// rollout-aware acquire-backoff delay
    /// (`clustering::drain::rollout_backoff_delay`) before each
    /// `steal_stale(OwnerStale)` attempt; it has no other route to
    /// `ClusteringConfig`, mirroring `lease_ttl`'s identical rationale.
    #[cfg(feature = "clustering")]
    pub pod_template_hash: Option<String>,
    /// ADR-0017 Phase 3 Slice 6: the cross-node resume live-handshake's
    /// bridge to this node's own `ConnectionRegistry` — the SAME `Arc` the
    /// swarm's `RelayActor` answers `RelayResumeSteal` asks through, not a
    /// second, independent bridge. Wired by
    /// `server/http.rs::create_sm_session_registry` once the connection
    /// registry exists (construction-order chicken-and-egg, mirroring
    /// `local_claims`). Feature-gated for the same reason as `local_claims`.
    #[cfg(feature = "clustering")]
    pub resume_bridge: Option<Arc<resume_bridge::ResumeStealBridge>>,
    /// ADR-0017 Phase 4 Slice 3: bridge used by the relay actor to apply
    /// ordered full-JID delivery effects locally, plus the shared sender-side
    /// sequence allocator used by origin routing calls.
    #[cfg(feature = "clustering")]
    pub ordered_relay_delivery_bridge: Option<Arc<route_bridge::OrderedRelayDeliveryBridge>>,
    /// This node's clustering-scope cancellation token (the same child
    /// token every clustering task races against — see
    /// `clustering_scope_token`'s doc comment). Exposed so a caller outside
    /// `clustering` (the cross-node resume asker,
    /// `server/http.rs::create_sm_session_registry`) can construct a
    /// `relay::RelayHandle` with the correct cancellation scope (ADR-0017
    /// Phase 3 Slice 6's `RelayHandle` cancellation-safety paydown).
    #[cfg(feature = "clustering")]
    pub stop_token: Option<CancellationToken>,
    /// One-shot fatal fencing signal for clustering workers that discover
    /// ambiguous post-CAS ownership state. Unlike `stop_token`, firing this
    /// signal drives the node-lease loop through its demote/not-ready fence
    /// path before clustering shuts down.
    #[cfg(feature = "clustering")]
    pub fatal_fence: Option<CancellationToken>,
    /// The resolved `ClusteringResumeHandshakeConfig::timeout` (ADR-0017
    /// Phase 3 Slice 6) — the cross-node resume path's held-response retry
    /// budget. Carried here (rather than threading `ServerConfig` itself)
    /// mirroring `lease_ttl`'s identical rationale: the SM session registry
    /// and its resume path run off `WebSocketState`, which does not
    /// otherwise carry `ClusteringConfig`.
    #[cfg(feature = "clustering")]
    pub resume_handshake_timeout: Option<std::time::Duration>,
}

impl ClusteringHandles {
    /// `Some((claim_store, node_identity))` only when both handles are
    /// present; `None` otherwise. Convenience for callers (e.g.
    /// `sm_persistence::open_for_cluster_mode`) that only ever want the
    /// pair together — a `ClaimStore` with no live identity to bind into
    /// its CAS calls (or vice versa) is not a usable combination.
    pub fn claim_pair(
        &self,
    ) -> Option<(
        Arc<dyn waddle_xmpp::ownership::ClaimStore>,
        waddle_xmpp::ownership::SharedNodeIdentity,
    )> {
        match (&self.claim_store, &self.node_identity) {
            (Some(store), Some(identity)) => Some((Arc::clone(store), identity.clone())),
            _ => None,
        }
    }

    /// The resolved resume-handshake timeout (ADR-0017 Phase 3 Slice 6),
    /// `None` under the exact same conditions `claim_pair` is `None`
    /// (clustering disabled, non-Postgres, or a build without the
    /// `clustering` feature). Unconditionally compiled, like `claim_pair`,
    /// so `stream_management.rs` (outside the `clustering` feature gate)
    /// can read it without conditional compilation.
    pub fn resume_handshake_timeout(&self) -> Option<std::time::Duration> {
        #[cfg(feature = "clustering")]
        {
            self.resume_handshake_timeout
        }
        #[cfg(not(feature = "clustering"))]
        {
            None
        }
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 2 (council-adjudicated): hard-kill the
    /// locally-claimed `RoomActor` for `room_jid`, mirroring the Demote
    /// relay ask's exact receiving-side call
    /// (`RelayActor`'s `Demote` handler calling
    /// `room_local_claims.demote(&entity)`). Every mutation-handler call
    /// site that observes `RoomMutationError::NotOwner`/an equivalent
    /// per-message `NotOwner` variant calls this so the deposed actor
    /// genuinely stops serving (hard `ActorRef::kill()`), instead of only
    /// bouncing the one request that discovered the ownership loss and
    /// leaving the actor to keep answering (and re-discovering the same
    /// staleness) for every subsequent ask.
    ///
    /// A no-op when clustering is disabled, this binary lacks the
    /// `clustering` Cargo feature, or `room_local_claims` was never wired
    /// (defensive — `NotOwner` can only ever be produced when a durable
    /// store, and therefore `room_local_claims`, is configured).
    pub async fn demote_room_actor(&self, room_jid: &jid::BareJid) {
        #[cfg(feature = "clustering")]
        {
            use self_fence::LocallyClaimedEntities as _;
            let Some(room_local_claims) = &self.room_local_claims else {
                return;
            };
            let entity = waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room_jid.to_string(),
            );
            room_local_claims.demote(&entity).await;
        }
        #[cfg(not(feature = "clustering"))]
        {
            let _ = room_jid;
        }
    }
}

/// The clustering subsystem's node-lease task join handle (ADR-0017 Phase 3
/// Slice 10), returned alongside [`ClusteringHandles`] rather than as one of
/// its fields: `JoinHandle` is neither `Clone` nor meaningfully `Default`,
/// and `ClusteringHandles` derives both for its many other call sites (it
/// is cloned onto `AppState`, `WebSocketState`, and every janitor). This
/// lets `server/mod.rs` await this node's per-entity graceful drain
/// (`self_fence::run_node_lease`'s own exit, which runs the drain sequence
/// on its way out) actually completing before process exit — "in parallel
/// with connection drain but completing before process exit," per element
/// 4's drain sequence text — rather than a fire-and-forget background task
/// racing shutdown with no ordering guarantee at all.
pub struct ClusteringShutdown(Option<tokio::task::JoinHandle<()>>);

impl ClusteringShutdown {
    /// Await the node-lease loop's own exit, bounded by `budget` so a
    /// wedged drain task cannot hang process shutdown forever — a timeout
    /// here is logged and the process proceeds to exit anyway: any
    /// un-released claims are simply fenced-safe and reclaimed later by
    /// another node's orphan reaper (`claims_abandoned_on_drain` already
    /// counts them). A `None` inner handle (clustering disabled, or a
    /// build without the `clustering` feature) returns immediately.
    pub async fn await_drain(self, budget: Duration) {
        let Some(handle) = self.0 else {
            return;
        };
        if tokio::time::timeout(budget, handle).await.is_err() {
            tracing::warn!(
                budget_ms = budget.as_millis() as u64,
                "clustering: node-lease drain did not complete within the shutdown budget; \
                 proceeding with process exit (fenced-safe: un-released claims are simply \
                 reclaimed later)"
            );
        }
    }
}

/// Conditionally start the clustering swarm subsystem.
///
/// Returns `Ok((ClusteringHandles::default(), ClusteringShutdown(None)))`
/// immediately, doing nothing, when clustering is disabled — the default
/// single-replica path, unchanged. When enabled it validates the Postgres
/// control-plane prerequisite and, on a `clustering`-feature build, brings
/// up the owned libp2p swarm (later slices) and returns live handles onto
/// the same `ClaimStore`/node-identity the node-lease loop itself uses,
/// plus the node-lease task's own join handle (Slice 10).
pub async fn start_if_enabled(
    config: &ClusteringConfig,
    db: &Database,
    stop_token: &CancellationToken,
    readiness: NodeLifecycle,
) -> Result<(ClusteringHandles, ClusteringShutdown), ClusteringError> {
    if !config.enabled {
        return Ok((ClusteringHandles::default(), ClusteringShutdown(None)));
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
        let fatal_fence = readiness.fatal_fence_token();
        // ADR-0017 Phase 3 Slice 6: constructed empty (the `ConnectionRegistry`
        // doesn't exist yet at this point in startup) and wired to it later by
        // `server/http.rs::create_sm_session_registry` — the same
        // construction-order chicken-and-egg fix `local_claims` already
        // applies. The swarm's `RelayActor` answers `RelayResumeSteal` asks
        // through this exact `Arc`.
        let resume_bridge = resume_bridge::ResumeStealBridge::new();
        let ordered_relay_delivery_bridge = route_bridge::OrderedRelayDeliveryBridge::new(
            clustering_stop.clone(),
            &config.messaging,
        );
        // ADR-0017 Phase 3 Slice 7: constructed empty for the same
        // construction-order reason as `resume_bridge` — the MUC room
        // registry doesn't exist yet at this point in startup (it is
        // spawned before `start_if_enabled` runs, in `server/mod.rs`) —
        // and wired later via `RoomRegistry::wire_clustering_claims`'s
        // caller in `server/mod.rs`. The swarm's `RelayActor` answers
        // `Demote` asks (the two-part demotion protocol's part (a))
        // through this exact `Arc`.
        let room_local_claims = local_claims::RoomLocalClaims::new();
        let user_local_claims = local_claims::UserLocalClaims::new();
        // The swarm leases its keypair-pool slot from `db` (Postgres control
        // plane) before binding, enforces the peer allowlist at the behaviour
        // layer, and registers its supervised relay actor in kademlia.
        let handle = swarm::spawn(
            config,
            db,
            clustering_stop.clone(),
            swarm::RelayBridges {
                resume_bridge: Arc::clone(&resume_bridge),
                room_local_claims: Arc::clone(&room_local_claims),
                ordered_relay_delivery_bridge: Arc::clone(&ordered_relay_delivery_bridge),
            },
        )
        .await?;
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
        let local_peer_id = handle.local_peer_id.to_string();
        prepare_node_lease(
            &node_lease,
            &node_identity,
            pod_template_hash.clone(),
            Some(local_peer_id.clone()),
        )
        .await?;

        // ADR-0017 Phase 3 Slice 4 follow-up plumbing: hand back a
        // `ClaimStore` view onto the same `clustering_claims` rows the
        // node-lease loop guards, plus a live handle onto the identity it
        // keeps current across re-registrations — so a caller outside
        // `clustering` (the Postgres-fenced `SmPersistenceStorage`) can
        // bind the *current* node identity into its own claim
        // acquire/fence calls instead of capturing a stale one at
        // construction time. `claim_store_handle` wraps the same `db`
        // clone as `node_lease` — not a second, independent store.
        let claim_store_handle: Arc<dyn waddle_xmpp::ownership::ClaimStore> =
            Arc::new(claims::PostgresClaimStore::new(db.clone()));
        let live_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(node_identity.clone());
        // ADR-0017 Phase 3 Slice 5 (carried debt (b)): construct the real
        // `LocallyClaimedEntities` empty now, hand the same `Arc` to both
        // `run_node_lease` (below) and the returned handles — the SM
        // session registry is wired into it later, once
        // `server/http.rs::create_sm_session_registry` builds it (see
        // `ClusteringHandles::local_claims`'s doc comment for the full
        // construction-order rationale).
        let local_claims = local_claims::SmSessionLocalClaims::new();
        // ADR-0017 Phase 3 Slice 7: `NodeLeaseRunConfig` takes exactly one
        // `Arc<dyn LocallyClaimedEntities>` handle — `CombinedLocalClaims`
        // dispatches across both concrete implementors by `entity_type`
        // (see its own doc comment) rather than either one widening to
        // know about the other's entities.
        let combined_local_claims = local_claims::CombinedLocalClaims::new(
            Arc::clone(&local_claims),
            Arc::clone(&room_local_claims),
            Arc::clone(&user_local_claims),
        );
        let node_lease_handle: Arc<dyn claims::NodeLeaseStore> =
            Arc::new(claims::PostgresClaimStore::new(db.clone()));
        // ADR-0017 Phase 3 Slice 7: the durable MUC room store. Built here
        // (not deferred to `server/mod.rs`) because it only needs `db` and
        // `clustering_stop` — both already in scope — unlike
        // `room_local_claims` above, which genuinely must wait for the room
        // registry to exist.
        //
        // FIX 8 (council-adjudicated): a failure to initialize is FATAL to
        // clustering startup, not logged-and-continue. Claims live without
        // this store means a `RoomActor` ownership move silently resets
        // config/affiliations/subject to defaults — an unprotected
        // deposal, not a degraded-but-safe fallback. Mirrors the co-location
        // discipline `sm_persistence`/`pending_delivery`'s
        // `open_for_cluster_mode` already enforce at startup for their own
        // durability stores.
        let muc_durable_store: Arc<dyn waddle_xmpp::muc::MucDurableStore> = Arc::new(
            crate::muc_durable::PostgresMucRoomStore::open(
                db.clone(),
                clustering_stop.clone(),
                live_identity.clone(),
            )
            .await
            .map_err(ClusteringError::MucDurableStoreInit)?,
        );
        let handles = ClusteringHandles {
            claim_store: Some(claim_store_handle),
            node_identity: Some(live_identity.clone()),
            local_claims: Some(Arc::clone(&local_claims)),
            room_local_claims: Some(Arc::clone(&room_local_claims)),
            user_local_claims: Some(Arc::clone(&user_local_claims)),
            muc_durable_store: Some(muc_durable_store),
            node_lease: Some(node_lease_handle),
            lease_ttl: Some(config.node_lease.lease_ttl),
            pod_template_hash: pod_template_hash.clone(),
            resume_bridge: Some(resume_bridge),
            ordered_relay_delivery_bridge: Some(ordered_relay_delivery_bridge),
            stop_token: Some(clustering_stop.clone()),
            fatal_fence: Some(fatal_fence.clone()),
            resume_handshake_timeout: Some(config.resume_handshake.timeout),
        };

        // ADR-0017 Phase 3 Slice 10: keep the join handle so
        // `server/mod.rs` can await this node's graceful drain (which this
        // task runs on its own exit) actually completing before process
        // exit — see `ClusteringShutdown`'s doc comment.
        let node_lease_task = tokio::spawn(self_fence::run_node_lease(
            node_lease,
            node_identity,
            clustering_stop,
            self_fence::NodeLeaseRunConfig {
                pod_template_hash,
                lease_config: config.node_lease.clone(),
                self_fence_config: config.self_fence.clone(),
                connected_peers: handle.connected_peers.clone(),
                local_claims: combined_local_claims,
                readiness,
                live_identity,
                peer_id: Some(local_peer_id),
                // FIX 4(b): the same `ClaimStore` view onto
                // `clustering_claims` as `claim_store_handle` above —
                // wraps the same `db` clone, never a second, independent
                // store.
                claim_store: Arc::new(claims::PostgresClaimStore::new(db.clone())),
                claim_release_budget: config.node_lease.claim_release_budget,
            },
        ));
        Ok((handles, ClusteringShutdown(Some(node_lease_task))))
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
    peer_id: Option<String>,
) -> Result<(), ClusteringError> {
    use claims::NodeLeaseStore as _;
    node_lease
        .register_with_peer_id(identity, pod_template_hash, peer_id)
        .await
        .map_err(ClusteringError::NodeLease)
}

/// Provision the Postgres-backed claim/node-lease control-plane schema before
/// the first node registration. Production startup must own this ordering; the
/// multi-process harness may also pre-provision tables, but a fresh clustered
/// deployment cannot rely on that.
#[cfg(feature = "clustering")]
async fn prepare_node_lease(
    node_lease: &claims::PostgresClaimStore,
    identity: &waddle_xmpp::ownership::NodeIdentity,
    pod_template_hash: Option<String>,
    peer_id: Option<String>,
) -> Result<(), ClusteringError> {
    use waddle_xmpp::ownership::ClaimStore as _;

    node_lease
        .ensure_schema()
        .await
        .map_err(ClusteringError::ClaimStoreSchema)?;
    register_node_lease(node_lease, identity, pod_template_hash, peer_id).await
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use waddle_xmpp::ownership::NodeIdentity;

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

    async fn isolated_test_control_db(
        name: &str,
        schema: &str,
    ) -> anyhow::Result<Option<(Database, sqlx::PgPool)>> {
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            return Ok(None);
        };
        let admin = sqlx::PgPool::connect(&url)
            .await
            .map_err(|error| anyhow::anyhow!("connect postgres admin pool: {error}"))?;
        let create_schema = format!("CREATE SCHEMA {schema}");
        sqlx::query(&create_schema)
            .execute(&admin)
            .await
            .map_err(|error| anyhow::anyhow!("create isolated schema {schema}: {error}"))?;
        let scoped_url = postgres_url_with_search_path(&url, schema);
        let db = match Database::from_config(
            name,
            &DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        {
            Ok(db) => db,
            Err(error) => {
                drop_isolated_schema(&admin, schema).await?;
                return Err(anyhow::anyhow!("open isolated test postgres: {error}"));
            }
        };
        Ok(Some((db, admin)))
    }

    #[tokio::test]
    async fn prepare_node_lease_provisions_schema_before_registration() -> anyhow::Result<()> {
        let schema = unique_postgres_schema_name("clustering_startup");
        let Some((db, admin)) =
            isolated_test_control_db("clustering-startup-schema-test", &schema).await?
        else {
            eprintln!(
                "skipping: WADDLE_TEST_POSTGRES_URL not set \
                 (clustering startup claim/node-lease schema regression)"
            );
            return Ok(());
        };

        let result = async {
            let store = claims::PostgresClaimStore::new(db.clone());
            let identity = NodeIdentity::new(
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
            );
            prepare_node_lease(
                &store,
                &identity,
                Some("test-template".to_string()),
                Some("test-peer".to_string()),
            )
            .await
            .map_err(|error| anyhow::anyhow!("prepare node lease: {error}"))?;

            let conn = db
                .guard()
                .await
                .map_err(|error| anyhow::anyhow!("guard: {error}"))?;
            let mut rows = conn
                .query(
                    "SELECT pod_template_hash, peer_id, expired \
                     FROM clustering_nodes \
                     WHERE node_id = ?",
                    crate::db_params![identity.node_id.as_str()],
                )
                .await
                .map_err(|error| anyhow::anyhow!("query node row: {error}"))?;
            let row = rows
                .next()
                .await
                .map_err(|error| anyhow::anyhow!("node row query: {error}"))?
                .ok_or_else(|| anyhow::anyhow!("node row missing"))?;
            let pod_template_hash = row
                .get::<Option<String>>(0)
                .map_err(|error| anyhow::anyhow!("decode pod_template_hash: {error}"))?;
            if pod_template_hash.as_deref() != Some("test-template") {
                anyhow::bail!(
                    "unexpected pod_template_hash: {:?}",
                    pod_template_hash.as_deref()
                );
            }
            let peer_id = row
                .get::<Option<String>>(1)
                .map_err(|error| anyhow::anyhow!("decode peer_id: {error}"))?;
            if peer_id.as_deref() != Some("test-peer") {
                anyhow::bail!("unexpected peer_id: {:?}", peer_id.as_deref());
            }
            let expired = row
                .get::<bool>(2)
                .map_err(|error| anyhow::anyhow!("decode expired: {error}"))?;
            if expired {
                anyhow::bail!("fresh node lease row must start non-expired");
            }
            Ok(())
        }
        .await;
        drop(db);

        let cleanup = drop_isolated_schema(&admin, &schema).await;
        result?;
        cleanup?;
        Ok(())
    }

    async fn drop_isolated_schema(admin: &sqlx::PgPool, schema: &str) -> anyhow::Result<()> {
        let drop_schema = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
        sqlx::query(&drop_schema)
            .execute(admin)
            .await
            .map_err(|error| anyhow::anyhow!("drop isolated schema {schema}: {error}"))?;
        Ok(())
    }

    fn unique_postgres_schema_name(prefix: &str) -> String {
        format!("waddle_test_{prefix}_{}", uuid::Uuid::new_v4().simple())
    }

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse postgres url");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }
}
