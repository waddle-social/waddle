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

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimStore, DisabledNodeIdentity, Entity, EntityType, NodeIdentity,
    SharedNodeIdentity,
};

use super::claims::NodeLeaseStore;
use super::metrics;
use super::{ClusteringReadiness, ClusteringShutdownDecision};
use crate::config::{ClusteringNodeLeaseConfig, ClusteringSelfFenceConfig};

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

/// Supplies the local owned-entity set the demotion-reconciliation diff
/// runs against, and demotes entities Postgres no longer attributes to
/// this node.
///
/// **No production implementor lands in Slice 2** — the mechanism and its
/// tests land here, but no code acquires a Postgres-backed `sm_session`
/// claim in production until the fenced `SmPersistenceStorage` (Phase 3
/// Slice 4) starts calling `ClaimStore::acquire` at `<enable/>` time. Until
/// then, [`start_if_enabled`](super::start_if_enabled) wires
/// [`NoLocallyClaimedEntities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimedHydrationHandoff {
    Accepted,
    NotAccepted,
}

#[async_trait]
pub trait LocallyClaimedEntities: Send + Sync {
    /// Every entity this process currently believes it owns, from local
    /// bookkeeping only — never a Postgres read. `async` (ADR-0017 Phase 3
    /// Slice 7 widening) because an actor-backed implementor (`RoomActor`'s
    /// registry) can only be queried through its own mailbox — the
    /// enumeration itself is still pure in-memory bookkeeping, never a
    /// Postgres round-trip.
    async fn owned(&self) -> Vec<Entity>;

    /// Snapshot one entity kind without requiring unrelated registries to
    /// answer.
    ///
    /// The default preserves compatibility for single-kind implementations.
    /// Composite implementations must override this so a wedged RoomRegistry
    /// cannot consume a UserActor drain's inventory budget (and vice versa).
    async fn owned_of_type(&self, entity_type: EntityType) -> Vec<Entity> {
        self.owned()
            .await
            .into_iter()
            .filter(|entity| entity.entity_type == entity_type)
            .collect()
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

    /// Demote work still recorded under an exact superseded node identity
    /// after identity rotation has made new admissions distinguishable. Most
    /// local actor registries do not retain owner identity per entry and use
    /// the ordinary pre-rotation sweep; typed SM claim fences do, so their
    /// implementation closes the final inventory-snapshot race precisely.
    async fn demote_owned_by(&self, _owner: &NodeIdentity) {}

    /// Health-ask the local actor owning `entity` (ADR-0017 Phase 3 Slice
    /// 3's owner-veto path): `true` if it answered promptly (the owner may
    /// then clear the entity's steal intent — FIX 1(e): the veto is
    /// enforced by serializing on the intent rows against a concurrent
    /// steal, deadlock-abort-safe per FIX 1(c), not by any inherent
    /// "unforgeability" of the write itself), `false` if the ask failed or
    /// timed out (wedged — the caller proactively demotes rather than
    /// waiting to be stolen from at `intent_ttl`, per element 4's "an owner
    /// whose internal health ask fails ... kills the wedged actor and
    /// conflict-closes its sockets" text).
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
        entities: &[(
            Entity,
            NodeIdentity,
            ClaimEpoch,
            waddle_xmpp::stream_management::ReclaimedClaimReservation,
        )],
    ) -> ReclaimedHydrationHandoff {
        let _ = entities;
        ReclaimedHydrationHandoff::NotAccepted
    }

    /// Reserve bounded local responsibility before inline self-fence reclaim
    /// performs an ownership-changing CAS.
    fn reserve_reclaimed_claim_capacity(
        &self,
        _entity: &Entity,
    ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
        None
    }

    /// Cancel a reservation after an ownership CAS is known not to have won.
    fn cancel_reclaimed_claim_capacity(
        &self,
        _entity: &Entity,
        _reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) {
    }

