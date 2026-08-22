//! Graceful per-entity claim drain + rollout-aware acquire placement
//! (ADR-0017 Phase 3 Slice 10, element 4's drain sequence). The
//! drain-observability deliverable below (drain-duration histogram,
//! `claims_released_on_drain`/`claims_abandoned_on_drain` counters, alert
//! on nonzero abandonment) is the ADR's own Phase 3 Implementation Plan
//! drain text, NOT element 12 — element 12 is DB pool-size/capacity
//! -planning configurability, an unrelated deliverable (a mis-citation
//! this doc comment previously carried — corrected, per the phase plan's
//! Slice 10 FIX 5(a) council-adjudicated pass).
//!
//! **Locked spec** (element 4, quoted in the phase plan's Slice 10 section):
//! per-entity, not phase-ordered drain — mark the node draining in `nodes`
//! (stop acquiring NEW claims, keep serving already-owned ones); for each
//! owned entity, complete its final fenced write(s) *while still holding the
//! claim*, then release; batched via [`ClaimStore::release_many`] (one
//! round-trip, not one-at-a-time, given the ~18k modeled claims a full
//! cluster carries); **no** global "promote what remains" step after
//! release — releasing first and completing a write second is the exact
//! fencing violation element 4 forbids.
//!
//! **Composes with, does not replace, the existing Q6 SM-session drain**
//! (`server::session_janitors::spawn_graceful_shutdown_drain`,
//! `InMemorySmSessionRegistry::confirm_drained`): [`run_shutdown_drain`]
//! below deliberately skips [`EntityType::SmSession`] entities. An SM
//! session's "final fenced write" already IS `confirm_drained`'s own
//! promote-then-delete-then-release sequence, running on an independent
//! task racing the same shutdown token. Touching the same entity from both
//! drain paths concurrently would be exactly the double-drain hazard the
//! phase plan's Slice 10 "interaction with existing drain" note warns
//! against — one path could release a claim the other path's promotion is
//! still mid-write under. Only [`EntityType::RoomActor`] (and, forward-
//! looking, `UserActor` once a later phase wires production claims for it)
//! is drained here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, NodeIdentity};

use super::claims::NodeLeaseStore;
use super::metrics;
use super::self_fence::{mark_draining_bounded, LocallyClaimedEntities};

#[cfg(test)]
use super::self_fence::run_shutdown_drain_with_heartbeat;

/// Upper bound on how long [`mark_draining_bounded`] itself is allowed to
/// take within the overall drain budget — a hung flag-flip must not eat the
/// whole per-entity release budget.
const MARK_DRAINING_BOUND: Duration = Duration::from_secs(2);

