use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::debug;

use crate::ownership::{
    ClaimError, ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    SharedNodeIdentity,
};

use super::persistence_codec::{
    detached_to_persisted, parse_xml_to_persisted_unacked, persisted_to_detached,
};
use super::{DetachedSession, SmRegistryError, DEFAULT_MAX_SESSIONS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PendingClaimAcquisitionDisposition {
    ReleaseRejectedEnable,
    RetainDetachedSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetachClaimFenceReservation {
    Owned,
    BorrowedRejectedEnable,
}

impl DetachClaimFenceReservation {
    pub(super) fn cancel_if_owned(self, registry: &InMemorySmSessionRegistry, stream_id: &str) {
        if self == Self::Owned {
            registry.cancel_claim_fence_reservation(stream_id);
        }
    }
}

const STREAM_LOCK_SHARDS: usize = 256;

/// Operation-owned capacity marker for a reclaimed SM claim mutation.
/// Only the operation holding this token may consume, cancel, or defer its
/// reservation, so an older same-stream lifecycle cannot erase a newer
/// ownership CAS's ambiguity marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReclaimedClaimReservation(u64);

impl ReclaimedClaimReservation {
    /// Construct a deterministic token for adapters that model reservation
    /// ownership in tests. Production tokens are issued by the registry.
    #[doc(hidden)]
    pub const fn from_generation(generation: u64) -> Self {
        Self(generation)
    }
}

/// Count distinct bounded ownership responsibilities across the three exact
/// fence inventories. A reservation paired with an active, non-terminal
/// fence is only an ambiguity marker for that same responsibility; counting
/// both would reject otherwise usable session capacity. A reservation next
/// to a pending-release fence remains distinct because that release may have
/// committed and a subsequent acquisition may mint a new generation.
fn occupied_claim_fence_capacity(
    reservations: &HashSet<String>,
    reclaimed_reservations: &HashMap<String, ReclaimedClaimReservation>,
    pending: &HashSet<(String, super::super::persistence::SmClaimFence)>,
    fences: &HashMap<String, super::super::persistence::SmClaimFence>,
) -> usize {
    let current_not_pending = fences
        .iter()
        .filter(|(id, fence)| !pending.contains(&(id.to_string(), (*fence).clone())))
        .count();
    let unrepresented_reservations = reservations
        .iter()
        .chain(reclaimed_reservations.keys())
        .filter(|id| {
            fences
                .get(*id)
                .is_none_or(|fence| pending.contains(&((*id).clone(), fence.clone())))
        })
        .count();

    pending
        .len()
        .saturating_add(current_not_pending)
        .saturating_add(unrepresented_reservations)
}

/// Bound on any `ClaimStore` acquire/`ensure_claimed` call made while this
/// registry holds one of its [`STREAM_LOCK_SHARDS`] stream-shard locks (FIX
/// 5, council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda:
/// `claim_session`, `claims.rs::acquire_claim_store_entry_for_detach`, and
/// [`InMemorySmSessionRegistry::hydrate_reclaimed`] below).
///
/// **Shard-fan-in rationale**: `stream_lock` hashes a stream id down to one
/// of a fixed, small number of shard mutexes — many unrelated stream ids
/// share the same shard. A hung `ClaimStore` call while holding one shard's
/// lock therefore does not just stall the one stream id it was issued for;
/// it stalls every OTHER live stream id that happens to hash to the same
/// shard too (store/take/claim/release, all of which take the same shard
/// lock before touching `sessions`/`claimed_sessions`). This is a strictly
/// wider blast radius than a genuinely per-entity lock would have, which is
/// why every `ClaimStore` call issued under a shard lock is bounded here —
/// mirrors `self_fence.rs::expire_bounded`'s bounded/best-effort/logged
/// pattern one level down (a per-entity claim call instead of a per-node
/// lease call).
pub(super) const CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// In-memory implementation of the SM session registry, optionally
/// backed by a [`SmPersistenceStorage`] so detached sessions survive
/// process restarts (issue #209 slice (d) phase 3, locked Q8 = B).
///
/// When `persistence` is `Some`, every `store_session` /
/// `take_session` / `cleanup_expired` mutation also writes to the
/// durable backend; on startup, [`Self::restore_from_persistence`]
/// rebuilds the in-memory view so an XEP-0198 `<resume previd='…'/>`
/// finds sessions that detached before the most recent restart.
///
/// Custom Debug skips the persistence handle (the
/// [`SmPersistenceStorage`] trait does not require `Debug`) and the
/// claim store (`dyn ClaimStore` does not require `Debug` either).
pub struct InMemorySmSessionRegistry {
    pub(super) sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) stream_locks: Vec<Arc<tokio::sync::Mutex<()>>>,
    pub(super) max_sessions: usize,
    /// Recently applied XEP-0424/0425 tombstones, kept for the
    /// promotion-time re-check (round-2 review R2). Bounded by
    /// [`super::tombstones::RECENT_TOMBSTONE_TTL`] +
    /// [`super::tombstones::MAX_RECENT_TOMBSTONES`].
    pub(super) recent_tombstones: RwLock<Vec<super::tombstones::RecentTombstone>>,
    /// Optional durable backing store. When `None` the registry is
    /// strictly in-memory (legacy behaviour); production wiring sets
    /// this via [`Self::with_persistence`] before Arc-wrapping.
    pub(super) persistence:
        Option<std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>>,
    /// The entity-ownership authority for this registry's SM-session claims
    /// (ADR-0017 Phase 3 Slice 1, Q2 "retrofit, not wrap"). Defaults to
    /// [`InProcessClaimStore`] — correct for every build today, since no
    /// caller yet constructs this registry with `clustering.enabled`; a
    /// later slice injects a Postgres-backed store via
    /// [`Self::with_claim_store`] once `SmPersistenceStorage` itself
    /// becomes claim-scoped (Slice 4+).
    ///
    /// This is the **authority** on whether a claim is granted
    /// (`claims.rs`'s `claim_session` gates its own outcome on
    /// [`ClaimStore::acquire`]'s result) and on when a claim ends
    /// (`release_claim`, every terminal branch of `complete_claim`/
    /// `complete_claim_if_resumable`, and `invalidate_sessions_for_jid`'s
    /// removal of a claimed session all call back into it). `stream_locks`/
    /// `sessions`/`claimed_sessions` remain exactly the in-process
    /// contention optimization and session-*state* holders the ADR names
    /// for `StreamLockMap` (element 4) — never a second source of
    /// ownership truth alongside this store, which is precisely the
    /// *wrap* design Q2 rejected.
    pub(super) claim_store: Arc<dyn ClaimStore>,
    /// This node's identity, as presented to `claim_store`. Single-node
    /// deployments use a [`SharedNodeIdentity`] wrapping
    /// [`NodeIdentity::local`]; [`Self::with_claim_store`] (ADR-0017 Phase 3
    /// Slice 5) instead wires in the SAME live, updatable handle
    /// `self_fence::run_node_lease` refreshes on every re-registration
    /// (mirroring `PostgresFencedSmPersistence`'s identical Slice 4
    /// follow-up plumbing fix). New acquisitions read `.current()` once;
    /// owned work then carries that immutable owner together with its epoch,
    /// so a later self-fence cannot silently rebind an old claim to the new
    /// node incarnation.
    pub(super) node_identity: SharedNodeIdentity,
    /// Tracks the immutable owner+epoch fence this registry last observed for each currently
    /// claimed SM-session entity, so `release_claim`/`complete_claim` can
    /// hand the right epoch back to `claim_store.release`. Purely local
    /// bookkeeping — the `ClaimStore` implementation itself is the
    /// authority on what epoch is actually current.
    pub(super) claim_fences: RwLock<HashMap<String, super::super::persistence::SmClaimFence>>,
    /// Exact terminal releases whose backend outcome was not confirmed.
    /// Separate from `claim_fences`: a session drained for promotion is absent
    /// from both maps but is not releasable until its durable delete commits.
    pub(super) pending_claim_releases:
        RwLock<HashSet<(String, super::super::persistence::SmClaimFence)>>,
    /// Acquisitions whose timeout made commit status ambiguous. The typed
    /// disposition distinguishes rejected enable admission (recover then
    /// release) from detach after durable snapshot publication (recover and
    /// retain ownership).
    pub(super) pending_claim_acquisitions:
        RwLock<HashSet<(String, NodeIdentity, PendingClaimAcquisitionDisposition)>>,
    /// Sessions removed from the resumable maps and handed to the XEP-0198
    /// promote-then-confirm lifecycle. Their exact claim must remain held
    /// across displacement, expiry, shutdown, invalidation, retry
    /// reinsertion, and caller cancellation until durable deletion is
    /// confirmed.
    pub(super) pending_promotions: RwLock<HashSet<String>>,
    /// Full payloads handed back by cancellation guards. They remain outside
    /// the resumable map until `drain_expired` reconciles them against the
    /// durable row, preventing stale pre-tombstone queues from being
    /// republished directly from `Drop`.
    pub(super) pending_promotion_retries: RwLock<HashMap<String, DetachedSession>>,
    /// Claimed sessions whose follow-up epoch lookup failed before the
    /// route could prove that the recorded exact fence still owns the backend
    /// row. Kept out of `sessions` until a read-only reconciliation proves
    /// the same owner+epoch or terminalizes the stale local lifecycle.
    pub(super) pending_epoch_failure_reconciliations: RwLock<HashSet<String>>,
    /// Exact reclaimed-session hydration work that has not yet reached a
    /// terminal outcome. This registry-owned inventory is the common safety
    /// net for both the supervised orphan reaper and the one-shot inline
    /// self-fence path: once a node wins a claim, a transient durable read or
    /// an identity rotation must not leave that live-owned claim invisible to
    /// every future orphan scan.
    pub(super) pending_reclaimed_hydrations: RwLock<
        HashMap<
            (
                String,
                super::super::persistence::SmClaimFence,
                ReclaimedClaimReservation,
            ),
            Entity,
        >,
    >,
    /// Ownership-changing calls whose timeout made the committed result
    /// unknown before an epoch could be returned. The attempted owner is
    /// enough to reconcile them without replaying a one-shot CAS: a later
    /// `current_claim` either supplies the exact epoch now owned by that
    /// incarnation or proves that this attempt did not remain authoritative.
    pub(super) pending_reclaimed_claim_lookups:
        RwLock<HashMap<(String, NodeIdentity, ReclaimedClaimReservation), Entity>>,
    /// Capacity reserved before an acquisition whose exact epoch is not yet
    /// known. A reservation survives an ambiguous timeout and is consumed
    /// only when reconciliation either records the resulting fence or proves
    /// that this node did not acquire the claim.
    pub(super) claim_fence_reservations: RwLock<HashSet<String>>,
    pub(super) reclaimed_claim_reservations: RwLock<HashMap<String, ReclaimedClaimReservation>>,
    next_reclaimed_claim_reservation: AtomicU64,
    /// ADR-0017 Phase 3 Slice 6: the cross-node "ask the live owner to
    /// detach" bridge for the XEP-0198 resume path's live-handshake branch.
    /// `None` for single-node/non-clustering deployments (the cross-node
    /// resume fallback then never has anything to ask — see
    /// `cross_node_resume::attempt_cross_node_resume`'s doc comment).
    /// Production wiring injects a `waddle-server`-side adapter over
    /// `RelayHandle` via [`Self::with_remote_resume_asker`].
    pub(super) remote_resume: Option<Arc<dyn super::cross_node_resume::RemoteResumeAsker>>,
}

impl Default for InMemorySmSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemorySmSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemorySmSessionRegistry")
            .field("max_sessions", &self.max_sessions)
            .field(
                "session_count",
                &self.sessions.read().map(|s| s.len()).unwrap_or(0),
            )
            .field(
                "claimed_count",
                &self.claimed_sessions.read().map(|s| s.len()).unwrap_or(0),
            )
            .field("stream_lock_shards", &self.stream_locks.len())
            .field("persistence_attached", &self.persistence.is_some())
            .field("node_identity", &self.node_identity.current())
            .finish()
    }
}