    /// Retain responsibility when cancellation makes the CAS outcome
    /// ambiguous. The implementation reconciles with a read-only lookup;
    /// the one-shot ownership mutation is never replayed.
    fn defer_uncertain_reclaimed_claim(
        &self,
        _entity: &Entity,
        _owner: &NodeIdentity,
        _reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) {
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

    /// Permanently stop local `RoomActor` claim acquisition and publication
    /// before the graceful room inventory is taken.
    ///
    /// The shutdown drain must establish this terminal registry gate before
    /// retiring any actor. Otherwise a queued room request can publish a new
    /// actor after the inventory snapshot or after an old actor is killed.
    /// Implementations fail closed unless they can prove the gate is active.
    async fn begin_room_shutdown(&self) -> bool {
        false
    }

    /// Irreversibly select terminal no-release handling for every local room
    /// claim. Implementations must close room admission, stop/inventory
    /// detached registry work, and retain all claims for node-expiry recovery.
    ///
    /// This is used for an explicit shutdown `Abandon` decision and for every
    /// failed precondition after a release fence was issued. It must never
    /// perform a claim release itself.
    async fn abandon_room_shutdown(&self) -> bool {
        false
    }

    /// Hard-retire one sealed room after [`Self::begin_room_shutdown`] has
    /// permanently disabled replacement publication.
    ///
    /// Returning `true` authorizes the caller to release the room claim.
    /// A live actor must never survive that release.
    async fn retire_room_after_shutdown(&self, entity: &Entity) -> bool {
        let _ = entity;
        false
    }

    /// Retire one `UserActor` and every connection resource it owns after
    /// publication authority has been terminally disabled, then verify that
    /// neither local registry can still route the user.
    ///
    /// This is deliberately separate from [`Self::seal_before_release`].
    /// User claims cannot be released while the live identity can still
    /// publish a same-incarnation replacement between the retirement check
    /// and the batched release. Requiring [`DisabledNodeIdentity`] makes the
    /// post-authority-disable ordering explicit and unforgeable outside
    /// [`SharedNodeIdentity::disable`]. Implementations return `false` on
    /// any timeout, teardown failure, or residual actor/resource; shutdown
    /// then leaves the claim held for ordinary fenced reclaim.
    async fn retire_user_after_authority_disabled(
        &self,
        entity: &Entity,
        disabled_identity: &DisabledNodeIdentity,
    ) -> bool {
        let _ = (entity, disabled_identity);
        false
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
    /// [`waddle_xmpp::ownership::SharedNodeIdentity::rotate`] on it every
    /// time it mints a fresh identity (initial value and every post-fence
    /// re-registration), so any other holder of a clone — e.g. the
    /// Postgres-fenced `SmPersistenceStorage`'s claim-acquire calls —
    /// always binds the identity currently in force, never a stale
    /// pre-fence snapshot.
    pub live_identity: waddle_xmpp::ownership::SharedNodeIdentity,
    /// The libp2p PeerId currently held by this process's leased swarm
    /// keypair, bound into every node-lease registration for the current
    /// process. Re-registrations mint a fresh node identity, but they do not
    /// mint a fresh swarm keypair, so the PeerId binding intentionally stays
    /// stable across a self-fence.
    pub peer_id: Option<String>,
    /// ADR-0017 Phase 3 Slice 10: the total graceful claim-drain budget
    /// (`ClusteringNodeLeaseConfig::claim_release_budget`,
    /// `claimReleaseBudget` in the ADR's own text). Its first half caps the
    /// all-or-none `RoomActor` batch; unused room time plus the second half
    /// is available to the separate verified `UserActor` release phase.
    pub claim_release_budget: Duration,
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

/// Best-effort local retirement inside the still-live claim-release
/// deadline. Exact-owner cleanup gets a bounded first attempt, then each
/// entity kind receives a reserved share of the remaining time. A wedged
/// room registry therefore cannot hang shutdown or starve UserActor cleanup.
async fn demote_local_claims_before_deadline(
    local_claims: &dyn LocallyClaimedEntities,
    identities: &[&NodeIdentity],
    deadline: tokio::time::Instant,
) {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let exact_owner_budget = remaining / 2;
    let exact_owner_deadline = tokio::time::Instant::now() + exact_owner_budget;
    let identity_count = identities.len();
    for (index, identity) in identities.iter().enumerate() {
        let remaining = exact_owner_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let identities_left =
            u32::try_from(identity_count - index).expect("identity count fits in u32");
        let identity_budget = remaining / identities_left;
        if !identity_budget.is_zero()
            && tokio::time::timeout(identity_budget, local_claims.demote_owned_by(identity))
                .await
                .is_err()
        {
            tracing::warn!(
                node_id = %identity.node_id,
                "clustering: exact-owner local cleanup exceeded its shutdown budget"
            );
        }
    }

    let entity_types = [
        EntityType::SmSession,
        EntityType::RoomActor,
        EntityType::UserActor,
    ];
    let entity_type_count = entity_types.len();
    for (index, entity_type) in entity_types.into_iter().enumerate() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let types_left = u32::try_from(entity_type_count - index).expect("only three entity types");
        let type_budget = remaining / types_left;
        if type_budget.is_zero() {
            continue;
        }
        let type_deadline = tokio::time::Instant::now() + type_budget;
        let entities = match tokio::time::timeout(
            type_budget,
            local_claims.owned_of_type(entity_type),
        )
        .await
        {
            Ok(entities) => entities,
            Err(_elapsed) => {
                tracing::warn!(
                    ?entity_type,
                    "clustering: local inventory exceeded its reserved shutdown cleanup budget"
                );
                continue;
            }
        };
        for entity in entities {
            let remaining = type_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!(
                    ?entity_type,
                    "clustering: local demotion exhausted its reserved shutdown cleanup budget"
                );
                break;
            }
            if tokio::time::timeout(remaining, local_claims.demote(&entity))
                .await
                .is_err()
            {
                tracing::warn!(
                    entity_type = ?entity.entity_type,
                    entity_id = %entity.id,
                    "clustering: local demotion exceeded its reserved shutdown cleanup budget"
                );
                break;
            }
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
/// independent cadence to notice them. The scan is owner-indexed and capped
/// to one raw page, so other nodes' orphan populations cannot inflate this
/// re-registration critical path. The general reaper remains the backstop
/// for overflow and every other dead owner.
///
/// Caller bounds this whole call against `config.lease_ttl` (mirroring
/// every other control-plane call in [`run_node_lease`]); this function
/// itself does not re-bound the individual `steal_stale`/hydrate calls —
/// a slow-but-not-hung candidate set is expected to be small (this one
/// node's own dropped claims only, not the cluster-wide set), so the
/// outer deadline is sufficient.
async fn reclaim_own_expired_claims<L>(
    lease: &L,
    claim_store: &dyn ClaimStore,
    old_identity: &NodeIdentity,
    fresh: &NodeIdentity,
    local_claims: &dyn LocallyClaimedEntities,
    lease_ttl: Duration,
) where
    L: NodeLeaseStore + Send + Sync,
{
    const INLINE_RECLAIM_LIMIT: usize = 64;
    let candidates = match lease
        .list_orphaned_sm_session_claims_for_owner(old_identity, INLINE_RECLAIM_LIMIT)
        .await
    {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(
                %error,
                node_id = %old_identity.node_id,
                "clustering: inline post-fence owner-scoped SM scan failed"
            );
            return;
        }
    };

    for candidate in candidates {
        let Some(reservation_token) =
            local_claims.reserve_reclaimed_claim_capacity(&candidate.entity)
        else {
            break;
        };
        let mut reservation = InlineReclaimReservation::new(
            local_claims,
            candidate.entity.clone(),
            fresh.clone(),
            reservation_token,
        );
        match lease
            .steal_own_expired_sm_session_claim(
                claim_store,
                &candidate.entity,
                candidate.epoch,
                fresh,
                lease_ttl,
            )
            .await
        {
            Ok(new_epoch) => {
                let handoff = local_claims
                    .hydrate_reclaimed(&[(
                        candidate.entity,
                        fresh.clone(),
                        new_epoch,
                        reservation_token,
                    )])
                    .await;
                // Hydration publishes exact retry responsibility before its
                // first await. Keep the reservation armed until that handoff
                // has returned; cancellation anywhere between the committed
                // steal and publication then falls back to a read-only owner
                // lookup instead of forgetting the fresh-owned claim.
                if handoff == ReclaimedHydrationHandoff::Accepted {
                    reservation.transfer();
                }
            }
            Err(ClaimError::Conflict) => {
                reservation.cancel();
                // The general orphan reaper (or another node) already
                // reclaimed it first — safe, no-op.
            }
            Err(error) => {
                reservation.cancel();
                tracing::warn!(
                    entity_id = %candidate.entity.id,
                    %error,
                    "clustering: inline post-fence reclaim: steal_stale(OwnerStale) failed"
                );
            }
        }
    }
}

/// Cancellation guard for the one-shot inline reclaim CAS. A timeout may
/// drop the future after Postgres committed but before the result arrived;
/// converting the reservation into a read-only lookup keeps that possibly
/// won claim owned without replaying the stale observed epoch.
struct InlineReclaimReservation<'a> {
    local_claims: &'a dyn LocallyClaimedEntities,
    entity: Entity,
    owner: NodeIdentity,
    reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    armed: bool,
}

impl<'a> InlineReclaimReservation<'a> {
    fn new(
        local_claims: &'a dyn LocallyClaimedEntities,
        entity: Entity,
        owner: NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) -> Self {
        Self {
            local_claims,
            entity,
            owner,
            reservation,
            armed: true,
        }
    }

    fn cancel(&mut self) {
        self.local_claims
            .cancel_reclaimed_claim_capacity(&self.entity, self.reservation);
        self.armed = false;
    }

    /// The exact returned epoch is about to be handed to hydration, which
    /// consumes the existing reservation into exact-fence responsibility.
    fn transfer(&mut self) {
        self.armed = false;
    }
}

impl Drop for InlineReclaimReservation<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.local_claims.defer_uncertain_reclaimed_claim(
                &self.entity,
                &self.owner,
                self.reservation,
            );
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
/// and the whole room-batch drain (sealing/retiring all owned `RoomActor`
/// claims before an all-or-none release, bounded by
/// `claim_release_budget`) ran with this node's
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
pub(super) struct ShutdownDrainTiming {
    pub(super) claim_release_budget: Duration,
    pub(super) lease_ttl: Duration,
}

pub(super) async fn run_shutdown_drain_with_heartbeat<L>(
    lease: &L,
    claim_store: &Arc<dyn ClaimStore>,
    identity: &NodeIdentity,
    local_claims: &Arc<dyn LocallyClaimedEntities>,
    timing: ShutdownDrainTiming,
    fence: crate::server::room_serving_quiescence::RoomServingFence,
    fatal_fence: &CancellationToken,
) -> crate::clustering::drain::RoomShutdownDrainOutcome
where
    L: NodeLeaseStore + Send + Sync,
{
    run_with_shutdown_heartbeat(
        lease,
        identity,
        crate::clustering::drain::run_shutdown_drain(
            lease,
            claim_store,
            identity,
            local_claims,
            timing.claim_release_budget,
            fence,
            fatal_fence,
        ),
        timing.lease_ttl,
    )
    .await
}

async fn run_disabled_user_drain_with_heartbeat<L>(
    lease: &L,
    claim_store: &Arc<dyn ClaimStore>,
    disabled_identity: &DisabledNodeIdentity,
    local_claims: &Arc<dyn LocallyClaimedEntities>,
    claim_release_budget: Duration,
    lease_ttl: Duration,
) where
    L: NodeLeaseStore + Send + Sync,
{
    run_with_shutdown_heartbeat(
        lease,
        disabled_identity.prior_identity(),
        crate::clustering::drain::run_disabled_user_shutdown_drain(
            claim_store,
            disabled_identity,
            local_claims,
            claim_release_budget,
        ),
        lease_ttl,
    )
    .await;
}

async fn run_with_shutdown_heartbeat<L, F, T>(
    lease: &L,
    identity: &NodeIdentity,
    drain: F,
    lease_ttl: Duration,
) -> T
where
    L: NodeLeaseStore + Send + Sync,
    F: std::future::Future<Output = T>,
{
    let drain = std::pin::pin!(drain);
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
            output = &mut drain => {
                return output;
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
/// On either trigger: locally demote every entity `local_claims.owned()`
/// currently lists (a purely local action that must succeed even while
/// Postgres is unreachable), flip `readiness` to not-ready, **mark the
/// just-fenced identity's row draining (FIX 1(b), bounded best-effort)**,
/// then loop re-registration attempts (exponential backoff) until one
/// succeeds *and* the hysteresis gate ([`can_reacquire_claims`]) is
/// satisfied — at which point readiness flips back to ready and normal
/// heartbeat/reconciliation resumes under the freshly minted identity.
///
/// **FIX 1(a)**: the fresh `node_id`/`node_epoch` identity for
/// re-registration is minted **once per fence**, before the retry loop —
/// every retry within that fence's re-registration loop reuses the same
/// identity. Minting a fresh random identity on every retry (including
/// hysteresis-rejected ones, which do not indicate a registration
/// failure) was a row-leak wedge: `register`'s `INSERT ... ON CONFLICT
/// (node_id) DO UPDATE` only refreshes an existing row when called
/// repeatedly with the *same* `node_id` — called with a fresh `node_id`
/// every time, it INSERTs a new phantom row per retry instead, and — at the
/// time this fix landed (Slice 2) — nothing in production ever expired node
/// rows (`NodeLeaseStore::expire` had no production caller yet), so
/// `clustering_nodes` would grow without bound across a single sustained
/// fence and permanently inflate every node's `count_other_live_nodes` —
/// eventually making [`can_reacquire_claims`] impossible to satisfy again (a
/// permanent not-ready wedge) and polluting the cluster-wide isolation
/// heuristic. (ADR-0017 Phase 3 Slice 5: `expire` now has its first two
/// production callers — this function's own `expire_bounded` call below,
/// and the orphan reaper — but this FIX 1(a) reasoning is unaffected: both
/// callers only ever expire an identity this loop or the reaper has
/// independently proven dead, never the identity a phantom-row retry would
/// have minted.)
struct TerminalFenceContext<'a, L> {
    lease: &'a L,
    live_identity: &'a SharedNodeIdentity,
    local_claims: &'a Arc<dyn LocallyClaimedEntities>,
    readiness: &'a ClusteringReadiness,
    stop_token: &'a CancellationToken,
    fatal_fence: &'a CancellationToken,
    shutdown_decision:
        &'a tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<ClusteringShutdownDecision>>>,
    control_plane_budget: Duration,
}

impl<L> TerminalFenceContext<'_, L>
where
    L: NodeLeaseStore + Send + Sync,
{
    async fn shutdown(
        &self,
        claim_store: &Arc<dyn ClaimStore>,
        identity: &NodeIdentity,
        claim_release_budget: Duration,
        lease_ttl: Duration,
    ) {
        // The clustering scope can be stopped independently of axum. Stop
        // admitting traffic and NEW claims first, but keep the current
        // identity authoritative while mailbox seal barriers complete the
        // final fenced writes for already-owned rooms.
        self.readiness.set_ready(false);
        let mark_draining = std::pin::pin!(mark_draining_bounded(
            self.lease,
            identity,
            self.control_plane_budget,
        ));
        tokio::select! {
            biased;
            _ = self.fatal_fence.cancelled() => {
                self.finish(identity, identity).await;
                return;
            }
            _ = mark_draining => {}
        }

        let decision = {
            let receiver = self.shutdown_decision.lock().await.take();
            let wait = async {
                let Some(receiver) = receiver else {
                    return ClusteringShutdownDecision::Abandon;
                };
                tokio::select! {
                    biased;
                    _ = self.fatal_fence.cancelled() => ClusteringShutdownDecision::Abandon,
                    decision = receiver => {
                        decision.unwrap_or(ClusteringShutdownDecision::Abandon)
                    }
                }
            };
            run_with_shutdown_heartbeat(self.lease, identity, wait, lease_ttl).await
        };

        if matches!(&decision, ClusteringShutdownDecision::Abandon)
            || self.fatal_fence.is_cancelled()
        {
            let abandon = tokio::time::timeout(
                claim_release_budget,
                self.local_claims.abandon_room_shutdown(),
            );
            let _ = run_with_shutdown_heartbeat(self.lease, identity, abandon, lease_ttl).await;
            if self.fatal_fence.is_cancelled() {
                self.finish(identity, identity).await;
                return;
            }
        }

        let release_deadline = tokio::time::Instant::now() + claim_release_budget;
        // Keep one configured total budget, but cap rooms at its first half.
        // A fast room phase donates its unused time to users below; a wedged
        // room can no longer consume the UserActor retirement opportunity.
        let room_release_budget = claim_release_budget / 2;
        match decision {
            ClusteringShutdownDecision::Release(fence) if !room_release_budget.is_zero() => {
                run_shutdown_drain_with_heartbeat(
                    self.lease,
                    claim_store,
                    identity,
                    self.local_claims,
                    ShutdownDrainTiming {
                        claim_release_budget: room_release_budget,
                        lease_ttl,
                    },
                    fence,
                    self.fatal_fence,
                )
                .await;
            }
            ClusteringShutdownDecision::Release(_fence) => {
                let _ = self.local_claims.abandon_room_shutdown().await;
            }
            ClusteringShutdownDecision::Abandon => {}
        }
        if self.fatal_fence.is_cancelled() {
            self.finish(identity, identity).await;
            return;
        }

        // Only after the room phase has reached a terminal result — either
        // the complete batch was released or every room claim was abandoned
        // for TTL recovery — may the publication barrier reject the
        // remaining identity. UserActor retirement must happen on the far
        // side of that barrier: otherwise an in-flight bind can publish a
        // same-incarnation replacement after the local quiescence check but
        // before release_many.
        let disabled_identity = self.live_identity.disable().await;
        let user_release_budget =
            release_deadline.saturating_duration_since(tokio::time::Instant::now());
        if !user_release_budget.is_zero() {
            let user_drain = std::pin::pin!(run_disabled_user_drain_with_heartbeat(
                self.lease,
                claim_store,
                &disabled_identity,
                self.local_claims,
                user_release_budget,
                lease_ttl,
            ));
            tokio::select! {
                biased;
                _ = self.fatal_fence.cancelled() => {
                    self.finish(identity, identity).await;
                    return;
                }
                _ = user_drain => {}
            }
        }

        // Exact demotion retires anything either bounded phase deliberately
        // abandoned. Failed UserActor retirement remains durably claimed and
        // is reclaimed only after this node lease expires.
        demote_local_claims_before_deadline(
            self.local_claims.as_ref(),
            &[identity],
            release_deadline,
        )
        .await;
    }

    async fn finish(&self, prior_identity: &NodeIdentity, registered_identity: &NodeIdentity) {
        self.readiness.set_ready(false);
        self.stop_token.cancel();
        let _ = tokio::time::timeout(
            self.control_plane_budget,
            self.local_claims.abandon_room_shutdown(),
        )
        .await;

        // A registration call is deliberately allowed to finish instead of
        // being cancellation-dropped: once it returns, rotate through a
        // publication barrier and retire both identities that might own work.
        // Disabled is a typed terminal state: publication guards and claim
        // stores both reject it for the rest of this clustering lifetime.
        self.live_identity.disable().await;
        let cleanup_deadline = tokio::time::Instant::now() + self.control_plane_budget;
        if registered_identity != prior_identity {
            demote_local_claims_before_deadline(
                self.local_claims.as_ref(),
                &[prior_identity, registered_identity],
                cleanup_deadline,
            )
            .await;
        } else {
            demote_local_claims_before_deadline(
                self.local_claims.as_ref(),
                &[prior_identity],
                cleanup_deadline,
            )
            .await;
        }
        mark_draining_bounded(self.lease, prior_identity, self.control_plane_budget).await;
        if registered_identity != prior_identity {
            mark_draining_bounded(self.lease, registered_identity, self.control_plane_budget).await;
        }
    }
}

async fn run_pre_ready_heartbeat<L>(
    lease: Arc<L>,
    identity: NodeIdentity,
    cancel: CancellationToken,
    fatal_fence: CancellationToken,
    heartbeat_interval: Duration,
    lease_ttl: Duration,
) where
    L: NodeLeaseStore + Send + Sync + 'static,
{
    let mut timer = tokio::time::interval(heartbeat_interval);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    timer.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = fatal_fence.cancelled() => return,
            _ = timer.tick() => {
                let renewal = std::pin::pin!(tokio::time::timeout(
                    lease_ttl,
                    lease.heartbeat(&identity, lease_ttl),
                ));
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    _ = fatal_fence.cancelled() => return,
                    result = renewal => result,
                };
                match result {
                    Ok(Ok(true)) => {}
                    Ok(Ok(false)) | Ok(Err(_)) | Err(_) => {
                        fatal_fence.cancel();
                        return;
                    }
                }
            }
        }
    }
}