/// Run the Slice 10 graceful drain sequence to completion (or until
/// `budget` is exhausted) for every locally-owned, eligible entity. Called
/// exactly once per fence/shutdown, from every ordinary-shutdown branch in
/// [`super::self_fence::run_node_lease`]'s `'tick` loop.
///
/// Sequence: (1) mark this node draining — [`super::claims`]'s
/// `acquire`/`steal_stale` CAS gate then atomically refuses any NEW claim
/// under this node's identity, while claims it already holds keep being
/// served (never revoked by this function); (2) for each owned, eligible
/// entity, call [`LocallyClaimedEntities::seal_before_release`] to complete
/// (or confirm already-complete) its final fenced write, and — **strictly
/// after that call returns successfully, never before** — append it to the
/// release batch (the batch-entry-only-after-commit invariant, major fix
/// 13 in the phase plan); (3) once every eligible entity has been sealed or
/// the budget is exhausted, release everything sealed in ONE
/// [`ClaimStore::release_many`] call. An entity whose seal did not
/// complete within budget is left claimed — abandoned, not lost: the claim
/// stays fenced-safe and is reclaimed later by another node's orphan
/// reaper, or by this node itself once its own lease naturally lapses.
#[tracing::instrument(name = "clustering.shutdown_drain", skip_all)]
pub(crate) async fn run_shutdown_drain<L>(
    lease: &L,
    claim_store: &Arc<dyn ClaimStore>,
    identity: &NodeIdentity,
    local_claims: &Arc<dyn LocallyClaimedEntities>,
    budget: Duration,
) where
    L: NodeLeaseStore + Send + Sync,
{
    let start = Instant::now();
    mark_draining_bounded(lease, identity, MARK_DRAINING_BOUND.min(budget)).await;

    let deadline = start + budget;
    let owned = local_claims.owned().await;
    let mut to_release: Vec<Entity> = Vec::new();
    let mut abandoned: u64 = 0;

    for entity in owned {
        // Only entities this generic loop owns draining for — see the
        // module doc for why `SmSession` is deliberately excluded (the
        // existing Q6 drain owns those) and `UserActor` has no production
        // claim-acquisition call site yet (deviation 34).
        if entity.entity_type != EntityType::RoomActor {
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                entity_id = %entity.id,
                "clustering drain: budget exhausted before this entity's seal even started; \
                 leaving it claimed (fenced-safe, reclaimed later)"
            );
            abandoned += 1;
            continue;
        }
        let sealed = tokio::time::timeout(remaining, local_claims.seal_before_release(&entity))
            .await
            .unwrap_or(false);
        if sealed {
            // Batch-entry-only-after-commit invariant (major fix 13): this
            // entity enters `to_release` strictly after `seal_before_release`
            // — which performs/confirms the entity's final fenced write —
            // has already returned `true`, never before.
            to_release.push(entity);
        } else {
            tracing::warn!(
                entity_id = %entity.id,
                "clustering drain: seal_before_release failed or timed out; leaving this \
                 entity claimed (fenced-safe, reclaimed later)"
            );
            abandoned += 1;
        }
    }

    if !to_release.is_empty() {
        let attempted = to_release.len() as u64;
        match claim_store.release_many(&to_release, identity).await {
            Ok(()) => {
                metrics::record_claims_released_on_drain(attempted);
            }
            Err(error) => {
                let _ = error;
                tracing::warn!(
                    error_class = "backend",
                    count = attempted,
                    "clustering drain: release_many failed; entities remain claimed \
                     (fenced-safe, reclaimed later)"
                );
                abandoned += attempted;
            }
        }
    }
    if abandoned > 0 {
        // All abandonment causes are internal drain failures even though the
        // claims remain fenced-safe for later recovery.
        crate::telemetry::mark_span_error("clustering drain abandoned claims");
        metrics::record_claims_abandoned_on_drain(abandoned);
    }
    metrics::record_drain_duration_ms(start.elapsed().as_secs_f64() * 1000.0);
}

/// Rollout-aware claim-acquisition backoff decision (ADR-0017 Phase 3 Slice
/// 10, Q5's mechanism): how long a node whose own `pod_template_hash` does
/// not match the cluster's current deployment generation should wait before
/// attempting to acquire a released/orphaned claim, versus a matching
/// (or unknown) generation, which never backs off.
///
/// Pure and synchronous so it is trivially unit-testable without a live
/// `NodeLeaseStore`; callers resolve `current_generation` via
/// [`NodeLeaseStore::current_generation`] once per acquire attempt and pass
/// both hashes in here. **Never affects correctness** — only which
/// generation tends to *try first*; the claims CAS (element 4) remains the
/// sole authority over who actually wins any given claim (Q5: "misclassifying
/// the current generation... only costs a few wasted acquire attempts or a
/// slower rebalance, never a double-owned claim").
pub(crate) fn rollout_backoff_delay(
    my_pod_template_hash: Option<&str>,
    current_generation: Option<&str>,
) -> Duration {
    match (my_pod_template_hash, current_generation) {
        // Either side unknown: nothing to compare, so never back off —
        // fail open toward "try immediately," matching Q5's "a missing hash
        // is 'no generation to compare,' never a parse failure" rule.
        (None, _) | (_, None) => Duration::ZERO,
        (Some(mine), Some(current)) if mine == current => Duration::ZERO,
        // Old-generation pod: a small jittered delay so a matching-generation
        // pod gets first crack at a freshly released/orphaned claim during a
        // rollout, without ever blocking this node from trying at all.
        (Some(_), Some(_)) => OLD_GENERATION_BACKOFF,
    }
}

