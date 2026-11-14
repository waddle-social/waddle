use crate::auth::AuthenticatedPrincipalRef;
use crate::ownership::CurrentNodeIdentityGuard;
use crate::ownership::ExactReleaseOutcome;
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::IngressCustodyDisposition;
use crate::stream_management::persistence::PersistedIngressAppend;
use crate::stream_management::SmDrainedIngressAppend;
use crate::stream_management::SmIngressAppendKey;
use std::collections::hash_map::DefaultHasher;
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

use super::persistence_codec::{detached_to_persisted, parse_xml_to_persisted_unacked};
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

// Buckets protect only lookup, never a database await. Weak entries retain no
// operation mutex once its last caller finishes; each lookup prunes dead keys.
type StreamLockBucket = std::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>;

/// Operation-owned capacity marker for a reclaimed SM claim mutation.
/// Only the operation holding this token may consume, cancel, or defer its
/// reservation, so an older same-stream lifecycle cannot erase a newer
/// ownership CAS's ambiguity marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReclaimedClaimReservation(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReclaimedClaimAdmissionError {
    BusySameStream,
    Exhausted,
    LockPoisoned,
}

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

/// Bound on a `ClaimStore` acquire or lookup made while holding a stream's
/// operation lock. A stalled backend must not indefinitely block resumption,
/// fanout or cleanup for that stream. Only synchronous mutex lookup is sharded;
/// unrelated streams have independent operation locks.
pub(super) const CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// In-memory implementation of the SM session registry, optionally
/// backed by a [`SmPersistenceStorage`] so detached sessions survive
/// process restarts (issue #209 slice (d) phase 3, locked Q8 = B).
///
/// When `persistence` is `Some`, every `store_session` /
/// `take_session` / `cleanup_expired` mutation also writes to the
/// durable backend. Startup never rebuilds the in-memory view from those
/// rows: the durable stream-management authority alone resumes sessions,
/// and the owner phase drains rows it does not hold before serving.
///
/// Custom Debug skips the persistence handle (the
/// [`SmPersistenceStorage`] trait does not require `Debug`) and the
/// claim store (`dyn ClaimStore` does not require `Debug` either).
pub struct InMemorySmSessionRegistry {
    pub(super) sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) claimed_sessions: RwLock<HashMap<String, DetachedSession>>,
    pub(super) stale_snapshots: RwLock<HashSet<SmSessionId>>,
    pub(super) promotion_snapshots:
        RwLock<HashMap<SmSessionId, super::reconciliation::PromotionSnapshot>>,
    pub(super) stream_locks: Vec<StreamLockBucket>,
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
    /// Pending custody needed to settle ACK evidence even when durable XML is corrupt.
    pub(super) pending_delivery_storage:
        Option<Arc<dyn crate::pending_delivery::storage::PendingDeliveryStorage>>,
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
    /// confirmed. Keep exact occupancy metadata while the payload is leased
    /// outside the maps so retirement probes cannot mistake that gap for loss
    /// of custody, including when persistence is disabled.
    pub(super) pending_promotions:
        RwLock<HashMap<String, (jid::FullJid, waddle_xmpp_core::OccupancySessionGeneration)>>,
    /// Full payloads handed back by cancellation guards. They remain outside
    /// the resumable map until `drain_expired` reconciles them against the
    /// durable row, preventing stale pre-tombstone queues from being
    /// republished directly from `Drop`.
    pub(super) pending_promotion_retries: RwLock<HashMap<String, DetachedSession>>,
    /// Clone-shared results for reserved local resume acquisitions. This is
    /// bounded by claim admission and carries no duplicate stanza payload.
    pub(super) pending_resume_claim_custody:
        RwLock<HashMap<String, super::PromotionCaptureCustody>>,
    /// Claimed sessions whose follow-up epoch lookup failed before the
    /// route could prove that the recorded exact fence still owns the backend
    /// row. Kept out of `sessions` until a read-only reconciliation proves
    /// the same owner+epoch or terminalizes the stale local lifecycle.
    pub(super) pending_epoch_failure_reconciliations: RwLock<HashSet<String>>,
    /// Exact reclaimed-claim settlement work that has not yet reached a
    /// terminal outcome. This registry-owned inventory is the common safety
    /// net for both the supervised orphan reaper and the one-shot inline
    /// self-fence path: once a node wins a claim, a transient authority write or
    /// an identity rotation must not leave that live-owned claim invisible to
    /// every future orphan scan.
    pub(super) pending_reclaimed_settlements: RwLock<
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
    /// detach" bridge the durable claim move uses. `None` for
    /// single-node/non-clustering deployments, which have no other owner to
    /// ask.
    /// Production wiring injects a `waddle-server`-side adapter over
    /// `RelayHandle` via [`Self::with_remote_resume_asker`].
    pub(super) remote_resume: Option<Arc<dyn super::cross_node_resume::RemoteResumeAsker>>,
    /// The durable authority's detached sessions, consulted by the detached
    /// readers and appenders after this registry's own maps. Set once, when
    /// the durable selector is active.
    pub(super) durable_detached:
        std::sync::OnceLock<Arc<dyn super::durable_detached::DurableDetachedSessions>>,
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
        self.reserve_reclaimed_claim_fence_capacity(&entity.id).ok()
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

    pub(super) fn reserve_claim_fence_capacity(&self, stream_id: &str) -> bool {
        self.reserve_claim_fence_capacity_up_to(stream_id, self.max_sessions)
    }

    pub(super) fn reserve_reclaimed_claim_fence_capacity(
        &self,
        stream_id: &str,
    ) -> Result<ReclaimedClaimReservation, ReclaimedClaimAdmissionError> {
        let (Ok(reservations), Ok(mut reclaimed), Ok(pending), Ok(fences)) = (
            self.claim_fence_reservations.read(),
            self.reclaimed_claim_reservations.write(),
            self.pending_claim_releases.read(),
            self.claim_fences.read(),
        ) else {
            return Err(ReclaimedClaimAdmissionError::LockPoisoned);
        };
        // Classify same-stream contention under the admission locks, before
        // counting capacity: another operation already owns this transition.
        if reservations.contains(stream_id) || reclaimed.contains_key(stream_id) {
            return Err(ReclaimedClaimAdmissionError::BusySameStream);
        }
        let occupied = occupied_claim_fence_capacity(&reservations, &reclaimed, &pending, &fences);
        let active_nonterminal = fences
            .get(stream_id)
            .is_some_and(|fence| !pending.contains(&(stream_id.to_string(), fence.clone())));
        if !active_nonterminal && occupied >= self.max_sessions {
            return Err(ReclaimedClaimAdmissionError::Exhausted);
        }
        let token = ReclaimedClaimReservation(
            self.next_reclaimed_claim_reservation
                .fetch_add(1, Ordering::Relaxed),
        );
        reclaimed.insert(stream_id.to_string(), token);
        Ok(token)
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
        // Reclaimed settlement and ambiguous-lookup inventories are already
        // represented here: every reclaim reserves before its ownership CAS,
        // then either retains that reservation while the epoch is unknown or
        // consumes it into `claim_fences` once the exact fence is known.
        // `try_record_verified_reclaimed_fence` removes the reservation and
        // inserts that fence while holding all three inventory write locks in
        // one non-awaiting critical section; no transient/cancellation window
        // can leave only `pending_reclaimed_settlements` behind.
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

    /// Occupied bounded-ownership capacity (reservations + reclaimed +
    /// pending releases + live fences). Observability accessor for
    /// cross-crate lifecycle tests, mirroring
    /// [`Self::pending_claim_release_count`].
    pub fn claim_fence_capacity_used(&self) -> usize {
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
            let session_id = SmSessionId::new(stream_id.to_string());
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
            // incarnations. Only the verified-settlement path may replace a
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
                let session_id = SmSessionId::new(stream_id.to_string());
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
            stale_snapshots: RwLock::new(HashSet::new()),
            promotion_snapshots: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            pending_delivery_storage: None,
            claim_store: crate::ownership::observed_claim_store(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_fences: RwLock::new(HashMap::new()),
            pending_claim_releases: RwLock::new(HashSet::new()),
            pending_claim_acquisitions: RwLock::new(HashSet::new()),
            pending_promotions: RwLock::new(HashMap::new()),
            pending_promotion_retries: RwLock::new(HashMap::new()),
            pending_resume_claim_custody: RwLock::new(HashMap::new()),
            pending_epoch_failure_reconciliations: RwLock::new(HashSet::new()),
            pending_reclaimed_settlements: RwLock::new(HashMap::new()),
            pending_reclaimed_claim_lookups: RwLock::new(HashMap::new()),
            claim_fence_reservations: RwLock::new(HashSet::new()),
            reclaimed_claim_reservations: RwLock::new(HashMap::new()),
            next_reclaimed_claim_reservation: AtomicU64::new(1),
            remote_resume: None,
            durable_detached: std::sync::OnceLock::new(),
        }
    }

    /// Create a registry with custom settings.
    pub fn with_capacity(max_sessions: usize) -> Self {
        Self {
            sessions: RwLock::new(HashMap::with_capacity(max_sessions.min(10000))),
            claimed_sessions: RwLock::new(HashMap::new()),
            stale_snapshots: RwLock::new(HashSet::new()),
            promotion_snapshots: RwLock::new(HashMap::new()),
            stream_locks: new_stream_locks(),
            max_sessions,
            recent_tombstones: RwLock::new(Vec::new()),
            persistence: None,
            pending_delivery_storage: None,
            claim_store: crate::ownership::observed_claim_store(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            claim_fences: RwLock::new(HashMap::new()),
            pending_claim_releases: RwLock::new(HashSet::new()),
            pending_claim_acquisitions: RwLock::new(HashSet::new()),
            pending_promotions: RwLock::new(HashMap::new()),
            pending_promotion_retries: RwLock::new(HashMap::new()),
            pending_resume_claim_custody: RwLock::new(HashMap::new()),
            pending_epoch_failure_reconciliations: RwLock::new(HashSet::new()),
            pending_reclaimed_settlements: RwLock::new(HashMap::new()),
            pending_reclaimed_claim_lookups: RwLock::new(HashMap::new()),
            claim_fence_reservations: RwLock::new(HashSet::new()),
            reclaimed_claim_reservations: RwLock::new(HashMap::new()),
            next_reclaimed_claim_reservation: AtomicU64::new(1),
            remote_resume: None,
            durable_detached: std::sync::OnceLock::new(),
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

    /// Attach pending custody for ACK settlement before poison quarantine.
    /// Without it, quarantine retains any unsettled ACK journal for retry.
    pub fn with_pending_delivery_storage(
        mut self,
        storage: Arc<dyn crate::pending_delivery::storage::PendingDeliveryStorage>,
    ) -> Self {
        self.pending_delivery_storage = Some(storage);
        self
    }

    /// Store a detached session with the typed principal reference that must
    /// authorize its eventual resume. The reference remains operation-local
    /// until the persistence seam consumes it in the same atomic write as the
    /// snapshot and unacked queue.
    pub async fn store_session_with_principal(
        &self,
        session: DetachedSession,
        principal: AuthenticatedPrincipalRef,
    ) -> Result<Vec<DetachedSession>, SmRegistryError> {
        if self.persistence.is_none() {
            return Err(SmRegistryError::Internal(
                "durable SM principal persistence requires an attached storage backend".to_string(),
            ));
        }
        self.store_session_with_principal_inner(session, Some(&principal), Vec::new())
            .await
            .map(|stored| stored.displaced)
    }

    /// [`Self::store_session_with_principal`] for a session whose queue holds frames
    /// drained from the detaching socket (issue #1789): each drained obligation is
    /// proven in the same atomic write as the snapshot it sits in.
    pub async fn store_session_with_drained_ingress_appends(
        &self,
        session: DetachedSession,
        principal: AuthenticatedPrincipalRef,
        drained_appends: Vec<SmDrainedIngressAppend>,
    ) -> Result<super::StoredDetachedSession, SmRegistryError> {
        if self.persistence.is_none() {
            return Err(SmRegistryError::Internal(
                "durable SM principal persistence requires an attached storage backend".to_string(),
            ));
        }
        self.store_session_with_principal_inner(session, Some(&principal), drained_appends)
            .await
    }

    /// Read the durable principal paired with a detached SM session. This is
    /// deliberately a storage read; it never reconstructs authority from
    /// local session state.
    pub async fn session_principal(
        &self,
        stream_id: &str,
    ) -> Result<Option<AuthenticatedPrincipalRef>, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(None);
        };
        storage
            .get_session_principal(&SmSessionId::new(stream_id.to_string()))
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
    /// leave it `None`, since there is no other node to ask.
    pub fn with_remote_resume_asker(
        mut self,
        asker: Arc<dyn super::cross_node_resume::RemoteResumeAsker>,
    ) -> Self {
        self.remote_resume = Some(asker);
        self
    }

    /// Settle an SM-session claim this node reclaimed from a lost owner:
    /// the orphan reaper's steal of a dead node's claim, the inline reclaim
    /// in `self_fence::run_node_lease` of this node's own superseded
    /// identity, or a retained uncertain CAS that resolved to this node.
    ///
    /// The registry never holds such a session: the durable authority keeps
    /// every resumable session, so settling a reclaimed claim records the
    /// loss of its owner with that authority (when one is attached and holds
    /// the session) and hands the exact claim back for release, so whichever
    /// node resumes or expires the session takes a free claim.
    ///
    /// Under the entity's stream-shard lock this re-checks that the stream
    /// is not held by a local session, and re-proves the caller's exact
    /// claim epoch with a bounded `ClaimStore::ensure_claimed` before the
    /// owner loss is recorded. [`ReclaimedClaimSettlement::Release`] tells
    /// the caller to release the exact claim with
    /// [`Self::release_reclaimed_claim`]; a transient failure keeps the work
    /// in the pending inventory for [`Self::retry_pending_reclaimed_settlements`].
    pub async fn settle_reclaimed_claim(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedClaimSettlement, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            return Ok(ReclaimedClaimSettlement::LostClaim);
        }
        if !self.try_record_pending_reclaimed_settlement(entity, caller_fence, reservation)? {
            self.clear_pending_reclaimed_settlement(entity, caller_fence, reservation);
            return Ok(ReclaimedClaimSettlement::LostClaim);
        }
        let stream_lock = self.stream_lock(&entity.id)?;
        let _stream_guard = stream_lock.lock().await;
        self.settle_reclaimed_claim_locked(entity, caller_fence, reservation)
            .await
    }

    fn try_record_pending_reclaimed_settlement(
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
            self.pending_reclaimed_settlements
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

    async fn settle_reclaimed_claim_locked(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedClaimSettlement, SmRegistryError> {
        let pending = self
            .pending_reclaimed_settlements
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
            self.clear_pending_reclaimed_settlement(entity, caller_fence, reservation);
            return Ok(ReclaimedClaimSettlement::LostClaim);
        }

        let outcome = self
            .settle_reclaimed_claim_once(entity, caller_fence, reservation)
            .await?;
        if matches!(
            outcome,
            ReclaimedClaimSettlement::AlreadyPresent | ReclaimedClaimSettlement::LostClaim
        ) {
            self.clear_pending_reclaimed_settlement(entity, caller_fence, reservation);
        }
        if outcome == ReclaimedClaimSettlement::LostClaim {
            self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
        }
        Ok(outcome)
    }

    fn clear_pending_reclaimed_settlement(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) {
        if let Ok(mut pending) = self.pending_reclaimed_settlements.write() {
            pending.remove(&(entity.id.clone(), fence.clone(), reservation));
        }
    }

    async fn settle_reclaimed_claim_once(
        &self,
        entity: &Entity,
        caller_fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ReclaimedClaimSettlement, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            return Ok(ReclaimedClaimSettlement::LostClaim);
        }
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedClaimSettlement::StaleIdentity);
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
                || promotions.contains_key(&stream_id)
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
                Ok(Ok(true)) => return Ok(ReclaimedClaimSettlement::StaleIdentity),
                Ok(Ok(_)) => return Ok(ReclaimedClaimSettlement::LostClaim),
                Ok(Err(_)) | Err(_) => return Ok(ReclaimedClaimSettlement::TransientFailure),
            }
            if !self.try_record_verified_reclaimed_fence(
                &stream_id,
                caller_fence.clone(),
                reservation,
            ) {
                return Ok(ReclaimedClaimSettlement::TransientFailure);
            }
            debug!(
                stream_id = %stream_id,
                "settle_reclaimed_claim: a local session holds the stream; skipping"
            );
            return Ok(ReclaimedClaimSettlement::AlreadyPresent);
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
                    "settle_reclaimed_claim: ClaimStore ensure_claimed self-reacquire failed \
                     (claim lost again since the caller's steal_stale); skipping"
                );
                return Ok(ReclaimedClaimSettlement::LostClaim);
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    %error,
                    "settle_reclaimed_claim: ClaimStore ensure_claimed could not verify the \
                     already-won claim; retaining reclaimed responsibility for repair"
                );
                return Ok(ReclaimedClaimSettlement::TransientFailure);
            }
            Err(_timeout) => {
                tracing::warn!(
                    stream_id = %stream_id,
                    timeout = ?CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                    "settle_reclaimed_claim: ClaimStore ensure_claimed timed out while holding \
                     this stream's shard lock; skipping this entity for this pass"
                );
                return Ok(ReclaimedClaimSettlement::TransientFailure);
            }
        };
        if epoch != caller_fence.epoch() {
            debug!(
                stream_id = %stream_id,
                expected_epoch = caller_fence.epoch().0,
                actual_epoch = epoch.0,
                "settle_reclaimed_claim: caller work belongs to a superseded claim epoch"
            );
            return Ok(ReclaimedClaimSettlement::LostClaim);
        }
        if !self.try_record_verified_reclaimed_fence(&stream_id, caller_fence.clone(), reservation)
        {
            return Ok(ReclaimedClaimSettlement::TransientFailure);
        }
        // The durable authority moves the session to detached under the
        // claim just proven; it is then free to resume or expire there.
        // Bounded like every other call made under the shard lock.
        if let Some(durable) = self.durable_detached.get() {
            let session_id = SmSessionId::new(stream_id.clone());
            match tokio::time::timeout(
                CLAIM_CALL_UNDER_SHARD_LOCK_TIMEOUT,
                durable.record_owner_lost(self, &session_id, caller_fence),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    debug!(
                        stream_id = %stream_id,
                        %error,
                        "settle_reclaimed_claim: durable owner loss could not be recorded; retaining the claim for retry"
                    );
                    return Ok(ReclaimedClaimSettlement::TransientFailure);
                }
                Err(_) => return Ok(ReclaimedClaimSettlement::TransientFailure),
            }
        }
        if self.node_identity.current() != *caller_fence.owner() {
            return Ok(ReclaimedClaimSettlement::StaleIdentity);
        }
        Ok(ReclaimedClaimSettlement::Release)
    }

    /// Retry bounded reclaimed-session work retained by
    /// [`Self::settle_reclaimed_claim`]. A live node's won claim is no
    /// longer discoverable by the orphan scan, so this inventory — not a
    /// future scan — owns retry until the owner loss is recorded, ownership is
    /// disproved, or terminal cleanup completes.
    pub async fn retry_pending_reclaimed_settlements(&self, limit: usize) -> usize {
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
                    .try_record_pending_reclaimed_settlement(&entity, &fence, reservation)
                    .unwrap_or(false)
                {
                    self.settle_reclaimed_claim_locked(&entity, &fence, reservation)
                        .await
                } else {
                    Ok(ReclaimedClaimSettlement::LostClaim)
                };
                let terminal = matches!(
                    outcome,
                    Ok(ReclaimedClaimSettlement::Release | ReclaimedClaimSettlement::StaleIdentity)
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
            .pending_reclaimed_settlements
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
                .settle_reclaimed_claim(&entity, &fence, reservation)
                .await
            {
                Ok(ReclaimedClaimSettlement::Release | ReclaimedClaimSettlement::StaleIdentity) => {
                    let _ = self
                        .release_reclaimed_claim(&entity, &fence, reservation)
                        .await;
                }
                Ok(
                    ReclaimedClaimSettlement::AlreadyPresent
                    | ReclaimedClaimSettlement::LostClaim
                    | ReclaimedClaimSettlement::TransientFailure,
                )
                | Err(_) => {}
            }
        }
        attempted
    }

    #[cfg(test)]
    pub(super) fn pending_reclaimed_settlement_count(&self) -> usize {
        self.pending_reclaimed_settlements
            .read()
            .map_or(0, |pending| pending.len())
    }

    pub async fn release_reclaimed_claim(
        &self,
        entity: &Entity,
        fence: &super::super::persistence::SmClaimFence,
        reservation: ReclaimedClaimReservation,
    ) -> Result<ExactReleaseOutcome, SmRegistryError> {
        let stream_lock = self.stream_lock(&entity.id)?;
        let _stream_guard = stream_lock.lock().await;
        match self.stream_liveness(&entity.id) {
            Some(true) => {
                // Responsibility transferred back to the live local session.
                // Never let terminal cleanup release its claim.
                self.clear_pending_reclaimed_settlement(entity, fence, reservation);
                self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
                return Ok(ExactReleaseOutcome::NotOwned);
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
            let session_id = SmSessionId::new(entity.id.clone());
            storage.evict_claim_cache(&session_id, fence);
        }
        self.clear_pending_reclaimed_settlement(entity, fence, reservation);
        Ok(outcome)
    }

    /// Resolve an ownership CAS whose result was dropped before the claim
    /// was settled. This is the terminal self-fence path:
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
    ) -> Result<ExactReleaseOutcome, SmRegistryError> {
        if entity.entity_type != EntityType::SmSession {
            self.cancel_reclaimed_claim_fence_reservation(&entity.id, reservation);
            return Ok(ExactReleaseOutcome::NotOwned);
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
/// What [`InMemorySmSessionRegistry::settle_reclaimed_claim`] decided about
/// a reclaimed SM-session claim.
pub enum ReclaimedClaimSettlement {
    /// A local session holds the stream; its own lifecycle keeps the claim.
    AlreadyPresent,
    /// The owner loss is recorded (or no authority holds the session): the
    /// caller releases the exact claim.
    Release,
    /// The claim moved to a newer epoch or owner; nothing is left to settle.
    LostClaim,
    /// This node's identity changed since the claim was won; the caller
    /// releases the exact claim it still names.
    StaleIdentity,
    /// The claim or the authority could not be read; the work is retried.
    TransientFailure,
}

impl InMemorySmSessionRegistry {
    /// Helper: delete every durable row for `stream_id` (session +
    /// unacked queue). Returns the underlying error so callers can
    /// adopt a "persist-first" ordering — refuse to mutate the
    /// in-memory map when the durable delete failed, so a transient
    /// storage hiccup doesn't leave an orphaned `sm_sessions` row
    /// behind.
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
            .delete_session(&SmSessionId::new(stream_id.to_string()))
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        self.forget_snapshot_reconciliation(&SmSessionId::new(stream_id));
        Ok(())
    }

    pub(super) async fn persist_delete_promotion_session(
        &self,
        stream_id: &str,
        expected_fence: &super::super::persistence::SmClaimFence,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        let stream = SmSessionId::new(stream_id);
        storage
            .delete_promotion_session(&stream, expected_fence)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))?;
        self.forget_snapshot_reconciliation(&stream);
        Ok(())
    }

    pub(super) async fn persist_delete_session_with_authority(
        &self,
        stream_id: &str,
        authority: &CurrentNodeIdentityGuard,
    ) -> Result<(), SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(());
        };
        storage
            .delete_session_with_authority(&SmSessionId::new(stream_id.to_string()), authority)
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))?;
        self.forget_snapshot_reconciliation(&SmSessionId::new(stream_id));
        Ok(())
    }

    pub(super) async fn persist_detached_session_snapshot(
        &self,
        session: &DetachedSession,
        principal: Option<&AuthenticatedPrincipalRef>,
        drained_appends: Vec<SmDrainedIngressAppend>,
    ) -> Result<Vec<SmIngressAppendKey>, SmRegistryError> {
        let Some(storage) = &self.persistence else {
            return Ok(Vec::new());
        };
        let persisted = detached_to_persisted(session)?;
        let mut unacked_rows = Vec::with_capacity(session.unacked_stanzas.len());
        for entry in &session.unacked_stanzas {
            unacked_rows.push(parse_xml_to_persisted_unacked(
                &session.stream_id,
                entry.sequence,
                &entry.stanza_xml,
                entry.original_receipt_at,
                entry.ingress_receipts.clone(),
                entry.provenance.clone(),
            )?);
        }
        let accepting_stream = SmSessionId::new(session.stream_id.clone());
        let appended_at = chrono::Utc::now();
        let appends = drained_appends
            .into_iter()
            .map(|append| PersistedIngressAppend {
                key: append.ticket.key,
                accepting_stream: accepting_stream.clone(),
                sequence: append.sequence,
                appended_at,
                payload: append.payload,
                original_receipt_at: append.original_receipt_at,
                disposition: IngressCustodyDisposition::Pending,
            })
            .collect::<Vec<_>>();
        match principal {
            // Only a principal-bearing detach drains a socket queue.
            Some(principal) if !appends.is_empty() => {
                storage
                    .store_session_atomic_with_principal_and_ingress_appends(
                        principal,
                        persisted,
                        unacked_rows,
                        appends,
                    )
                    .await
            }
            Some(principal) => storage
                .store_session_atomic_with_principal(principal, persisted, unacked_rows)
                .await
                .map(|()| Vec::new()),
            None => storage
                .store_session_atomic(persisted, unacked_rows)
                .await
                .map(|()| appends.into_iter().map(|append| append.key).collect()),
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
    /// Takes the stream lock so deletion serializes with incremental
    /// detached appends and initial snapshots that could otherwise race it.
    /// No in-memory mutation happens here — the caller owns the drained session
    /// and drops the entries from its local copy.
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
            .delete_unacked(&SmSessionId::new(stream_id.to_string()), sequences)
            .await
            .map_err(|e| SmRegistryError::Internal(e.to_string()))
    }

    /// Prune an accepted prefix only under its immutable capture authority.
    pub async fn delete_promotion_unacked_sequences(
        &self,
        session: &DetachedSession,
        sequences: &[u32],
    ) -> Result<u64, SmRegistryError> {
        let lock = self.stream_lock(&session.stream_id)?;
        let _guard = lock.lock().await;
        let fence = self.registered_promotion_fence_locked(session).await?;
        let Some(storage) = &self.persistence else {
            return Ok(0);
        };
        if sequences.is_empty() {
            return Ok(0);
        }
        storage
            .delete_promotion_unacked(&SmSessionId::new(&session.stream_id), sequences, &fence)
            .await
            .map_err(|error| SmRegistryError::Internal(error.to_string()))
    }

    /// Serialize effects with local demotion and claim replacement. No node
    /// identity guard is retained: fenced storage acquires its own authority.
    pub async fn lock_promotion_operation(
        &self,
        session: &DetachedSession,
    ) -> Result<super::SmSessionOperationGuard, SmRegistryError> {
        let operation = self.lock_session_operation(&session.stream_id).await?;
        self.registered_promotion_fence_locked(session).await?;
        Ok(operation)
    }

    pub(super) fn stream_lock(
        &self,
        stream_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, SmRegistryError> {
        let mut hasher = DefaultHasher::new();
        stream_id.hash(&mut hasher);
        let shard = (hasher.finish() as usize) % self.stream_locks.len();
        let mut locks = self.stream_locks[shard]
            .lock()
            .map_err(|_| SmRegistryError::Internal("Stream lock lookup poisoned".into()))?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(stream_id).and_then(std::sync::Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(stream_id.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
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
}

fn new_stream_locks() -> Vec<StreamLockBucket> {
    (0..STREAM_LOCK_SHARDS)
        .map(|_| std::sync::Mutex::new(HashMap::new()))
        .collect()
}