/// Drive the production node-lease loop without ever authorizing graceful
/// claim release.
///
/// This public fail-closed entry point exists for the external clustering
/// harness, which must exercise heartbeat and veto behavior directly but
/// cannot possess the server's process-wide room-serving fence. On shutdown
/// every local claim is abandoned for ordinary node-expiry recovery.
pub async fn run_node_lease_fail_closed<L>(
    lease: L,
    identity: NodeIdentity,
    stop_token: CancellationToken,
    run_config: NodeLeaseRunConfig,
) where
    L: NodeLeaseStore + Send + Sync + 'static,
{
    let (decision, decision_rx) = tokio::sync::oneshot::channel();
    decision
        .send(ClusteringShutdownDecision::Abandon)
        .expect("node-lease shutdown decision receiver remains live");
    run_node_lease_authorized(lease, identity, stop_token, run_config, decision_rx).await;
}

#[cfg(test)]
fn clean_test_room_serving_fence() -> crate::server::room_serving_quiescence::RoomServingFence {
    use crate::server::room_serving_quiescence::{
        RoomServingQuiescence, RoomServingTerminalOutcome,
    };

    let (_admission, closer) = RoomServingQuiescence::create();
    closer.close();
    match closer.finalize() {
        RoomServingTerminalOutcome::Clean(fence) => fence,
        outcome => panic!("test room-serving fence must be clean: {outcome:?}"),
    }
}

#[cfg(test)]
pub async fn run_node_lease<L>(
    lease: L,
    identity: NodeIdentity,
    stop_token: CancellationToken,
    run_config: NodeLeaseRunConfig,
) where
    L: NodeLeaseStore + Send + Sync + 'static,
{
    let (decision, decision_rx) = tokio::sync::oneshot::channel();
    decision
        .send(ClusteringShutdownDecision::Release(
            clean_test_room_serving_fence(),
        ))
        .expect("test shutdown decision receiver remains live");
    run_node_lease_authorized(lease, identity, stop_token, run_config, decision_rx).await;
}

