//! Self-fencing, isolation detection with the N=2 lone-survivor carve-out,
//! and re-registration hysteresis (ADR-0017 Phase 3 Slice 2, element 4).
//!
//! Mirrors `swarm.rs`'s keypair-slot lease heartbeat
//! ([`super::swarm`]'s `run_heartbeat`): single-flight renewals, a `biased`
//! deadline-arm `select!` (never a timeout-dropped in-flight sqlx future —
//! see that function's doc comment for why), and a fencing loss that
//! cancels only the clustering scope, never the whole process. What
//! differs: **what is fenced**. The keypair-slot lease (`lease.rs`) guards
//! *which libp2p identity* a process holds — losing it means another node
//! may already be using this node's `PeerId`, so the swarm/relay subsystem
//! must stop entirely. The node lease here guards *entity-ownership
//! claims* (`clustering_claims`, via [`super::claims::NodeLeaseStore`]) —
//! losing it means this node no longer authoritatively owns any of its
//! claimed entities, but its libp2p identity and swarm membership remain
//! valid. The two leases are orthogonal (Q5's "no coupling" precedent
//! between distinct clustering leases applies here too): a node that loses
//! its node lease demotes locally-claimed entities and flips readiness, but
//! does not tear down the swarm — it works to re-register under a fresh
//! node identity and resume serving.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use waddle_xmpp::ownership::{ClaimEpoch, ClaimStore, Entity, NodeIdentity};
#[cfg(test)]
use waddle_xmpp::ownership::{ClaimError, EntityType};

use super::claims::NodeLeaseStore;
use super::metrics;
use super::ClusteringReadiness;
use crate::config::{ClusteringNodeLeaseConfig, ClusteringSelfFenceConfig};

/// Strict per-entity result used by post-fence recovery's readiness gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimedEntityHydration {
    /// The exact candidate grant is represented in local state.
    Local,
    /// A current-claim read proved a different node now owns the entity.
    Elsewhere,
    /// The exact recovery grant was released or was already absent.
    TerminallyReleased,
    /// Ownership/storage/local initialization is uncertain; stay not-ready.
    Retry,
}

/// Readable snapshot of the swarm's current connected-peer count (Phase 2
/// Slice 1's connected-peer gauge, reused here rather than inventing a
/// second peer-tracking mechanism — the plan's own Files line for this
/// module says to reuse it).
///
/// **Deliberate simplification, noted as a resolved ambiguity — and FIX 5's
/// corrected safety direction (the previous revision of this doc comment
/// had it backwards)**: element 4's isolation rule is "this node can reach
/// **none of** [the two-or-more other live nodes] over the swarm." Exact
/// per-node reachability would require correlating specific `PeerId`s to
/// specific `clustering_nodes` rows, which needs an allowlist/kademlia
/// identity mapping this phase does not build (that correlation is a Phase
/// 4 concern, once cross-node routing needs to address a specific node).
/// Slice 2 approximates "reaches none of the live peers" as "zero
/// connected swarm peers, of *any* kind" — coarser than per-node
/// reachability, and **the approximation can only ever UNDER-fence,
/// never over-fence**:
///
/// - If this node is connected to at least one *live* peer, the real
///   condition is false (not isolated) — and `reachable_peers >= 1`, so
///   the approximation also correctly says "not isolated." The two agree.
/// - If this node is connected to **zero** peers of any kind, it cannot
///   possibly be connected to a live one either, so the real condition is
///   also true — the two again agree, and this is the case that fences.
/// - The gap: this node could be connected to one or more **stale/
///   non-live** peers (a peer whose own `clustering_nodes` row has since
///   gone `expired`/`draining`, but whose libp2p connection has not yet
///   been torn down) while reaching **zero** live peers. Here the real
///   condition is true (isolated from every live peer) but
///   `reachable_peers >= 1` (the stale connection still counts toward the
///   coarse swarm-level gauge), so the approximation says "not isolated"
///   and **fails to fence when the ADR's literal rule would**.
///
/// So the coarse signal is a safe approximation in exactly one direction:
/// it can never manufacture isolation that isn't real (no false
/// self-fence), but it can under-count real isolation and delay or skip a
/// self-fence the exact per-node rule would have triggered. Accepted as an
/// interim gap pending the Phase 4 `PeerId` ↔ `NodeId` correlation (plan
/// deviation #17).
#[derive(Debug, Clone, Default)]
pub struct ConnectedPeerCount(Arc<AtomicI64>);

impl ConnectedPeerCount {
    pub fn new() -> Self {
        Self(Arc::new(AtomicI64::new(0)))
    }

    pub fn set(&self, count: i64) {
        self.0.store(count, Ordering::Release);
    }

    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

/// Supplies the authoritative local owned-entity set the
/// demotion-reconciliation diff runs against, and demotes entities Postgres
/// no longer attributes to this node.
///
/// **No production implementor lands in Slice 2** — the mechanism and its
/// tests land here, but no code acquires a Postgres-backed `sm_session`
/// claim in production until the fenced `SmPersistenceStorage` (Phase 3
/// Slice 4) starts calling `ClaimStore::acquire` at `<enable/>` time. Until
/// then, [`start_if_enabled`](super::start_if_enabled) wires
/// [`NoLocallyClaimedEntities`].
#[async_trait]
pub trait LocallyClaimedEntities: Send + Sync {
    /// Every entity whose durable claim this process currently believes it
    /// owns, from local authoritative bookkeeping only — never a Postgres
    /// read. A socket or proxy physically hosted by this process but owned by
    /// another node MUST NOT appear here: this list is compared directly with
    /// Postgres and every absent entry is demoted.
    ///
    /// `async` (ADR-0017 Phase 3 Slice 7 widening) because an actor-backed
    /// implementor (`RoomActor`'s registry) can only be queried through its
    /// own mailbox — the enumeration itself is still pure in-memory
    /// bookkeeping, never a Postgres round-trip.
    async fn owned(&self) -> Vec<Entity>;

    /// Demote everything that must stop when this entire node loses its lease.
    ///
    /// Unlike [`Self::owned`], this terminal hook may include resources that
    /// are only physically hosted here, such as clustered remote-user sockets
    /// whose authoritative `UserActor` belongs to another node. The default is
    /// correct for claim types whose hosted and authoritative ownership sets
    /// are identical; implementations with proxy/remote resources override it.
    /// Returns `true` only when terminal teardown completed. A `false` result
    /// keeps the node not-ready and is retried before claim service resumes;
    /// timing out a cooperative socket close must never let an old-generation
    /// connection survive into a newly ready identity.
    async fn demote_all_on_self_fence(&self) -> bool {
        for entity in self.owned().await {
            self.demote(&entity).await;
        }
        true
    }

    /// Demote local state for an entity Postgres no longer attributes to
    /// this node (claim stolen, or this node's own claim row is gone).
    /// Purely local: must succeed even when Postgres is unreachable — the
    /// self-fencing trigger this method exists to serve. Best-effort and
    /// idempotent.
    ///
    /// **FIX 3 — must be effective against a wedged target.** `demote` is
    /// called from two distinct situations, both of which must actually
    /// stop the entity from continuing to act as owner even if its actor is
    /// stuck:
    ///
    /// 1. **Reconcile-deposed** (`reconcile`'s lost-claims list, and the
    ///    self-fenced block at the bottom of [`run_node_lease`]): Postgres
    ///    has already moved the claim elsewhere, so this node's copy is
    ///    provably stale — but the local actor may still be alive and, if
    ///    wedged, unresponsive to an ordinary `tell`.
    /// 2. **Veto-health-fail** (the steal-intent scan's `local_claims.
    ///    health_check(&entity)` returning `false`): the actor has *already*
    ///    failed to answer a bounded health ask — by construction, this is
    ///    the wedged case, not a healthy actor that merely hasn't gotten to
    ///    it yet.
    ///
    /// A correct implementation therefore MUST NOT be a mailbox `tell` that
    /// simply queues behind whatever the actor is wedged inside — that
    /// would never run. It must use the same **hard-kill discipline** as
    /// [`waddle_xmpp::registry::user_actor::health_check_or_wedge_kill`]
    /// (`waddle-xmpp`'s exemplar for this exact shape): tear down owned
    /// resources best-effort, then call the actor handle's `kill()`
    /// directly, which drops the actor's state (and every resource sender
    /// it holds) regardless of whether the actor's mailbox loop is
    /// otherwise stuck. A future `UserActor`/`RoomActor`-backed
    /// implementation of this trait must route `demote` through that kind
    /// of hard kill, not a `tell`.
    async fn demote(&self, entity: &Entity);

    /// Health-ask the local actor owning `entity` (ADR-0017 Phase 3 Slice
    /// 3's owner-veto path): `true` if it answered promptly (the owner may
    /// then clear the entity's steal intent — FIX 1(e): the veto is
    /// enforced by serializing on the intent rows against a concurrent
    /// steal, deadlock-abort-safe per FIX 1(c), not by any inherent
    /// "unforgeability" of the write itself), `false` if the ask failed or
    /// timed out (wedged — the caller proactively demotes rather than
    /// waiting to be stolen from at `intent_ttl`, per element 4's "an owner
    /// whose internal health ask fails ... kills the wedged actor and
    /// retires its sockets" text).
    ///
    /// Never called against an entity absent from [`Self::owned`] — the
    /// veto-scan loop only health-asks entities `owner_steal_intents`
    /// reports as both currently claimed by this node *and* bearing an
    /// outstanding intent.
    async fn health_check(&self, entity: &Entity) -> bool;

    /// Targeted-hydration hook for FIX 4's inline post-fence reclaim
    /// (ADR-0017 Phase 3 Slice 5 corrigenda, council-adjudicated): given
    /// entities this node's re-registration retry just re-won via
    /// `steal_stale` (paired with the epoch the steal returned), hydrate
    /// them into whatever local state this implementation backs.
    ///
    /// Implementations MUST honor the same discipline
    /// [`waddle_xmpp::stream_management::InMemorySmSessionRegistry::hydrate_reclaimed`]
    /// documents — never a table scan, never a blind insert — since this
    /// is called from a node that is already back to serving live traffic
    /// by the time it runs. Default no-op, mirroring [`Self::demote`]'s
    /// own no-op default: correct for [`NoLocallyClaimedEntities`], which
    /// owns nothing to hydrate.
    async fn hydrate_reclaimed(
        &self,
        owner: &NodeIdentity,
        entity: &Entity,
        epoch: ClaimEpoch,
    ) -> ReclaimedEntityHydration {
        let _ = owner;
        let _ = entity;
        let _ = epoch;
        ReclaimedEntityHydration::Retry
    }

    /// ADR-0017 Phase 3 Slice 10: complete `entity`'s final fenced write —
    /// whatever "durable, up to date, and safe to hand to a new owner"
    /// means for this entity kind — and report whether it is now safe to
    /// queue for release. Called ONLY from the graceful-drain sequence
    /// (`clustering::drain::run_shutdown_drain`), and strictly BEFORE that
    /// caller ever queues `entity` into a batched
    /// [`ClaimStore::release_many`](waddle_xmpp::ownership::ClaimStore::release_many)
    /// call — never after (releasing first and completing a write second
    /// is the exact fencing violation element 4 forbids). Returns `false`
    /// on any failure/timeout: the caller leaves the claim held (counted
    /// `claims_abandoned_on_drain`, fenced-safe, reclaimed later by the
    /// orphan reaper) rather than force-releasing an entity whose final
    /// state it could not confirm.
    ///
    /// Default `true`: correct for [`NoLocallyClaimedEntities`] (nothing
    /// owned, never called) and for any entity kind whose durable state is
    /// already fenced-written synchronously on every mutation, so there is
    /// nothing new to flush at drain time.
    async fn seal_before_release(&self, entity: &Entity) -> bool {
        let _ = entity;
        true
    }
}

/// No claimed entities — see the trait doc for why this is the only
/// production wiring Slice 2 has.
pub struct NoLocallyClaimedEntities;

#[async_trait]
impl LocallyClaimedEntities for NoLocallyClaimedEntities {
    async fn owned(&self) -> Vec<Entity> {
        Vec::new()
    }

    async fn demote(&self, _entity: &Entity) {}

    // Never actually invoked in production this slice: `owned()` is always
    // empty, so `run_node_lease`'s veto scan (Slice 3) has nothing to
    // health-ask against. Trivially healthy rather than panicking/`todo!`,
    // matching this type's existing no-op `demote`.
    async fn health_check(&self, _entity: &Entity) -> bool {
        true
    }
}

/// Isolation-with-hysteresis decision state (element 4's locked spec).
/// Pure and synchronous — no timer of its own — so [`run_node_lease`] (and
/// this module's own unit tests) drive it once per heartbeat interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IsolationTracker {
    consecutive_isolated_intervals: u32,
}

impl IsolationTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one interval's observation and return whether renewal should be
    /// refused *this* interval (isolation fencing).
    ///
    /// `other_live_nodes` is this interval's live-row count from
    /// `clustering_nodes`, excluding self (fresh per Postgres, the
    /// authority); `reachable_peers` is the current swarm connected-peer
    /// count. Fencing requires **both**: `other_live_nodes >= 2` (the N=2
    /// lone-survivor carve-out — with exactly one other live node, swarm
    /// unreachability alone never fences: there is no witness to assign
    /// blame) and zero reachable peers for `isolation_intervals` (M)
    /// consecutive intervals running (a single blip does not fence).
    pub fn observe(
        &mut self,
        other_live_nodes: usize,
        reachable_peers: usize,
        isolation_intervals: u32,
    ) -> bool {
        let isolated_this_interval = other_live_nodes >= 2 && reachable_peers == 0;
        if isolated_this_interval {
            self.consecutive_isolated_intervals =
                self.consecutive_isolated_intervals.saturating_add(1);
        } else {
            self.consecutive_isolated_intervals = 0;
        }
        self.consecutive_isolated_intervals >= isolation_intervals.max(1)
    }

    /// Reset after a re-registration, so a freshly re-registered identity
    /// starts with a clean isolation history.
    pub fn reset(&mut self) {
        self.consecutive_isolated_intervals = 0;
    }

    #[cfg(test)]
    fn consecutive_isolated_intervals(&self) -> u32 {
        self.consecutive_isolated_intervals
    }
}