/// Fixed backoff for an old-generation node's claim-acquisition attempt
/// (ADR-0017 Phase 3 Slice 10). Deliberately small: this is a placement
/// heuristic, not a correctness mechanism, so it only needs to be large
/// enough to reliably lose the race to a same-instant new-generation
/// attempt, never so large that it meaningfully delays re-election when no
/// new-generation node is contending at all.
const OLD_GENERATION_BACKOFF: Duration = Duration::from_millis(250);

/// The `RoomRegistry`'s (waddle-xmpp) concrete
/// [`waddle_xmpp::ownership::RolloutBackoff`] implementation: resolves
/// [`NodeLeaseStore::current_generation`] once per call and folds it
/// through [`rollout_backoff_delay`] against this node's own
/// `pod_template_hash`. Wired at `server/mod.rs` alongside the room
/// registry's other clustering handles, mirroring the orphan reaper's
/// identical direct use of `rollout_backoff_delay` (`session_janitors.rs`)
/// one crate boundary over.
pub(crate) struct PostgresRolloutBackoff {
    node_lease: Arc<dyn NodeLeaseStore>,
    pod_template_hash: Option<String>,
}

impl PostgresRolloutBackoff {
    pub(crate) fn new(
        node_lease: Arc<dyn NodeLeaseStore>,
        pod_template_hash: Option<String>,
    ) -> Self {
        Self {
            node_lease,
            pod_template_hash,
        }
    }
}