pub(crate) async fn run_node_lease_authorized<L>(
    lease: L,
    mut identity: NodeIdentity,
    stop_token: CancellationToken,
    run_config: NodeLeaseRunConfig,
    shutdown_decision: tokio::sync::oneshot::Receiver<ClusteringShutdownDecision>,
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
    } = run_config;
    let lease = Arc::new(lease);
    let fatal_fence = readiness.fatal_fence_token();
    let shutdown_decision = tokio::sync::Mutex::new(Some(shutdown_decision));
    let terminal_fence_context = TerminalFenceContext {
        lease: lease.as_ref(),
        live_identity: &live_identity,
        local_claims: &local_claims,
        readiness: &readiness,
        stop_token: &stop_token,
        fatal_fence: &fatal_fence,
        shutdown_decision: &shutdown_decision,
        control_plane_budget: config.heartbeat_interval,
    };
    // Seed the shared handle with the identity this loop starts under —
    // see `live_identity`'s doc comment and the `identity = fresh;`
    // reassignment below, which keeps it current across every
    // re-registration.
    live_identity.rotate(identity.clone()).await;
    let mut isolation = IsolationTracker::new();
    let mut backoff = ReregistrationBackoff::new(
        self_fence_cfg.reregister_backoff_base,
        self_fence_cfg.reregister_backoff_max,
    );

    'registered: loop {
        let mut terminal_fence = false;
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
                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence = true;
                    break;
                }
                _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                    break;
                }
                _ = timer.tick() => {}
            }

            metrics::record_node_heartbeat_age_ms(last_success.elapsed().as_secs_f64() * 1000.0);

            let write_started = tokio::time::Instant::now();
            let renewal = std::pin::pin!(lease.heartbeat(&identity, config.lease_ttl));
            let renewed = tokio::select! {
                biased;
                _ = stop_token.cancelled() => {
                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence = true;
                    break;
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
                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence = true;
                    break;
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

            let owned_future = std::pin::pin!(local_claims.owned());
            let owned = tokio::select! {
                biased;
                _ = stop_token.cancelled() => {
                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence = true;
                    break;
                }
                _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                    break;
                }
                owned = owned_future => owned,
            };
            let reconcile_future = std::pin::pin!(lease.reconcile(&identity, &owned));
            let reconcile_result = tokio::select! {
                biased;
                _ = stop_token.cancelled() => {
                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence = true;
                    break;
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
                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence = true;
                    break;
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
                                terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                                return;
                            }
                            _ = fatal_fence.cancelled() => {
                                terminal_fence = true;
                                break 'tick;
                            }
                            _ = tokio::time::sleep_until(last_success + config.lease_ttl) => {
                                break 'tick;
                            }
                            result = health_check_future => result,
                        };
                        if healthy {
                            let clear_future =
                                std::pin::pin!(lease.clear_steal_intent(&entity, &identity, epoch));
                            let cleared = tokio::select! {
                                biased;
                                _ = stop_token.cancelled() => {
                                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                                    return;
                                }
                                _ = fatal_fence.cancelled() => {
                                    terminal_fence = true;
                                    break 'tick;
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
                                            terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                                            return;
                                        }
                                        _ = fatal_fence.cancelled() => {
                                            terminal_fence = true;
                                            break 'tick;
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
                                    terminal_fence_context.shutdown(&claim_store, &identity, claim_release_budget, config.lease_ttl).await;
                                    return;
                                }
                                _ = fatal_fence.cancelled() => {
                                    terminal_fence = true;
                                    break 'tick;
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

        // Fatal recovery ambiguity is terminal for this clustering lifetime.
        // Disable the shared identity before taking the final ownership
        // snapshot: `rotate` waits for every old-identity publication guard,
        // then rejects any later publication under that owner. Local
        // retirement is bounded and reserves time per entity kind, so a
        // wedged registry cannot hang terminal shutdown or starve UserActor
        // cleanup.
        if terminal_fence {
            readiness.set_ready(false);
            stop_token.cancel();
            let _ = tokio::time::timeout(
                terminal_fence_context.control_plane_budget,
                local_claims.abandon_room_shutdown(),
            )
            .await;
            let superseded_identity = identity.clone();
            live_identity.disable().await;
            let cleanup_deadline =
                tokio::time::Instant::now() + terminal_fence_context.control_plane_budget;
            demote_local_claims_before_deadline(
                local_claims.as_ref(),
                &[&superseded_identity],
                cleanup_deadline,
            )
            .await;
            mark_draining_bounded(
                lease.as_ref(),
                &identity,
                terminal_fence_context.control_plane_budget,
            )
            .await;
            return;
        }

        // Self-fenced (either trigger): stop serving before the lease
        // becomes stealable, then flip client-facing readiness.
        for entity in local_claims.owned().await {
            local_claims.demote(&entity).await;
        }
        readiness.set_ready(false);
        isolation.reset();

        // FIX 1(b): best-effort mark the just-fenced identity's row
        // draining, bounded — narrows how long other nodes keep counting
        // it as live (FIX 1(c) already excludes draining rows regardless
        // of heartbeat freshness).
        mark_draining_bounded(lease.as_ref(), &identity, config.heartbeat_interval).await;

        // FIX 1(a): mint the fresh re-registration identity ONCE per
        // fence — every retry below (including hysteresis-rejected ones)
        // reuses it. See this function's doc comment for the row-leak
        // wedge this closes.
        let fresh = NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );

        // Re-registration with hysteresis + exponential backoff.
        loop {
            tokio::select! {
                biased;
                _ = stop_token.cancelled() => {
                    terminal_fence_context.finish(&identity, &fresh).await;
                    return;
                }
                _ = fatal_fence.cancelled() => {
                    terminal_fence_context.finish(&identity, &fresh).await;
                    return;
                }
                _ = tokio::time::sleep(backoff.next_delay()) => {}
            }
            let registration_lease = Arc::clone(&lease);
            let registration_identity = fresh.clone();
            let registration_fatal = fatal_fence.clone();
            let registration_stop = stop_token.clone();
            let registration_hash = pod_template_hash.clone();
            let registration_peer_id = peer_id.clone();
            let control_plane_budget = config.heartbeat_interval;
            let mut registration = tokio::spawn(async move {
                let result = registration_lease
                    .register_with_peer_id(
                        &registration_identity,
                        registration_hash,
                        registration_peer_id,
                    )
                    .await;
                if registration_fatal.is_cancelled() || registration_stop.is_cancelled() {
                    mark_draining_bounded(
                        registration_lease.as_ref(),
                        &registration_identity,
                        control_plane_budget,
                    )
                    .await;
                }
                result
            });
            let registration_result = tokio::select! {
                biased;
                _ = fatal_fence.cancelled() => {
                    terminal_fence_context.finish(&identity, &fresh).await;
                    return;
                }
                _ = stop_token.cancelled() => {
                    terminal_fence_context.finish(&identity, &fresh).await;
                    return;
                }
                joined = &mut registration => match joined {
                    Ok(result) => result,
                    Err(error) => Err(ClaimError::Backend(format!(
                        "node re-registration task failed: {error}"
                    ))),
                }
            };
            match registration_result {
                Ok(()) => {
                    if fatal_fence.is_cancelled() {
                        terminal_fence_context.finish(&identity, &fresh).await;
                        return;
                    }
                    let count_identity = fresh.clone();
                    let count_future = std::pin::pin!(
                        lease.count_other_live_nodes(&count_identity, config.lease_ttl)
                    );
                    let other_live = tokio::select! {
                        biased;
                        _ = fatal_fence.cancelled() => {
                            terminal_fence_context.finish(&identity, &fresh).await;
                            return;
                        }
                        _ = stop_token.cancelled() => {
                            terminal_fence_context.finish(&identity, &fresh).await;
                            return;
                        }
                        result = count_future => result.unwrap_or(usize::MAX),
                    };
                    if fatal_fence.is_cancelled() {
                        terminal_fence_context.finish(&identity, &fresh).await;
                        return;
                    }
                    let reachable = usize::try_from(connected_peers.get().max(0)).unwrap_or(0);
                    if can_reacquire_claims(other_live, reachable) {
                        // ADR-0017 Phase 3 Slice 5 (plan deviation #19,
                        // closed by FIX 4): the ADR's readiness gate is
                        // "re-registration under a fresh node_id/node_epoch
                        // **plus claim re-acquisition**." `local_claims` is
                        // no longer vacuous (Slice 5's
                        // `local_claims::SmSessionLocalClaims`), so the
                        // conjunct must do real work here, not just be
                        // documented as a future TODO.
                        // FIX 4(a): re-run the demote sweep ONE more time,
                        // right before flipping readiness. The self-fenced
                        // block's own sweep (above, at fence-entry) is a
                        // one-shot snapshot of `local_claims.owned()` taken
                        // BEFORE this retry loop even started — a session
                        // that detached (and self-claimed, via
                        // `acquire_claim_store_entry_for_detach`) during
                        // the retry window did so under `live_identity`'s
                        // STALE, pre-fence value, so that
                        // snapshot missed it entirely. Re-running the sweep
                        // here catches anything acquired during that window
                        // before this node ever claims to be ready again.
                        let owned_future = std::pin::pin!(local_claims.owned());
                        let owned = tokio::select! {
                            biased;
                            _ = fatal_fence.cancelled() => {
                                terminal_fence_context.finish(&identity, &fresh).await;
                                return;
                            }
                            _ = stop_token.cancelled() => {
                                terminal_fence_context.finish(&identity, &fresh).await;
                                return;
                            }
                            owned = owned_future => owned,
                        };
                        for entity in owned {
                            local_claims.demote(&entity).await;
                        }

                        let superseded_identity = identity.clone();
                        identity = fresh;
                        live_identity.rotate(identity.clone()).await;
                        let recovery_heartbeat_cancel = CancellationToken::new();
                        let mut recovery_heartbeat = tokio::spawn(run_pre_ready_heartbeat(
                            Arc::clone(&lease),
                            identity.clone(),
                            recovery_heartbeat_cancel.clone(),
                            fatal_fence.clone(),
                            config.heartbeat_interval,
                            config.lease_ttl,
                        ));
                        let exact_demote =
                            std::pin::pin!(local_claims.demote_owned_by(&superseded_identity));
                        tokio::select! {
                            biased;
                            _ = fatal_fence.cancelled() => {
                                recovery_heartbeat_cancel.cancel();
                                recovery_heartbeat.abort();
                                let _ = (&mut recovery_heartbeat).await;
                                terminal_fence_context.finish(&superseded_identity, &identity).await;
                                return;
                            }
                            _ = stop_token.cancelled() => {
                                recovery_heartbeat_cancel.cancel();
                                recovery_heartbeat.abort();
                                let _ = (&mut recovery_heartbeat).await;
                                terminal_fence_context.finish(&superseded_identity, &identity).await;
                                return;
                            }
                            _ = exact_demote => {}
                        }

                        // What "claim re-acquisition" can mean AT THIS
                        // EXACT POINT: every entity this node owned before
                        // the fence (and anything caught by the re-sweep
                        // just above) is now demoted (forgotten locally,
                        // never released in Postgres — FIX 3's "must
                        // succeed even when Postgres is unreachable"
                        // contract for `demote`), so `local_claims.owned()`
                        // is empty and there is nothing left in THIS
                        // process's bookkeeping to "re-acquire" by name.
                        // What this node uniquely knows, that no other node
                        // can assert as confidently, is that its OWN
                        // just-superseded identity is genuinely dead — so it
                        // commits that knowledge to Postgres immediately
                        // via `NodeLeaseStore::expire`.
                        let expire_future = std::pin::pin!(expire_bounded(
                            lease.as_ref(),
                            &superseded_identity,
                            config.lease_ttl,
                        ));
                        tokio::select! {
                            biased;
                            _ = fatal_fence.cancelled() => {
                                recovery_heartbeat_cancel.cancel();
                                recovery_heartbeat.abort();
                                let _ = (&mut recovery_heartbeat).await;
                                terminal_fence_context.finish(&superseded_identity, &identity).await;
                                return;
                            }
                            _ = stop_token.cancelled() => {
                                recovery_heartbeat_cancel.cancel();
                                recovery_heartbeat.abort();
                                let _ = (&mut recovery_heartbeat).await;
                                terminal_fence_context.finish(&superseded_identity, &identity).await;
                                return;
                            }
                            _ = expire_future => {}
                        }

                        // FIX 4(b), council-adjudicated: rather than
                        // leaving every one of this node's own dropped
                        // claims to wait out the general orphan reaper's
                        // independent 120s cadence, reclaim them inline,
                        // right here, under the freshly re-registered
                        // identity — bounded/deadline-armed like every
                        // sibling control-plane call in this function. The
                        // general reaper remains the backstop for every
                        // OTHER node's genuinely dead claims (never
                        // touched by this inline step — see
                        // `reclaim_own_expired_claims`'s owner filter).
                        let reclaim_timed_out = {
                            let reclaim_future = std::pin::pin!(reclaim_own_expired_claims(
                                lease.as_ref(),
                                claim_store.as_ref(),
                                &superseded_identity,
                                &identity,
                                local_claims.as_ref(),
                                config.lease_ttl,
                            ));
                            let reclaim = std::pin::pin!(tokio::time::timeout(
                                config.lease_ttl,
                                reclaim_future
                            ));
                            tokio::select! {
                                biased;
                                _ = fatal_fence.cancelled() => {
                                    recovery_heartbeat_cancel.cancel();
                                    recovery_heartbeat.abort();
                                    let _ = (&mut recovery_heartbeat).await;
                                    terminal_fence_context.finish(&superseded_identity, &identity).await;
                                    return;
                                }
                                _ = stop_token.cancelled() => {
                                    recovery_heartbeat_cancel.cancel();
                                    recovery_heartbeat.abort();
                                    let _ = (&mut recovery_heartbeat).await;
                                    terminal_fence_context.finish(&superseded_identity, &identity).await;
                                    return;
                                }
                                result = reclaim => result.is_err(),
                            }
                        };
                        if reclaim_timed_out {
                            tracing::warn!(
                                node_id = %identity.node_id,
                                "clustering: inline post-fence reclaim of this node's own \
                                 just-expired identity's SM-session claims timed out; the \
                                 general orphan reaper remains the backstop for these entities"
                            );
                        }

                        recovery_heartbeat_cancel.cancel();
                        let _ = (&mut recovery_heartbeat).await;

                        if fatal_fence.is_cancelled() {
                            terminal_fence_context
                                .finish(&superseded_identity, &identity)
                                .await;
                            return;
                        }

                        // The background heartbeat keeps the fresh row live
                        // during recovery; prove it once more immediately
                        // before readiness so no stale fresh identity can be
                        // published as serving.
                        let final_heartbeat = std::pin::pin!(tokio::time::timeout(
                            config.lease_ttl,
                            lease.heartbeat(&identity, config.lease_ttl),
                        ));
                        let heartbeat_is_fresh = tokio::select! {
                            biased;
                            _ = fatal_fence.cancelled() => false,
                            _ = stop_token.cancelled() => false,
                            result = final_heartbeat => matches!(result, Ok(Ok(true))),
                        };
                        if !heartbeat_is_fresh {
                            fatal_fence.cancel();
                            terminal_fence_context
                                .finish(&superseded_identity, &identity)
                                .await;
                            return;
                        }

                        backoff.reset();
                        if stop_token.is_cancelled() {
                            terminal_fence_context
                                .shutdown(
                                    &claim_store,
                                    &identity,
                                    claim_release_budget,
                                    config.lease_ttl,
                                )
                                .await;
                            return;
                        }
                        readiness.set_ready(true);
                        if fatal_fence.is_cancelled() {
                            readiness.set_ready(false);
                            terminal_fence_context
                                .finish(&superseded_identity, &identity)
                                .await;
                            return;
                        }
                        if stop_token.is_cancelled() {
                            terminal_fence_context
                                .shutdown(
                                    &claim_store,
                                    &identity,
                                    claim_release_budget,
                                    config.lease_ttl,
                                )
                                .await;
                            return;
                        }
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

    use std::sync::atomic::{AtomicBool, AtomicU32};

    struct FakeLease {
        heartbeat_result: std::sync::Mutex<Box<dyn FnMut() -> Result<bool, ClaimError> + Send>>,
        registrations: Arc<AtomicU32>,
        /// Every distinct `node_id` ever passed to `register` — lets tests
        /// assert FIX 1(a)'s invariant directly: a single fence's
        /// re-registration retries must all reuse the same identity, never
        /// mint a fresh one per retry.
        registered_node_ids: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
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
        orphaned_sm_claims:
            Arc<std::sync::Mutex<Vec<crate::clustering::claims::OrphanedSmSessionClaim>>>,
        registration_entered: Option<Arc<tokio::sync::Notify>>,
        registration_release: Option<Arc<tokio::sync::Notify>>,
    }

    impl FakeLease {
        fn new(heartbeat_result: Box<dyn FnMut() -> Result<bool, ClaimError> + Send>) -> Self {
            Self {
                heartbeat_result: std::sync::Mutex::new(heartbeat_result),
                registrations: Arc::new(AtomicU32::new(0)),
                registered_node_ids: Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
                draining_calls: Arc::new(AtomicU32::new(0)),
                other_live_nodes: Arc::new(AtomicU32::new(0)),
                steal_intents: Arc::new(std::sync::Mutex::new(Vec::new())),
                cleared_intents: Arc::new(std::sync::Mutex::new(Vec::new())),
                clear_reports_zero_rows: Arc::new(AtomicBool::new(false)),
                orphaned_sm_claims: Arc::new(std::sync::Mutex::new(Vec::new())),
                registration_entered: None,
                registration_release: None,
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_heartbeat_continues_while_terminal_decision_is_pending() {
        let heartbeat_calls = Arc::new(AtomicU32::new(0));
        let recorded_calls = Arc::clone(&heartbeat_calls);
        let lease = FakeLease::new(Box::new(move || {
            recorded_calls.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }));
        let me = identity();
        let lease_ttl = Duration::from_millis(100);

        let decision = run_with_shutdown_heartbeat(
            &lease,
            &me,
            async {
                tokio::time::sleep(Duration::from_millis(260)).await;
                ClusteringShutdownDecision::Abandon
            },
            lease_ttl,
        )
        .await;

        assert!(matches!(decision, ClusteringShutdownDecision::Abandon));
        assert!(
            heartbeat_calls.load(Ordering::SeqCst) >= 4,
            "the node lease must remain fresh for the entire authorization wait"
        );
    }

    #[async_trait]
    impl NodeLeaseStore for FakeLease {
        async fn list_orphaned_room_actor_claims_page(
            &self,
            _after: Option<crate::clustering::claims::RoomOrphanScanCursor>,
            _limit: usize,
        ) -> Result<crate::clustering::claims::OrphanedRoomActorClaimPage, ClaimError> {
            Ok(crate::clustering::claims::OrphanedRoomActorClaimPage {
                candidates: Vec::new(),
                next_cursor: None,
                has_more: false,
                quarantined: 0,
            })
        }

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
            if let Some(entered) = &self.registration_entered {
                entered.notify_one();
            }
            if let Some(release) = &self.registration_release {
                release.notified().await;
            }
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
            Ok(self.orphaned_sm_claims.lock().expect("lock").clone())
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

    struct ReservedUserDrainClaims {
        room: Entity,
        user: Entity,
        user_retired: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LocallyClaimedEntities for ReservedUserDrainClaims {
        async fn owned(&self) -> Vec<Entity> {
            std::future::pending().await
        }

        async fn owned_of_type(&self, entity_type: EntityType) -> Vec<Entity> {
            match entity_type {
                EntityType::RoomActor => vec![self.room.clone()],
                EntityType::UserActor => vec![self.user.clone()],
                EntityType::SmSession => Vec::new(),
            }
        }

        async fn demote(&self, _entity: &Entity) {}

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }

        async fn seal_before_release(&self, entity: &Entity) -> bool {
            assert_eq!(entity, &self.room);
            std::future::pending().await
        }

        async fn begin_room_shutdown(&self) -> bool {
            true
        }

        async fn retire_room_after_shutdown(&self, entity: &Entity) -> bool {
            assert_eq!(entity, &self.room);
            true
        }

        async fn retire_user_after_authority_disabled(
            &self,
            entity: &Entity,
            _disabled_identity: &DisabledNodeIdentity,
        ) -> bool {
            assert_eq!(entity, &self.user);
            self.user_retired.store(true, Ordering::SeqCst);
            true
        }
    }

    #[tokio::test(start_paused = true)]
    async fn wedged_room_drain_preserves_half_the_budget_for_user_claim_release() {
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        let me = identity();
        let room = Entity::new(EntityType::RoomActor, "wedged@muc.example.com");
        let user = Entity::new(EntityType::UserActor, "alice@example.com");
        let claim_store: Arc<dyn ClaimStore> =
            Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        let room_epoch = claim_store.acquire(&room, &me).await.expect("acquire room");
        let user_epoch = claim_store.acquire(&user, &me).await.expect("acquire user");
        let user_retired = Arc::new(AtomicBool::new(false));
        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(ReservedUserDrainClaims {
            room: room.clone(),
            user: user.clone(),
            user_retired: Arc::clone(&user_retired),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let fatal_fence = CancellationToken::new();
        let live_identity = SharedNodeIdentity::new(me.clone());
        let (decision, decision_rx) = tokio::sync::oneshot::channel();
        decision
            .send(ClusteringShutdownDecision::Release(
                clean_test_room_serving_fence(),
            ))
            .expect("terminal decision receiver remains live");
        let shutdown_decision = tokio::sync::Mutex::new(Some(decision_rx));
        let context = TerminalFenceContext {
            lease: &lease,
            live_identity: &live_identity,
            local_claims: &local_claims,
            readiness: &readiness,
            stop_token: &stop_token,
            fatal_fence: &fatal_fence,
            shutdown_decision: &shutdown_decision,
            control_plane_budget: Duration::from_millis(10),
        };
        let total_budget = Duration::from_millis(100);

        tokio::time::timeout(
            total_budget * 2,
            context.shutdown(&claim_store, &me, total_budget, Duration::from_secs(1)),
        )
        .await
        .expect("shutdown must not fall back to the wedged aggregate inventory");

        assert!(
            claim_store
                .fence(&room, &me, room_epoch)
                .await
                .unwrap_or(false),
            "the wedged room must remain claimed"
        );
        assert!(user_retired.load(Ordering::SeqCst));
        assert!(
            !claim_store
                .fence(&user, &me, user_epoch)
                .await
                .unwrap_or(true),
            "the reserved second half must retire and release the UserActor claim"
        );
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
        let live_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(identity());
        let task = tokio::spawn(run_node_lease(
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
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
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
        task.await.expect("node lease exits after shutdown drain");
        assert!(
            !readiness.is_ready(),
            "clustering shutdown must revoke readiness even after re-registration"
        );
        assert!(
            !live_identity.current().is_active(),
            "clustering shutdown must revoke publication authority"
        );
    }

    // FIX 1(a): the row-leak wedge regression test. With `other_live_nodes`
    // fixed at 1 and `connected_peers` at 0, `can_reacquire_claims(1, 0)` is
    // always false, so every re-registration attempt succeeds at
    // `register()` but is then rejected by the hysteresis gate — forcing
    // several retries within a single fence. Before the fix, each retry
    // minted a brand-new random identity (a phantom row per retry); after
    // the fix, every retry within the same fence reuses one identity.
    #[tokio::test(start_paused = true)]
    async fn re_registration_retries_within_one_fence_reuse_the_same_identity() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_millis(150);
        let lease = FakeLease::new(Box::new(|| Ok(false)));
        lease.other_live_nodes.store(1, Ordering::SeqCst);
        let registered_node_ids = Arc::clone(&lease.registered_node_ids);
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
            "every retry within a single fence must reuse the same freshly-minted \
             identity, never mint a new one per retry (the row-leak wedge)"
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
        // reachability — forcing several retries against the SAME
        // freshly-minted identity within each fence, exactly the shape the
        // row-leak bug manifested in.
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
        // assert it never exceeds "other + fenced-old + fresh-new", proving
        // retries do not leak rows.
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let count = count_clustering_nodes_rows(&db).await;
            assert!(
                count <= 3,
                "clustering_nodes must stay bounded across re-registration retries \
                 within one fence, got {count} rows"
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
            3,
            "other + fenced-old (draining) + the one fresh identity from this fence"
        );

        // Identify cycle 1's fresh identity for the second fence below.
        let cycle1_node_id = {
            let conn = db.guard().await.expect("guard");
            let mut rows = conn
                .query(
                    "SELECT node_id FROM clustering_nodes WHERE node_id != ? AND node_id != ?",
                    crate::db_params![other.node_id.clone(), initial_identity.node_id.clone()],
                )
                .await
                .expect("query cycle1 identity");
            rows.next()
                .await
                .expect("row present")
                .expect("exactly one row: cycle1's fresh identity")
                .get::<String>(0)
                .expect("column present")
        };

        // --- Fence cycle 2: repeat, forcing cycle 1's identity to expire.
        connected_peers.set(0);
        force_expire_row(&db, &cycle1_node_id).await;
        wait_until(
            || !readiness.is_ready(),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;

        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let count = count_clustering_nodes_rows(&db).await;
            assert!(
                count <= 4,
                "clustering_nodes must stay bounded across the SECOND fence's \
                 re-registration retries too, got {count} rows"
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
            4,
            "other + 2 fenced-and-draining old identities + the current fresh identity"
        );

        // FIX 1(c): `count_other_live_nodes` must have recovered to the
        // truthful value from `other`'s perspective — exactly the current
        // (cycle 2) identity, not an accumulation of every identity ever
        // minted across both fences.
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
        exact_demoted_owners: Arc<std::sync::Mutex<Vec<NodeIdentity>>>,
        /// Every entity ever passed to `hydrate_reclaimed`, in call order —
        /// lets FIX 4(b)'s inline-post-fence-reclaim test assert the
        /// targeted-hydration hook actually fires. Defaults are wired via
        /// `FakeLocalClaims::unhydrated()` for tests that don't care.
        hydrated: Arc<std::sync::Mutex<Vec<Entity>>>,
        live_identity_at_hydration: Option<waddle_xmpp::ownership::SharedNodeIdentity>,
        stale_hydration_observed: Arc<AtomicBool>,
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

        async fn demote_owned_by(&self, owner: &NodeIdentity) {
            self.exact_demoted_owners
                .lock()
                .expect("lock")
                .push(owner.clone());
        }

        async fn health_check(&self, _entity: &Entity) -> bool {
            self.healthy.load(Ordering::SeqCst)
        }

        async fn begin_room_shutdown(&self) -> bool {
            true
        }

        async fn retire_room_after_shutdown(&self, _entity: &Entity) -> bool {
            true
        }

        async fn hydrate_reclaimed(
            &self,
            entities: &[(
                Entity,
                NodeIdentity,
                ClaimEpoch,
                waddle_xmpp::stream_management::ReclaimedClaimReservation,
            )],
        ) -> ReclaimedHydrationHandoff {
            if let Some(live_identity) = &self.live_identity_at_hydration {
                let current = live_identity.current();
                if entities.iter().any(|(_, owner, _, _)| *owner != current) {
                    self.stale_hydration_observed.store(true, Ordering::SeqCst);
                }
            }
            self.hydrated.lock().expect("lock").extend(
                entities
                    .iter()
                    .map(|(entity, _owner, _epoch, _reservation)| entity.clone()),
            );
            ReclaimedHydrationHandoff::Accepted
        }

        fn reserve_reclaimed_claim_capacity(
            &self,
            _entity: &Entity,
        ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
            Some(waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(1))
        }
    }

    struct CapacityLocalClaims {
        remaining: AtomicU32,
        admission_attempts: AtomicU32,
        hydrated: std::sync::Mutex<Vec<Entity>>,
    }

    struct CancelledHydrationLocalClaims {
        hydration_started: tokio::sync::Notify,
        deferred: std::sync::Mutex<Vec<Entity>>,
    }

    struct RejectingHydrationLocalClaims {
        deferred: std::sync::Mutex<Vec<Entity>>,
    }

    struct BlockingSealLocalClaims {
        entity: Entity,
        seal_started: Arc<tokio::sync::Notify>,
        seal_release: Arc<tokio::sync::Notify>,
        exact_demoted_owners: Arc<std::sync::Mutex<Vec<NodeIdentity>>>,
    }

    struct HangingFatalCleanupClaims {
        user: Entity,
        user_demoted: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LocallyClaimedEntities for HangingFatalCleanupClaims {
        async fn owned(&self) -> Vec<Entity> {
            std::future::pending().await
        }

        async fn owned_of_type(&self, entity_type: EntityType) -> Vec<Entity> {
            match entity_type {
                EntityType::UserActor => vec![self.user.clone()],
                EntityType::SmSession | EntityType::RoomActor => Vec::new(),
            }
        }

        async fn demote(&self, entity: &Entity) {
            if entity == &self.user {
                self.user_demoted.store(true, Ordering::SeqCst);
            }
        }

        async fn demote_owned_by(&self, _owner: &NodeIdentity) {
            std::future::pending().await
        }

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }
    }

    #[async_trait]
    impl LocallyClaimedEntities for BlockingSealLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            vec![self.entity.clone()]
        }

        async fn demote(&self, _entity: &Entity) {}

        async fn demote_owned_by(&self, owner: &NodeIdentity) {
            self.exact_demoted_owners
                .lock()
                .expect("lock")
                .push(owner.clone());
        }

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }

        async fn seal_before_release(&self, _entity: &Entity) -> bool {
            self.seal_started.notify_one();
            self.seal_release.notified().await;
            true
        }

        async fn begin_room_shutdown(&self) -> bool {
            true
        }

        async fn retire_room_after_shutdown(&self, _entity: &Entity) -> bool {
            true
        }
    }

    #[async_trait]
    impl LocallyClaimedEntities for RejectingHydrationLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            Vec::new()
        }

        async fn demote(&self, _entity: &Entity) {}

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }

        fn reserve_reclaimed_claim_capacity(
            &self,
            _entity: &Entity,
        ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
            Some(waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(1))
        }

        fn defer_uncertain_reclaimed_claim(
            &self,
            entity: &Entity,
            _owner: &NodeIdentity,
            _reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
        ) {
            self.deferred.lock().expect("lock").push(entity.clone());
        }
    }

    #[async_trait]
    impl LocallyClaimedEntities for CancelledHydrationLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            Vec::new()
        }

        async fn demote(&self, _entity: &Entity) {}

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }

        async fn hydrate_reclaimed(
            &self,
            _entities: &[(
                Entity,
                NodeIdentity,
                ClaimEpoch,
                waddle_xmpp::stream_management::ReclaimedClaimReservation,
            )],
        ) -> ReclaimedHydrationHandoff {
            self.hydration_started.notify_one();
            std::future::pending().await
        }

        fn reserve_reclaimed_claim_capacity(
            &self,
            _entity: &Entity,
        ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
            Some(waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(1))
        }

        fn defer_uncertain_reclaimed_claim(
            &self,
            entity: &Entity,
            _owner: &NodeIdentity,
            _reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
        ) {
            self.deferred.lock().expect("lock").push(entity.clone());
        }
    }

    #[async_trait]
    impl LocallyClaimedEntities for CapacityLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            Vec::new()
        }

        async fn demote(&self, _entity: &Entity) {}

        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }

        fn reserve_reclaimed_claim_capacity(
            &self,
            _entity: &Entity,
        ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
            self.admission_attempts.fetch_add(1, Ordering::SeqCst);
            self.remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .ok()
                .map(|generation| {
                    waddle_xmpp::stream_management::ReclaimedClaimReservation::from_generation(
                        u64::from(generation) + 1,
                    )
                })
        }

        async fn hydrate_reclaimed(
            &self,
            entities: &[(
                Entity,
                NodeIdentity,
                ClaimEpoch,
                waddle_xmpp::stream_management::ReclaimedClaimReservation,
            )],
        ) -> ReclaimedHydrationHandoff {
            self.hydrated
                .lock()
                .expect("lock")
                .extend(entities.iter().map(|(entity, _, _, _)| entity.clone()));
            ReclaimedHydrationHandoff::Accepted
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
            exact_demoted_owners: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
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
            exact_demoted_owners: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
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
            exact_demoted_owners: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
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
    // ALL local claims" loop just above (`for entity in
    // local_claims.owned().await { local_claims.demote(&entity).await }`,
    // reached once the `'tick` loop breaks on either fencing trigger) was
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
            exact_demoted_owners: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
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
    async fn fatal_orphan_recovery_fence_demotes_every_claim_before_clustering_stops() {
        let interval = Duration::from_millis(50);
        let lease_ttl = Duration::from_secs(10);
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        let entities = vec![
            user_actor_entity("fatal-fence-user"),
            waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                "fatal-fence-room",
            ),
            waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::SmSession,
                "fatal-fence-sm",
            ),
        ];
        let demoted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let exact_demoted_owners = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: entities.clone(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::clone(&demoted),
            exact_demoted_owners: Arc::clone(&exact_demoted_owners),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
        });
        let readiness = ClusteringReadiness::new();
        let fatal_fence = readiness.fatal_fence_token();
        let stop_token = CancellationToken::new();
        let initial_identity = identity();
        let live_identity =
            waddle_xmpp::ownership::SharedNodeIdentity::new(initial_identity.clone());
        let task = tokio::spawn(run_node_lease(
            lease,
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
                    isolation_intervals: 1_000,
                    reregister_backoff_base: Duration::from_millis(10),
                    reregister_backoff_max: Duration::from_millis(20),
                },
                connected_peers: ConnectedPeerCount::new(),
                local_claims,
                readiness: readiness.clone(),
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
            },
        ));

        tokio::task::yield_now().await;
        fatal_fence.cancel();
        task.await.expect("node lease exits after terminal fence");

        let mut demoted_entities = demoted.lock().expect("lock").clone();
        demoted_entities.sort_by(|a, b| a.id.cmp(&b.id));
        let mut expected = entities;
        expected.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(demoted_entities, expected);
        assert!(!readiness.is_ready());
        assert_ne!(
            live_identity.current(),
            initial_identity,
            "fatal fencing must enter a terminally disabled publication state"
        );
        assert_eq!(
            exact_demoted_owners.lock().expect("lock").as_slice(),
            std::slice::from_ref(&initial_identity),
            "all work published under the superseded identity must be demoted"
        );
        assert!(
            stop_token.is_cancelled(),
            "sibling clustering tasks stop only after local authority is demoted"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fatal_fence_bounds_a_wedged_exact_owner_registry_and_reaches_user_cleanup() {
        let interval = Duration::from_millis(60);
        let initial_identity = identity();
        let user_demoted = Arc::new(AtomicBool::new(false));
        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(HangingFatalCleanupClaims {
            user: user_actor_entity("fatal-user-after-wedged-room"),
            user_demoted: Arc::clone(&user_demoted),
        });
        let readiness = ClusteringReadiness::new();
        let fatal_fence = readiness.fatal_fence_token();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            FakeLease::new(Box::new(|| Ok(true))),
            initial_identity.clone(),
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
                live_identity: SharedNodeIdentity::new(initial_identity),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
            },
        ));

        tokio::task::yield_now().await;
        fatal_fence.cancel();
        advance_until(Duration::from_millis(10), 30, || task.is_finished()).await;
        task.await
            .expect("fatal cleanup must return within its control-plane budget");

        assert!(
            user_demoted.load(Ordering::SeqCst),
            "a wedged exact-owner registry must not starve reserved UserActor cleanup"
        );
        assert!(!readiness.is_ready());
        assert!(stop_token.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn fatal_fence_latched_during_reregistration_cannot_restore_readiness() {
        let interval = Duration::from_millis(50);
        let mut lease = FakeLease::new(Box::new(|| Ok(false)));
        let registration_entered = Arc::new(tokio::sync::Notify::new());
        let registration_release = Arc::new(tokio::sync::Notify::new());
        lease.registration_entered = Some(Arc::clone(&registration_entered));
        lease.registration_release = Some(Arc::clone(&registration_release));

        let initial_identity = identity();
        let live_identity =
            waddle_xmpp::ownership::SharedNodeIdentity::new(initial_identity.clone());
        let exact_demoted_owners = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            exact_demoted_owners: Arc::clone(&exact_demoted_owners),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
        });
        let readiness = ClusteringReadiness::new();
        let fatal_fence = readiness.fatal_fence_token();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            initial_identity.clone(),
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
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
            },
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(interval + Duration::from_millis(20)).await;
        registration_entered.notified().await;
        fatal_fence.cancel();
        registration_release.notify_one();
        task.await
            .expect("terminal latch exits after registration returns");

        assert!(!readiness.is_ready());
        assert!(stop_token.is_cancelled());
        let published = live_identity.current();
        assert_ne!(published, initial_identity);
        let exact = exact_demoted_owners.lock().expect("lock");
        assert!(exact.contains(&initial_identity));
        assert_eq!(
            exact.len(),
            2,
            "both pre-fence and freshly registered owners retire"
        );
        assert!(
            exact.iter().all(|owner| owner != &published),
            "the live identity must be terminally disabled, not either retired owner"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_reregistration_disables_the_superseded_identity() {
        let interval = Duration::from_millis(50);
        let mut lease = FakeLease::new(Box::new(|| Ok(false)));
        let registration_entered = Arc::new(tokio::sync::Notify::new());
        let registration_release = Arc::new(tokio::sync::Notify::new());
        lease.registration_entered = Some(Arc::clone(&registration_entered));
        lease.registration_release = Some(Arc::clone(&registration_release));

        let initial_identity = identity();
        let live_identity =
            waddle_xmpp::ownership::SharedNodeIdentity::new(initial_identity.clone());
        let exact_demoted_owners = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            exact_demoted_owners: Arc::clone(&exact_demoted_owners),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
        });
        let readiness = ClusteringReadiness::new();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            lease,
            initial_identity.clone(),
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
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
            },
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(interval + Duration::from_millis(20)).await;
        registration_entered.notified().await;
        stop_token.cancel();
        registration_release.notify_one();
        task.await
            .expect("shutdown exits after terminally disabling publication");

        assert!(!readiness.is_ready());
        assert!(!live_identity.current().is_active());
        let exact = exact_demoted_owners.lock().expect("lock");
        assert!(exact.contains(&initial_identity));
        assert_eq!(
            exact.len(),
            2,
            "both pre-fence and potentially registered owners retire"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fatal_fence_preempts_an_in_progress_graceful_drain() {
        let interval = Duration::from_millis(50);
        let initial_identity = identity();
        let live_identity =
            waddle_xmpp::ownership::SharedNodeIdentity::new(initial_identity.clone());
        let seal_started = Arc::new(tokio::sync::Notify::new());
        let seal_release = Arc::new(tokio::sync::Notify::new());
        let exact_demoted_owners = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(BlockingSealLocalClaims {
            entity: waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                "fatal-during-drain",
            ),
            seal_started: Arc::clone(&seal_started),
            seal_release,
            exact_demoted_owners: Arc::clone(&exact_demoted_owners),
        });
        let readiness = ClusteringReadiness::new();
        let fatal_fence = readiness.fatal_fence_token();
        let stop_token = CancellationToken::new();
        let task = tokio::spawn(run_node_lease(
            FakeLease::new(Box::new(|| Ok(true))),
            initial_identity.clone(),
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
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
            },
        ));

        tokio::task::yield_now().await;
        stop_token.cancel();
        seal_started.notified().await;
        assert!(
            live_identity.current().is_active(),
            "ordinary drain keeps authority active through the seal barrier"
        );

        fatal_fence.cancel();
        task.await
            .expect("fatal ambiguity interrupts the blocked graceful drain");

        assert!(!readiness.is_ready());
        assert!(!live_identity.current().is_active());
        assert_eq!(
            exact_demoted_owners.lock().expect("lock").as_slice(),
            std::slice::from_ref(&initial_identity),
            "fatal preemption exact-demotes the active identity once"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn node_lease_post_rotation_sweeps_the_exact_superseded_owner() {
        let interval = Duration::from_millis(50);
        let initial_identity = identity();
        let live_identity =
            waddle_xmpp::ownership::SharedNodeIdentity::new(initial_identity.clone());
        let exact_demoted_owners = Arc::new(std::sync::Mutex::new(Vec::new()));
        let local_claims = Arc::new(FakeLocalClaims {
            // Both ordinary inventories are deliberately empty. This models
            // an admission landing after the final snapshot; the exact-owner
            // hook must still run after rotation.
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            exact_demoted_owners: Arc::clone(&exact_demoted_owners),
            hydrated: FakeLocalClaims::unhydrated(),
            live_identity_at_hydration: None,
            stale_hydration_observed: Arc::new(AtomicBool::new(false)),
        });
        let stop_token = CancellationToken::new();
        let mut heartbeat_calls = 0usize;
        tokio::spawn(run_node_lease(
            FakeLease::new(Box::new(move || {
                heartbeat_calls += 1;
                Ok(heartbeat_calls > 1)
            })),
            initial_identity.clone(),
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
                readiness: ClusteringReadiness::new(),
                live_identity: live_identity.clone(),
                peer_id: None,
                claim_store: Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
            },
        ));

        advance_until(interval, 20, || {
            !exact_demoted_owners.lock().expect("lock").is_empty()
        })
        .await;
        assert_eq!(
            exact_demoted_owners.lock().expect("lock").as_slice(),
            std::slice::from_ref(&initial_identity)
        );
        assert_ne!(
            live_identity.current(),
            initial_identity,
            "exact-owner cleanup must run only after admissions use the fresh identity"
        );
        stop_token.cancel();
    }

    // --- FIX 4(b) (ADR-0017 Phase 3 Slice 5 corrigenda, council-
    // adjudicated): the inline post-fence reclaim of this node's own
    // just-expired identity's SM-session claims. Postgres-gated: exercises
    // the real `PostgresClaimStore`/`NodeLeaseStore` CAS, not a fake
    // double, because the whole point is proving `steal_stale(OwnerStale)`
    // actually wins against a real `clustering_claims` row.

    #[tokio::test]
    async fn inline_reclaim_stops_before_a_cas_when_exact_fence_capacity_is_full() {
        let old = identity();
        let fresh = identity();
        let first = Entity::new(
            waddle_xmpp::ownership::EntityType::SmSession,
            "capacity-first",
        );
        let second = Entity::new(
            waddle_xmpp::ownership::EntityType::SmSession,
            "capacity-second",
        );
        let store = waddle_xmpp::ownership::InProcessClaimStore::new();
        let first_epoch = store.acquire(&first, &old).await.expect("first claim");
        let second_epoch = store.acquire(&second, &old).await.expect("second claim");
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        *lease.orphaned_sm_claims.lock().expect("lock") = vec![
            crate::clustering::claims::OrphanedSmSessionClaim {
                entity: first.clone(),
                epoch: first_epoch,
                owner: old.clone(),
            },
            crate::clustering::claims::OrphanedSmSessionClaim {
                entity: second.clone(),
                epoch: second_epoch,
                owner: old.clone(),
            },
        ];
        let local = CapacityLocalClaims {
            remaining: AtomicU32::new(1),
            admission_attempts: AtomicU32::new(0),
            hydrated: std::sync::Mutex::new(Vec::new()),
        };

        reclaim_own_expired_claims(
            &lease,
            &store,
            &old,
            &fresh,
            &local,
            Duration::from_secs(10),
        )
        .await;

        assert_eq!(local.admission_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            local.hydrated.lock().expect("lock").as_slice(),
            std::slice::from_ref(&first)
        );
        assert_eq!(
            store
                .current_claim(&first)
                .await
                .expect("first lookup")
                .expect("first claim remains")
                .owner,
            fresh
        );
        assert_eq!(
            store
                .current_claim(&second)
                .await
                .expect("second lookup")
                .expect("second claim remains")
                .owner,
            old,
            "capacity rejection must happen before the second ownership CAS"
        );
    }

    #[tokio::test]
    async fn cancelled_inline_hydration_retains_fresh_claim_responsibility() {
        let old = identity();
        let fresh = identity();
        let entity = Entity::new(
            waddle_xmpp::ownership::EntityType::SmSession,
            "cancelled-inline-hydration",
        );
        let store = Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        let old_epoch = store.acquire(&entity, &old).await.expect("old claim");
        let lease = Arc::new(FakeLease::new(Box::new(|| Ok(true))));
        *lease.orphaned_sm_claims.lock().expect("lock") =
            vec![crate::clustering::claims::OrphanedSmSessionClaim {
                entity: entity.clone(),
                epoch: old_epoch,
                owner: old.clone(),
            }];
        let local = Arc::new(CancelledHydrationLocalClaims {
            hydration_started: tokio::sync::Notify::new(),
            deferred: std::sync::Mutex::new(Vec::new()),
        });
        let reclaim_lease = lease.clone();
        let reclaim_store = store.clone();
        let reclaim_local = local.clone();
        let reclaim_old = old.clone();
        let reclaim_fresh = fresh.clone();
        let reclaim = tokio::spawn(async move {
            reclaim_own_expired_claims(
                reclaim_lease.as_ref(),
                reclaim_store.as_ref(),
                &reclaim_old,
                &reclaim_fresh,
                reclaim_local.as_ref(),
                Duration::from_secs(10),
            )
            .await;
        });
        local.hydration_started.notified().await;
        reclaim.abort();
        assert!(reclaim.await.unwrap_err().is_cancelled());

        assert_eq!(
            store
                .current_claim(&entity)
                .await
                .unwrap()
                .expect("fresh claim")
                .owner,
            fresh
        );
        assert_eq!(
            local.deferred.lock().expect("lock").as_slice(),
            std::slice::from_ref(&entity),
            "cancellation after CAS commit must transfer the armed reservation into read-only reconciliation"
        );
    }

    #[tokio::test]
    async fn rejected_inline_hydration_retains_fresh_claim_responsibility() {
        let old = identity();
        let fresh = identity();
        let entity = Entity::new(
            waddle_xmpp::ownership::EntityType::SmSession,
            "rejected-inline-hydration",
        );
        let store = waddle_xmpp::ownership::InProcessClaimStore::new();
        let old_epoch = store.acquire(&entity, &old).await.expect("old claim");
        let lease = FakeLease::new(Box::new(|| Ok(true)));
        *lease.orphaned_sm_claims.lock().expect("lock") =
            vec![crate::clustering::claims::OrphanedSmSessionClaim {
                entity: entity.clone(),
                epoch: old_epoch,
                owner: old.clone(),
            }];
        let local = RejectingHydrationLocalClaims {
            deferred: std::sync::Mutex::new(Vec::new()),
        };

        reclaim_own_expired_claims(
            &lease,
            &store,
            &old,
            &fresh,
            &local,
            Duration::from_secs(10),
        )
        .await;

        assert_eq!(
            store
                .current_claim(&entity)
                .await
                .unwrap()
                .expect("fresh claim")
                .owner,
            fresh
        );
        assert_eq!(
            local.deferred.lock().expect("lock").as_slice(),
            std::slice::from_ref(&entity),
            "a hydration hook that declines responsibility must leave the fallback reservation armed"
        );
    }

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
        let live_identity =
            waddle_xmpp::ownership::SharedNodeIdentity::new(initial_identity.clone());
        let stale_hydration_observed = Arc::new(AtomicBool::new(false));
        let local_claims = Arc::new(FakeLocalClaims {
            owned: Vec::new(),
            healthy: Arc::new(AtomicBool::new(true)),
            demoted: Arc::new(std::sync::Mutex::new(Vec::new())),
            exact_demoted_owners: Arc::new(std::sync::Mutex::new(Vec::new())),
            hydrated: Arc::clone(&hydrated),
            live_identity_at_hydration: Some(live_identity.clone()),
            stale_hydration_observed: Arc::clone(&stale_hydration_observed),
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
                live_identity,
                peer_id: None,
                claim_store: task_claim_store,
                claim_release_budget: TEST_CLAIM_RELEASE_BUDGET,
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
        assert!(
            !stale_hydration_observed.load(Ordering::SeqCst),
            "the fresh registered identity must be published before inline reclaimed hydration begins"
        );

        // The claims row must now show the FRESH re-registered identity as
        // owner — `steal_stale(OwnerStale)` actually won against the real
        // CAS, not just a fake double.
        let conn = db.guard().await.expect("guard");
        let mut rows = conn
            .query(
                "SELECT node_id FROM clustering_claims WHERE entity = ?",
                crate::db_params![format!(
                    "{}:{}",
                    EntityType::SmSession.as_db_str(),
                    entity.id
                )],
            )
            .await
            .expect("query claims row");
        let owner_node_id: String = rows
            .next()
            .await
            .expect("row present")
            .expect("row present")
            .get(0)
            .expect("column present");
        assert_ne!(
            owner_node_id, initial_identity.node_id,
            "the claim must be owned by the FRESH re-registered identity, not the expired one"
        );

        stop_token.cancel();
    }
}