/// Re-acquisition hysteresis gate (element 4): after a fence, claims are
/// re-acquired only once this holds — "whenever other live node rows
/// exist, only after observing swarm reachability to at least one of
/// them." With no other live rows at all (this node is the sole survivor
/// of its own re-registration), there is nothing to wait for.
pub fn can_reacquire_claims(other_live_nodes: usize, reachable_peers: usize) -> bool {
    other_live_nodes == 0 || reachable_peers >= 1
}

/// Exponential backoff between post-fence re-registration attempts
/// (element 4: without it, two mutually swarm-partitioned survivors
/// oscillate forever — fence, re-register, observe still-isolated, fence
/// again).
pub struct ReregistrationBackoff {
    base: Duration,
    max: Duration,
    attempts: u32,
}

impl ReregistrationBackoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempts: 0,
        }
    }

    /// The delay before the next re-registration attempt; advances the
    /// attempt counter so the *following* call doubles again.
    pub fn next_delay(&mut self) -> Duration {
        let shift = self.attempts.min(31);
        let scaled = self
            .base
            .checked_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
            .unwrap_or(self.max);
        self.attempts = self.attempts.saturating_add(1);
        scaled.min(self.max)
    }

    /// Reset after a successful re-registration.
    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    #[cfg(test)]
    fn attempts(&self) -> u32 {
        self.attempts
    }
}

/// Grouped configuration for [`run_node_lease`] — construction args bundled
/// into a struct rather than widening a positional argument list (clippy
/// `too_many_arguments`; mirrors this codebase's `AppStateDeps` precedent
/// for the same concern, per the repo's no-`#[allow]` hard rule).
pub struct NodeLeaseRunConfig {
    pub pod_template_hash: Option<String>,
    pub lease_config: ClusteringNodeLeaseConfig,
    pub self_fence_config: ClusteringSelfFenceConfig,
    pub connected_peers: ConnectedPeerCount,
    pub local_claims: Arc<dyn LocallyClaimedEntities>,
    pub readiness: ClusteringReadiness,
    /// FIX 4(b) (ADR-0017 Phase 3 Slice 5 corrigenda, council-adjudicated):
    /// the same `ClaimStore` handle every other clustering-aware call site
    /// binds — never a second, independent store — used by the
    /// re-registration success path to `steal_stale(OwnerStale)` this
    /// node's own just-expired identity's SM-session claims back under
    /// the freshly re-registered one, inline, rather than waiting out the
    /// general orphan reaper's independent cadence.
    pub claim_store: Arc<dyn ClaimStore>,
    /// Live, shared view of this node's current identity (ADR-0017 Phase 3
    /// Slice 4 follow-up plumbing note): [`run_node_lease`] calls
    /// [`waddle_xmpp::ownership::SharedNodeIdentity::set`] on it after every
    /// post-fence epoch rotation, so any other holder of a clone — e.g. the
    /// Postgres-fenced `SmPersistenceStorage`'s claim-acquire calls —
    /// always binds the identity currently in force, never a stale
    /// pre-fence snapshot.
    pub live_identity: waddle_xmpp::ownership::SharedNodeIdentity,
    /// The libp2p PeerId currently held by this process's leased swarm
    /// keypair, bound into every node-lease registration for the current
    /// process. Re-registrations rotate the node epoch, but they do not change
    /// the process `node_id` or mint a fresh swarm keypair, so both the relay
    /// address and PeerId binding stay stable across a self-fence.
    pub peer_id: Option<String>,
    /// ADR-0017 Phase 3 Slice 10: the graceful per-entity claim-release
    /// drain's time budget (`ClusteringNodeLeaseConfig::claim_release_budget`,
    /// `claimReleaseBudget` in the ADR's own text) — bound on
    /// [`crate::clustering::drain::run_shutdown_drain`], called from every
    /// ordinary-shutdown branch in [`run_node_lease`]'s `'tick` loop below.
    pub claim_release_budget: Duration,
    /// Exact predecessor replaced during cold start. When present, the
    /// current identity was registered draining and this task must run the
    /// same strict reclaim/hydrate/activate state machine before serving.
    pub startup_recovery_source: Option<NodeIdentity>,
}

/// Best-effort, time-bounded [`NodeLeaseStore::mark_draining`] — mirrors
/// `swarm.rs::release_slot_bounded`'s pattern exactly, one lease kind over:
/// a hung call must not stall the caller, whether that caller is the
/// re-registration loop marking a just-fenced identity draining (FIX 1(b))
/// or ordinary shutdown marking the still-live identity draining before
/// returning (FIX 3). Marking draining is advisory cleanup only — the row
/// ages out on its own via its own lapsed heartbeat/TTL either way, and
/// [`super::claims::PostgresClaimStore::count_other_live_nodes`] already
/// excludes draining rows regardless of heartbeat freshness (FIX 1(c)) — so
/// a timed-out or failed attempt here is logged and ignored, never retried
/// or escalated: it only narrows how quickly other nodes stop counting this
/// row as live, never a correctness requirement.
pub(super) async fn mark_draining_bounded<L>(lease: &L, identity: &NodeIdentity, budget: Duration)
where
    L: NodeLeaseStore + Send + Sync,
{
    match tokio::time::timeout(budget, lease.mark_draining(identity)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(
                %error,
                node_id = %identity.node_id,
                "clustering: failed to mark node-lease row draining"
            );
        }
        Err(_) => {
            tracing::warn!(
                node_id = %identity.node_id,
                "clustering: marking node-lease row draining timed out; row ages out via \
                 its own lapsed heartbeat"
            );
        }
    }
}

/// Best-effort, time-bounded [`NodeLeaseStore::expire`] on this node's own
/// just-superseded identity (ADR-0017 Phase 3 Slice 5, plan deviation #19)
/// — see the call site's doc comment for the full rationale. A failure or
/// timeout here is logged and ignored: the row still ages out naturally via
/// its own lapsed heartbeat, so this is purely an acceleration, never a
/// correctness requirement — nothing downstream depends on it succeeding.
async fn expire_bounded<L>(lease: &L, identity: &NodeIdentity, lease_ttl: Duration)
where
    L: NodeLeaseStore + Send + Sync,
{
    match tokio::time::timeout(lease_ttl, lease.expire(identity, lease_ttl)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(
                %error,
                node_id = %identity.node_id,
                "clustering: failed to expire this node's own just-superseded identity; \
                 orphaned claims still age out via the row's own lapsed heartbeat"
            );
        }
        Err(_) => {
            tracing::warn!(
                node_id = %identity.node_id,
                "clustering: expiring this node's own just-superseded identity timed out; \
                 orphaned claims still age out via the row's own lapsed heartbeat"
            );
        }
    }
}

/// FIX 4(b) (ADR-0017 Phase 3 Slice 5 corrigenda, council-adjudicated):
/// reclaim `old_identity`'s own orphaned `sm_session` claims inline, under
/// `fresh`, rather than waiting for the general orphan reaper's
/// independent cadence to notice them. `list_orphaned_sm_session_claims`
/// returns candidates cluster-wide (any node's dead claims); this function
/// filters to exactly `old_identity` — this node's own just-superseded
/// identity, provably dead because THIS loop is the one that just
/// re-registered past it — so it never touches another node's genuinely
/// dead claims (the general reaper remains the sole mechanism for those,
/// preserving the "backstop for other nodes" carried-risk boundary FIX 4's
/// own doc note names honestly).
///
/// This sequence is deliberately not cancellation-raced or timeout-wrapped:
/// once any `steal_stale` commits, dropping the future before targeted
/// hydration would strand a fresh-owned durable session outside the local
/// registry. The control-plane calls retain their database/pool bounds, but
/// the mutation-plus-hydration sequence itself runs to a definite outcome.
struct RecoveryState {
    sources: HashSet<NodeIdentity>,
    registration_source: NodeIdentity,
    candidate: NodeIdentity,
    candidate_registered: bool,
    candidate_grants: HashMap<Entity, ClaimEpoch>,
}

impl RecoveryState {
    fn new(identity: &NodeIdentity) -> Self {
        let mut sources = HashSet::new();
        sources.insert(identity.clone());
        Self {
            sources,
            registration_source: identity.clone(),
            candidate: NodeIdentity::new(
                identity.node_id.clone(),
                uuid::Uuid::new_v4().to_string(),
            ),
            candidate_registered: false,
            candidate_grants: HashMap::new(),
        }
    }

    fn from_registered_candidate(source: NodeIdentity, candidate: NodeIdentity) -> Self {
        let mut sources = HashSet::new();
        sources.insert(source.clone());
        Self {
            sources,
            registration_source: source,
            candidate,
            candidate_registered: true,
            candidate_grants: HashMap::new(),
        }
    }

    fn rotate_candidate(&mut self) {
        self.sources.insert(self.candidate.clone());
        self.registration_source = self.candidate.clone();
        self.candidate = NodeIdentity::new(
            self.registration_source.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
        );
        self.candidate_registered = false;
        // Every grant in this map belongs to the candidate that just became
        // a source. Once the replacement row is registered, the exact source
        // scan will rediscover it and mint a new grant for the new candidate.
        self.candidate_grants.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPass {
    Complete,
    Retry,
    RotateCandidate,
}

async fn hydrate_pending_recovery_grants(
    state: &mut RecoveryState,
    local_claims: &dyn LocallyClaimedEntities,
) -> bool {
    let pending = state
        .candidate_grants
        .iter()
        .map(|(entity, epoch)| (entity.clone(), *epoch))
        .collect::<Vec<_>>();
    let mut complete = true;
    for (entity, epoch) in pending {
        let outcome = local_claims
            .hydrate_reclaimed(&state.candidate, &entity, epoch)
            .await;
        match outcome {
            // Keep locally hydrated grants as recovery provenance. Every
            // outer retry terminally demotes local state before entering the
            // next pass, so the exact candidate-owned grant must be hydrated
            // again on that later pass before readiness can be restored.
            ReclaimedEntityHydration::Local => {}
            ReclaimedEntityHydration::Elsewhere | ReclaimedEntityHydration::TerminallyReleased => {
                state.candidate_grants.remove(&entity);
            }
            ReclaimedEntityHydration::Retry => complete = false,
        }
    }
    complete
}

async fn reclaim_own_expired_claims<L>(
    lease: &L,
    claim_store: &dyn ClaimStore,
    state: &mut RecoveryState,
    local_claims: &dyn LocallyClaimedEntities,
    lease_ttl: Duration,
) -> RecoveryPass
where
    L: NodeLeaseStore + Send + Sync,
{
    // Hydrate every candidate-owned grant at most once per recovery pass.
    // A retry stays sticky for this pass and is attempted again only after
    // the outer backoff/terminal-demotion boundary.
    let mut complete = hydrate_pending_recovery_grants(state, local_claims).await;
    let candidates = match lease.list_orphaned_sm_session_claims().await {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(
                %error,
                node_id = %state.candidate.node_id,
                "clustering: inline post-fence reclaim: list_orphaned_sm_session_claims failed"
            );
            return RecoveryPass::Retry;
        }
    };

    for candidate in candidates {
        if !state.sources.contains(&candidate.owner) {
            continue;
        }
        let reclaimed = claim_store
            .reclaim_after_self_fence(
                &candidate.entity,
                candidate.epoch,
                &candidate.owner,
                &state.candidate,
                lease_ttl,
            )
            .await;
        let grant = match reclaimed {
            Ok(new_epoch) => Some(new_epoch),
            Err(error) => {
                tracing::warn!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "clustering: inline post-fence exact reclaim did not return a grant"
                );
                // Conflict is not inherently benign: the source may still
                // own the grant because the destination expired. Resolve
                // every losing/ambiguous result against the current claim.
                match claim_store.current_claim(&candidate.entity).await {
                    Ok(Some(snapshot)) if snapshot.owner == state.candidate => {
                        Some(snapshot.claim_epoch)
                    }
                    Ok(Some(snapshot)) if state.sources.contains(&snapshot.owner) => {
                        complete = false;
                        None
                    }
                    Ok(Some(snapshot)) if snapshot.owner.node_id != state.candidate.node_id => {
                        // A different process won; this node must not hydrate
                        // a second copy.
                        None
                    }
                    Ok(None) => None,
                    Ok(Some(_)) | Err(_) => return RecoveryPass::RotateCandidate,
                }
            }
        };

        if let Some(epoch) = grant {
            state
                .candidate_grants
                .insert(candidate.entity.clone(), epoch);
            let outcome = local_claims
                .hydrate_reclaimed(&state.candidate, &candidate.entity, epoch)
                .await;
            match outcome {
                // Retain the exact grant for any later recovery pass. If a
                // subsequent completion check fails, the outer loop demotes
                // this local state and the next pass must rehydrate it.
                ReclaimedEntityHydration::Local => {}
                ReclaimedEntityHydration::Elsewhere
                | ReclaimedEntityHydration::TerminallyReleased => {
                    state.candidate_grants.remove(&candidate.entity);
                }
                ReclaimedEntityHydration::Retry => complete = false,
            }
        }
    }