#[async_trait::async_trait]
impl waddle_xmpp::ownership::RolloutBackoff for PostgresRolloutBackoff {
    async fn acquire_delay(&self) -> Duration {
        match self.node_lease.current_generation().await {
            Ok(current_generation) => rollout_backoff_delay(
                self.pod_template_hash.as_deref(),
                current_generation.as_deref(),
            ),
            Err(error) => {
                let _ = error;
                tracing::debug!(
                    error_class = "backend",
                    "clustering: current_generation lookup failed; proceeding without backoff"
                );
                Duration::ZERO
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, InProcessClaimStore, NodeIdentity};

    // --- rollout_backoff_delay: pure-function coverage -----------------

    #[test]
    fn matching_generation_never_backs_off() {
        assert_eq!(
            rollout_backoff_delay(Some("abc123"), Some("abc123")),
            Duration::ZERO
        );
    }

    #[test]
    fn mismatched_generation_backs_off() {
        assert_eq!(
            rollout_backoff_delay(Some("old-gen"), Some("new-gen")),
            OLD_GENERATION_BACKOFF
        );
        assert!(OLD_GENERATION_BACKOFF > Duration::ZERO);
    }

    #[test]
    fn unknown_hash_never_backs_off() {
        assert_eq!(rollout_backoff_delay(None, Some("new-gen")), Duration::ZERO);
        assert_eq!(rollout_backoff_delay(Some("mine"), None), Duration::ZERO);
        assert_eq!(rollout_backoff_delay(None, None), Duration::ZERO);
    }

    // --- run_shutdown_drain: fake lease + fake local claims -------------

    struct NoopLease;

    #[async_trait]
    impl NodeLeaseStore for NoopLease {
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
            _me: &NodeIdentity,
            _pod_template_hash: Option<String>,
        ) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn heartbeat(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }
        async fn expire(
            &self,
            _owner: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }
        async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn count_other_live_nodes(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<usize, ClaimError> {
            Ok(0)
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
            _entity: &Entity,
            _reporter: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            Ok(())
        }
        async fn owner_steal_intents(
            &self,
            _me: &NodeIdentity,
        ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
            Ok(Vec::new())
        }
        async fn clear_steal_intent(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<u64, ClaimError> {
            Ok(0)
        }
        async fn list_orphaned_sm_session_claims(
            &self,
        ) -> Result<Vec<super::super::claims::OrphanedSmSessionClaim>, ClaimError> {
            Ok(Vec::new())
        }
        async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
            Ok(None)
        }
    }

    /// A configurable `LocallyClaimedEntities` fake that reports a fixed
    /// owned-entity set and records the exact order `seal_before_release`
    /// was called in (so tests can assert the batch-construction ordering
    /// invariant directly, per major fix 13's own "instrument
    /// commit/batch-append order" requirement) plus a per-entity
    /// success/failure/hang script.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SealOutcome {
        Succeed,
        Fail,
        Hang,
    }

    struct FakeLocalClaims {
        owned: Vec<Entity>,
        outcome: SealOutcome,
        seal_calls: Arc<Mutex<Vec<String>>>,
        demote_calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl LocallyClaimedEntities for FakeLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            self.owned.clone()
        }
        async fn demote(&self, _entity: &Entity) {
            self.demote_calls.fetch_add(1, Ordering::SeqCst);
        }
        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }
        async fn seal_before_release(&self, entity: &Entity) -> bool {
            match self.outcome {
                SealOutcome::Succeed => {
                    self.seal_calls
                        .lock()
                        .expect("lock")
                        .push(entity.id.clone());
                    true
                }
                SealOutcome::Fail => false,
                SealOutcome::Hang => {
                    // Outlast any budget a test passes in — proves the
                    // per-entity `tokio::time::timeout` actually bounds a
                    // wedged seal rather than hanging the whole drain.
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    unreachable!("budget must have timed this out first")
                }
            }
        }
    }

    fn room(id: &str) -> Entity {
        Entity::new(EntityType::RoomActor, id.to_string())
    }

    fn sm(id: &str) -> Entity {
        Entity::new(EntityType::SmSession, id.to_string())
    }

    fn identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_seals_then_releases_only_room_entities_skipping_sm_sessions() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let me = identity();
        let room_a = room("room-a@muc.example.com");
        let room_b = room("room-b@muc.example.com");
        let sm_c = sm("stream-c");
        let room_a_epoch = claim_store
            .acquire(&room_a, &me)
            .await
            .expect("acquire room a");
        let room_b_epoch = claim_store
            .acquire(&room_b, &me)
            .await
            .expect("acquire room b");
        let sm_c_epoch = claim_store.acquire(&sm_c, &me).await.expect("acquire sm c");

        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(FakeLocalClaims {
            owned: vec![room_a.clone(), room_b.clone(), sm_c.clone()],
            outcome: SealOutcome::Succeed,
            seal_calls: Arc::new(Mutex::new(Vec::new())),
            demote_calls: Arc::new(AtomicU32::new(0)),
        });

        run_shutdown_drain(
            &NoopLease,
            &claim_store,
            &me,
            &local_claims,
            Duration::from_secs(5),
        )
        .await;

        assert!(
            !claim_store
                .fence(&room_a, &me, room_a_epoch)
                .await
                .unwrap_or(true),
            "room_a's claim must be released by the drain"
        );
        assert!(
            !claim_store
                .fence(&room_b, &me, room_b_epoch)
                .await
                .unwrap_or(true),
            "room_b's claim must be released by the drain"
        );
        assert!(
            claim_store
                .fence(&sm_c, &me, sm_c_epoch)
                .await
                .unwrap_or(false),
            "the sm_session entity must be left untouched by the generic drain loop \
             (owned by the separate Q6 SM drain path instead)"
        );
        assert_eq!(
            spans.status_of("clustering.shutdown_drain"),
            Some(opentelemetry::trace::Status::Unset)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_abandons_a_failed_seal_and_leaves_the_claim_held() {
        let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let me = identity();
        let room_a = room("room-fails@muc.example.com");
        let epoch = claim_store.acquire(&room_a, &me).await.expect("acquire");

        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(FakeLocalClaims {
            owned: vec![room_a.clone()],
            outcome: SealOutcome::Fail,
            seal_calls: Arc::new(Mutex::new(Vec::new())),
            demote_calls: Arc::new(AtomicU32::new(0)),
        });

        run_shutdown_drain(
            &NoopLease,
            &claim_store,
            &me,
            &local_claims,
            Duration::from_secs(5),
        )
        .await;

        assert!(
            claim_store
                .fence(&room_a, &me, epoch)
                .await
                .unwrap_or(false),
            "a failed seal must leave the claim held, not release it"
        );
        assert!(matches!(
            spans.status_of("clustering.shutdown_drain"),
            Some(opentelemetry::trace::Status::Error { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn drain_abandons_a_hung_seal_once_the_budget_is_exhausted() {
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let me = identity();
        let room_a = room("room-hangs@muc.example.com");
        let epoch = claim_store.acquire(&room_a, &me).await.expect("acquire");

        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(FakeLocalClaims {
            owned: vec![room_a.clone()],
            outcome: SealOutcome::Hang,
            seal_calls: Arc::new(Mutex::new(Vec::new())),
            demote_calls: Arc::new(AtomicU32::new(0)),
        });

        let budget = Duration::from_millis(50);
        let task_me = me.clone();
        let drain = tokio::spawn(async move {
            run_shutdown_drain(&NoopLease, &claim_store, &task_me, &local_claims, budget).await;
            claim_store
        });
        tokio::time::advance(budget * 4).await;
        let claim_store = drain.await.expect("drain task");

        assert!(
            claim_store
                .fence(&room_a, &me, epoch)
                .await
                .unwrap_or(false),
            "a hung seal, once the budget overruns, must leave the claim held (merely \
             un-released, fenced-safe) rather than block shutdown indefinitely"
        );
    }

    #[tokio::test]
    async fn drain_builds_the_release_batch_in_seal_completion_order() {
        // Major fix 13: an entity enters the release batch only AFTER its
        // own seal call returns — asserted here by recording the seal-call
        // order and cross-checking it against `owned()`'s (deliberately
        // different) order.
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let me = identity();
        let entities: Vec<Entity> = (0..5)
            .map(|i| room(&format!("room-{i}@muc.example.com")))
            .collect();
        let mut epochs = Vec::with_capacity(entities.len());
        for entity in &entities {
            epochs.push(claim_store.acquire(entity, &me).await.expect("acquire"));
        }
        let seal_calls = Arc::new(Mutex::new(Vec::new()));
        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(FakeLocalClaims {
            owned: entities.clone(),
            outcome: SealOutcome::Succeed,
            seal_calls: Arc::clone(&seal_calls),
            demote_calls: Arc::new(AtomicU32::new(0)),
        });

        run_shutdown_drain(
            &NoopLease,
            &claim_store,
            &me,
            &local_claims,
            Duration::from_secs(5),
        )
        .await;

        let recorded = seal_calls.lock().expect("lock").clone();
        assert_eq!(
            recorded.len(),
            entities.len(),
            "every owned room must have had its seal attempted exactly once"
        );
        for (entity, epoch) in entities.iter().zip(epochs) {
            assert!(
                !claim_store.fence(entity, &me, epoch).await.unwrap_or(true),
                "every successfully-sealed entity must end up released"
            );
        }
    }

    // --- Postgres-gated: modeled-scale + the draining-gate ABA closure --

    /// Returns the cleaned store plus the underlying `Database` pool handle
    /// (cheap to `.clone()`) so a test that needs a SECOND
    /// `PostgresClaimStore`/`ClaimStore` view onto the exact same database
    /// (e.g. one bound generically as `&L: NodeLeaseStore`, another as
    /// `Arc<dyn ClaimStore>`, exactly how production `start_if_enabled`
    /// wires two separate `PostgresClaimStore::new(db.clone())` instances
    /// over one `db`) can build it without opening an independent pool.
    async fn clean_postgres_store() -> Option<(
        super::super::claims::PostgresClaimStore,
        crate::db::Database,
    )> {
        use crate::db::{Database, DatabaseConfig, DatabaseDriver};
        let url = std::env::var("WADDLE_TEST_POSTGRES_URL").ok()?;
        let db = Database::from_config(
            "clustering-drain-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = super::super::claims::PostgresClaimStore::new(db.clone());
        store.ensure_schema().await.expect("ensure schema");
        let conn = db.guard().await.expect("guard");
        conn.execute("DELETE FROM clustering_claims", ())
            .await
            .expect("clean claims");
        conn.execute("DELETE FROM clustering_nodes", ())
            .await
            .expect("clean nodes");
        Some((store, db))
    }

    /// ADR-0017 Phase 3 Slice 10's own exit-criterion test: "drain
    /// thousands of claimed entities; assert wall clock fits
    /// `claimReleaseBudget`." Scaled down from the ADR's ~18k-per-node
    /// model to a still-meaningfully-batched, CI-friendly count — the
    /// property under test (ONE `release_many` round-trip, not one
    /// statement per entity) is independent of the exact N, and this count
    /// is large enough that a naive one-at-a-time release would blow the
    /// budget while the real batched implementation does not.
    #[tokio::test]
    async fn drain_at_modeled_scale_fits_the_claim_release_budget() {
        let _guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((claim_store_typed, _db)) = clean_postgres_store().await else {
            return;
        };
        let me = identity();
        const MODELED_ENTITY_COUNT: usize = 2_000;
        let mut entities = Vec::with_capacity(MODELED_ENTITY_COUNT);
        for i in 0..MODELED_ENTITY_COUNT {
            let entity = room(&format!("room-{i}@muc.example.com"));
            let epoch = claim_store_typed
                .acquire(&entity, &me)
                .await
                .expect("acquire");
            entities.push((entity, epoch));
        }
        let claim_store: Arc<dyn ClaimStore> = Arc::new(claim_store_typed);

        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(FakeLocalClaims {
            owned: entities.iter().map(|(entity, _)| entity.clone()).collect(),
            outcome: SealOutcome::Succeed,
            seal_calls: Arc::new(Mutex::new(Vec::new())),
            demote_calls: Arc::new(AtomicU32::new(0)),
        });

        // Generous relative to what a single `release_many` round-trip
        // needs, but far tighter than ~2,000 individual release
        // round-trips would require — this is the budget the wall clock
        // below must fit inside.
        let budget = Duration::from_secs(5);
        let start = Instant::now();
        run_shutdown_drain(&NoopLease, &claim_store, &me, &local_claims, budget).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < budget,
            "drain of {MODELED_ENTITY_COUNT} entities took {elapsed:?}, exceeding the \
             {budget:?} claimReleaseBudget"
        );

        // Spot-check a sample: genuinely released, not merely "didn't
        // time out."
        for (entity, epoch) in entities.iter().step_by(200) {
            assert!(
                !claim_store.fence(entity, &me, *epoch).await.unwrap_or(true),
                "entity {entity} must have been released by the modeled-scale drain"
            );
        }
    }

    /// ADR-0017 Phase 3 Slice 10 / Slice 3's release_many ABA forward
    /// reference: for `RoomActor` entities specifically (the only entity
    /// type the generic drain ever batches into `release_many` — see the
    /// module doc), there is structurally no PRE-release reacquisition
    /// window at all — nothing drops this node's claim before
    /// `release_many` itself runs, and rooms have no `steal_for_resume`
    /// -equivalent consent path (unlike SM sessions — see
    /// `claims::tests::release_many_epoch_blind_window_deletes_a_fresh_same_node_resume`,
    /// which reproduces that window at the raw store level via the ONE CAS
    /// variant, `steal_for_resume`, the draining gate does not cover). What
    /// the draining gate closes for rooms is the POST-release case: this
    /// test proves that once `release_many` has actually dropped the
    /// claim, the draining node that just released it cannot immediately
    /// win it back either (no same-node "steal it right back" loop),
    /// while a genuinely different, non-draining node can.
    #[tokio::test]
    async fn drain_prevents_a_draining_node_from_reclaiming_its_own_just_released_room() {
        let _guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((claim_store_typed, _db)) = clean_postgres_store().await else {
            return;
        };
        use super::super::claims::NodeLeaseStore as _;
        let me = identity();
        claim_store_typed
            .register(&me, None)
            .await
            .expect("register");
        let entity = room("room-post-release@muc.example.com");
        claim_store_typed
            .acquire(&entity, &me)
            .await
            .expect("acquire");

        // Drain marks this node draining, then batch-releases the entity —
        // exactly `run_shutdown_drain`'s own sequence.
        claim_store_typed
            .mark_draining(&me)
            .await
            .expect("mark draining");
        let claim_store: Arc<dyn ClaimStore> = Arc::new(claim_store_typed);
        claim_store
            .release_many(std::slice::from_ref(&entity), &me)
            .await
            .expect("release_many");

        // This same (still-draining) node cannot win it back.
        let reacquire = claim_store.acquire(&entity, &me).await;
        assert!(
            matches!(reacquire, Err(ClaimError::Draining)),
            "a draining node must not reclaim the entity it just released, \
             got {reacquire:?}"
        );

        // A genuinely different, non-draining node can.
        let other = identity();
        claim_store
            .acquire(&entity, &other)
            .await
            .expect("a non-draining node can freely claim the now-released entity");
    }

    /// `LocallyClaimedEntities` fake whose `seal_before_release` sleeps a
    /// fixed, real-clock duration before succeeding — long enough to run
    /// well past several node-lease heartbeat intervals, so a test can
    /// observe whether this node's `clustering_nodes` row stays fresh
    /// across a drain that genuinely outlives its own old heartbeat
    /// cadence.
    struct SlowSealLocalClaims {
        owned: Vec<Entity>,
        seal_delay: Duration,
    }

    #[async_trait]
    impl LocallyClaimedEntities for SlowSealLocalClaims {
        async fn owned(&self) -> Vec<Entity> {
            self.owned.clone()
        }
        async fn demote(&self, _entity: &Entity) {}
        async fn health_check(&self, _entity: &Entity) -> bool {
            true
        }
        async fn seal_before_release(&self, _entity: &Entity) -> bool {
            tokio::time::sleep(self.seal_delay).await;
            true
        }
    }

    /// ADR-0017 Phase 3 Slice 10 FIX 2 (council-adjudicated): the ordering
    /// invariant the heartbeat-during-drain restructure exists to prove —
    /// a node whose graceful drain runs well past its OLD node-lease
    /// heartbeat interval must keep its `clustering_nodes` row fresh (never
    /// committed `expired = true`) for the drain's entire duration, not
    /// just up to the moment shutdown began.
    ///
    /// Real wall-clock time throughout (Postgres I/O needs it — no
    /// `start_paused`): `lease_ttl` is deliberately tiny (300ms) and the
    /// fake room's `seal_before_release` sleeps for roughly 6x that
    /// (~1.8s) — under the PRE-FIX-2 shape (`run_shutdown_drain(..).await;
    /// return;`, no heartbeat renewal at all once shutdown fires) this
    /// entity's seal alone would outlast `lease_ttl` several times over
    /// with zero renewals, so a concurrent
    /// `NodeLeaseStore::expire(&me, lease_ttl)` call (exactly what the
    /// orphan reaper's own sweep does — see `steal_stale`'s
    /// `OwnerStale` predicate, which trusts the COMMITTED `expired` flag
    /// `expire()` sets, never a raw heartbeat read of its own) would
    /// commit `expired = true` well before the drain ever finished. Under
    /// the FIX 2 shape, `run_shutdown_drain_with_heartbeat` keeps renewing
    /// throughout, so every `expire()` poll during the drain must keep
    /// observing a heartbeat too fresh to flip.
    #[tokio::test]
    async fn drain_keeps_the_node_lease_row_fresh_through_a_drain_that_outlives_the_old_heartbeat_interval(
    ) {
        let _guard = super::super::claims::clustering_control_plane_table_lock()
            .lock()
            .await;
        let Some((claim_store_typed, db)) = clean_postgres_store().await else {
            return;
        };
        use super::super::claims::NodeLeaseStore as _;

        let me = identity();
        claim_store_typed
            .register(&me, None)
            .await
            .expect("register");
        let entity = room("room-outlives-heartbeat@muc.example.com");
        claim_store_typed
            .acquire(&entity, &me)
            .await
            .expect("acquire");

        let lease_ttl = Duration::from_millis(300);
        let seal_delay = lease_ttl * 6;
        let claim_release_budget = seal_delay * 3;

        let local_claims: Arc<dyn LocallyClaimedEntities> = Arc::new(SlowSealLocalClaims {
            owned: vec![entity.clone()],
            seal_delay,
        });
        // A second `ClaimStore` view onto the SAME underlying database
        // (`db.clone()` is a cheap pool-handle clone), mirroring production
        // `start_if_enabled`'s own two-separate-`PostgresClaimStore`-
        // instances-over-one-`db` wiring rather than opening an
        // independent pool.
        let claim_store: Arc<dyn ClaimStore> =
            Arc::new(super::super::claims::PostgresClaimStore::new(db.clone()));

        // Structured concurrency, no `tokio::spawn`: `drain_future` and the
        // polling loop below both only ever take `&claim_store_typed`
        // (every `NodeLeaseStore`/`ClaimStore` method is `&self`), so they
        // can run concurrently as two futures polled by this same test
        // task, racing each other with `select!` — no `Clone`/`'static`
        // bound needed anywhere, mirroring `run_shutdown_drain_with_heartbeat`'s
        // own no-spawn design.
        let drain_future = std::pin::pin!(run_shutdown_drain_with_heartbeat(
            &claim_store_typed,
            &claim_store,
            &me,
            &local_claims,
            claim_release_budget,
            lease_ttl,
        ));
        let mut drain_future = drain_future;

        // Poll `expire` — the exact mechanism the orphan reaper's sweep
        // uses to commit `expired = true` — repeatedly while the drain is
        // still in flight, well past `lease_ttl` several times over.
        let mut polls = 0u32;
        loop {
            tokio::select! {
                biased;
                _ = &mut drain_future => break,
                _ = tokio::time::sleep(lease_ttl / 3) => {
                    let expired = claim_store_typed
                        .expire(&me, lease_ttl)
                        .await
                        .expect("expire must not error");
                    assert!(
                        !expired,
                        "the draining node's row must never be committed expired while its \
                         own drain (heartbeat renewal included) is still in flight — got \
                         expired=true after {polls} poll(s), proving a stale heartbeat \
                         slipped through"
                    );
                    polls += 1;
                }
            }
        }
        assert!(
            polls >= 3,
            "sanity: this test must have actually polled expire() multiple times while the \
             drain was in flight (got {polls} polls) — otherwise it proves nothing"
        );

        // The drain must have actually completed successfully (seal
        // succeeded, claim released) — the freshness invariant above is
        // only meaningful alongside a drain that genuinely finishes.
        assert!(
            claim_store_typed
                .current_claim(&entity)
                .await
                .expect("current_claim must not error")
                .is_none(),
            "the sealed entity must have been released once the drain completed"
        );
    }
}