impl InMemorySmSessionRegistry {
    /// Reserve bounded local responsibility before an external caller
    /// performs an ownership-changing CAS for a reclaimed SM session.
    /// The reservation is consumed when the exact returned fence is
    /// published, or cancelled when the CAS is known not to have won.
    pub fn reserve_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
    ) -> Option<ReclaimedClaimReservation> {
        if entity.entity_type != EntityType::SmSession {
            return None;
        }
        self.reserve_reclaimed_claim_fence_capacity(&entity.id)
    }

    pub fn cancel_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
        reservation: ReclaimedClaimReservation,
    ) {
        self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
    }

    /// Retain a timed-out ownership mutation without replaying it. The
    /// registry later uses a read-only `current_claim` to discover the exact
    /// epoch iff this attempted owner actually won.
    pub fn defer_uncertain_reclaimed_claim(
        &self,
        entity: &Entity,
        owner: &NodeIdentity,
        reservation: ReclaimedClaimReservation,
    ) {
        if !self.has_reclaimed_claim_fence_reservation(&entity.id, reservation) {
            return;
        }
        if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
            pending.insert(
                (entity.id.clone(), owner.clone(), reservation),
                entity.clone(),
            );
        }
    }

    /// Convert matching reclaimed/active responsibility into terminal exact
    /// release for the cross-node repair path. The caller must hold this
    /// stream's shard through this conversion and local-lifecycle removal.
    pub(super) fn transfer_reclaimed_claim_to_exact_release(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<bool, SmRegistryError> {
        let key = (entity.id.clone(), fence.clone());
        let (
            Ok(sessions),
            Ok(claimed),
            Ok(promotions),
            Ok(reservations),
            Ok(mut reclaimed),
            Ok(mut pending),
            Ok(mut fences),
        ) = (
            self.sessions.read(),
            self.claimed_sessions.read(),
            self.pending_promotions.read(),
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        )
        else {
            return Err(SmRegistryError::Internal(
                "cross-node repair could not inspect exact-release bookkeeping".to_string(),
            ));
        };
        let promotion_pending = promotions.contains(&entity.id);
        let stream_live = sessions.contains_key(&entity.id)
            || claimed.contains_key(&entity.id)
            || promotion_pending;
        let matching_reservation = reclaimed.get(&entity.id) == Some(&reservation);
        let matching_active_fence = fences.get(&entity.id) == Some(fence);
        let matching_pending_release = pending.contains(&key);
        let conflicting_generic_reservation = reservations.contains(&entity.id);
        let conflicting_reservation = reclaimed
            .get(&entity.id)
            .is_some_and(|current| current != &reservation);
        let conflicting_active_fence = fences
            .get(&entity.id)
            .is_some_and(|current| current != fence);
        let pending_only =
            matching_pending_release && !matching_reservation && !matching_active_fence;
        if promotion_pending
            || conflicting_generic_reservation
            || conflicting_reservation
            || conflicting_active_fence
            || (pending_only && stream_live)
            || (!matching_reservation && !matching_active_fence && !matching_pending_release)
        {
            return Ok(false);
        }
        if matching_reservation {
            reclaimed.remove(&entity.id);
        }
        if matching_active_fence {
            fences.remove(&entity.id);
        }
        pending.insert(key);
        drop(fences);
        drop(pending);
        drop(reclaimed);
        self.clear_pending_reclaimed_hydration(entity, fence, reservation);
        if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
            pending.remove(&(entity.id.clone(), fence.owner().clone(), reservation));
        }
        Ok(true)
    }

    pub(super) fn complete_terminal_claim_release(
        &self,
        stream_id: &str,
        fence: &super::super::persistence::SmClaimFence,
    ) {
        if let Ok(mut pending) = self.pending_claim_releases.write() {
            pending.remove(&(stream_id.to_string(), fence.clone()));
        }
    }

    pub(super) fn reserve_claim_fence_capacity(&self, stream_id: &str) -> bool {
        self.reserve_claim_fence_capacity_up_to(stream_id, self.max_sessions)
    }

    fn reserve_reclaimed_claim_fence_capacity(
        &self,
        stream_id: &str,
    ) -> Option<ReclaimedClaimReservation> {
        let (Ok(reservations), Ok(mut reclaimed), Ok(pending), Ok(fences)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return None;
        };
        if reservations.contains(stream_id) || reclaimed.contains_key(stream_id) {
            return None;
        }
        let occupied = occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
        let active_nonterminal = fences
            .get(stream_id)
            .is_some_and(|fence| !pending.contains(&(stream_id.to_string(), fence.clone())));
        if !active_nonterminal && occupied >= self.max_sessions {
            return None;
        }
        let token = ReclaimedClaimReservation(
            self.next_reclaimed_claim_reservation
                .fetch_add(1, Ordering::Relaxed),
        );
        reclaimed.insert(stream_id.to_string(), token);
        Some(token)
    }

    fn cancel_reclaimed_claim_fence_reservation(
        &self,
        stream_id: &str,
        reservation: ReclaimedClaimReservation,
    ) {
        if let Ok(mut reservations) = self.reclaimed_claim_reservations.write() {
            if reservations.get(stream_id) == Some(&reservation) {
                reservations.remove(stream_id);
            }
        }
    }

    fn has_reclaimed_claim_fence_reservation(
        &self,
        stream_id: &str,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        self.reclaimed_claim_reservations
            .read()
            .is_ok_and(|reservations| reservations.get(stream_id) == Some(&reservation))
    }

    /// Reserve the exact-fence slot needed by a live detach. Capacity
    /// eviction briefly needs both the displaced session's fence (until its
    /// caller confirms promotion) and the replacement session's fence. Keep
    /// one explicitly bounded turnover slot for that transition; subsequent
    /// detaches reject until the displaced responsibility is drained.
    pub(super) fn reserve_detach_claim_fence_capacity(
        &self,
        stream_id: &str,
    ) -> Option<DetachClaimFenceReservation> {
        // A detach can intentionally supersede this stream's timed-out,
        // rejected-enable acquisition. Both paths are serialized by the
        // stream shard before the detach reaches this point, so transferring
        // that already-counted marker is not the unsafe concurrent sharing
        // rejected by the general reservation API.
        let rejected_enable_handoff = self
            .claim_fence_reservations
            .read()
            .is_ok_and(|reservations| reservations.contains(stream_id))
            && self.pending_claim_acquisitions.read().is_ok_and(|pending| {
                let has_rejected_enable = pending.iter().any(|(id, _, disposition)| {
                    id == stream_id
                        && *disposition == PendingClaimAcquisitionDisposition::ReleaseRejectedEnable
                });
                let has_uncertain_detach = pending.iter().any(|(id, _, disposition)| {
                    id == stream_id
                        && *disposition == PendingClaimAcquisitionDisposition::RetainDetachedSession
                });
                has_rejected_enable && !has_uncertain_detach
            });
        if rejected_enable_handoff {
            return Some(DetachClaimFenceReservation::BorrowedRejectedEnable);
        }
        self.reserve_claim_fence_capacity_up_to(stream_id, self.max_sessions.saturating_add(1))
            .then_some(DetachClaimFenceReservation::Owned)
    }

    fn reserve_claim_fence_capacity_up_to(&self, stream_id: &str, capacity: usize) -> bool {
        // Reclaimed hydration and ambiguous-lookup inventories are already
        // represented here: every reclaim reserves before its ownership CAS,
        // then either retains that reservation while the epoch is unknown or
        // consumes it into `claim_fences` once the exact fence is known.
        // `try_record_verified_reclaimed_fence` removes the reservation and
        // inserts that fence while holding all three inventory write locks in
        // one non-awaiting critical section; no transient/cancellation window
        // can leave only `pending_reclaimed_hydrations` behind.
        // Counting those retry maps separately would double-charge the same
        // ownership responsibility and reject usable capacity.
        let (Ok(mut reservations), Ok(reclaimed), Ok(pending), Ok(fences)) = (
            self.claim_fence_reservations.write(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return false;
        };
        // A reservation is an operation-owned ambiguity marker, not an
        // idempotent shared lease. Admitting another same-stream mutation
        // onto it would let the loser cancel the winner's only capacity
        // representation after an external CAS committed ambiguously.
        if reservations.contains(stream_id) || reclaimed.contains_key(stream_id) {
            return false;
        }
        if let Some(fence) = fences.get(stream_id) {
            // A confirmed-current fence makes ensure_claimed idempotent and
            // cannot create another generation. A fence whose terminal
            // release timed out is different: the release may have committed,
            // so the next ensure can mint a new generation and must reserve a
            // second exact-fence slot before touching the backend.
            if !pending.contains(&(stream_id.to_string(), fence.clone())) {
                // Even an idempotent self-ensure can be cancelled before its
                // outcome is observed. Publish an in-flight marker paired
                // with this already-counted fence so demotion can transfer
                // the ambiguity into reservation-backed retry responsibility
                // before removing the confirmed fence.
                reservations.insert(stream_id.to_string());
                return true;
            }
        }
        let occupied = occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
        if occupied >= capacity {
            return false;
        }
        reservations.insert(stream_id.to_string());
        true
    }

    pub(super) fn cancel_claim_fence_reservation(&self, stream_id: &str) {
        if let Ok(mut reservations) = self.claim_fence_reservations.write() {
            reservations.remove(stream_id);
        }
    }

    pub(super) fn has_claim_fence_reservation(&self, stream_id: &str) -> bool {
        self.claim_fence_reservations
            .read()
            .is_ok_and(|reservations| reservations.contains(stream_id))
    }

    #[cfg(test)]
    pub(super) fn claim_fence_capacity_used(&self) -> usize {
        let (Ok(reservations), Ok(reclaimed), Ok(pending), Ok(fences)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return self.max_sessions;
        };
        occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences)
    }

    pub(super) fn try_record_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        let (Ok(mut reservations), Ok(reclaimed), Ok(mut pending), Ok(mut fences)) = (
            self.claim_fence_reservations.write(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        ) else {
            return false;
        };
        if fences.get(stream_id) == Some(&fence) {
            reservations.remove(stream_id);
            return true;
        }
        let reserved = reservations.remove(stream_id);
        let occupied = occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
        if !reserved && occupied >= self.max_sessions {
            return false;
        }
        if let Some(previous) = fences.insert(stream_id.to_string(), fence) {
            pending.insert((stream_id.to_string(), previous));
        }
        true
    }

    /// Convert a reserved acquisition slot into terminal exact-release
    /// responsibility without publishing the supplied fence as authority for
    /// live-session persistence. This is used when a claim belongs to an old
    /// node incarnation while a newer, claimless lifecycle occupies the same
    /// stream id.
    pub(super) fn try_record_terminal_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        let (Ok(mut reservations), Ok(reclaimed), Ok(mut pending), Ok(mut fences)) = (
            self.claim_fence_reservations.write(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        ) else {
            return false;
        };
        if pending.contains(&(stream_id.to_string(), fence.clone())) {
            reservations.remove(stream_id);
            if fences.get(stream_id) == Some(&fence) {
                fences.remove(stream_id);
            }
            return true;
        }
        let reserved = reservations.remove(stream_id);
        let converts_active = fences.get(stream_id) == Some(&fence);
        let occupied = occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
        if !reserved && !converts_active && occupied >= self.max_sessions {
            return false;
        }
        if converts_active {
            fences.remove(stream_id);
        }
        pending.insert((stream_id.to_string(), fence));
        true
    }

    /// Convert a generic commit-unknown acquisition reservation into the
    /// exact terminal fence returned by a later successful `ensure_claimed`.
    /// That read/write result is authoritative and therefore directionally
    /// supersedes every older same-stream fence; retaining an older active
    /// fence after the local lifecycle disappeared would leak bounded
    /// ownership capacity forever.
    pub(super) fn try_record_verified_terminal_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        self.try_record_verified_acquisition_fence(stream_id, fence, true)
    }

    /// Publish a later verified acquisition as the active fence while
    /// directionally retiring every older same-stream generation. Used when
    /// an in-flight displacement still owns the promote/confirm lifecycle.
    pub(super) fn try_record_verified_claim_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        self.try_record_verified_acquisition_fence(stream_id, fence, false)
    }

    fn try_record_verified_acquisition_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        terminal: bool,
    ) -> bool {
        let mut superseded = Vec::new();
        {
            let (Ok(mut reservations), Ok(reclaimed), Ok(mut pending), Ok(mut fences)) = (
                self.claim_fence_reservations.write(),
                self.reclaimed_claim_reservations.read(),
                self.pending_claim_releases.write(),
                self.claim_fences.write(),
            ) else {
                return false;
            };
            if reclaimed.contains_key(stream_id) || !reservations.remove(stream_id) {
                return false;
            }
            if let Some(old) = fences.remove(stream_id) {
                if old != fence {
                    superseded.push(old);
                }
            }
            pending.retain(|(id, old)| {
                if id == stream_id {
                    if old != &fence {
                        superseded.push(old.clone());
                    }
                    false
                } else {
                    true
                }
            });
            if terminal {
                pending.insert((stream_id.to_string(), fence.clone()));
            } else {
                fences.insert(stream_id.to_string(), fence.clone());
            }
        }
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
            superseded.sort_by(|left, right| {
                left.owner()
                    .node_id
                    .cmp(&right.owner().node_id)
                    .then_with(|| left.owner().node_epoch.cmp(&right.owner().node_epoch))
                    .then_with(|| left.epoch().cmp(&right.epoch()))
            });
            superseded.dedup();
            for old in superseded {
                storage.evict_claim_cache(&session_id, &old);
            }
        }
        true
    }

    fn try_record_terminal_reclaimed_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        let (Ok(_reservations), Ok(mut reclaimed), Ok(mut pending), Ok(mut fences)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.write(),
            self.claim_fences.write(),
        ) else {
            return false;
        };
        if reclaimed.get(stream_id) != Some(&reservation) {
            return false;
        }
        reclaimed.remove(stream_id);
        if fences.get(stream_id) == Some(&fence) {
            fences.remove(stream_id);
        }
        pending.insert((stream_id.to_string(), fence));
        true
    }

    /// Retain exact cleanup while local liveness cannot be read. Unlike a
    /// terminal conversion, this keeps a matching active fence in place: a
    /// poisoned session-map lock is not evidence that the lifecycle ended.
    /// Adding a fence already represented by the active map consumes no new
    /// capacity; an externally reclaimed fence must fit the normal bound.
    pub(super) fn try_record_uncertain_release_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
    ) -> bool {
        let (Ok(reservations), Ok(reclaimed), Ok(mut pending), Ok(fences)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.read(),
            self.pending_claim_releases.write(),
            self.claim_fences.read(),
        ) else {
            return false;
        };
        let key = (stream_id.to_string(), fence.clone());
        if pending.contains(&key) {
            return true;
        }
        let represented_exact = fences.get(stream_id) == Some(&fence);
        let represented_other = fences.contains_key(stream_id)
            || pending.iter().any(|(id, _)| id == stream_id)
            || reservations.contains(stream_id)
            || reclaimed.contains_key(stream_id);
        if represented_other && !represented_exact {
            // Direction cannot be inferred from numeric epochs across node
            // incarnations. Only the verified-hydration path may replace a
            // same-stream generation.
            return false;
        }
        let occupied = occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
        if !represented_exact && occupied >= self.max_sessions {
            return false;
        }
        pending.insert(key);
        true
    }

    /// Publish a reclaimed fence only after `ensure_claimed` proved that it
    /// is the backend's current owner+epoch. That proof makes replacement of
    /// every older same-stream generation directional and lets the new fence
    /// consume an existing reservation without growing bounded inventory.
    pub(super) fn try_record_verified_reclaimed_fence(
        &self,
        stream_id: &str,
        fence: super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> bool {
        let mut superseded = Vec::new();
        let recorded = {
            let (Ok(reservations), Ok(mut reclaimed), Ok(mut pending), Ok(mut fences)) = (
                self.claim_fence_reservations.read(),
                self.reclaimed_claim_reservations.write(),
                self.pending_claim_releases.write(),
                self.claim_fences.write(),
            ) else {
                return false;
            };
            let reserved = reclaimed.get(stream_id) == Some(&reservation);
            if reserved {
                reclaimed.remove(stream_id);
            }
            let represented = reserved
                || fences.contains_key(stream_id)
                || pending.iter().any(|(id, _)| id == stream_id);
            let occupied =
                occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
            if !represented && occupied >= self.max_sessions {
                return false;
            }
            if let Some(old) = fences.remove(stream_id) {
                if old != fence {
                    superseded.push(old);
                }
            }
            pending.retain(|(id, old)| {
                if id == stream_id {
                    if old != &fence {
                        superseded.push(old.clone());
                    }
                    false
                } else {
                    true
                }
            });
            fences.insert(stream_id.to_string(), fence.clone());
            true
        };
        if recorded {
            if let Ok(mut acquisitions) = self.pending_claim_acquisitions.write() {
                acquisitions.retain(|(id, _, _)| id != stream_id);
            }
            if let Some(storage) = &self.persistence {
                let session_id = crate::pending_delivery::SmSessionId::new(stream_id.to_string());
                superseded.sort_by(|left, right| {
                    left.owner()
                        .node_id
                        .cmp(&right.owner().node_id)
                        .then_with(|| left.owner().node_epoch.cmp(&right.owner().node_epoch))
                        .then_with(|| left.epoch().cmp(&right.epoch()))
                });
                superseded.dedup();
                for old in superseded {
                    storage.evict_claim_cache(&session_id, &old);
                }
            }
        }
        recorded
    }

    /// Create a new in-memory registry with default settings.
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_fences: RwLock::new(HashMap::new()),
            pending_claim_releases: RwLock::new(HashSet::new()),
            pending_claim_acquisitions: RwLock::new(HashSet::new()),
            pending_promotions: RwLock::new(HashSet::new()),
            pending_promotion_retries: RwLock::new(HashMap::new()),
            pending_epoch_failure_reconciliations: RwLock::new(HashSet::new()),
            pending_reclaimed_hydrations: RwLock::new(HashMap::new()),
            pending_reclaimed_claim_lookups: RwLock::new(HashMap::new()),
            claim_fence_reservations: RwLock::new(HashSet::new()),
            reclaimed_claim_reservations: RwLock::new(HashMap::new()),
            next_reclaimed_claim_reservation: AtomicU64::new(1),
            remote_resume: None,
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_fences: RwLock::new(HashMap::new()),
            pending_claim_releases: RwLock::new(HashSet::new()),
            pending_claim_acquisitions: RwLock::new(HashSet::new()),
            pending_promotions: RwLock::new(HashSet::new()),
            pending_promotion_retries: RwLock::new(HashMap::new()),
            pending_epoch_failure_reconciliations: RwLock::new(HashSet::new()),
            pending_reclaimed_hydrations: RwLock::new(HashMap::new()),
            pending_reclaimed_claim_lookups: RwLock::new(HashMap::new()),
            claim_fence_reservations: RwLock::new(HashSet::new()),
            reclaimed_claim_reservations: RwLock::new(HashMap::new()),
            next_reclaimed_claim_reservation: AtomicU64::new(1),
            remote_resume: None,
        }
    }

    /// Attach a durable backing store. Must be called once at
    /// construction time before the registry is wrapped in `Arc`.
    /// Subsequent mutating writes are mirrored into `storage`; reads
    /// stay in-memory for hot-path latency.
    pub fn with_persistence(
        mut self,
        storage: std::sync::Arc<dyn super::super::persistence::SmPersistenceStorage>,
    ) -> Self {
        self.persistence = Some(storage);
        self
    }

    /// Store a detached session with the typed principal reference that must
    /// authorize its eventual resume. The reference remains operation-local
    /// until the persistence seam consumes it in the same atomic write as the
    /// snapshot and unacked queue.
    pub async fn store_session_with_principal(
        &self,
        session: DetachedSession,
        principal: crate::auth::AuthenticatedPrincipalRef,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        if self.persistence.is_none() {
            return Err(SmRegistryError::Internal(
                "durable SM principal persistence requires an attached storage backend".to_string(),
            ));
        }
        self.store_session_with_principal_inner(session, Some(&principal))
            .await
    }

    /// Read the durable principal paired with a detached SM session. This is
    /// deliberately a storage read; it never reconstructs authority from
    /// local session state.
    pub async fn session_principal(
        &self,
        stream_id: &str,
    ) -> Result<Option<crate::auth::AuthenticatedPrincipalRef>, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(None);
        };
        storage
            .get_session_principal(&crate::pending_delivery::SmSessionId::new(
                stream_id.to_string(),
            ))
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    /// Inject a `ClaimStore`/live-identity pair other than the single-node
    /// [`InProcessClaimStore`] default (ADR-0017 Phase 3, Q2). Must be
    /// called once at construction time before the registry is wrapped in
    /// `Arc`. ADR-0017 Phase 3 Slice 5 wires this in production
    /// (`server/http.rs::create_sm_session_registry`) with
    /// `ClusteringHandles::claim_pair()`'s pair — the *same* `SharedNodeIdentity`
    /// `self_fence::run_node_lease` updates on every re-registration, not a
    /// one-time snapshot, so this registry's claim calls always bind
    /// whatever identity is currently in force.
    pub fn with_claim_store(
        mut self,
        claim_store: Arc<dyn ClaimStore>,
        me: SharedNodeIdentity,
    ) -> Self {
        self.claim_store = claim_store;
        self.node_identity = me;
        self
    }

    /// Inject the cross-node "ask the live owner to detach" bridge
    /// (ADR-0017 Phase 3 Slice 6). Must be called once at construction time
    /// before the registry is wrapped in `Arc`, exactly like
    /// [`Self::with_claim_store`]. Production wiring
    /// (`server/http.rs::create_sm_session_registry`) sets this alongside
    /// the claim store whenever clustering is enabled; single-node builds
    /// leave it `None`, so `cross_node_resume::attempt_cross_node_resume`'s
    /// live-handshake branch never has anything to ask (byte-identical
    /// single-node behavior).
    pub fn with_remote_resume_asker(
        mut self,
        asker: Arc<dyn super::cross_node_resume::RemoteResumeAsker>,
    ) -> Self {
        self.remote_resume = Some(asker);
        self
    }

    /// Rebuild the in-memory view from the attached durable store.
    /// Called on server startup before any traffic is accepted, so
    /// an XEP-0198 `<resume previd='…'/>` for a session that
    /// detached before restart still succeeds.
    ///
    /// **Startup-time operation only (FIX 2, council-adjudicated ADR-0017
    /// Phase 3 Slice 5 corrigenda)**: this method's unfenced, unscoped
    /// `list_all_sessions_with_unacked` table scan is safe only because
    /// nothing else can plausibly be racing it for a stream id it has not
    /// yet reached — this runs once, before any traffic is accepted. It
    /// MUST NOT be re-run against a live, already-serving registry (the
    /// orphan reaper previously re-ran it after every successful steal,
    /// which re-scans every row this node already holds on every sweep and
    /// — worse — can observe a row a live session concurrently
    /// completes/re-claims mid-scan). [`Self::hydrate_reclaimed`] is the
    /// live-safe alternative for exactly that case: given the specific
    /// entities a caller just proved ownership of (via `steal_stale` or an
    /// equivalent CAS), it hydrates only those, under each one's own
    /// stream-shard lock, with a fresh in-memory absence re-check — never a
    /// table scan, never a blind insert.
    ///
    /// **ADR-0017 Phase 3 Slice 5 — acquire-then-hydrate** (element 9,
    /// quoted verbatim: *"hydrates only sessions whose claim this node
    /// holds or can acquire at startup ... it never performs unscoped
    /// full-table hydration"*): the read below (`list_all_sessions_with_unacked`)
    /// is still a full, unfenced table scan — it has to be, there is no
    /// other way to discover which stream ids exist — but every row is now
    /// gated on a per-entity [`ClaimStore::ensure_claimed`] call before it
    /// is allowed into `self.sessions`. A row this node successfully claims
    /// (a fresh claim on a single-node/first-ever-restore deployment, or a
    /// self-reacquire of this exact node's own pre-restart claim once
    /// `ensure_claimed`'s self-match fires under the *same* `node_id` — see
    /// that method's doc comment) is hydrated; a row genuinely claimed by
    /// a different, still-live node is skipped — that node already has it
    /// in memory (or will, on its own restore pass), and this node MUST NOT
    /// also hydrate a copy (the exact double-ownership hazard this slice
    /// closes). A row whose owner has died is left unclaimed here (a
    /// concurrent restore/steal never matches this node's identity, so it
    /// stays `AlreadyClaimed` against the dead owner until that owner's
    /// `clustering_nodes` row is provably stale) — the **orphan reaper**
    /// (`server::session_janitors::spawn_orphan_reaper_janitor`) is the
    /// mechanism that reclaims those, not this startup pass, since a
    /// dead-owner determination requires the owner-stale predicate this
    /// unfenced per-row read does not evaluate.
    ///
    /// **Restart-time expired-row deletion (element 9/element 4)**: this
    /// slice does *not* add an unscoped delete-on-restore step. Code
    /// research for this slice found no existing unscoped delete to
    /// claim-scope here — issue #1098 deliberately *hydrates* expired
    /// sessions rather than deleting them at restore time, specifically so
    /// their unacked queues still run the Q6 promote → confirm chain
    /// instead of being silently discarded. Deleting a claimed session
    /// eagerly here, before that chain runs, would re-introduce exactly
    /// the data-loss bug #1098 fixed. Once a row is hydrated under this
    /// node's claim, the (now itself claim-scoped, see
    /// `server::session_janitors::spawn_sm_expiry_janitor`) SM-expiry
    /// janitor's `drain_expired`/promote/`confirm_drained` chain is the
    /// sole deletion path, and its writes already run under the row-locked
    /// fenced epoch via `PostgresFencedSmPersistence`. Recorded as
    /// deviation 28 (plan doc; corrected from an earlier "deviation 27"
    /// citation — see the plan's Slice 5 "Design addition (major fix 6)"
    /// paragraph, amended in place to point at 28) — the plan's
    /// major-fix-6 premise of an existing unscoped restore-time delete
    /// does not match this codebase's actual state.
    ///
    /// **Per-row stream-shard-lock discipline (FIX 2)**: each row's
    /// eventual in-memory insert takes that row's own stream-shard lock —
    /// the same lock every other registry mutator (`store_session`,
    /// `take_session`, `claim_session`, …) takes before touching
    /// `sessions`/`claimed_sessions` — and re-checks the stream id is
    /// absent from BOTH maps immediately before inserting. This is cheap
    /// safety for this method's startup-time role (see above): at true
    /// cold start nothing else can have raced ahead, but the same
    /// discipline the live-only [`Self::hydrate_reclaimed`] needs is applied
    /// here too rather than special-cased away, so a row this node's own
    /// Slice-4 lazy first-fenced-write path (or a live detach) already
    /// raced into memory ahead of this scan reaching the same row is
    /// skipped rather than overwritten with a stale durable read.
    ///
    /// Returns the number of sessions hydrated. No-op when no
    /// persistence is attached.
    pub async fn restore_from_persistence(&self) -> Result<usize, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        let now = chrono::Utc::now();
        // Single round-trip — replaces an N+1 (1 list_all_sessions +
        // N list_unacked) with a single SELECT … LEFT JOIN sm_unacked
        // on backends that override (libSQL/Postgres). In-memory
        // backends fall back to the trait-default N+1 path. Issue
        // #209 PR #405. This read is unfenced/unscoped by necessity (see
        // this method's doc comment) — the per-row `ensure_claimed` call
        // below is what scopes which rows this node is actually allowed to
        // hydrate.
        let stored = storage
            .list_all_sessions_with_unacked()
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        let mut hydrated = 0usize;
        let mut expired = 0usize;
        let mut bad_rows = 0usize;
        let mut foreign_claims = 0usize;
        let mut already_present = 0usize;
        // Read once per call, not once per row: `restore_from_persistence`
        // only ever runs at startup, well before this node could have
        // self-fenced and re-registered under a fresh identity, but reading
        // through `.current()` here (rather than caching a snapshot for the
        // whole call) keeps this consistent with every other call site's
        // discipline of never holding a stale identity across an `.await`.
        for (persisted, unacked) in stored {
            let identity = self.node_identity.current();
            let entity = Entity::new(
                EntityType::SmSession,
                persisted.stream_id.as_str().to_string(),
            );
            let epoch = match self.claim_store.ensure_claimed(&entity, &identity).await {
                Ok(epoch) => epoch,
                Err(crate::ownership::ClaimError::AlreadyClaimed) => {
                    // Another (live) node already holds this entity's
                    // claim — never hydrate a second in-memory copy. The
                    // orphan reaper, not this pass, is what reclaims a row
                    // whose owner has actually died.
                    foreign_claims += 1;
                    continue;
                }
                Err(error) => {
                    // A transient backend failure: skip this row rather
                    // than failing the whole restore pass. It is retried
                    // on this node's next restart, or reclaimed by the
                    // orphan reaper if its owner (this node, under a
                    // now-superseded identity) is later found stale.
                    debug!(
                        stream_id = %persisted.stream_id,
                        %error,
                        "restore_from_persistence: ClaimStore ensure_claimed failed; \
                         skipping this row for this pass"
                    );
                    continue;
                }
            };
            // Expired-during-downtime sessions (detached_at +
            // max_resume_duration <= now) are hydrated too (issue
            // #1098): deleting their rows here would silently discard
            // their unacked queues, violating XEP-0198 §5 ("treat
            // unacknowledged stanzas … like stanzas to an unavailable
            // resource"). They are not resumable on the wire —
            // peek/take/claim all gate on `is_expired()` — and the
            // SM-expiry janitor's next `drain_expired` pass runs the
            // promote → confirm chain, which is what finally deletes
            // the durable rows via `confirm_drained`.
            let expires_at = persisted.detached_at
                + chrono::Duration::from_std(persisted.max_resume_duration)
                    .unwrap_or(chrono::Duration::seconds(0));
            if expires_at <= now {
                expired += 1;
            }
            let session = match persisted_to_detached(&persisted, &unacked) {
                Ok(session) => session,
                Err(error) => {
                    debug!(
                        stream_id = %persisted.stream_id,
                        error = %error,
                        "skipping persisted session: row decode failed (poison pill)"
                    );
                    // Claimed above but never hydrated (a genuine
                    // poison-pill row, not a claim conflict) — release the
                    // now-unused claim rather than leak it, so a future
                    // pass (or the orphan reaper, once this identity is
                    // stale) can act on the row again.
                    self.release_claim_store_entry_under(
                        persisted.stream_id.as_str(),
                        super::super::persistence::SmClaimFence::new(identity.clone(), epoch),
                    )
                    .await;
                    bad_rows += 1;
                    continue;
                }
            };
            if self.node_identity.current() != identity {
                self.release_claim_store_entry_under(
                    persisted.stream_id.as_str(),
                    super::super::persistence::SmClaimFence::new(identity, epoch),
                )
                .await;
                continue;
            }
            // FIX 2: per-row stream-shard-lock discipline (see this
            // method's doc comment) — take this row's own shard lock and
            // re-check both maps immediately before inserting, rather than
            // batching every hydrated row into one insert pass after the
            // loop (the previous shape, which held no lock at all across
            // the whole scan).
            let stream_id = session.stream_id.clone();
            let stream_lock = self.stream_lock(&stream_id)?;
            let _stream_guard = stream_lock.lock().await;
            let present = {
                let sessions = self
                    .sessions
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                let claimed = self
                    .claimed_sessions
                    .read()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                sessions.contains_key(&stream_id) || claimed.contains_key(&stream_id)
            };
            if present {
                already_present += 1;
                continue;
            }
            {
                let mut sessions = self
                    .sessions
                    .write()
                    .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
                sessions.insert(stream_id.clone(), session);
            }
            if let Ok(mut claim_fences) = self.claim_fences.write() {
                claim_fences.insert(
                    stream_id,
                    super::super::persistence::SmClaimFence::new(identity.clone(), epoch),
                );
            }
            hydrated += 1;
        }
        debug!(
            hydrated,
            expired,
            bad_rows,
            foreign_claims,
            already_present,
            "restored detached SM sessions from persistence"
        );
        Ok(hydrated)
    }

    /// Targeted hydration for freshly-reclaimed SM-session claims (FIX 2,
    /// council-adjudicated ADR-0017 Phase 3 Slice 5 corrigenda) — the
    /// live-safe counterpart to [`Self::restore_from_persistence`] (a
    /// startup-time-only, whole-table operation; see its doc comment).
    /// Callers: the orphan reaper janitor, after a successful
    /// `steal_stale(OwnerStale)` for one or more entities
    /// (`server::session_janitors::run_orphan_reaper_sweep`), and the
    /// inline post-fence reclaim in `self_fence::run_node_lease` (FIX 4),
    /// after this node's own just-superseded identity's claims are stolen
    /// back under the freshly re-registered identity. Neither caller may
    /// re-run `restore_from_persistence` — the server is already serving
    /// live traffic, and an unscoped table scan racing a live session that
    /// completes/re-claims mid-scan is exactly the **live restore hazard**
    /// this method exists to close.
    ///
    /// Per entity, under that entity's own stream-shard lock (never a
    /// table scan, never a blind insert):
    /// 1. Entities whose type is not `SmSession` are skipped (logged) —
    ///    this registry only ever hydrates SM-session claims.
    /// 2. Re-checks the stream id is absent from BOTH `sessions` and
    ///    `claimed_sessions` — if either already holds it (a live session
    ///    completed, another concurrent hydration already landed it, or
    ///    this entity was reclaimed more than once across overlapping
    ///    sweeps), skip: never overwrite a live in-memory copy with a
    ///    stale durable read.
    /// 3. Re-confirms this node still holds the claim via a bounded
    ///    `ClaimStore::ensure_claimed` self-reacquire (FIX 5 — bounded
    ///    because this call runs under the stream-shard lock; see
    ///    [`CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`]'s doc comment for the
    ///    shard-fan-in rationale) — a defensive re-check rather than
    ///    trusting the caller-supplied epoch blindly, since the caller's
    ///    `steal_stale` may have committed some time before this call
    ///    actually reaches this entity's turn in a batch.
    /// 4. Loads the durable row (`get_session` + `list_unacked`); a
    ///    missing row (already promoted/deleted by a concurrent sweep) is
    ///    a no-op, not an error.
    /// 5. Inserts into `sessions`, recording the epoch `ensure_claimed`
    ///    confirmed in step 3.
    ///
    /// Returns the number of entities actually hydrated — entities skipped
    /// by steps 1-4 are not counted and do not produce an `Err`, mirroring
    /// `restore_from_persistence`'s best-effort, skip-and-continue
    /// semantics for individual rows.
    pub async fn hydrate_reclaimed_typed(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        if !self.try_record_pending_reclaimed_hydration(entity, caller_fence, reservation)? {
            self.clear_pending_reclaimed_hydration(entity, caller_fence, reservation);
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        let stream_lock = self.stream_lock(&entity.id)?;
        let _stream_guard = stream_lock.lock().await;
        self.hydrate_reclaimed_typed_locked(entity, caller_fence, reservation)
            .await
    }

    fn try_record_pending_reclaimed_hydration(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<bool, SmRegistryError> {
        let (reservations, releases, fences, mut hydrations) = (
            self.reclaimed_claim_reservations
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_claim_releases
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.claim_fences
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
            self.pending_reclaimed_hydrations
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?,
        );
        let represented = reservations.get(&entity.id) == Some(&reservation)
            || fences.get(&entity.id) == Some(caller_fence)
            || releases.contains(&(entity.id.clone(), caller_fence.clone()));
        if represented {
            hydrations.insert(
                (entity.id.clone(), caller_fence.clone(), reservation),
                entity.clone(),
            );
        }
        Ok(represented)
    }

    async fn hydrate_reclaimed_typed_locked(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        let pending = self
            .pending_reclaimed_hydrations
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .contains_key(&(entity.id.clone(), caller_fence.clone(), reservation));
        let represented = self
            .reclaimed_claim_reservations
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
            .get(&entity.id)
            == Some(&reservation)
            || self
                .claim_fences
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .get(&entity.id)
                == Some(caller_fence)
            || self
                .pending_claim_releases
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?
                .contains(&(entity.id.clone(), caller_fence.clone()));
        if !pending || !represented {
            self.clear_pending_reclaimed_hydration(entity, caller_fence, reservation);
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }

        let outcome = self
            .hydrate_reclaimed_once(entity, caller_fence, reservation)
            .await?;
        if matches!(
            outcome,
            ReclaimedHydrationOutcome::Hydrated
                | ReclaimedHydrationOutcome::AlreadyPresent
                | ReclaimedHydrationOutcome::LostClaim
        ) {
            self.clear_pending_reclaimed_hydration(entity, caller_fence, reservation);
        }
        if outcome == ReclaimedHydrationOutcome::LostClaim {
            self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
        }
        Ok(outcome)
    }

    fn clear_pending_reclaimed_hydration(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) {
        if let Ok(mut pending) = self.pending_reclaimed_hydrations.write() {
            pending.remove(&(entity.id.clone(), fence.clone(), reservation));
        }
    }

    async fn hydrate_reclaimed_once(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        }
        let stream_id = entity.id.clone();

        let present = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            let promotions = self
                .pending_promotions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.contains_key(&stream_id)
                || claimed.contains_key(&stream_id)
                || promotions.contains(&stream_id)
        };
        if present {
            match tokio::time::timeout(
                CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                self.claim_store
                    .fence(entity, caller_fence.owner(), caller_fence.epoch()),
            )
            .await
            {
                Ok(Ok(true)) if self.node_identity.current() == *caller_fence.owner() => {}
                Ok(Ok(true)) => return Ok(ReclaimedHydrationOutcome::StaleIdentity),
                Ok(Ok(_)) => return Ok(ReclaimedHydrationOutcome::LostClaim),
                Ok(Err(_)) | Err(_) => return Ok(ReclaimedHydrationOutcome::TransientFailure),
            }
            if !self.try_record_verified_reclaimed_fence(
                &stream_id,
                caller_fence.clone(),
                reservation,
            ) {
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
            debug!(
                stream_id = %stream_id,
                "hydrate_reclaimed: already present in-memory (live session, or already \
                 hydrated by an overlapping sweep); skipping"
            );
            return Ok(ReclaimedHydrationOutcome::AlreadyPresent);
        }

        // FIX 5: bounded — see `CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT`'s
        // doc comment. On timeout or a lost self-reacquire, skip this
        // entity for this pass rather than insert a session this node
        // can no longer prove it owns; the entity remains eligible for
        // a future sweep.
        let epoch = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .ensure_claimed(entity, caller_fence.owner()),
        )
        .await
        {
            Ok(Ok(epoch)) => epoch,
            Ok(Err(
                error @ (ClaimError::AlreadyClaimed | ClaimError::Conflict | ClaimError::Draining),
            )) => {
                debug!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: ClaimStore ensure_claimed self-reacquire failed \
                     (claim lost again since the caller's steal_stale); skipping"
                );
                return Ok(ReclaimedHydrationOutcome::LostClaim);
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: ClaimStore ensure_claimed could not verify the \
                     already-won claim; retaining reclaimed responsibility for repair"
                );
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
            Err(_timeout) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "hydrate_reclaimed: ClaimStore ensure_claimed timed out while holding \
                     this stream's shard lock; skipping this entity for this pass"
                );
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
        };
        if epoch != caller_fence.epoch() {
            debug!(
                stream_id = %stream_id,
                expected_epoch = caller_fence.epoch().0,
                actual_epoch = epoch.0,
                "hydrate_reclaimed: caller work belongs to a superseded claim epoch"
            );
            return Ok(ReclaimedHydrationOutcome::LostClaim);
        }
        if !self.try_record_verified_reclaimed_fence(&stream_id, caller_fence.clone(), reservation)
        {
            return Ok(ReclaimedHydrationOutcome::TransientFailure);
        }
        let Some(storage) = &self.persistence else {
            return Ok(ReclaimedHydrationOutcome::MissingDurable);
        };

        let session_id = crate::pending_delivery::SmSessionId::new(stream_id.clone());
        let persisted = match storage.get_session(&session_id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                debug!(
                    stream_id = %stream_id,
                    "hydrate_reclaimed: no durable row (already promoted/deleted by a \
                     concurrent sweep); skipping"
                );
                return Ok(ReclaimedHydrationOutcome::MissingDurable);
            }
            Err(super::super::persistence::SmPersistenceError::Corrupt {
                stream_id: corrupt_stream,
                detail,
            }) if corrupt_stream == session_id => {
                debug!(stream_id = %stream_id, %detail, "hydrate_reclaimed: corrupt durable session row");
                return self
                    .quarantine_reclaimed_poison(
                        storage.as_ref(),
                        entity,
                        caller_fence,
                        &session_id,
                    )
                    .await;
            }
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: get_session failed; skipping this entity for this pass"
                );
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
        };
        let unacked = match storage.list_unacked(&session_id).await {
            Ok(rows) => rows,
            Err(super::super::persistence::SmPersistenceError::Corrupt {
                stream_id: corrupt_stream,
                detail,
            }) if corrupt_stream == session_id => {
                debug!(stream_id = %stream_id, %detail, "hydrate_reclaimed: corrupt durable unacked row");
                return self
                    .quarantine_reclaimed_poison(
                        storage.as_ref(),
                        entity,
                        caller_fence,
                        &session_id,
                    )
                    .await;
            }
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: list_unacked failed; skipping this entity for this pass"
                );
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
        };
        let session = match persisted_to_detached(&persisted, &unacked) {
            Ok(session) => session,
            Err(error) => {
                debug!(
                    stream_id = %stream_id,
                    %error,
                    "hydrate_reclaimed: row decode failed (poison pill); skipping"
                );
                return self
                    .quarantine_reclaimed_poison(
                        storage.as_ref(),
                        entity,
                        caller_fence,
                        &session_id,
                    )
                    .await;
            }
        };
        // Every persistence read above is an await point. Re-prove both the
        // node incarnation and exact claim epoch immediately before the
        // synchronous in-memory publication.
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        }
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .fence(entity, caller_fence.owner(), caller_fence.epoch()),
        )
        .await
        {
            Ok(Ok(true)) if self.node_identity.current() == *caller_fence.owner() => {}
            Ok(Ok(true)) => return Ok(ReclaimedHydrationOutcome::StaleIdentity),
            Ok(Ok(_)) => return Ok(ReclaimedHydrationOutcome::LostClaim),
            Ok(Err(error)) => {
                debug!(stream_id = %stream_id, %error, "hydrate_reclaimed: final exact fence failed");
                return Ok(ReclaimedHydrationOutcome::TransientFailure);
            }
            Err(_) => return Ok(ReclaimedHydrationOutcome::TransientFailure),
        }
        let Some(_identity_guard) = self
            .node_identity
            .guard_if_current(caller_fence.owner())
            .await
        else {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        };
        {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions.insert(stream_id.clone(), session);
        }
        if let Ok(mut claim_fences) = self.claim_fences.write() {
            claim_fences.insert(stream_id, caller_fence.clone());
        }
        Ok(ReclaimedHydrationOutcome::Hydrated)
    }

    async fn quarantine_reclaimed_poison(
        &self,
        storage: &dyn super::super::persistence::SmPersistenceStorage,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        session_id: &crate::pending_delivery::SmSessionId,
    ) -> Result<ReclaimedHydrationOutcome, SmRegistryError> {
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedHydrationOutcome::StaleIdentity);
        }
        match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .fence(entity, caller_fence.owner(), caller_fence.epoch()),
        )
        .await
        {
            Ok(Ok(true)) if self.node_identity.current() == *caller_fence.owner() => {}
            Ok(Ok(true)) => return Ok(ReclaimedHydrationOutcome::StaleIdentity),
            Ok(Ok(_)) => return Ok(ReclaimedHydrationOutcome::LostClaim),
            Ok(Err(_)) | Err(_) => return Ok(ReclaimedHydrationOutcome::TransientFailure),
        }
        // The clustered implementation binds `caller_fence` into the same
        // transaction that removes both durable tables. Thus stale work can
        // neither quarantine a newer epoch nor report terminal success
        // before the poison state is actually gone.
        match storage.quarantine_session(session_id, caller_fence).await {
            Ok(()) => Ok(ReclaimedHydrationOutcome::PoisonReleased),
            Err(super::super::persistence::SmPersistenceError::NotOwner { .. }) => {
                Ok(ReclaimedHydrationOutcome::LostClaim)
            }
            Err(error) => {
                debug!(
                    stream_id = %session_id,
                    %error,
                    "hydrate_reclaimed: poison quarantine failed; retaining exact claim for retry"
                );
                Ok(ReclaimedHydrationOutcome::TransientFailure)
            }
        }
    }

    pub async fn hydrate_reclaimed(
        &self,
        entities: &[(
            Entity,
            super::super::persistence::SmClaimFence,
            ReclaimedClaimReservation,
        )],
    ) -> Result<usize, SmRegistryError> {
        let mut hydrated = 0usize;
        for (entity, fence, reservation) in entities {
            if self
                .hydrate_reclaimed_typed(entity, fence, *reservation)
                .await?
                == ReclaimedHydrationOutcome::Hydrated
            {
                hydrated += 1;
            }
        }
        Ok(hydrated)
    }

    /// Retry bounded reclaimed-session work retained by
    /// [`Self::hydrate_reclaimed_typed`]. A live node's won claim is no
    /// longer discoverable by the orphan scan, so this inventory — not a
    /// future scan — owns retry until hydration succeeds, ownership is
    /// disproved, or terminal cleanup completes.
    pub async fn retry_pending_reclaimed_hydrations(&self, limit: usize) -> usize {
        let lookups = self
            .pending_reclaimed_claim_lookups
            .read()
            .map(|pending| {
                pending
                    .iter()
                    .take(limit)
                    .map(|((_, owner, reservation), entity)| {
                        (entity.clone(), owner.clone(), *reservation)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut attempted = 0;
        for (entity, owner, reservation) in lookups {
            attempted += 1;
            let Ok(stream_lock) = self.stream_lock(&entity.id) else {
                continue;
            };
            let stream_guard = stream_lock.lock().await;
            let lookup_key = (entity.id.clone(), owner.clone(), reservation);
            let still_pending = self
                .pending_reclaimed_claim_lookups
                .read()
                .map(|pending| pending.contains_key(&lookup_key))
                .unwrap_or(false);
            if !still_pending {
                continue;
            }
            let snapshot = match tokio::time::timeout(
                CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                self.claim_store.current_claim(&entity),
            )
            .await
            {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(_)) | Err(_) => continue,
            };
            if let Some(snapshot) = snapshot.filter(|snapshot| snapshot.owner == owner) {
                if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
                    pending.remove(&lookup_key);
                }
                let fence =
                    super::super::persistence::SmClaimFence::new(owner, snapshot.claim_epoch);
                let outcome = if self
                    .try_record_pending_reclaimed_hydration(&entity, &fence, reservation)
                    .unwrap_or(false)
                {
                    self.hydrate_reclaimed_typed_locked(&entity, &fence, reservation)
                        .await
                } else {
                    Ok(ReclaimedHydrationOutcome::LostClaim)
                };
                let terminal = matches!(
                    outcome,
                    Ok(ReclaimedHydrationOutcome::MissingDurable
                        | ReclaimedHydrationOutcome::PoisonReleased
                        | ReclaimedHydrationOutcome::StaleIdentity)
                );
                drop(stream_guard);
                if terminal {
                    let _ = self
                        .release_reclaimed_claim(&entity, &fence, reservation)
                        .await;
                }
            } else {
                if let Ok(mut pending) = self.pending_reclaimed_claim_lookups.write() {
                    pending.remove(&lookup_key);
                }
                self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
            }
        }
        let remaining = limit.saturating_sub(attempted);
        let pending = self
            .pending_reclaimed_hydrations
            .read()
            .map(|pending| {
                pending
                    .iter()
                    .take(remaining)
                    .map(|((_, fence, reservation), entity)| {
                        (entity.clone(), fence.clone(), *reservation)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (entity, fence, reservation) in pending {
            attempted += 1;
            match self
                .hydrate_reclaimed_typed(&entity, &fence, reservation)
                .await
            {
                Ok(
                    ReclaimedHydrationOutcome::MissingDurable
                    | ReclaimedHydrationOutcome::PoisonReleased
                    | ReclaimedHydrationOutcome::StaleIdentity,
                ) => {
                    let _ = self
                        .release_reclaimed_claim(&entity, &fence, reservation)
                        .await;
                }
                Ok(
                    ReclaimedHydrationOutcome::Hydrated
                    | ReclaimedHydrationOutcome::AlreadyPresent
                    | ReclaimedHydrationOutcome::LostClaim
                    | ReclaimedHydrationOutcome::TransientFailure,
                )
                | Err(_) => {}
            }
        }
        attempted
    }

    #[cfg(test)]
    pub(super) fn pending_reclaimed_hydration_count(&self) -> usize {
        self.pending_reclaimed_hydrations
            .read()
            .map_or(0, |pending| pending.len())
    }

    pub async fn release_reclaimed_claim(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<crate::ownership::ExactReleaseOutcome, SmRegistryError> {
        let stream_lock = self.stream_lock(&entity.id)?;
        let _stream_guard = stream_lock.lock().await;
        match self.stream_liveness(&entity.id) {
            Some(true) => {
                // Responsibility transferred back to the live local session.
                // Never let terminal cleanup release its claim.
                self.clear_pending_reclaimed_hydration(entity, fence, reservation);
                self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
                return Ok(crate::ownership::ExactReleaseOutcome::NotOwned);
            }
            None => {
                if !self.try_record_terminal_reclaimed_fence(&entity.id, fence.clone(), reservation)
                    && !self.try_record_uncertain_release_fence(&entity.id, fence.clone())
                {
                    return Err(SmRegistryError::Internal(
                        "release_reclaimed_claim: local liveness is uncertain and exact retry capacity is exhausted".to_string(),
                    ));
                }
                // Retain the exact fence locally as well as reporting a
                // retryable failure. This covers both the supervised worker
                // and one-shot self-fence callers.
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: local session liveness is uncertain; exact cleanup retained".to_string(),
                ));
            }
            Some(false) => {}
        }
        if !self.try_record_terminal_reclaimed_fence(&entity.id, fence.clone(), reservation) {
            if !self.reserve_claim_fence_capacity(&entity.id) {
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: exact-release retry capacity exhausted".to_string(),
                ));
            }
            if !self.try_record_claim_fence(&entity.id, fence.clone()) {
                self.cancel_claim_fence_reservation(&entity.id);
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: failed to retain exact claim fence".to_string(),
                ));
            }
        }
        let outcome = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store
                .release_exact(entity, fence.owner(), fence.epoch()),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => {
                if let Ok(mut pending) = self.pending_claim_releases.write() {
                    pending.insert((entity.id.clone(), fence.clone()));
                }
                return Err(SmRegistryError::Internal(
                    "release_reclaimed_claim: exact release failed and was retained for retry"
                        .to_string(),
                ));
            }
        };
        if let Ok(mut fences) = self.claim_fences.write() {
            if fences.get(&entity.id) == Some(fence) {
                fences.remove(&entity.id);
            }
        }
        if let Ok(mut pending) = self.pending_claim_releases.write() {
            pending.remove(&(entity.id.clone(), fence.clone()));
        }
        if let Some(storage) = &self.persistence {
            let session_id = crate::pending_delivery::SmSessionId::new(entity.id.clone());
            storage.evict_claim_cache(&session_id, fence);
        }
        self.clear_pending_reclaimed_hydration(entity, fence, reservation);
        Ok(outcome)
    }

    /// Resolve an ownership CAS whose result was dropped without ever
    /// hydrating the session locally. This is the terminal self-fence path:
    /// a read-only claim lookup discovers whether `attempted_owner` won, and
    /// an exact release retires only that observed generation. Until that
    /// generation is observed, the CAS may still commit after the lookup, so
    /// every lookup failure or non-matching snapshot keeps the local capacity
    /// reservation and forces the caller to remain self-fenced.
    pub async fn retire_uncertain_reclaimed_claim(
        &self,
        entity: &Entity,
        attempted_owner: &NodeIdentity,
        reservation: ReclaimedClaimReservation,
    ) -> Result<crate::ownership::ExactReleaseOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
            return Ok(crate::ownership::ExactReleaseOutcome::NotOwned);
        }
        let snapshot = match tokio::time::timeout(
            CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                return Err(SmRegistryError::Internal(format!(
                    "retire_uncertain_reclaimed_claim: exact owner lookup failed: {error}"
                )));
            }
            Err(_) => {
                return Err(SmRegistryError::Internal(
                    "retire_uncertain_reclaimed_claim: exact owner lookup timed out".to_string(),
                ));
            }
        };
        let Some(snapshot) = snapshot.filter(|snapshot| snapshot.owner == *attempted_owner) else {
            return Err(SmRegistryError::Internal(
                "retire_uncertain_reclaimed_claim: attempted owner not yet observable; \
                 reservation retained because the ownership CAS may still commit"
                    .to_string(),
            ));
        };
        let fence = super::super::persistence::SmClaimFence::new(
            attempted_owner.clone(),
            snapshot.claim_epoch,
        );
        self.release_reclaimed_claim(entity, &fence, reservation)
            .await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimedHydrationOutcome {
    Hydrated,
    AlreadyPresent,
    MissingDurable,
    LostClaim,
    StaleIdentity,
    TransientFailure,
    PoisonReleased,
}