    // A second exact scan is the completion barrier. The first scan can race
    // a final pre-teardown detach; readiness is forbidden while any durable
    // SM claim still names any epoch superseded during this recovery.
    let remaining_sources = match lease.list_orphaned_sm_session_claims().await {
        Ok(candidates) => candidates
            .into_iter()
            .any(|candidate| state.sources.contains(&candidate.owner)),
        Err(error) => {
            tracing::warn!(%error, "clustering: recovery completion scan failed");
            return RecoveryPass::Retry;
        }
    };
    if complete && !remaining_sources {
        // Every retained grant was hydrated exactly once in this pass, and
        // no source claim remains. Consume recovery bookkeeping only at the
        // completion barrier so an incomplete pass can survive the outer
        // loop's terminal demotion and rehydrate on its next attempt.
        state.candidate_grants.clear();
        RecoveryPass::Complete
    } else {
        RecoveryPass::Retry
    }
}

/// Poll a potentially long multi-session recovery pass while keeping the
/// exact draining candidate's lease alive. A false heartbeat is remembered,
/// but does not cancel the in-flight pass: a reclaim CAS may already have
/// committed and must reach a definite hydration outcome before the
/// candidate becomes a lineage source.
async fn reclaim_with_candidate_heartbeat<L>(
    lease: &L,
    claim_store: &dyn ClaimStore,
    state: &mut RecoveryState,
    local_claims: &dyn LocallyClaimedEntities,
    heartbeat_interval: Duration,
    lease_ttl: Duration,
) -> RecoveryPass
where
    L: NodeLeaseStore + Send + Sync,
{
    let candidate = state.candidate.clone();
    let recovery = std::pin::pin!(reclaim_own_expired_claims(
        lease,
        claim_store,
        state,
        local_claims,
        lease_ttl,
    ));
    let mut recovery = recovery;
    let mut ticker = tokio::time::interval(heartbeat_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    let mut candidate_lost = false;

    loop {
        tokio::select! {
            biased;
            result = &mut recovery => {
                return if candidate_lost {
                    RecoveryPass::RotateCandidate
                } else {
                    result
                };
            }
            _ = ticker.tick(), if !candidate_lost => {
                match lease.heartbeat(&candidate, lease_ttl).await {
                    Ok(true) => {}
                    Ok(false) => candidate_lost = true,
                    Err(error) => tracing::warn!(
                        %error,
                        node_epoch = %candidate.node_epoch,
                        "recovery candidate heartbeat failed during hydration"
                    ),
                }
            }
        }
    }
}

/// ADR-0017 Phase 3 Slice 10 FIX 2 (council-adjudicated): run
/// [`crate::clustering::drain::run_shutdown_drain`] while CONTINUING to
/// renew this node's node-lease heartbeat for the drain's full duration —
/// never as a terminal action taken only AFTER the last heartbeat.
///
/// **The bug this closes**: every `stop_token.cancelled()` arm in
/// [`run_node_lease`]'s `'tick` loop previously did
/// `crate::clustering::drain::run_shutdown_drain(..).await; return;`
/// directly — the instant shutdown fired, heartbeat renewal stopped dead,
/// and the whole per-entity drain (sealing/releasing owned `RoomActor`
/// claims, bounded by `claim_release_budget`) ran with this node's
/// `clustering_nodes` row frozen at whatever heartbeat it last landed.
/// Element 4's drain sequence requires a draining node to "stay live in
/// `nodes`" (heartbeat fresh, draining flag set) for as long as it is
/// still actually draining — a node that is still legitimately sealing and
/// writing owned claims must not simultaneously look heartbeat-stale to
/// another node's orphan reaper (120s sweep), or that reaper's
/// `expire()`/`steal_stale(OwnerStale)` can steal a claim this node has not
/// actually released yet: two nodes writing the same entity, the exact
/// split-brain fencing exists to exclude. This gap widens with
/// `claim_release_budget` (an operator-raised budget, or a drain that
/// legitimately runs long sealing many entities) the closer it gets to
/// `node_lease_ttl` — see `config.rs`'s validation requiring
/// `node_lease_ttl` to comfortably exceed `claim_release_budget` for the
/// defense-in-depth half of this fix.
///
/// **The fix**: poll the drain future and a heartbeat-renewal ticker
/// concurrently, in THIS SAME task/stack frame — no `tokio::spawn`, so no
/// `Clone`/`'static` bound is needed on `lease` (both futures simply
/// borrow it for the duration of this call, ordinary structured
/// concurrency). [`run_node_lease`] does not return from this call until
/// the drain future itself resolves; the heartbeat ticker keeps this node
/// looking live to the rest of the cluster for exactly as long as that
/// takes. Heartbeat renewal here is deliberately best-effort only — a
/// failed/errored renewal during drain is logged and ignored, never
/// escalated to a second fence: this node already knows it is shutting
/// down, and the sole purpose of these renewals is to keep the row
/// visible as live for as long as this node is still legitimately
/// finishing its own writes, not to re-run the ordinary fencing-loss
/// handling mid-drain.
pub(super) async fn run_shutdown_drain_with_heartbeat<L>(
    lease: &L,
    claim_store: &Arc<dyn ClaimStore>,
    identity: &NodeIdentity,
    local_claims: &Arc<dyn LocallyClaimedEntities>,
    claim_release_budget: Duration,
    lease_ttl: Duration,
) where
    L: NodeLeaseStore + Send + Sync,
{
    let drain = std::pin::pin!(crate::clustering::drain::run_shutdown_drain(
        lease,
        claim_store,
        identity,
        local_claims,
        claim_release_budget,
    ));
    let mut drain = drain;

    // Renew comfortably inside `lease_ttl` — halved, floored at a tiny 10ms
    // (only to guard `tokio::time::interval`'s own "period must be > 0"
    // panic against a pathological near-zero `lease_ttl`; config
    // validation already requires `lease_ttl >= heartbeat_interval * 2 >
    // 0`, so this floor is never the binding constraint in practice).
    // Deliberately NOT floored at anything close to `lease_ttl` itself —
    // a fixed floor (e.g. 1s) would make renewal SLOWER than a small
    // configured `lease_ttl`, inverting this function's entire purpose:
    // the row would still go stale mid-drain, just via a different
    // mechanism. This node's heartbeat is already fresh going into this
    // call (the ordinary tick loop's own last successful renewal is what
    // got it here), so the ticker's first tick is deliberately consumed
    // unused below rather than firing an immediate, redundant renewal on
    // the common, fast-draining path.
    let renewal_period = (lease_ttl / 2).max(Duration::from_millis(10));
    let mut ticker = tokio::time::interval(renewal_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = &mut drain => {
                return;
            }
            _ = ticker.tick() => {
                match lease.heartbeat(identity, lease_ttl).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(
                            node_id = %identity.node_id,
                            "clustering: heartbeat renewal during graceful drain reported \
                             fencing loss (0 rows affected); continuing the drain regardless \
                             — this node is already shutting down"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            node_id = %identity.node_id,
                            "clustering: heartbeat renewal during graceful drain failed; the \
                             row may go stale before the drain completes (best-effort only — \
                             this node is already shutting down)"
                        );
                    }
                }
            }
        }
    }
}