impl InMemorySmSessionRegistry {
    /// Helper: delete every durable row for `stream_id` (session +
    /// unacked queue). Returns the underlying error so callers can
    /// adopt a "persist-first" ordering — refuse to mutate the
    /// in-memory map when the durable delete failed, so a transient
    /// storage hiccup doesn't leave an orphaned `sm_sessions` row
    /// that `restore_from_persistence` would resurrect on restart.
    /// (Codex P1 + Copilot + Qodo on PR #344: best-effort silent
    /// swallow allowed durable orphans whenever the in-memory state
    /// had already moved on.)
    pub(super) async fn persist_delete_session(
        &self,
        stream_id: &str,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session(&crate::pending_delivery::SmSessionId::new(
                stream_id.to_string(),
            ))
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) async fn persist_delete_session_with_authority(
        &self,
        stream_id: &str,
        authority: &crate::ownership::CurrentNodeIdentityGuard,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session_with_authority(
                &crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
                authority,
            )
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) async fn persist_detached_session_snapshot(
        &self,
        session: &DetachedSession,
        principal: Option<&crate::auth::AuthenticatedPrincipalRef>,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        let persisted = detached_to_persisted(session)?;
        let mut unacked_rows = Vec::with_capacity(session.unacked_stanzas.len());
        for entry in &session.unacked_stanzas {
            unacked_rows.push(parse_xml_to_persisted_unacked(
                &session.stream_id,
                entry.sequence,
                &entry.stanza_xml,
                entry.original_receipt_at,
            )?);
        }
        match principal {
            Some(principal) => {
                storage
                    .store_session_atomic_with_principal(principal, persisted, unacked_rows)
                    .await
            }
            None => storage.store_session_atomic(persisted, unacked_rows).await,
        }
        .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    /// Durably delete the named unacked rows for a stream — exact
    /// `(stream_id, sequence)` matches, idempotent for absent rows.
    ///
    /// Used by the Q6 promotion retry path (round-2 review R4): after
    /// a PARTIAL promotion failure, the successfully promoted stanzas'
    /// `pending_delivery` rows are already committed, so their
    /// `sm_unacked` rows must be erased before the session is
    /// re-inserted for retry — otherwise every janitor tick re-promotes
    /// the whole queue and duplicates the already-queued stanzas.
    /// Ordering is crash-safe: the pending row commits BEFORE its
    /// `sm_unacked` row is deleted here, preserving at-least-once.
    ///
    /// Takes the stream lock so the delete serializes with
    /// detached-append full snapshots that could otherwise resurrect
    /// the rows. No in-memory mutation happens here — the caller owns
    /// the drained session and drops the entries from its local copy.
    pub async fn delete_unacked_sequences(
        &self,
        stream_id: &str,
        sequences: &[u32],
    ) -> Result<u64, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        if sequences.is_empty() {
            return Ok(0);
        }
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;
        storage
            .delete_unacked(
                &crate::pending_delivery::SmSessionId::new(stream_id.to_string()),
                sequences,
            )
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    pub(super) fn stream_lock(
        &self,
        stream_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, SmRegistryError> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stream_id.hash(&mut hasher);
        let shard = (hasher.finish() as usize) % self.stream_locks.len();
        Ok(Arc::clone(&self.stream_locks[shard]))
    }

    pub async fn lock_session_operation(
        &self,
        stream_id: &str,
    ) -> Result<super::SmSessionOperationGuard, SmRegistryError> {
        let shard = self.stream_lock(stream_id)?;
        let guard = shard.clone().lock_owned().await;
        Ok(super::SmSessionOperationGuard {
            stream_id: stream_id.to_string(),
            shard,
            _guard: guard,
        })
    }

    pub(super) fn find_session_id_matching(
        &self,
        predicate: impl Fn(&DetachedSession) -> bool,
    ) -> Result<Option<String>, SmRegistryError> {
        let sessions = self
            .sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        if let Some((stream_id, _)) = sessions.iter().find(|(_, session)| predicate(session)) {
            return Ok(Some(stream_id.clone()));
        }
        drop(sessions);

        let claimed = self
            .claimed_sessions
            .read()
            .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
        Ok(claimed
            .iter()
            .find(|(_, session)| predicate(session))
            .map(|(stream_id, _)| stream_id.clone()))
    }

    pub(super) async fn update_detached_session_snapshot(
        &self,
        stream_id: &str,
        predicate: impl Fn(&DetachedSession) -> bool,
        mutate: impl FnOnce(&mut DetachedSession) -> bool,
    ) -> Result<bool, SmRegistryError> {
        let stream_lock = self.stream_lock(stream_id)?;
        let _stream_guard = stream_lock.lock().await;

        let current = {
            let sessions = self
                .sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            sessions
                .get(stream_id)
                .filter(|session| predicate(session))
                .cloned()
        };
        let current = if current.is_some() {
            current
        } else {
            let claimed = self
                .claimed_sessions
                .read()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            claimed
                .get(stream_id)
                .filter(|session| predicate(session))
                .cloned()
        };

        let Some(mut updated) = current else {
            return Ok(false);
        };
        if !mutate(&mut updated) {
            // No-op mutation (stale or duplicate input): skip the durable
            // snapshot entirely. Persistence restamps `detached_at`, so a
            // persisted no-op would silently extend the session's resume
            // window on every retry.
            return Ok(true);
        }

        // Durable snapshot first, then publish the same typed state in memory.
        // The stream lock serializes this full-snapshot write with other appends
        // and with claim completion/deletion so an older clone cannot overwrite
        // a newer replay window.
        self.persist_detached_session_snapshot(&updated, None)
            .await?;

        let updated = {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if sessions.contains_key(stream_id) {
                sessions.insert(stream_id.to_string(), updated);
                return Ok(true);
            }
            updated
        };

        let found_claimed = {
            let mut claimed = self
                .claimed_sessions
                .write()
                .map_err(|_| SmRegistryError::Internal("Lock poisoned".to_string()))?;
            if claimed.contains_key(stream_id) {
                claimed.insert(stream_id.to_string(), updated);
                true
            } else {
                false
            }
        };
        if found_claimed {
            return Ok(true);
        }

        // The session vanished from both maps between the stream-lock
        // read and this recheck. The only remover that does NOT take
        // this stream's lock is displacement by `store_session` (jid
        // collision / max_sessions eviction, which holds only the NEW
        // stream's shard lock) — and displaced sessions follow the
        // persist-until-confirmed contract (traits.rs): their durable
        // rows must survive until the promote → confirm_drained chain
        // erases them. The previous fail-closed `persist_delete_session`
        // here (PR #486, guarding against hypothetical lock-free
        // removers resurrecting an already-consumed stream) deleted a
        // displaced session's rows mid-promotion, losing the queue on a
        // crash. Every consuming path (take_session, complete_claim,
        // confirm_drained) takes
        // this stream lock, so the consumed-stream-resurrection concern
        // cannot arise here; deletion stays owned by
        // confirm_drained / the janitor. Worst case is an orphan
        // snapshot row that restore_from_persistence rehydrates and the
        // janitor later promotes — at-least-once, never data loss.
        Ok(false)
    }
}

fn new_stream_locks() -> Vec<Arc<tokio::sync::Mutex<()>>> {
    (0..STREAM_LOCK_SHARDS)
        .map(|_| Arc::new(tokio::sync::Mutex::new(())))
        .collect()
}