/// Drive node-lease renewal, demotion reconciliation, isolation-aware
/// self-fencing, and post-fence re-registration until `stop_token` fires.
///
/// Two independent fencing triggers, exactly per element 4: (1) the
/// heartbeat CAS affects zero rows (`Ok(false)` — another registration or
/// steal already invalidated this identity); (2) `lease_ttl` elapses since
/// the last successful renewal without one landing (Postgres unreachable)
/// — the same deadline-arm shape as `swarm::run_heartbeat`, for the same
/// reason: an abandoned in-flight sqlx future during a sustained partition
/// would otherwise wedge the pool's background `ping()` re-vetting, one
/// connection per tick. A hung renewal is polled to completion, raced only
/// against the deadline and shutdown (single-flight — never more than one
/// renewal in flight, never dropped except on fence/shutdown).
///
/// **FIX 2**: `count_other_live_nodes` and `reconcile` (and, from ADR-0017
/// Phase 3 Slice 3, `owner_steal_intents`/`clear_steal_intent`) are
/// control-plane calls too, and are deadline-armed identically — raced
/// against the same
/// `sleep_until(last_success + lease_ttl)` deadline the heartbeat itself
/// uses (biased, so the deadline always wins a simultaneous wakeup): a hung
/// call here fences exactly as if the heartbeat call itself had blown its
/// deadline, rather than parking the loop indefinitely with no bound. As
/// with the heartbeat, at most one abandoned in-flight future is ever
/// dropped per fence (never per-tick) — the deadline fires at most once
/// before the loop exits to the self-fenced block below.
///
/// On either trigger: flip `readiness` to not-ready, then concurrently run
/// `local_claims.demote_all_on_self_fence()` (a purely local action that must
/// stop every authoritative claim and hosted proxy even while Postgres is
/// unreachable) and **mark the just-fenced identity's row draining (FIX 1(b),
/// bounded best-effort)**, then
/// loop re-registration attempts (exponential backoff) until one succeeds
/// *and* the hysteresis gate ([`can_reacquire_claims`]) is satisfied — at which
/// point readiness flips back to ready and normal heartbeat/reconciliation
/// resumes under the process's stable `node_id` and freshly minted epoch.
///
/// **FIX 1(a)**: `node_id` is the process-stable clustering address used by
/// both the node lease and the relay actor. A fresh `node_epoch` is minted
/// **once per fence**, before the retry loop, and every retry within that
/// fence reuses it. Rotating `node_id` would strand the relay actor under its
/// original name and leak a new `clustering_nodes` row per fence; minting a
/// new epoch on every retry would make each successful registration supersede
/// the identity used by the previous retry. Keeping the address stable and
/// rotating one epoch per fence gives Postgres a single bounded node row while
/// still fencing all claims from the prior incarnation.
pub async fn run_node_lease<L>(
    lease: L,
    mut identity: NodeIdentity,
    stop_token: CancellationToken,
    run_config: NodeLeaseRunConfig,
) where
    L: NodeLeaseStore + Send + Sync + 'static,
{
    let NodeLeaseRunConfig {
        pod_template_hash,
        lease_config: config,
        self_fence_config: self_fence_cfg,
        connected_peers,
        local_claims,
        readiness,
        live_identity,
        peer_id,
        claim_store,
        claim_release_budget,
        startup_recovery_source,
    } = run_config;
    // Seed the shared handle with the identity this loop starts under —
    // see `live_identity`'s doc comment and the `identity = fresh;`
    // reassignment below, which keeps it current across every
    // re-registration.
    live_identity.set(identity.clone());
    let mut isolation = IsolationTracker::new();
    let mut backoff = ReregistrationBackoff::new(
        self_fence_cfg.reregister_backoff_base,
        self_fence_cfg.reregister_backoff_max,
    );

    let mut startup_recovery_source = startup_recovery_source;
    'registered: loop {
        let startup_source = startup_recovery_source.take();
        if startup_source.is_none() {
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + config.heartbeat_interval,
                config.heartbeat_interval,
            );
            timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut last_success = tokio::time::Instant::now();

            // Runs until this identity self-fences (either trigger). Both
            // trigger paths converge on identical handling below, so this
            // block's exit is unconditional (never a plain `break`).
            //
            // FIX 2: labeled so the per-entity veto-scan loop below (which
            // nests its own `for` loop over `owner_steal_intents`'s result) can
            // `break 'tick` straight out to the self-fenced handling from
            // inside that nested loop, exactly as if the heartbeat/count/
            // reconcile calls above had blown the same deadline.
            'tick: loop {
                tokio::select! {
                    biased;
                    _ = stop_token.cancelled() => {
                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                        return;
                    }
                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                        break;
                    }
                    _ = timer.tick() => {}
                }

                metrics::record_node_heartbeat_age_ms(
                    last_success.elapsed().as_secs_f64() * 1000.0,
                );

                let write_started = tokio::time::Instant::now();
                let renewal = std::pin::pin!(lease.heartbeat(&identity, config.lease_ttl));
                let renewed = tokio::select! {
                    biased;
                    _ = stop_token.cancelled() => {
                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                        return;
                    }
                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                        break;
                    }
                    result = renewal => result,
                };
                metrics::record_node_heartbeat_write_latency_ms(
                    write_started.elapsed().as_secs_f64() * 1000.0,
                );

                match renewed {
                    Ok(true) => {
                        last_success = tokio::time::Instant::now();
                    }
                    Ok(false) => {
                        tracing::error!(
                            node_id = %identity.node_id,
                            "clustering node-lease heartbeat lost (fencing): stopping local claims"
                        );
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            node_id = %identity.node_id,
                            "clustering node-lease heartbeat error; will retry next tick"
                        );
                        continue;
                    }
                }

                // Isolation check + demotion reconciliation, once per
                // successful renewal — "each heartbeat interval, alongside the
                // renewal CAS" (element 4). FIX 2: both control-plane calls
                // below are deadline-armed identically to the heartbeat above
                // — see this function's doc comment.
                let count_future =
                    std::pin::pin!(lease.count_other_live_nodes(&identity, config.lease_ttl));
                let other_live_result = tokio::select! {
                    biased;
                    _ = stop_token.cancelled() => {
                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                        return;
                    }
                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                        break;
                    }
                    result = count_future => result,
                };
                let other_live = match other_live_result {
                    Ok(count) => count,
                    Err(error) => {
                        tracing::warn!(%error, "clustering: failed to count other live nodes this interval");
                        continue;
                    }
                };
                let reachable = usize::try_from(connected_peers.get().max(0)).unwrap_or(0);
                if isolation.observe(other_live, reachable, self_fence_cfg.isolation_intervals) {
                    tracing::error!(
                        node_id = %identity.node_id,
                        other_live,
                        "clustering node self-fencing: swarm-isolated from >= 2 live nodes for the \
                         configured interval count"
                    );
                    break;
                }

                let owned = local_claims.owned().await;
                let reconcile_future = std::pin::pin!(lease.reconcile(&identity, &owned));
                let reconcile_result = tokio::select! {
                    biased;
                    _ = stop_token.cancelled() => {
                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                        return;
                    }
                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                        break;
                    }
                    result = reconcile_future => result,
                };
                match reconcile_result {
                    Ok(lost) => {
                        for entity in &lost {
                            local_claims.demote(entity).await;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "clustering demotion-reconciliation query failed; will retry next interval");
                    }
                }

                // ADR-0017 Phase 3 Slice 3: owner-side steal-intent veto scan,
                // riding the same per-interval cadence and deadline-arm as the
                // other control-plane calls above — "every owner's heartbeat
                // loop reads intents against its own claims" (element 4).
                // Vacuous in production this slice: `local_claims.owned()` is
                // always empty (`NoLocallyClaimedEntities`, per Slice 2's own
                // wiring — no code acquires a `UserActor`/`RoomActor` claim
                // until Slices 5-7), so `owner_steal_intents` always returns an
                // empty set and this block is a no-op every interval. It is
                // exercised directly against `PostgresClaimStore` and a
                // configurable `LocallyClaimedEntities` fake in this module's
                // own tests.
                let intents_future = std::pin::pin!(lease.owner_steal_intents(&identity));
                let intents_result = tokio::select! {
                    biased;
                    _ = stop_token.cancelled() => {
                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                        return;
                    }
                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                        break;
                    }
                    result = intents_future => result,
                };
                match intents_result {
                    Ok(intents) => {
                        // FIX 2: each per-entity await below (`health_check`,
                        // `clear_steal_intent`, `demote`) is deadline/
                        // cancellation-armed exactly like every other
                        // control-plane call above — a slow-but-not-hung
                        // `health_check` across N owned entities must not blow
                        // this node's own heartbeat deadline unobserved, and
                        // shutdown must not block behind a hung ask.
                        for (entity, epoch) in intents {
                            let health_check_future =
                                std::pin::pin!(local_claims.health_check(&entity));
                            let healthy = tokio::select! {
                                biased;
                                _ = stop_token.cancelled() => {
                                    run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                                    return;
                                }
                                _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                                    break 'tick;
                                }
                                result = health_check_future => result,
                            };
                            if healthy {
                                let clear_future = std::pin::pin!(
                                    lease.clear_steal_intent(&entity, &identity, epoch)
                                );
                                let cleared = tokio::select! {
                                    biased;
                                    _ = stop_token.cancelled() => {
                                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                                        return;
                                    }
                                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                                        break 'tick;
                                    }
                                    result = clear_future => result,
                                };
                                match cleared {
                                    Ok(rows) if rows > 0 => {}
                                    Ok(_) => {
                                        // FIX 1(b): `owner_steal_intents` just
                                        // reported this entity as ours with an
                                        // outstanding intent, so zero rows
                                        // affected by the epoch-fenced DELETE
                                        // means the DELETE's own
                                        // claims-ownership check failed — a
                                        // steal already won the race (FIX 1(a)'s
                                        // consume-CTE design) between our health
                                        // check and this clear call. Treat this
                                        // as "possibly deposed" and demote
                                        // immediately rather than believing the
                                        // veto succeeded.
                                        tracing::warn!(
                                            entity_id = %entity.id,
                                            "clustering: clear_steal_intent affected zero rows; \
                                             this node may already have been deposed for this \
                                             entity — demoting locally"
                                        );
                                        let demote_future =
                                            std::pin::pin!(local_claims.demote(&entity));
                                        tokio::select! {
                                            biased;
                                            _ = stop_token.cancelled() => {
                                                run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                                                return;
                                            }
                                            _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                                                break 'tick;
                                            }
                                            _ = demote_future => {}
                                        }
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            %error,
                                            entity_id = %entity.id,
                                            "clustering: failed to clear a vetoed steal intent; the \
                                             reporter's next scan will re-observe it"
                                        );
                                    }
                                }
                            } else {
                                // Proactive wedge-kill (element 4): the health
                                // ask failed, so this owner already knows the
                                // steal will proceed at `intent_ttl` — demote
                                // now rather than keep serving (or squatting on)
                                // a wedged actor until then.
                                tracing::error!(
                                    entity_id = %entity.id,
                                    "clustering: local actor failed its internal health ask during \
                                     steal-intent processing; proactively demoting ahead of the \
                                     pending steal"
                                );
                                let demote_future = std::pin::pin!(local_claims.demote(&entity));
                                tokio::select! {
                                    biased;
                                    _ = stop_token.cancelled() => {
                                        run_shutdown_drain_with_heartbeat(&lease, &claim_store, &identity, &local_claims, claim_release_budget, config.lease_ttl).await;
                                        return;
                                    }
                                    _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                                        break 'tick;
                                    }
                                    _ = demote_future => {}
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "clustering owner steal-intent scan failed; will retry next interval");
                    }
                }
            }
        }

        // Self-fenced (either trigger): refuse new sessions immediately,
        // then stop every local claim/proxy while marking this identity
        // draining. Both operations are independently bounded/best-effort;
        // neither should serialize the other during a partition.
        readiness.set_ready(false);
        let (teardown_complete, ()) = tokio::join!(
            local_claims.demote_all_on_self_fence(),
            mark_draining_bounded(&lease, &identity, config.heartbeat_interval),
        );
        if !teardown_complete {
            tracing::error!(
                node_id = %identity.node_id,
                "clustering node self-fenced with incomplete local teardown; readiness will remain false until a pre-recovery retry succeeds"
            );
        }
        isolation.reset();

        // FIX 1(a): preserve the process-stable node_id/relay address and
        // mint one fresh fencing epoch per fence. Every retry below
        // (including hysteresis-rejected ones) reuses this incarnation.
        let mut recovery = match startup_source {
            Some(source) => RecoveryState::from_registered_candidate(source, identity.clone()),
            None => RecoveryState::new(&identity),
        };

        // Re-registration with hysteresis + exponential backoff.
        loop {
            let mut delay = backoff.next_delay();
            if recovery.candidate_registered {
                // Once a draining candidate exists, keep its exact lease
                // alive at the ordinary heartbeat cadence instead of letting
                // exponential backoff exceed the node TTL.
                delay = delay.min(config.heartbeat_interval);
            }
            tokio::select! {
                biased;
                _ = stop_token.cancelled() => {
                    // FIX 3: `fresh` may already be live in
                    // `clustering_nodes` from an earlier hysteresis-rejected
                    // retry in this same fence (or may not exist yet at
                    // all, in which case this is a harmless no-op) — mark
                    // it draining too before returning on ordinary
                    // shutdown.
                    mark_draining_bounded(
                        &lease,
                        &recovery.candidate,
                        config.heartbeat_interval,
                    )
                    .await;
                    return;
                }
                _ = tokio::time::sleep(delay) => {}
            }

            if recovery.candidate_registered {
                match lease.heartbeat(&recovery.candidate, config.lease_ttl).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(
                            node_epoch = %recovery.candidate.node_epoch,
                            "clustering recovery candidate lease elapsed; rotating epoch"
                        );
                        recovery.rotate_candidate();
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            node_epoch = %recovery.candidate.node_epoch,
                            "clustering recovery candidate heartbeat failed"
                        );
                        continue;
                    }
                }
            }

            // Never supersede the old fencing epoch while a resource from
            // that incarnation may still be alive. A failed terminal sweep
            // means exactly that, so retry teardown before publishing the
            // candidate epoch to Postgres.
            if !local_claims.demote_all_on_self_fence().await {
                tracing::error!(
                    node_id = %identity.node_id,
                    "clustering node terminal teardown remains incomplete; refusing to rotate the node epoch"
                );
                continue;
            }

            match lease
                .register_draining_with_peer_id(
                    &recovery.registration_source,
                    &recovery.candidate,
                    pod_template_hash.clone(),
                    peer_id.clone(),
                    config.lease_ttl,
                )
                .await
            {
                Ok(()) => {
                    recovery.candidate_registered = true;
                    let other_live = lease
                        .count_other_live_nodes(&recovery.candidate, config.lease_ttl)
                        .await
                        .unwrap_or(usize::MAX);
                    let reachable = usize::try_from(connected_peers.get().max(0)).unwrap_or(0);
                    if can_reacquire_claims(other_live, reachable) {
                        // ADR-0017 Phase 3 Slice 5 (plan deviation #19,
                        // closed by FIX 4): the ADR's readiness gate is
                        // "re-registration under a fresh node epoch
                        // **plus claim re-acquisition**." `local_claims` is
                        // no longer vacuous (Slice 5's
                        // `local_claims::SmSessionLocalClaims`), so the
                        // conjunct must do real work here, not just be
                        // documented as a future TODO.
                        //
                        // FIX 4(a): re-run the demote sweep ONE more time,
                        // right before flipping readiness. The self-fenced
                        // block's own sweep (above, at fence-entry) is a
                        // one-shot terminal teardown taken BEFORE this retry
                        // loop even started — a session
                        // that detached (and self-claimed, via
                        // `acquire_claim_store_entry_for_detach`) during
                        // the retry window did so under `live_identity`'s
                        // STALE, pre-fence value (this loop only calls
                        // `live_identity.set(fresh)` a few lines below, once
                        // re-registration itself succeeds), so that
                        // snapshot missed it entirely. Re-running the sweep
                        // here catches anything acquired during that window
                        // before this node ever claims to be ready again.
                        if !local_claims.demote_all_on_self_fence().await {
                            tracing::error!(
                                node_id = %recovery.candidate.node_id,
                                "clustering node re-registered but terminal local teardown is still incomplete; refusing to restore readiness"
                            );
                            // The candidate was atomically registered as
                            // draining and stays that way across this retry;
                            // it has never been advertised as serving.
                            continue;
                        }

                        // What "claim re-acquisition" can mean AT THIS
                        // EXACT POINT: every entity this node owned before
                        // the fence (and anything caught by the re-sweep
                        // just above) is now demoted (forgotten locally,
                        // never released in Postgres — FIX 3's "must
                        // succeed even when Postgres is unreachable"
                        // contract for `demote`), so the authoritative
                        // `local_claims.owned()` set is empty and there is
                        // nothing left in THIS
                        // process's bookkeeping to "re-acquire" by name.
                        // What this node uniquely knows, that no other node
                        // can assert as confidently, is that its OWN
                        // just-superseded identity (`identity`, before the
                        // reassignment below) is genuinely dead — so it
                        // commits that knowledge to Postgres immediately
                        // via `NodeLeaseStore::expire`.
                        let sources = recovery.sources.iter().cloned().collect::<Vec<_>>();
                        for source in sources {
                            expire_bounded(&lease, &source, config.lease_ttl).await;
                        }

                        // FIX 4(b), council-adjudicated: rather than
                        // leaving every one of this node's own dropped
                        // claims to wait out the general orphan reaper's
                        // independent 120s cadence, reclaim them inline,
                        // right here, under the freshly re-registered
                        // identity. Once a claim-steal commits this sequence
                        // is intentionally uncancellable through targeted
                        // hydration; abandoning it midway would strand a
                        // fresh-owned claim outside local state. The
                        // general reaper remains the backstop for every
                        // OTHER node's genuinely dead claims (never
                        // touched by this inline step — see
                        // `reclaim_own_expired_claims`'s owner filter).
                        let recovery_pass = reclaim_with_candidate_heartbeat(
                            &lease,
                            claim_store.as_ref(),
                            &mut recovery,
                            local_claims.as_ref(),
                            config.heartbeat_interval,
                            config.lease_ttl,
                        )
                        .await;
                        if stop_token.is_cancelled() {
                            let _ = local_claims.demote_all_on_self_fence().await;
                            mark_draining_bounded(
                                &lease,
                                &recovery.candidate,
                                config.heartbeat_interval,
                            )
                            .await;
                            return;
                        }
                        match recovery_pass {
                            RecoveryPass::Complete => {}
                            RecoveryPass::Retry => continue,
                            RecoveryPass::RotateCandidate => {
                                mark_draining_bounded(
                                    &lease,
                                    &recovery.candidate,
                                    config.heartbeat_interval,
                                )
                                .await;
                                recovery.rotate_candidate();
                                continue;
                            }
                        }

                        // Recovery steals above are the sole acquisition
                        // exception allowed while this exact candidate is
                        // draining. Only after every moved session has been
                        // hydrated do we activate the row. Thus ordinary
                        // local admissions remain bound to the old (now
                        // ineligible) SharedNodeIdentity, readiness stays
                        // false, and the database never advertises this
                        // candidate as generally acquisition-eligible during
                        // reclaim.
                        let activated = match lease
                            .activate(&recovery.candidate, config.lease_ttl)
                            .await
                        {
                            Ok(activated) => activated,
                            Err(error) => {
                                tracing::warn!(
                                    node_id = %recovery.candidate.node_id,
                                    %error,
                                    "clustering node recovery activation failed after reclaim; rotating candidate"
                                );
                                false
                            }
                        };
                        if stop_token.is_cancelled() {
                            let _ = local_claims.demote_all_on_self_fence().await;
                            mark_draining_bounded(
                                &lease,
                                &recovery.candidate,
                                config.heartbeat_interval,
                            )
                            .await;
                            return;
                        }
                        if !activated {
                            mark_draining_bounded(
                                &lease,
                                &recovery.candidate,
                                config.heartbeat_interval,
                            )
                            .await;
                            recovery.rotate_candidate();
                            continue;
                        }

                        // Reclaim may legitimately consume most of a lease
                        // TTL. Renew and then prove the exact candidate row is
                        // still active (non-draining), non-expired, and fresh
                        // immediately before publishing its identity and
                        // readiness. This also closes the window where a
                        // watchdog expires the candidate while recovery is
                        // hydrating durable SM state.
                        let active_and_fresh = match lease
                            .renew_active(&recovery.candidate, config.lease_ttl)
                            .await
                        {
                            Ok(renewed) => renewed,
                            Err(error) => {
                                tracing::warn!(
                                    node_id = %recovery.candidate.node_id,
                                    %error,
                                    "clustering node recovery lease revalidation failed after reclaim; retrying"
                                );
                                false
                            }
                        };
                        if stop_token.is_cancelled() {
                            let _ = local_claims.demote_all_on_self_fence().await;
                            mark_draining_bounded(
                                &lease,
                                &recovery.candidate,
                                config.heartbeat_interval,
                            )
                            .await;
                            return;
                        }
                        if !active_and_fresh {
                            mark_draining_bounded(
                                &lease,
                                &recovery.candidate,
                                config.heartbeat_interval,
                            )
                            .await;
                            // Claims may already have moved to `fresh` and
                            // been hydrated before the final liveness proof
                            // failed. Reusing the same candidate would make
                            // the next terminal sweep forget them while the
                            // old-owner filter found nothing to reclaim.
                            // Rotate the recovery epoch and make this failed
                            // candidate the next reclaim source instead.
                            recovery.rotate_candidate();
                            continue;
                        }

                        identity = recovery.candidate.clone();
                        live_identity.set(identity.clone());
                        backoff.reset();
                        readiness.set_ready(true);
                        tracing::info!(
                            node_id = %identity.node_id,
                            "clustering node re-registered; readiness restored"
                        );
                        continue 'registered;
                    }
                    tracing::warn!(
                        "clustering node re-registered but the re-acquisition hysteresis gate \
                         is not yet satisfied; retrying"
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "clustering node re-registration failed; retrying with backoff");
                    // A registration reply may be ambiguous. If the exact
                    // candidate occupies the stable row, heartbeat is the
                    // liveness proof; a false heartbeat means it is expired
                    // or stale and must become a lineage source before a new
                    // epoch is minted. If the row still contains an older
                    // known source, retry the same candidate against that
                    // exact predecessor.
                    match lease.registered_identity(&recovery.candidate.node_id).await {
                        Ok(Some(current)) if current == recovery.candidate => {
                            match lease.heartbeat(&recovery.candidate, config.lease_ttl).await {
                                Ok(true) => recovery.candidate_registered = true,
                                Ok(false) => recovery.rotate_candidate(),
                                Err(heartbeat_error) => tracing::warn!(
                                    %heartbeat_error,
                                    "failed to reconcile ambiguous recovery registration"
                                ),
                            }
                        }
                        Ok(Some(current)) if recovery.sources.contains(&current) => {
                            recovery.registration_source = current;
                        }
                        Ok(Some(current)) => tracing::error!(
                            node_epoch = %current.node_epoch,
                            "stable node row contains an unknown recovery epoch; staying fenced"
                        ),
                        Ok(None) => {}
                        Err(read_error) => tracing::warn!(
                            %read_error,
                            "failed to read exact node identity after registration error"
                        ),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0017 Phase 3 Slice 10: a generous fixed budget for
    /// `ClusteringNodeLeaseConfig::claim_release_budget` in tests that don't
    /// otherwise care about drain timing — kept well above every test's own
    /// timer/backoff constants so it never itself becomes the bottleneck.
    const TEST_CLAIM_RELEASE_BUDGET: Duration = Duration::from_secs(5);

    #[test]
    fn lone_survivor_never_isolation_fences() {
        // N=2 carve-out: exactly one other live node, zero reachable peers,
        // for many intervals — must never fence.
        let mut tracker = IsolationTracker::new();
        for _ in 0..50 {
            assert!(!tracker.observe(1, 0, 3));
        }
    }

    #[test]
    fn two_other_live_nodes_fences_after_m_consecutive_isolated_intervals() {
        let mut tracker = IsolationTracker::new();
        assert!(!tracker.observe(2, 0, 3));
        assert!(!tracker.observe(2, 0, 3));
        assert!(tracker.observe(2, 0, 3));
    }

    #[test]
    fn a_single_reachable_peer_resets_the_isolation_counter() {
        let mut tracker = IsolationTracker::new();
        assert!(!tracker.observe(2, 0, 3));
        assert!(!tracker.observe(2, 0, 3));
        // A blip of reachability resets the streak — a single dropped-then
        // -restored link must not accumulate toward fencing.
        assert!(!tracker.observe(2, 1, 3));
        assert_eq!(tracker.consecutive_isolated_intervals(), 0);
        assert!(!tracker.observe(2, 0, 3));
        assert!(!tracker.observe(2, 0, 3));
        assert!(tracker.observe(2, 0, 3));
    }

    #[test]
    fn reachable_peers_never_fence_regardless_of_other_live_count() {
        let mut tracker = IsolationTracker::new();
        for _ in 0..10 {
            assert!(!tracker.observe(5, 1, 3));
        }
    }

    #[test]
    fn reset_clears_the_isolation_streak() {
        let mut tracker = IsolationTracker::new();
        tracker.observe(2, 0, 3);
        tracker.observe(2, 0, 3);
        tracker.reset();
        assert_eq!(tracker.consecutive_isolated_intervals(), 0);
        assert!(!tracker.observe(2, 0, 3));
    }

    #[test]
    fn reacquire_gate_requires_reachability_only_when_other_rows_exist() {
        assert!(
            can_reacquire_claims(0, 0),
            "sole survivor: nothing to wait for"
        );
        assert!(
            !can_reacquire_claims(1, 0),
            "other rows exist: must observe reachability"
        );
        assert!(can_reacquire_claims(1, 1));
        assert!(can_reacquire_claims(3, 1));
    }

    #[test]
    fn backoff_doubles_up_to_the_configured_ceiling() {
        let mut backoff =
            ReregistrationBackoff::new(Duration::from_millis(100), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
        assert_eq!(backoff.next_delay(), Duration::from_millis(200));
        assert_eq!(backoff.next_delay(), Duration::from_millis(400));
        assert_eq!(backoff.next_delay(), Duration::from_millis(800));
        // Would be 1600ms uncapped — clamped to the 1s ceiling.
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        assert_eq!(backoff.attempts(), 6);
    }

    #[test]
    fn backoff_reset_restarts_from_the_base_delay() {
        let mut backoff =
            ReregistrationBackoff::new(Duration::from_millis(50), Duration::from_secs(10));
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(50));
    }

    #[test]
    fn connected_peer_count_reads_back_what_was_set() {
        let count = ConnectedPeerCount::new();
        assert_eq!(count.get(), 0);
        count.set(4);
        assert_eq!(count.get(), 4);
        let cloned = count.clone();
        cloned.set(7);
        assert_eq!(count.get(), 7, "clones share the same underlying counter");
    }

    // --- `run_node_lease` deadline-arm + fencing-loss behavior, driven with
    // paused tokio time and a fake `NodeLeaseStore` — mirrors
    // `swarm.rs`'s own `PartitionedLease`-based heartbeat tests exactly,
    // one level up (node lease instead of keypair-slot lease).

    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};

    struct FakeLease {
        heartbeat_result: std::sync::Mutex<Box<dyn FnMut() -> Result<bool, ClaimError> + Send>>,
        registrations: Arc<AtomicU32>,
        /// Every distinct `node_id` ever passed to `register` — lets tests
        /// assert FIX 1(a)'s invariant directly: the process relay address
        /// stays stable across every re-registration retry.
        registered_node_ids: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
        /// Every distinct epoch passed to `register`; retries within one
        /// fence must reuse the same incarnation rather than superseding one
        /// another before readiness is restored.
        registered_node_epochs: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
        /// Count of `mark_draining` calls — lets tests assert FIX 1(b)/FIX 3
        /// actually fire (bounded best-effort call issued, not skipped).
        draining_calls: Arc<AtomicU32>,
        /// What `count_other_live_nodes` reports, overridable per test so
        /// the re-acquisition hysteresis gate can be made to genuinely
        /// reject several retries before succeeding.
        other_live_nodes: Arc<AtomicU32>,
        /// What `owner_steal_intents` reports each interval (ADR-0017 Phase
        /// 3 Slice 3's veto-scan test double).
        steal_intents: Arc<std::sync::Mutex<Vec<(Entity, ClaimEpoch)>>>,
        /// Every entity `clear_steal_intent` was called with, in order.
        cleared_intents: Arc<std::sync::Mutex<Vec<Entity>>>,
        /// FIX 1(b) test double: when set, `clear_steal_intent` reports
        /// zero rows affected (and does not remove the intent) instead of
        /// succeeding — simulates the "possibly deposed" case where a
        /// concurrent steal already won the race under FIX 1(a)'s
        /// consume-CTE design between this node's health check and its
        /// clear call.
        clear_reports_zero_rows: Arc<AtomicBool>,
        /// Scripted post-reclaim liveness proofs. Empty means success;
        /// tests push `false` to force a recovery-candidate rotation after
        /// claims have already moved and been hydrated.
        renew_active_results: Arc<std::sync::Mutex<std::collections::VecDeque<bool>>>,
        /// Optional dynamic orphan source for recovery-loop tests. The fake
        /// reads the claim store on every scan so a claim moved from one
        /// failed recovery epoch is surfaced under that exact new owner on
        /// the next retry.
        orphan_claim: Option<(Arc<dyn ClaimStore>, Entity)>,
    }

    impl FakeLease {
        fn new(heartbeat_result: Box<dyn FnMut() -> Result<bool, ClaimError> + Send>) -> Self {
            Self {
                heartbeat_result: std::sync::Mutex::new(heartbeat_result),
                registrations: Arc::new(AtomicU32::new(0)),
                registered_node_ids: Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
                registered_node_epochs: Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
                draining_calls: Arc::new(AtomicU32::new(0)),
                other_live_nodes: Arc::new(AtomicU32::new(0)),
                steal_intents: Arc::new(std::sync::Mutex::new(Vec::new())),
                cleared_intents: Arc::new(std::sync::Mutex::new(Vec::new())),
                clear_reports_zero_rows: Arc::new(AtomicBool::new(false)),
                renew_active_results: Arc::new(std::sync::Mutex::new(
                    std::collections::VecDeque::new(),
                )),
                orphan_claim: None,
            }
        }

        fn with_orphan_claim(mut self, store: Arc<dyn ClaimStore>, entity: Entity) -> Self {
            self.orphan_claim = Some((store, entity));
            self
        }
    }

    #[async_trait]
    impl NodeLeaseStore for FakeLease {
        async fn register(
            &self,
            me: &NodeIdentity,
            _pod_template_hash: Option<String>,
        ) -> Result<(), ClaimError> {
            self.registrations.fetch_add(1, Ordering::SeqCst);
            self.registered_node_ids
                .lock()
                .expect("lock")
                .insert(me.node_id.clone());
            self.registered_node_epochs
                .lock()
                .expect("lock")
                .insert(me.node_epoch.clone());
            Ok(())
        }
        async fn heartbeat(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            (self.heartbeat_result.lock().expect("lock"))()
        }
        async fn expire(
            &self,
            _owner: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }
        async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
            self.draining_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn renew_active(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(self
                .renew_active_results
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or(true))
        }
        async fn count_other_live_nodes(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<usize, ClaimError> {
            Ok(self.other_live_nodes.load(Ordering::SeqCst) as usize)
        }
        async fn reconcile(
            &self,
            _me: &NodeIdentity,
            _locally_owned: &[Entity],
        ) -> Result<Vec<Entity>, ClaimError> {
            Ok(Vec::new())
        }
        async fn report_steal_intent(
            &self,
            entity: &Entity,
            _target_owner: &NodeIdentity,
            _target_epoch: ClaimEpoch,
            _reporter: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.steal_intents
                .lock()
                .expect("lock")
                .push((entity.clone(), ClaimEpoch(0)));
            Ok(())
        }
        async fn owner_steal_intents(
            &self,
            _me: &NodeIdentity,
        ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
            Ok(self.steal_intents.lock().expect("lock").clone())
        }
        async fn clear_steal_intent(
            &self,
            entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<u64, ClaimError> {
            if self.clear_reports_zero_rows.load(Ordering::SeqCst) {
                return Ok(0);
            }
            self.cleared_intents
                .lock()
                .expect("lock")
                .push(entity.clone());
            self.steal_intents
                .lock()
                .expect("lock")
                .retain(|(e, _)| e != entity);
            Ok(1)
        }
        async fn list_orphaned_sm_session_claims(
            &self,
        ) -> Result<Vec<crate::clustering::claims::OrphanedSmSessionClaim>, ClaimError> {
            let Some((store, entity)) = &self.orphan_claim else {
                return Ok(Vec::new());
            };
            Ok(store
                .current_claim(entity)
                .await?
                .map(|snapshot| {
                    vec![crate::clustering::claims::OrphanedSmSessionClaim {
                        entity: entity.clone(),
                        epoch: snapshot.claim_epoch,
                        owner: snapshot.owner,
                    }]
                })
                .unwrap_or_default())
        }
        async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
            // Not exercised by this module's tests (the rollout-aware
            // backoff heuristic is tested directly against
            // `PostgresClaimStore` in `claims.rs`, and as a pure function in
            // `clustering::drain`'s own tests).
            Ok(None)
        }
    }

    fn identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    /// Advance the paused clock in small steps, yielding after each, until
    /// `condition` holds or the step budget is exhausted. A single
    /// `tokio::time::advance` call wakes elapsed timers but does not itself
    /// drive a woken task through however many subsequent polls its
    /// synchronous follow-up work needs, so asserting immediately after one
    /// `advance` + one `yield_now` can flake; stepping in a loop is the
    /// robust pattern.
    async fn advance_until(step: Duration, max_steps: u32, mut condition: impl FnMut() -> bool) {
        for _ in 0..max_steps {
            if condition() {
                return;
            }
            tokio::time::advance(step).await;
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn node_lease_self_fences_once_deadline_blown_without_postgres() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(200);
        let lease = FakeLease::new(Box::new(|| {
            Err(ClaimError::Backend(
                "simulated Postgres partition".to_string(),
            ))
        }));
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(NoLocallyClaimedEntities),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(Duration::from_millis(20), 5, || !readiness.is_ready()).await;
        assert!(
            readiness.is_ready(),
            "must not self-fence before the lease deadline elapses"
        );

        advance_until(interval, 20, || !readiness.is_ready()).await;
        assert!(
            !readiness.is_ready(),
            "must self-fence once the node-lease deadline is exceeded"
        );
        stop_token.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn node_lease_self_fences_on_heartbeat_cas_zero_rows() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_secs(10);
        let lease = FakeLease::new(Box::new(|| Ok(false)));
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(NoLocallyClaimedEntities),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !readiness.is_ready()).await;
        assert!(
            !readiness.is_ready(),
            "a zero-rows-affected heartbeat CAS is fencing loss, not a retryable error"
        );
        stop_token.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn node_lease_re_registers_and_restores_readiness_after_a_fence() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let fenced_once = Arc::new(AtomicBool::new(false));
        let fenced_once_writer = Arc::clone(&fenced_once);
        let lease = FakeLease::new(Box::new(move || {
            if fenced_once_writer.load(Ordering::SeqCst) {
                Ok(true)
            } else {
                fenced_once_writer.store(true, Ordering::SeqCst);
                Ok(false)
            }
        }));
        let draining_calls = Arc::clone(&lease.draining_calls);
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(NoLocallyClaimedEntities),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        // First tick fences (heartbeat_result returns Ok(false) once).
        advance_until(interval, 20, || !readiness.is_ready()).await;
        assert!(!readiness.is_ready(), "must fence on the first CAS miss");

        // The re-registration loop retries on its own backoff timer with no
        // other live nodes (`count_other_live_nodes` fakes 0), so the
        // hysteresis gate is trivially satisfied and readiness recovers.
        advance_until(Duration::from_millis(10), 20, || readiness.is_ready()).await;
        assert!(
            readiness.is_ready(),
            "must re-register and restore readiness once the sole-survivor gate is satisfied"
        );
        // FIX 1(b): the just-fenced identity must have been marked draining.
        assert!(
            draining_calls.load(Ordering::SeqCst) >= 1,
            "the fenced identity must be marked draining before re-registration"
        );
        stop_token.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn cold_start_reclaims_the_exact_replaced_predecessor_before_readiness() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let predecessor = identity();
        let current = NodeIdentity::new(
            predecessor.node_id.clone(),
            uuid::Uuid::new_v4().to_string(),
        );
        let entity = Entity::new(EntityType::SmSession, "cold-start-predecessor");
        let concrete_store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        concrete_store
            .acquire(&entity, &predecessor)
            .await
            .expect("seed predecessor claim");
        let claim_store: Arc<dyn ClaimStore> = concrete_store.clone();
        let lease = FakeLease::new(Box::new(|| Ok(true)))
            .with_orphan_claim(Arc::clone(&claim_store), entity.clone());
        let local_state = Arc::new(RecoveryLocalState::default());
        let readiness = ClusteringReadiness::new();
        readiness.set_ready(false);
        let live_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(current.clone());
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            current.clone(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(RecoveryLocalClaims {
                    state: Arc::clone(&local_state),
                }),
                readiness: readiness.clone(),
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::clone(&claim_store),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: Some(predecessor),
            },
        ));

        advance_until(Duration::from_millis(10), 30, || readiness.is_ready()).await;
        assert!(readiness.is_ready());
        assert_eq!(live_identity.current(), current);
        let snapshot = claim_store
            .current_claim(&entity)
            .await
            .expect("claim read")
            .expect("claim present");
        assert_eq!(snapshot.owner, current);
        assert_eq!(
            local_state.owned.lock().expect("lock").get(&entity),
            Some(&current)
        );

        stop_token.cancel();
        task.await.expect("node lease exits");
    }

    #[tokio::test(start_paused = true)]
    async fn post_reclaim_renewal_failure_rotates_and_rehydrates_before_readiness() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let initial = identity();
        let entity = Entity::new(EntityType::SmSession, "recovery-renew-retry");
        let concrete_store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        concrete_store
            .acquire(&entity, &initial)
            .await
            .expect("seed pre-fence claim");
        let claim_store: Arc<dyn ClaimStore> = concrete_store.clone();

        let fenced_once = Arc::new(AtomicBool::new(false));
        let fenced_once_writer = Arc::clone(&fenced_once);
        let lease = FakeLease::new(Box::new(move || {
            if fenced_once_writer.swap(true, Ordering::SeqCst) {
                Ok(true)
            } else {
                Ok(false)
            }
        }))
        .with_orphan_claim(Arc::clone(&claim_store), entity.clone());
        lease
            .renew_active_results
            .lock()
            .expect("lock")
            .push_back(false);
        let registered_epochs = Arc::clone(&lease.registered_node_epochs);

        let local_state = Arc::new(RecoveryLocalState::default());
        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(RecoveryLocalClaims {
            state: Arc::clone(&local_state),
        });
        let readiness = ClusteringReadiness::new();
        let live_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(initial.clone());
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            initial,
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness: readiness.clone(),
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::clone(&claim_store),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !readiness.is_ready()).await;
        assert!(
            !readiness.is_ready(),
            "the initial heartbeat miss self-fences"
        );
        advance_until(Duration::from_millis(10), 50, || readiness.is_ready()).await;
        assert!(
            readiness.is_ready(),
            "a second recovery incarnation should eventually pass the final liveness proof"
        );

        let hydration_owners = local_state.hydration_owners.lock().expect("lock").clone();
        let distinct_hydration_epochs = hydration_owners
            .iter()
            .map(|owner| owner.node_epoch.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            distinct_hydration_epochs.len(),
            2,
            "the claim must be verified under the failed candidate and again under its replacement"
        );
        assert_eq!(
            registered_epochs.lock().expect("lock").len(),
            2,
            "the retry must publish two distinct candidate incarnations"
        );
        let live = live_identity.current();
        let snapshot = claim_store
            .current_claim(&entity)
            .await
            .expect("read final claim")
            .expect("claim remains present");
        assert_eq!(snapshot.owner, live);
        assert_eq!(
            local_state.owned.lock().expect("lock").get(&entity),
            Some(&live),
            "readiness requires the final claim owner to be hydrated locally under the same epoch"
        );

        stop_token.cancel();
        task.await.expect("node lease exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_stays_not_ready_until_a_committed_grant_is_hydrated() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let initial = identity();
        let entity = Entity::new(EntityType::SmSession, "recovery-hydration-retry");
        let concrete_store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        concrete_store
            .acquire(&entity, &initial)
            .await
            .expect("seed pre-fence claim");
        let claim_store: Arc<dyn ClaimStore> = concrete_store.clone();

        let fenced_once = Arc::new(AtomicBool::new(false));
        let fenced_once_writer = Arc::clone(&fenced_once);
        let lease = FakeLease::new(Box::new(move || {
            if fenced_once_writer.swap(true, Ordering::SeqCst) {
                Ok(true)
            } else {
                Ok(false)
            }
        }))
        .with_orphan_claim(Arc::clone(&claim_store), entity.clone());
        let local_state = Arc::new(RecoveryLocalState::default());
        local_state.hydration_failures.store(1, Ordering::SeqCst);
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            initial,
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(RecoveryLocalClaims {
                    state: Arc::clone(&local_state),
                }),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store,
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !readiness.is_ready()).await;
        advance_until(Duration::from_millis(5), 20, || {
            !local_state
                .hydration_owners
                .lock()
                .expect("lock")
                .is_empty()
        })
        .await;
        assert!(
            !readiness.is_ready(),
            "a fresh-owned claim with failed local hydration must keep recovery fenced"
        );

        advance_until(Duration::from_millis(10), 30, || readiness.is_ready()).await;
        assert!(
            readiness.is_ready(),
            "recovery may publish only after the pending exact grant hydrates"
        );
        assert!(local_state
            .owned
            .lock()
            .expect("lock")
            .contains_key(&entity));

        stop_token.cancel();
        task.await.expect("node lease exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn slow_recovery_keeps_the_draining_candidate_heartbeat_fresh() {
        let interval = Duration::from_millis(40);
        let lease_ttl = Duration::from_millis(120);
        let initial = identity();
        let entity = Entity::new(EntityType::SmSession, "slow-recovery-hydration");
        let concrete_store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        concrete_store
            .acquire(&entity, &initial)
            .await
            .expect("seed pre-fence claim");
        let claim_store: Arc<dyn ClaimStore> = concrete_store.clone();
        let heartbeat_calls = Arc::new(AtomicUsize::new(0));
        let heartbeat_calls_writer = Arc::clone(&heartbeat_calls);
        let lease = FakeLease::new(Box::new(move || {
            let call = heartbeat_calls_writer.fetch_add(1, Ordering::SeqCst);
            Ok(call != 0)
        }))
        .with_orphan_claim(Arc::clone(&claim_store), entity.clone());
        let local_state = Arc::new(RecoveryLocalState {
            // One hydration must itself span the lease TTL. Before locally
            // successful grants were consumed, the duplicate second
            // hydration accidentally made two 80ms calls satisfy this test.
            hydration_delay: Duration::from_millis(160),
            ..RecoveryLocalState::default()
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            initial,
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(RecoveryLocalClaims {
                    state: Arc::clone(&local_state),
                }),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store,
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !readiness.is_ready()).await;
        advance_until(Duration::from_millis(20), 40, || readiness.is_ready()).await;
        assert!(readiness.is_ready());
        assert!(
            heartbeat_calls.load(Ordering::SeqCst) >= 3,
            "a hydration pass longer than the TTL must renew the draining candidate"
        );
        assert!(local_state
            .owned
            .lock()
            .expect("lock")
            .contains_key(&entity));

        stop_token.cancel();
        task.await.expect("node lease exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_during_hydration_never_activates_or_restores_readiness() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let initial = identity();
        let entity = Entity::new(EntityType::SmSession, "cancelled-recovery-hydration");
        let concrete_store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        concrete_store
            .acquire(&entity, &initial)
            .await
            .expect("seed pre-fence claim");
        let claim_store: Arc<dyn ClaimStore> = concrete_store.clone();
        let fenced_once = Arc::new(AtomicBool::new(false));
        let fenced_once_writer = Arc::clone(&fenced_once);
        let lease = FakeLease::new(Box::new(move || {
            if fenced_once_writer.swap(true, Ordering::SeqCst) {
                Ok(true)
            } else {
                Ok(false)
            }
        }))
        .with_orphan_claim(Arc::clone(&claim_store), entity.clone());
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Notify::new());
        let local_state = Arc::new(RecoveryLocalState::default());
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            initial,
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(BlockingHydrationClaims {
                    state: Arc::clone(&local_state),
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                }),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store,
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !readiness.is_ready()).await;
        advance_until(Duration::from_millis(5), 30, || {
            started.load(Ordering::SeqCst)
        })
        .await;
        assert!(
            started.load(Ordering::SeqCst),
            "strict hydration must start"
        );
        stop_token.cancel();
        release.notify_one();
        task.await
            .expect("recovery exits after finishing hydration");

        assert!(
            !readiness.is_ready(),
            "shutdown cancellation must not publish the recovered candidate"
        );
        assert!(
            local_state.owned.lock().expect("lock").is_empty(),
            "the post-hydration cancellation path must terminally clear local state"
        );
    }

    // FIX 1(a): the stable-address/row-leak regression test. With `other_live_nodes`
    // fixed at 1 and `connected_peers` at 0, `can_reacquire_claims(1, 0)` is
    // always false, so every re-registration attempt succeeds at
    // `register()` but is then rejected by the hysteresis gate — forcing
    // several retries within a single fence. The process `node_id` must remain
    // the relay actor's stable address while every retry reuses the one epoch
    // minted for this fence.
    #[tokio::test(start_paused = true)]
    async fn re_registration_retries_within_one_fence_reuse_the_same_identity() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let fenced_once = Arc::new(AtomicBool::new(false));
        let fenced_once_writer = Arc::clone(&fenced_once);
        let lease = FakeLease::new(Box::new(move || {
            if fenced_once_writer.swap(true, Ordering::SeqCst) {
                Ok(true)
            } else {
                Ok(false)
            }
        }));
        lease.other_live_nodes.store(1, Ordering::SeqCst);
        let registered_node_ids = Arc::clone(&lease.registered_node_ids);
        let registered_node_epochs = Arc::clone(&lease.registered_node_epochs);
        let registrations = Arc::clone(&lease.registrations);
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 3,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(15),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims: Arc::new(NoLocallyClaimedEntities),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        // Fences on the very first heartbeat, then retries re-registration
        // repeatedly (the hysteresis gate never clears — `other_live_nodes`
        // stays 1, `connected_peers` stays 0) until several attempts have
        // definitely landed.
        advance_until(interval, 5, || !readiness.is_ready()).await;
        assert!(!readiness.is_ready(), "must fence on the first CAS miss");
        advance_until(Duration::from_millis(10), 50, || {
            registrations.load(Ordering::SeqCst) >= 5
        })
        .await;
        assert!(
            registrations.load(Ordering::SeqCst) >= 5,
            "the retry loop must have actually retried several times"
        );
        assert!(
            !readiness.is_ready(),
            "the hysteresis gate never clears in this test, so readiness must stay down"
        );
        assert_eq!(
            registered_node_ids.lock().expect("lock").len(),
            1,
            "every retry must preserve the process-stable node_id used by the relay actor"
        );
        assert_eq!(
            registered_node_epochs.lock().expect("lock").len(),
            1,
            "every retry within one fence must reuse its single freshly minted epoch"
        );
        stop_token.cancel();
    }

    // --- FIX 1(d): Postgres-gated regression test driving several real
    // fence -> re-register cycles through the ACTUAL production
    // identity-lifecycle logic (`PostgresClaimStore`, not `FakeLease`) —
    // proves the row-leak fix holds against the real CAS, not merely the
    // fake double's bookkeeping. Real time, not `start_paused`: mixing
    // paused virtual time with real Postgres network I/O is unreliable, so
    // this uses short real-millisecond intervals instead, mirroring every
    // other Postgres-gated test in this crate.

    async fn count_clustering_nodes_rows(db: &crate::db::Database) -> i64 {
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query("SELECT COUNT(*) FROM clustering_nodes", ())
            .await
            .expect("count query");
        rows.next()
            .await
            .expect("row present")
            .expect("row present")
            .get::<i64>(0)
            .expect("column present")
    }

    async fn force_expire_row(db: &crate::db::Database, node_id: &str) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            "UPDATE clustering_nodes SET expired = true WHERE node_id = ?",
            crate::db_params![node_id.to_string()],
        )
        .await
        .expect("force-expire row");
    }

    async fn seed_detached_sm_session_row(db: &crate::db::Database, stream_id: &str) {
        let conn = db.guard().await.expect("guard");
        conn.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested,
                presence_available, presence_priority
            ) VALUES (?, ?, ?, 0, 0, 0, NULL, 0, 60000, 0, 0, 0, 0, 0)
            ON CONFLICT (stream_id) DO NOTHING
            "#,
            crate::db_params![
                stream_id.to_string(),
                "alice".to_string(),
                "alice@example.com/web".to_string(),
            ],
        )
        .await
        .expect("seed sm_sessions row");
    }

    async fn wait_until(mut condition: impl FnMut() -> bool, step: Duration, deadline: Duration) {
        let start = tokio::time::Instant::now();
        loop {
            if condition() {
                return;
            }
            assert!(
                start.elapsed() < deadline,
                "condition did not become true within {deadline:?}"
            );
            tokio::time::sleep(step).await;
        }
    }

    #[tokio::test]
    async fn repeated_fence_and_reregister_cycles_keep_clustering_nodes_bounded() {
        use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
        use crate::db::{
            Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE,
        };
        use waddle_xmpp::ownership::ClaimStore as _;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "self-fence-lifecycle-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = PostgresClaimStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");
        {
            let conn = db.guard().await.expect("guard");
            conn.execute("DELETE FROM clustering_claims", ())
                .await
                .expect("clean claims");
            conn.execute("DELETE FROM clustering_nodes", ())
                .await
                .expect("clean nodes");
        }

        // A second, permanently-live node row: gives `count_other_live_nodes`
        // a nonzero count so the re-acquisition hysteresis gate genuinely
        // rejects re-registration attempts until `connected_peers` reports
        // reachability — forcing several retries against the same freshly
        // minted epoch within each fence while the process node_id remains
        // stable.
        let other = NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        store
            .register(&other, None)
            .await
            .expect("register other live node");

        let initial_identity = NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        store
            .register(&initial_identity, None)
            .await
            .expect("register initial identity");

        let interval = Duration::from_millis(80);
        let lease_ttl = Duration::from_millis(300);
        let connected_peers = ConnectedPeerCount::new();
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();

        let task_store = PostgresClaimStore::new(db.clone());
        tokio::spawn(run_node_lease(
            task_store,
            initial_identity.clone(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    // Isolation fencing is not what this test drives —
                    // fencing is forced directly via `expired = true`.
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(30),
                    reregister_backoff_max: Duration::from_millis(60),
                },
                connected_peers: connected_peers.clone(),
                local_claims: Arc::new(NoLocallyClaimedEntities),
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        assert!(readiness.is_ready(), "starts ready");
        assert_eq!(
            count_clustering_nodes_rows(&db).await,
            2,
            "other + initial identity"
        );

        // --- Fence cycle 1: force the active identity's row to
        // committed-expired, simulating fencing loss on the next heartbeat.
        force_expire_row(&db, &initial_identity.node_id).await;
        wait_until(
            || !readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;

        // While not-ready, the re-registration loop retries repeatedly
        // (the hysteresis gate never clears: `other` keeps
        // `count_other_live_nodes` at 1, and `connected_peers` is still 0)
        // — sample the row count several times across that retry storm and
        // assert the process row is updated in place rather than leaking a
        // fresh row or relay address.
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let count = count_clustering_nodes_rows(&db).await;
            assert_eq!(
                count, 2,
                "clustering_nodes must remain other + the process-stable node row \
                 across re-registration retries"
            );
        }

        // Satisfy the re-acquisition hysteresis gate and let the loop
        // recover.
        connected_peers.set(1);
        wait_until(
            || readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(
            count_clustering_nodes_rows(&db).await,
            2,
            "other + the process-stable node row after its epoch rotates"
        );

        // --- Fence cycle 2: repeat against the same process node row. Its
        // epoch changed during cycle 1, but its relay-address node_id did not.
        connected_peers.set(0);
        force_expire_row(&db, &initial_identity.node_id).await;
        wait_until(
            || !readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;

        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let count = count_clustering_nodes_rows(&db).await;
            assert_eq!(
                count, 2,
                "the second fence must still update the process-stable node row in place"
            );
        }

        connected_peers.set(1);
        wait_until(
            || readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(
            count_clustering_nodes_rows(&db).await,
            2,
            "repeated fences must retain exactly other + the process-stable node row"
        );

        // FIX 1(c): `count_other_live_nodes` must have recovered to the
        // truthful value from `other`'s perspective — exactly the current
        // process incarnation, not an accumulation of rows from old epochs.
        let truthful_count = store
            .count_other_live_nodes(&other, lease_ttl)
            .await
            .expect("count call succeeds");
        assert_eq!(
            truthful_count, 1,
            "count_other_live_nodes must return to the truthful live count after \
             repeated fence/re-register cycles, not an inflated count from \
             accumulated phantom/draining rows"
        );

        stop_token.cancel();
    }

    // --- Owner-side steal-intent veto scan wiring (ADR-0017 Phase 3 Slice 3) ---

    fn user_actor_entity(id: &str) -> Entity {
        Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            id.to_string(),
        )
    }

    /// Configurable `LocallyClaimedEntities` test double: a fixed `owned()`
    /// set, a toggle-able `health_check` outcome, and a record of every
    /// `demote` call — lets the veto-scan tests below assert
    /// `run_node_lease` calls `health_check`/`demote`/`clear_steal_intent`
    /// exactly as the ADR's owner-veto text describes, without needing a
    /// real `UserActor`/`RoomActor` (which no production `ClaimStore` caller
    /// wires up until Slices 5-7).
    struct FakeLocalClaims {
        owned: Vec<Entity>,
        healthy: Arc<AtomicBool>,
        demoted: Arc<std::sync::Mutex<Vec<Entity>>>,
        /// Every entity ever passed to `hydrate_reclaimed`, in call order —
        /// lets FIX 4(b)'s inline-post-fence-reclaim test assert the
        /// targeted-hydration hook actually fires. Defaults are wired via
        /// `FakeLocalClaims::unhydrated()` for tests that don't care.
        hydrated: Arc<std::sync::Mutex<Vec<Entity>>>,
    }

    impl FakeLocalClaims {
        /// Shared empty `hydrated` sink for tests that don't assert on it.
        fn unhydrated() -> Arc<std::sync::Mutex<Vec<Entity>>> {
            Arc::new(std::sync::Mutex::new(Vec::new()))
        }
    }

    #[async_trait]
    impl LocallyClaimedEntities for FakeLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            self.owned.clone()
        }

        async fn demote(&self, entity: &Entity) {
            self.demoted.lock().expect("lock").push(entity.clone());
        }

        async fn health_check(&self, _entity: &Entity) -> bool {
            self.healthy.load(Ordering::SeqCst)
        }

        async fn hydrate_reclaimed(
            &self,
            _owner: &NodeIdentity,
            entity: &Entity,
            _epoch: ClaimEpoch,
        ) -> ReclaimedEntityHydration {
            self.hydrated.lock().expect("lock").push(entity.clone());
            ReclaimedEntityHydration::Local
        }
    }

    #[tokio::test]
    async fn successful_recovery_hydration_retains_grant_for_a_later_pass() {
        let source = identity();
        let entity = Entity::new(EntityType::SmSession, "pending-hydration-once");
        let mut recovery = RecoveryState::new(&source);
        recovery
            .candidate_grants
            .insert(entity.clone(), ClaimEpoch(41));
        let hydrated = FakeLocalClaims::unhydrated();
        let local_claims = FakeLocalClaims {
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: Arc::clone(&hydrated),
        };

        assert!(hydrate_pending_recovery_grants(&mut recovery, &local_claims).await);
        assert_eq!(
            recovery.candidate_grants.get(&entity),
            Some(&ClaimEpoch(41)),
            "a successful hydration must retain the exact grant as recovery provenance"
        );
        assert!(hydrate_pending_recovery_grants(&mut recovery, &local_claims).await);
        assert_eq!(
            hydrated.lock().expect("lock").as_slice(),
            &[entity.clone(), entity],
            "a later recovery pass must rehydrate after the outer loop's terminal demotion"
        );
    }

    #[tokio::test]
    async fn completed_recovery_pass_hydrates_grant_once_and_consumes_bookkeeping() {
        let source = identity();
        let entity = Entity::new(EntityType::SmSession, "completed-hydration-once");
        let mut recovery = RecoveryState::new(&source);
        recovery
            .candidate_grants
            .insert(entity.clone(), ClaimEpoch(42));
        let hydrated = FakeLocalClaims::unhydrated();
        let local_claims = FakeLocalClaims {
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: Arc::clone(&hydrated),
        };
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        let claim_store = waddle_xmpp::ownership::InProcessClaimStore::new();

        assert_eq!(
            reclaim_own_expired_claims(
                &lease,
                &claim_store,
                &mut recovery,
                &local_claims,
                Duration::from_secs(1),
            )
            .await,
            RecoveryPass::Complete
        );
        assert!(recovery.candidate_grants.is_empty());

        assert_eq!(
            reclaim_own_expired_claims(
                &lease,
                &claim_store,
                &mut recovery,
                &local_claims,
                Duration::from_secs(1),
            )
            .await,
            RecoveryPass::Complete
        );
        assert_eq!(
            hydrated.lock().expect("lock").as_slice(),
            std::slice::from_ref(&entity),
            "a completed pass must consume its grant instead of hydrating it twice"
        );
    }

    #[derive(Default)]
    struct RecoveryLocalState {
        owned: std::sync::Mutex<std::collections::HashMap<Entity, NodeIdentity>>,
        hydration_owners: std::sync::Mutex<Vec<NodeIdentity>>,
        hydration_failures: AtomicUsize,
        hydration_delay: Duration,
    }

    struct RecoveryLocalClaims {
        state: Arc<RecoveryLocalState>,
    }

    #[async_trait]
    impl LocallyClaimedEntities for RecoveryLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            self.state
                .owned
                .lock()
                .expect("lock")
                .keys()
                .cloned()
                .collect()
        }

        async fn demote(&self, entity: &Entity) {
            self.state.owned.lock().expect("lock").remove(entity);
        }

        async fn health_check(&self, entity: &Entity) -> bool {
            self.state.owned.lock().expect("lock").contains_key(entity)
        }

        async fn hydrate_reclaimed(
            &self,
            owner: &NodeIdentity,
            entity: &Entity,
            _epoch: ClaimEpoch,
        ) -> ReclaimedEntityHydration {
            if !self.state.hydration_delay.is_zero() {
                tokio::time::sleep(self.state.hydration_delay).await;
            }
            self.state
                .hydration_owners
                .lock()
                .expect("lock")
                .push(owner.clone());
            if self
                .state
                .hydration_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return ReclaimedEntityHydration::Retry;
            }
            self.state
                .owned
                .lock()
                .expect("lock")
                .insert(entity.clone(), owner.clone());
            ReclaimedEntityHydration::Local
        }
    }

    struct BlockingTerminalClaims {
        started: Arc<AtomicBool>,
        release: Arc<tokio::sync::Notify>,
    }

    struct BlockingHydrationClaims {
        state: Arc<RecoveryLocalState>,
        started: Arc<AtomicBool>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl LocallyClaimedEntities for BlockingHydrationClaims {
        async fn owned(&self) -> Vec<Entity> {
            self.state
                .owned
                .lock()
                .expect("lock")
                .keys()
                .cloned()
                .collect()
        }

        async fn demote(&self, entity: &Entity) {
            self.state.owned.lock().expect("lock").remove(entity);
        }

        async fn health_check(&self, entity: &Entity) -> bool {
            self.state.owned.lock().expect("lock").contains_key(entity)
        }

        async fn hydrate_reclaimed(
            &self,
            owner: &NodeIdentity,
            entity: &Entity,
            _epoch: ClaimEpoch,
        ) -> ReclaimedEntityHydration {
            if !self.started.swap(true, Ordering::SeqCst) {
                self.release.notified().await;
            }
            self.state
                .owned
                .lock()
                .expect("lock")
                .insert(entity.clone(), owner.clone());
            ReclaimedEntityHydration::Local
        }
    }

    #[async_trait]
    impl LocallyClaimedEntities for BlockingTerminalClaims {
        async fn owned(&self) -> Vec<Entity> {
            Vec::new()
        }

        async fn demote_all_on_self_fence(&self) -> bool {
            self.started.store(true, Ordering::SeqCst);
            self.release.notified().await;
            true
        }

        async fn demote(&self, _entity: &Entity) {
            panic!("the terminal override, not per-entity demotion, must be used");
        }

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }
    }

    struct RetriableTerminalClaims {
        calls: Arc<AtomicUsize>,
        allow_completion: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LocallyClaimedEntities for RetriableTerminalClaims {
        async fn owned(&self) -> Vec<Entity> {
            Vec::new()
        }

        async fn demote_all_on_self_fence(&self) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.allow_completion.load(Ordering::SeqCst)
        }

        async fn demote(&self, _entity: &Entity) {}

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }
    }

    #[tokio::test(start_paused = true)]
    async fn run_node_lease_clears_a_healthy_owners_steal_intent() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_secs(10);
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        let entity = user_actor_entity("room-1");
        lease
            .steal_intents
            .lock()
            .expect("lock")
            .push((entity.clone(), ClaimEpoch(0)));
        let cleared_intents = Arc::clone(&lease.cleared_intents);
        let demoted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: vec![entity.clone()],
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::clone(&demoted),
            hydrated: FakeLocalClaims::unhydrated(),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness,
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || {
            !cleared_intents.lock().expect("lock").is_empty()
        })
        .await;
        assert_eq!(
            cleared_intents.lock().expect("lock").as_slice(),
            &[entity],
            "a healthy owner must clear the intent via the epoch-fenced veto DELETE"
        );
        assert!(
            demoted.lock().expect("lock").is_empty(),
            "a healthy owner must not demote the entity it just vetoed for"
        );
        stop_token.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn run_node_lease_demotes_on_a_failed_health_check_instead_of_clearing() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_secs(10);
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        let entity = user_actor_entity("room-2");
        lease
            .steal_intents
            .lock()
            .expect("lock")
            .push((entity.clone(), ClaimEpoch(0)));
        let cleared_intents = Arc::clone(&lease.cleared_intents);
        let demoted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: vec![entity.clone()],
            // The health-ask fails: the owner already knows the steal will
            // proceed, so it demotes proactively (the wedge-kill path)
            // instead of waiting to be stolen from.
            healthy: Arc::new(AtomicBool::new(false)),
            demoted: Arc::clone(&demoted),
            hydrated: FakeLocalClaims::unhydrated(),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness,
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !demoted.lock().expect("lock").is_empty()).await;
        assert_eq!(
            demoted.lock().expect("lock").as_slice(),
            &[entity],
            "a failed health-ask must proactively demote the wedged entity"
        );
        assert!(
            cleared_intents.lock().expect("lock").is_empty(),
            "a wedged owner must never clear the intent it could not answer for"
        );
        stop_token.cancel();
    }

    // FIX 1(b): a healthy owner whose `clear_steal_intent` call reports
    // zero rows affected (a concurrent steal already won the race between
    // this node's health check and its clear call, under FIX 1(a)'s
    // consume-CTE design) must demote the entity immediately — treating
    // the zero-rows outcome as "possibly deposed" — rather than believing
    // the veto succeeded and leaving the entity served locally.
    #[tokio::test(start_paused = true)]
    async fn run_node_lease_demotes_when_clear_steal_intent_reports_zero_rows() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_secs(10);
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        let entity = user_actor_entity("room-3");
        lease
            .steal_intents
            .lock()
            .expect("lock")
            .push((entity.clone(), ClaimEpoch(0)));
        lease.clear_reports_zero_rows.store(true, Ordering::SeqCst);
        let cleared_intents = Arc::clone(&lease.cleared_intents);
        let demoted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: vec![entity.clone()],
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::clone(&demoted),
            hydrated: FakeLocalClaims::unhydrated(),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness,
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || !demoted.lock().expect("lock").is_empty()).await;
        assert_eq!(
            demoted.lock().expect("lock").as_slice(),
            &[entity],
            "zero rows affected by clear_steal_intent must be treated as possibly deposed \
             and demote immediately"
        );
        assert!(
            cleared_intents.lock().expect("lock").is_empty(),
            "the fake never records a zero-rows clear as a successful clear"
        );
        stop_token.cancel();
    }

    // FIX A (ADR-0017 Phase 3 Slice 11 corrigenda, council-adjudicated,
    // deviation 109): the terminal "self-fenced (either trigger): demote
    // everything hosted here" hook just above (reached once the `'tick` loop
    // breaks on either fencing trigger) was
    // previously exercised only with an EMPTY `owned()` set (the real-fence
    // tests above use `NoLocallyClaimedEntities`) or with a `FakeLease` that
    // never actually loses fencing (`Ok(true)` on every heartbeat, in every
    // populated-`FakeLocalClaims` veto-scan test above). Neither combination
    // proves the terminal demote-all loop itself demotes every owned claim,
    // which is exactly what the phase's exit criterion 2 requires. This
    // test combines both: a heartbeat CAS that reports fencing loss on
    // every call, and a populated `FakeLocalClaims` with more than one
    // owned entity.
    #[tokio::test(start_paused = true)]
    async fn node_lease_demotes_every_owned_claim_on_fencing_loss() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_secs(10);
        let lease = FakeLease::new(Box::new(|| Ok(false)));
        let entity_a = user_actor_entity("room-fence-a");
        let entity_b = user_actor_entity("room-fence-b");
        let demoted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: vec![entity_a.clone(), entity_b.clone()],
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::clone(&demoted),
            hydrated: FakeLocalClaims::unhydrated(),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || demoted.lock().expect("lock").len() >= 2).await;
        let mut demoted_entities = demoted.lock().expect("lock").clone();
        demoted_entities.sort_by(|a, b| a.id.cmp(&b.id));
        let mut expected = vec![entity_a, entity_b];
        expected.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(
            demoted_entities, expected,
            "a node that loses fencing (heartbeat CAS returns 0 rows) must demote EVERY \
             locally owned claim in the terminal self-fenced branch, not merely the ones \
             touched by the veto scan"
        );
        assert!(
            !readiness.is_ready(),
            "readiness must flip not-ready before the lease becomes stealable"
        );
        stop_token.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn node_lease_drops_readiness_before_terminal_teardown_with_no_owned_claims() {
        let interval = Duration::from_millis(50);
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Notify::new());
        let local_claims = Arc::new(BlockingTerminalClaims {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            FakeLease::new(Box::new(|| Ok(false))),
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl: Duration::from_secs(10),
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(interval, 20, || started.load(Ordering::SeqCst)).await;
        assert!(
            started.load(Ordering::SeqCst),
            "whole-node fencing must invoke the terminal hook even when owned() is empty"
        );
        assert!(
            !readiness.is_ready(),
            "readiness must be false while terminal teardown is still blocked"
        );

        stop_token.cancel();
        release.notify_waiters();
        task.await
            .expect("node-lease task exits after cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn node_lease_retries_terminal_teardown_before_restoring_readiness() {
        let interval = Duration::from_millis(50);
        let calls = Arc::new(AtomicUsize::new(0));
        let allow_completion = Arc::new(AtomicBool::new(false));
        let local_claims = Arc::new(RetriableTerminalClaims {
            calls: Arc::clone(&calls),
            allow_completion: Arc::clone(&allow_completion),
        });
        let lease = FakeLease::new(Box::new(|| Ok(false)));
        let draining_calls = Arc::clone(&lease.draining_calls);
        let registrations = Arc::clone(&lease.registrations);
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            identity(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl: Duration::from_secs(10),
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(identity()),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        advance_until(Duration::from_millis(10), 20, || {
            calls.load(Ordering::SeqCst) >= 2
        })
        .await;
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "fencing must run an initial teardown and retry it before epoch rotation"
        );
        assert!(
            !readiness.is_ready(),
            "an incomplete teardown retry must keep the candidate identity not-ready"
        );
        assert_eq!(
            registrations.load(Ordering::SeqCst),
            0,
            "an incomplete teardown must prevent the old epoch from being superseded"
        );
        assert!(
            draining_calls.load(Ordering::SeqCst) >= 1,
            "the fenced old identity must remain marked draining"
        );

        allow_completion.store(true, Ordering::SeqCst);
        advance_until(Duration::from_millis(5), 20, || readiness.is_ready()).await;
        assert!(
            readiness.is_ready(),
            "readiness may recover only after a later terminal teardown succeeds"
        );

        stop_token.cancel();
        task.await
            .expect("node-lease task exits after cancellation");
    }

    // --- FIX 4(b) (ADR-0017 Phase 3 Slice 5 corrigenda, council-
    // adjudicated): the inline post-fence reclaim of this node's own
    // just-expired identity's SM-session claims. Postgres-gated: exercises
    // the real `PostgresClaimStore`/`NodeLeaseStore` CAS, not a fake
    // double, because the whole point is proving `steal_stale(OwnerStale)`
    // actually wins against a real `clustering_claims` row.

    #[tokio::test]
    async fn fix4_inline_post_fence_reclaim_hydrates_this_nodes_own_expired_sm_session_claims() {
        use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
        use crate::db::{
            Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE,
        };
        use waddle_xmpp::ownership::EntityType;

        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "self-fence-fix4-reclaim-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = PostgresClaimStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");
        {
            let conn = db.guard().await.expect("guard");
            conn.execute("DELETE FROM clustering_claims", ())
                .await
                .expect("clean claims");
            conn.execute("DELETE FROM clustering_nodes", ())
                .await
                .expect("clean nodes");
        }

        let initial_identity = NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        store
            .register(&initial_identity, None)
            .await
            .expect("register initial identity");

        // Seed an SM-session claim owned by the identity that is about to
        // self-fence — standing in for a session this node detached and
        // self-claimed before the fence (deviation 29's "claim held
        // continuously" invariant).
        let entity = Entity::new(
            waddle_xmpp::ownership::EntityType::SmSession,
            format!("stream-fix4-{}", uuid::Uuid::new_v4()),
        );
        seed_detached_sm_session_row(&db, &entity.id).await;
        store
            .acquire(&entity, &initial_identity)
            .await
            .expect("seed sm_session claim owned by the initial identity");

        let interval = Duration::from_millis(80);
        let lease_ttl = Duration::from_millis(300);
        let connected_peers = ConnectedPeerCount::new();
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let hydrated = FakeLocalClaims::unhydrated();
        let local_claims = Arc::new(FakeLocalClaims {
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: Arc::clone(&hydrated),
        });

        let task_lease = PostgresClaimStore::new(db.clone());
        let task_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
        tokio::spawn(run_node_lease(
            task_lease,
            initial_identity.clone(),
            stop_token.clone(),
            NodeLeaseRunConfig {
                pod_template_hash: None,
                lease_config: ClusteringNodeLeaseConfig {
                    heartbeat_interval: interval,
                    lease_ttl,
                    claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                },
                self_fence_config: ClusteringSelfFenceConfig {
                    // Isolation fencing is not what this test drives — the
                    // fence is forced directly via `expired = true`.
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(30),
                    reregister_backoff_max: Duration::from_millis(60),
                },
                connected_peers: connected_peers.clone(),
                local_claims,
                readiness: readiness.clone(),
                live_identity: waddle_xmpp::ownership::SharedNodeIdentity::new(
                    initial_identity.clone(),
                ),
                peer_id: None,
                claim_store: task_claim_store,
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
                startup_recovery_source: None,
            },
        ));

        assert!(readiness.is_ready(), "starts ready");

        force_expire_row(&db, &initial_identity.node_id).await;
        wait_until(
            || !readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;

        // Sole survivor (no other live node rows seeded): the
        // re-acquisition hysteresis gate is trivially satisfied, so
        // re-registration completes on its own backoff timer.
        wait_until(
            || readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;

        // FIX 4(b): the inline reclaim must have hydrated exactly the
        // entity this node's own just-expired identity held.
        wait_until(
            || !hydrated.lock().expect("lock").is_empty(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(
            hydrated.lock().expect("lock").as_slice(),
            std::slice::from_ref(&entity),
            "the inline post-fence reclaim must hydrate this node's own just-expired \
             identity's sm_session claim, not leave it for the general reaper's slower cadence"
        );

        // The claims row must now show the process's FRESH re-registered epoch
        // as owner — `steal_stale(OwnerStale)` actually won against the real
        // CAS, not just a fake double. The node_id remains stable because it
        // is also the relay actor's address.
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT node_id, node_epoch FROM clustering_claims WHERE entity = ?",
                crate::db_params![format!(
                    "{}:{}",
                    EntityType::SmSession.as_db_str(),
                    entity.id
                )],
            )
            .await
            .expect("query claims row");
        let row = rows
            .next()
            .await
            .expect("row present")
            .expect("row present");
        let owner_node_id: String = row.get(0).expect("column present");
        let owner_node_epoch: String = row.get(1).expect("column present");
        assert_eq!(
            owner_node_id, initial_identity.node_id,
            "the process-stable node_id must remain the relay address"
        );
        assert_ne!(
            owner_node_epoch, initial_identity.node_epoch,
            "the claim must be owned by the fresh re-registered epoch, not the expired one"
        );

        stop_token.cancel();
    }
}
