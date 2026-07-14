//! MUC Room Registry Actor
//!
//! Kameo actor that manages all MUC room actors. Replaces the DashMap-based
//! `MucRoomRegistry` with a single-writer actor that owns the room map and
//! spawns per-room `RoomActor` instances on demand.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::message::Context;
use kameo::Actor;
use thiserror::Error;
use tracing::{debug, info, warn};

use super::affiliation::DurableMembershipSource;
use super::durable::MucDurableStore;
use super::room_actor::{
    GetRoomSealState, HydrateDurableRecipients, RestoreDurableRoomState, RoomActor, RoomSealState,
    SealGuard, SealIfInactive, SealIfInactiveOutcome,
};
use super::{MucRoom, RoomConfig};
use crate::metrics;
use crate::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, ExactReleaseOutcome,
    InProcessClaimStore, NodeIdentity, RolloutBackoff, SharedNodeIdentity, StalePredicate,
};
use crate::xep::xep0421::OccupantIdSecret;

/// A locally-spawned room's actor ref plus the Postgres claim epoch this
/// node acquired/won it under (ADR-0017 Phase 3 Slice 7). The epoch
/// travels with the actor ref so [`RoomRegistryActor::DestroyRoom`] can
/// release the exact claim this incarnation holds.
#[derive(Clone)]
struct RoomEntry {
    actor_ref: ActorRef<RoomActor>,
    claim_fence: super::RoomClaimFenceContext,
}

#[derive(Clone)]
struct PendingReclaimedState {
    claim_fence: super::RoomClaimFenceContext,
    previous_owner: NodeIdentity,
    retry_order: u64,
    first_pending_at: std::time::Instant,
}

#[derive(Clone)]
struct PendingRoomReleaseState {
    retry_order: u64,
    first_pending_at: std::time::Instant,
}

#[derive(Clone)]
struct PendingRoomAcquisitionState {
    retry_order: u64,
    first_pending_at: std::time::Instant,
}

/// Actor that owns the mapping from room JIDs to per-room actors.
///
/// All room creation, lookup, and destruction flows through this actor,
/// so no external synchronisation is needed.
#[derive(Actor)]
pub struct RoomRegistryActor {
    rooms: HashMap<BareJid, RoomEntry>,
    poisoned_rooms: HashSet<BareJid>,
    /// Reclaimed epochs that are neither served by a local actor nor
    /// confirmed released yet. The orphan reaper asks for a bounded page on
    /// later sweeps and retries each through the same serialized adoption
    /// path; this prevents a transient store failure from stranding a fresh
    /// claim forever.
    pending_reclaimed_rooms:
        HashMap<(BareJid, super::RoomClaimFenceContext), PendingReclaimedState>,
    pending_reclaimed_reservations: HashSet<BareJid>,
    /// Ordinary terminal removals whose exact claim release was uncertain.
    /// Multiple owner+epoch generations for one room are intentional. A
    /// timed-out release may have deleted the row, after which another owner
    /// can recreate it with a fresh globally monotonic epoch. Retaining every
    /// exact fence still matters because each timed-out delete can commit out
    /// of order and must reach its own typed outcome before the registry drops
    /// that release responsibility. The global bound prevents churn from
    /// growing this inventory without limit.
    pending_room_releases:
        HashMap<(BareJid, super::RoomClaimFenceContext), PendingRoomReleaseState>,
    /// Claim CAS calls whose timeout/backend error left commit status
    /// uncertain. Until a read proves the row missing/foreign or transfers
    /// the exact fence into actor/release ownership, this inventory remains
    /// responsible for the possibly committed claim.
    pending_room_acquisitions: HashMap<(BareJid, NodeIdentity), PendingRoomAcquisitionState>,
    pending_retry_order: u64,
    pending_retry_timer_generation: u64,
    scheduled_pending_retry_generation: Option<u64>,
    /// Terminal shutdown has begun. Once set inside the actor mailbox, no
    /// later demand or orphan-reaper message may acquire fresh room authority.
    terminal_claim_acquisition_disabled: bool,
    muc_domain: String,
    /// Per-deployment XEP-0421 occupant-id HMAC key. Forwarded to every
    /// `RoomActor` at spawn so all rooms in this deployment share the
    /// same keying material.
    occupant_id_secret: OccupantIdSecret,
    /// Durable membership source used to hydrate each freshly spawned
    /// `RoomActor`'s durable-recipient set (#1135). `None` in
    /// deployments/tests without a durable membership store; such
    /// rooms fall back to session-observed affiliations only.
    membership_source: Option<Arc<dyn DurableMembershipSource>>,
    /// Entity-ownership claim store (ADR-0017 Phase 3 Slice 7). Defaults
    /// to [`InProcessClaimStore`] — the single-node fallback — so every
    /// existing construction site (tests, single-node deployments)
    /// behaves exactly as before: a `GetOrCreateRoom` on the only node
    /// that could ever contend for the entity always succeeds
    /// immediately. Replaced with the real `PostgresClaimStore`-backed
    /// handle by [`WireClusteringClaims`] once clustering is configured
    /// (construction-order note: the room registry is spawned before
    /// `clustering::start_if_enabled` runs, mirroring the `local_claims`/
    /// `resume_bridge` fill-in-later cell pattern).
    claim_store: Arc<dyn ClaimStore>,
    /// This node's claim identity. Defaults to [`NodeIdentity::local`] —
    /// meaningless but harmless for [`InProcessClaimStore`], which never
    /// checks cross-node identity. Replaced by [`WireClusteringClaims`].
    node_identity: SharedNodeIdentity,
    /// Durable room-state store (ADR-0017 Phase 3 Slice 7): `None` in
    /// single-node/non-clustering deployments, matching today's purely
    /// in-memory room behavior exactly. Wired by [`WireClusteringClaims`].
    durable_store: Option<Arc<dyn MucDurableStore>>,
    /// Rollout-aware claim-acquisition backoff (ADR-0017 Phase 3 Slice 10):
    /// `None` (the default) in single-node/non-clustering deployments and
    /// every existing test — correct, since there is only ever one
    /// generation to place. Wired by [`WireClusteringClaims`].
    rollout_backoff: Option<Arc<dyn RolloutBackoff>>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomRegistryError {
    #[error("room {0} already exists")]
    RoomAlreadyExists(BareJid),
    #[error("room actor state for {0} was lost; explicit destroy/recreate is required")]
    RoomActorStateLost(BareJid),
    /// A request to the registry actor exceeded
    /// [`ROOM_REGISTRY_REPLY_TIMEOUT`](crate::muc::room_registry_handle::ROOM_REGISTRY_REPLY_TIMEOUT)
    /// without a reply. Surfaced (instead of hanging the caller indefinitely)
    /// so a wedged registry produces a visible, typed failure — the #757
    /// production incident class.
    #[error("room registry request timed out")]
    Timeout,
    /// The registry actor could not be reached (stopped, or its mailbox is
    /// saturated/closed). Distinct from [`RoomRegistryError::Timeout`]: the
    /// request never entered processing.
    #[error("room registry unavailable")]
    Unavailable,
    #[error("room {0}'s ownership store is unavailable")]
    OwnershipUnavailable(BareJid),
    /// A prior exact room-ownership generation is still converging. Unlike
    /// [`RoomRegistryError::OwnershipUnavailable`], this is an expected,
    /// bounded demand-side deferral rather than a claim-store failure.
    #[error("room {0}'s prior ownership generation is still reconciling")]
    OwnershipReconciliationPending(BareJid),
    /// ADR-0017 Phase 3 Slice 7: `entity`'s Postgres claim is held by
    /// another, currently-live node, and this slice does not wire
    /// cross-node MUC message/join proxying (the ADR's own text names it
    /// as part of the design, but Phase 3's Non-goals exclude cross-node
    /// stanza routing GA — that lands in Phase 4). A live foreign owner
    /// therefore cannot be joined/created from this node yet; a dead
    /// owner's claim is instead stolen automatically (re-election) and
    /// never surfaces this variant.
    #[error("room {0}'s ownership claim is held by another live node")]
    ClaimHeldByAnotherNode(BareJid),
}

impl RoomRegistryActor {
    /// Create a new registry for the given MUC service domain.
    pub fn new(muc_domain: String, occupant_id_secret: OccupantIdSecret) -> Self {
        info!(domain = %muc_domain, "Creating RoomRegistryActor");
        Self {
            rooms: HashMap::new(),
            poisoned_rooms: HashSet::new(),
            pending_reclaimed_rooms: HashMap::new(),
            pending_reclaimed_reservations: HashSet::new(),
            pending_room_releases: HashMap::new(),
            pending_room_acquisitions: HashMap::new(),
            pending_retry_order: 0,
            pending_retry_timer_generation: 0,
            scheduled_pending_retry_generation: None,
            terminal_claim_acquisition_disabled: false,
            muc_domain,
            occupant_id_secret,
            membership_source: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            durable_store: None,
            rollout_backoff: None,
        }
    }

    fn has_pending_release_capacity(
        &self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
    ) -> bool {
        self.pending_room_releases
            .contains_key(&(room_jid.clone(), claim_fence.clone()))
            || self.pending_room_releases.len() < MAX_PENDING_ROOM_RELEASES
    }

    /// Remove an actor after its own durable fence proved that this exact
    /// incarnation no longer owns the room.
    ///
    /// A same-identity negative database fence is terminal, so releasing it
    /// would only create a possible late-delete responsibility. A different
    /// local identity is special: the durable gate rejects the cached old
    /// fence before querying the claim store, so the old exact row can still
    /// exist and must receive one safe best-effort release. That old owner can
    /// never match a claim acquired by the current identity.
    async fn retire_ownership_lost_entry(&mut self, room_jid: &BareJid, entry: RoomEntry) {
        entry.actor_ref.kill();
        if entry.claim_fence.owner() != self.node_identity.current() {
            self.release_room_claim(room_jid, &entry.claim_fence).await;
        } else if let Some(store) = &self.durable_store {
            store.forget_claim_fence(room_jid, &entry.claim_fence);
        }
    }

    async fn evict_ownership_lost_room(&mut self, room_jid: &BareJid, entry: RoomEntry) {
        self.rooms.remove(room_jid);
        self.poisoned_rooms.remove(room_jid);
        self.retire_ownership_lost_entry(room_jid, entry).await;
    }

    fn remember_pending_room_release(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
    ) -> bool {
        if !self.has_pending_release_capacity(&room_jid, &claim_fence) {
            return false;
        }
        self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
        let retry_order = self.pending_retry_order;
        self.pending_room_releases
            .entry((room_jid, claim_fence.clone()))
            .and_modify(|current| current.retry_order = retry_order)
            .or_insert(PendingRoomReleaseState {
                retry_order,
                first_pending_at: std::time::Instant::now(),
            });
        true
    }

    /// Replace one bounded, bare-JID reclaimed-room reservation with the
    /// exact fence observed for it. This transfer may take the ordinary
    /// release inventory above [`MAX_PENDING_ROOM_RELEASES`], but cannot
    /// increase the combined number of responsibilities: the reservation
    /// was already admitted under [`MAX_PENDING_RECLAIMED_ROOMS`]. Keeping
    /// the typed fence in actor state before awaiting release prevents a
    /// terminal backend failure from degrading exact ownership back to an
    /// ambiguous room JID.
    fn transfer_reclaimed_reservation_to_pending_release(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
    ) {
        debug_assert!(self.pending_reclaimed_reservations.contains(&room_jid));
        self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
        let retry_order = self.pending_retry_order;
        self.pending_room_releases
            .entry((room_jid.clone(), claim_fence))
            .and_modify(|current| current.retry_order = retry_order)
            .or_insert(PendingRoomReleaseState {
                retry_order,
                first_pending_at: std::time::Instant::now(),
            });
        self.pending_reclaimed_reservations.remove(&room_jid);
    }

    fn reserve_pending_room_acquisition(
        &mut self,
        room_jid: &BareJid,
        owner: &NodeIdentity,
    ) -> bool {
        let key = (room_jid.clone(), owner.clone());
        if self.pending_room_acquisitions.contains_key(&key) {
            return true;
        }
        if self.pending_room_acquisitions.len() >= MAX_PENDING_ROOM_ACQUISITIONS {
            return false;
        }
        self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
        self.pending_room_acquisitions.insert(
            key,
            PendingRoomAcquisitionState {
                retry_order: self.pending_retry_order,
                first_pending_at: std::time::Instant::now(),
            },
        );
        true
    }

    fn clear_pending_room_acquisition(&mut self, room_jid: &BareJid, owner: &NodeIdentity) {
        self.pending_room_acquisitions
            .remove(&(room_jid.clone(), owner.clone()));
    }

    fn has_pending_room_retry_work(&self) -> bool {
        !self.pending_room_acquisitions.is_empty() || !self.pending_room_releases.is_empty()
    }

    fn schedule_pending_room_retry(&mut self, actor_ref: &ActorRef<Self>) {
        if self.scheduled_pending_retry_generation.is_some() {
            return;
        }
        self.pending_retry_timer_generation = self.pending_retry_timer_generation.wrapping_add(1);
        let generation = self.pending_retry_timer_generation;
        self.scheduled_pending_retry_generation = Some(generation);
        std::mem::drop(
            actor_ref
                .tell(RetryPendingRoomWork {
                    generation,
                    limit: PENDING_ROOM_RETRY_BATCH,
                })
                .send_after(PENDING_ROOM_RETRY_DELAY),
        );
    }

    async fn reconcile_pending_room_acquisition(
        &mut self,
        room_jid: &BareJid,
        owner: &NodeIdentity,
    ) {
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let snapshot = tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.claim_store.current_claim(&entity),
        )
        .await;
        let snapshot = match snapshot {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(_)) | Err(_) => {
                self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
                if let Some(pending) = self
                    .pending_room_acquisitions
                    .get_mut(&(room_jid.clone(), owner.clone()))
                {
                    pending.retry_order = self.pending_retry_order;
                }
                return;
            }
        };
        let Some(snapshot) = snapshot else {
            self.clear_pending_room_acquisition(room_jid, owner);
            return;
        };
        if snapshot.owner != *owner {
            self.clear_pending_room_acquisition(room_jid, owner);
            return;
        }
        let claim_fence =
            super::RoomClaimFenceContext::new(entity, owner.clone(), snapshot.claim_epoch);
        if self
            .rooms
            .get(room_jid)
            .is_some_and(|entry| entry.actor_ref.is_alive() && entry.claim_fence == claim_fence)
        {
            self.clear_pending_room_acquisition(room_jid, owner);
            return;
        }
        if !self.has_pending_release_capacity(room_jid, &claim_fence) {
            self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
            if let Some(pending) = self
                .pending_room_acquisitions
                .get_mut(&(room_jid.clone(), owner.clone()))
            {
                pending.retry_order = self.pending_retry_order;
            }
            return;
        }
        self.release_room_claim(room_jid, &claim_fence).await;
        self.clear_pending_room_acquisition(room_jid, owner);
    }

    fn clear_pending_room_release(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
    ) {
        self.pending_room_releases
            .remove(&(room_jid.clone(), claim_fence.clone()));
    }

    async fn retry_oldest_pending_room_release(&mut self) -> bool {
        let Some((room_jid, claim_fence)) = self
            .pending_room_releases
            .iter()
            .min_by_key(|(_, state)| state.retry_order)
            .map(|((room_jid, claim_fence), _)| (room_jid.clone(), claim_fence.clone()))
        else {
            return false;
        };
        self.release_room_claim(&room_jid, &claim_fence).await;
        true
    }

    /// Resolve every exact terminal-release responsibility for this room
    /// before attempting a new demand claim. A timed-out release can commit
    /// after its future is dropped; self-reacquiring that still-present exact
    /// epoch would let the late delete remove a newly published actor's claim.
    /// Only typed `Released`/`NotOwned` outcomes clear these entries.
    async fn converge_pending_room_releases_before_acquire(
        &mut self,
        room_jid: &BareJid,
        deadline: tokio::time::Instant,
    ) -> bool {
        let pending = self
            .pending_room_releases
            .keys()
            .filter(|(pending_room, _)| pending_room == room_jid)
            .map(|(_, claim_fence)| claim_fence.clone())
            .collect::<Vec<_>>();
        for claim_fence in pending {
            let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            else {
                return false;
            };
            self.release_room_claim_with_timeout(
                room_jid,
                &claim_fence,
                remaining.min(ROOM_OWNERSHIP_CALL_TIMEOUT),
                ClaimReleaseContext::PreAcquire,
            )
            .await;
            if self
                .pending_room_releases
                .contains_key(&(room_jid.clone(), claim_fence))
            {
                return false;
            }
        }
        !self
            .pending_room_releases
            .keys()
            .any(|(pending_room, _)| pending_room == room_jid)
    }

    /// Reclaimed epochs use a separate bounded retry inventory because they
    /// arrive from the dead-owner sweeper. They carry the same late-delete
    /// hazard as ordinary releases, so demand must converge every exact
    /// generation for this room before acquiring a fresh claim. A bare-JID
    /// reservation has no exact epoch to fence and therefore blocks demand
    /// until the reaper replaces it with typed state.
    async fn converge_pending_reclaimed_before_acquire(
        &mut self,
        room_jid: &BareJid,
        deadline: tokio::time::Instant,
    ) -> bool {
        if self.pending_reclaimed_reservations.contains(room_jid) {
            return false;
        }
        let pending = self
            .pending_reclaimed_rooms
            .iter()
            .filter(|((pending_room, _), _)| pending_room == room_jid)
            .map(|((_, claim_fence), state)| (claim_fence.clone(), state.previous_owner.clone()))
            .collect::<Vec<_>>();
        for (claim_fence, previous_owner) in pending {
            let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            else {
                return false;
            };
            let outcome = self
                .release_reclaimed_room_claim_with_timeout(
                    room_jid,
                    &claim_fence,
                    &previous_owner,
                    remaining.min(RECLAIMED_ROOM_RELEASE_TIMEOUT),
                )
                .await;
            if outcome == ReclaimedRoomOutcome::PendingRetry {
                return false;
            }
        }
        !self.pending_reclaimed_reservations.contains(room_jid)
            && !self
                .pending_reclaimed_rooms
                .keys()
                .any(|(pending_room, _)| pending_room == room_jid)
    }

    async fn retry_pending_room_work(&mut self, limit: usize) -> usize {
        enum RetryWork {
            Acquisition(BareJid, NodeIdentity),
            Release(BareJid, super::RoomClaimFenceContext),
        }

        let mut pending = self
            .pending_room_acquisitions
            .iter()
            .map(|((room_jid, owner), state)| {
                (
                    state.retry_order,
                    RetryWork::Acquisition(room_jid.clone(), owner.clone()),
                )
            })
            .chain(
                self.pending_room_releases
                    .iter()
                    .map(|((room_jid, claim_fence), state)| {
                        (
                            state.retry_order,
                            RetryWork::Release(room_jid.clone(), claim_fence.clone()),
                        )
                    }),
            )
            .collect::<Vec<_>>();
        pending.sort_by_key(|(retry_order, _)| *retry_order);
        pending.truncate(limit);
        let attempted = pending.len();
        for (_, work) in pending {
            match work {
                RetryWork::Acquisition(room_jid, owner) => {
                    self.reconcile_pending_room_acquisition(&room_jid, &owner)
                        .await;
                }
                RetryWork::Release(room_jid, claim_fence) => {
                    self.release_room_claim(&room_jid, &claim_fence).await;
                    self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
                    if let Some(current) =
                        self.pending_room_releases.get_mut(&(room_jid, claim_fence))
                    {
                        current.retry_order = self.pending_retry_order;
                    }
                }
            }
        }
        attempted
    }

    /// Attach a durable membership source so every spawned `RoomActor`
    /// hydrates its durable-recipient set before serving snapshots (#1135).
    #[must_use]
    pub fn with_membership_source(mut self, source: Arc<dyn DurableMembershipSource>) -> Self {
        self.membership_source = Some(source);
        self
    }

    fn remember_pending_reclaimed_room(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        previous_owner: NodeIdentity,
    ) {
        self.pending_reclaimed_reservations.remove(&room_jid);
        self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
        let retry_order = self.pending_retry_order;
        self.pending_reclaimed_rooms
            .entry((room_jid, claim_fence.clone()))
            .and_modify(|current| current.retry_order = retry_order)
            .or_insert(PendingReclaimedState {
                claim_fence: claim_fence.clone(),
                previous_owner,
                retry_order,
                first_pending_at: std::time::Instant::now(),
            });
    }

    fn clear_pending_reclaimed_room(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
    ) {
        self.pending_reclaimed_rooms
            .remove(&(room_jid.clone(), claim_fence.clone()));
    }

    fn has_live_room_with_fence(
        &self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
    ) -> bool {
        self.rooms
            .get(room_jid)
            .is_some_and(|entry| entry.actor_ref.is_alive() && entry.claim_fence == *claim_fence)
    }

    /// Acquire this room's Postgres claim (ADR-0017 Phase 3 Slice 7),
    /// stealing from a dead owner (re-election) when the current owner's
    /// own node lease is no longer fresh. Returns the epoch this node now
    /// holds the claim under.
    ///
    /// A live foreign owner (steal not applicable) is reported as
    /// [`RoomRegistryError::ClaimHeldByAnotherNode`] rather than
    /// attempted via any cross-node proxy — see that variant's doc
    /// comment for why that is out of this slice's scope.
    async fn acquire_room_claim(
        &mut self,
        room_jid: &BareJid,
        actor_ref: &ActorRef<Self>,
    ) -> Result<super::RoomClaimFenceContext, RoomRegistryError> {
        if self.terminal_claim_acquisition_disabled {
            return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
        }
        let convergence_deadline = tokio::time::Instant::now() + PRE_ACQUIRE_CONVERGENCE_BUDGET;
        if !self
            .converge_pending_reclaimed_before_acquire(room_jid, convergence_deadline)
            .await
            || !self
                .converge_pending_room_releases_before_acquire(room_jid, convergence_deadline)
                .await
        {
            debug!(room = %room_jid, "room claim acquisition deferred until exact-release ambiguity converges");
            return Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid.clone(),
            ));
        }
        // A newly acquired generation can require an exact terminal-release
        // retry if identity rotates or actor preparation loses its final
        // fence. Refuse acquisition while the bounded retry inventory is
        // saturated; acquiring first would create responsibility that the
        // registry has nowhere bounded to retain.
        if self.pending_room_releases.len() >= MAX_PENDING_ROOM_RELEASES {
            warn!(room = %room_jid, "room claim acquisition refused: exact-release retry backlog is full");
            return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
        }
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let identity = self.node_identity.current();
        if !self.reserve_pending_room_acquisition(room_jid, &identity) {
            warn!(room = %room_jid, "room claim acquisition refused: uncertain-acquisition backlog is full");
            return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
        }
        // The reservation represents responsibility for a possibly
        // committed claim. Drive reconciliation from the actor itself so a
        // transient backend outage cannot fill the bounded inventory and
        // permanently refuse unrelated future room acquisitions.
        self.schedule_pending_room_retry(actor_ref);
        let epoch = match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.claim_store.ensure_claimed(&entity, &identity),
        )
        .await
        {
            Ok(Ok(epoch)) => {
                self.clear_pending_room_acquisition(room_jid, &identity);
                epoch
            }
            Ok(Err(ClaimError::AlreadyClaimed)) => {
                self.steal_from_dead_owner(&entity, room_jid, &identity)
                    .await?
            }
            Ok(Err(error)) => {
                warn!(room = %room_jid, %error, "room claim acquisition failed");
                return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
            }
            Err(_) => return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone())),
        };
        if self.node_identity.current() != identity {
            let claim_fence = super::RoomClaimFenceContext::new(entity, identity, epoch);
            self.release_room_claim(room_jid, &claim_fence).await;
            return Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid.clone(),
            ));
        }
        Ok(super::RoomClaimFenceContext::new(entity, identity, epoch))
    }

    /// The re-election path: `entity`'s claim is held by another node —
    /// steal it if (and only if) that node's own liveness lease is no
    /// longer fresh (element 7's "steal after owner death").
    async fn steal_from_dead_owner(
        &mut self,
        entity: &Entity,
        room_jid: &BareJid,
        identity: &NodeIdentity,
    ) -> Result<ClaimEpoch, RoomRegistryError> {
        let snapshot = match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(Some(snapshot))) => snapshot,
            Ok(Ok(None)) => {
                self.clear_pending_room_acquisition(room_jid, identity);
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
            Ok(Err(error)) => {
                self.clear_pending_room_acquisition(room_jid, identity);
                warn!(room = %room_jid, %error, "room claim lookup failed during ownership steal");
                return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
            }
            Err(_) => {
                self.clear_pending_room_acquisition(room_jid, identity);
                return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
            }
        };
        if snapshot.owner_lease_fresh {
            self.clear_pending_room_acquisition(room_jid, identity);
            return Err(RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone()));
        }
        // ADR-0017 Phase 3 Slice 10 (Q5's rollout-aware placement rule): an
        // old-generation node backs off before racing a matching/newer
        // -generation node for a dead owner's claim, so each room moves
        // approximately once per deploy instead of up to N times. Purely a
        // placement heuristic — never affects correctness (the epoch CAS
        // below remains the sole authority over who actually wins).
        if let Some(backoff) = &self.rollout_backoff {
            let delay = backoff.acquire_delay().await;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.claim_store.steal_stale(
                entity,
                snapshot.claim_epoch,
                StalePredicate::OwnerStale,
                identity,
            ),
        )
        .await
        {
            Ok(Ok(new_epoch)) => {
                self.clear_pending_room_acquisition(room_jid, identity);
                info!(
                    room = %room_jid,
                    previous_owner = %snapshot.owner.node_id,
                    "re-elected room ownership from a dead owner"
                );
                self.notify_previous_owner_demoted(room_jid, &snapshot.owner, new_epoch);
                Ok(new_epoch)
            }
            Ok(Err(ClaimError::Conflict | ClaimError::AlreadyClaimed)) => {
                self.clear_pending_room_acquisition(room_jid, identity);
                Err(self
                    .classify_claim_after_steal_conflict(entity, room_jid, identity, &snapshot)
                    .await)
            }
            Ok(Err(_)) => Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone())),
            Err(_) => Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone())),
        }
    }

    /// Classify a lost stale-owner CAS from a fresh claim-store read. The CAS
    /// contract folds several zero-row causes into `Conflict`: another owner
    /// may have renewed, the claim may have disappeared or changed generation,
    /// or this node's own lease may no longer authorize acquisition. Only the
    /// same observed foreign owner/epoch becoming fresh proves remote
    /// ownership; every other successful read is a retryable convergence race.
    async fn classify_claim_after_steal_conflict(
        &self,
        entity: &Entity,
        room_jid: &BareJid,
        identity: &NodeIdentity,
        observed: &ClaimSnapshot,
    ) -> RoomRegistryError {
        match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.claim_store.current_claim(entity),
        )
        .await
        {
            Ok(Ok(Some(current)))
                if current.owner_lease_fresh
                    && current.owner != *identity
                    && current.owner == observed.owner
                    && current.claim_epoch == observed.claim_epoch =>
            {
                RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone())
            }
            Ok(Ok(_)) => {
                debug!(
                    room = %room_jid,
                    "room claim changed while stealing stale ownership; deferring acquisition"
                );
                RoomRegistryError::OwnershipReconciliationPending(room_jid.clone())
            }
            Ok(Err(error)) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "room claim lookup failed after stale-owner steal conflict"
                );
                RoomRegistryError::OwnershipUnavailable(room_jid.clone())
            }
            Err(_) => {
                warn!(
                    room = %room_jid,
                    "room claim lookup timed out after stale-owner steal conflict"
                );
                RoomRegistryError::OwnershipUnavailable(room_jid.clone())
            }
        }
    }

    /// Two-part demotion protocol, part (a) (element 7): fire a
    /// best-effort, detached Demote notification at the node this room
    /// was just stolen from. Never awaited by the caller — the
    /// guaranteed backstop is the fenced pre-fan-out check
    /// `waddle-server`'s `dispatch_to_room` runs independently.
    fn notify_previous_owner_demoted(
        &self,
        room_jid: &BareJid,
        previous_owner: &NodeIdentity,
        new_epoch: ClaimEpoch,
    ) {
        let Some(store) = self.durable_store.clone() else {
            return;
        };
        let room_jid = room_jid.clone();
        let previous_owner = previous_owner.clone();
        tokio::spawn(async move {
            if let Err(error) = store
                .notify_previous_owner_demoted(
                    &room_jid,
                    &previous_owner.node_id,
                    &previous_owner.node_epoch,
                    new_epoch,
                )
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "best-effort Demote notification to the previous owner failed \
                     (the guaranteed fenced pre-fan-out backstop is unaffected)"
                );
            }
        });
    }

    /// Spawn a `RoomActor` for the given room and insert it into the map.
    ///
    /// When a [`MucDurableStore`] is configured, a
    /// [`RestoreDurableRoomState`] message is enqueued first (element 7:
    /// restore configuration/affiliations/subject before accepting any
    /// join), followed by [`HydrateDurableRecipients`] when a
    /// [`DurableMembershipSource`] is configured — both before the actor
    /// ref is handed to any caller, so every later message this
    /// incarnation processes observes fully-hydrated state (#1135's
    /// established FIFO-mailbox ordering guarantee, extended here).
    ///
    /// Returns a reference to the spawned actor.
    async fn spawn_room(
        &mut self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
        claim_fence: super::RoomClaimFenceContext,
    ) -> Result<ActorRef<RoomActor>, RoomRegistryError> {
        let actor_ref = self
            .prepare_room(room_jid.clone(), waddle_id, channel_id, config)
            .await;
        let owner = claim_fence.owner();
        let still_owned = matches!(
            tokio::time::timeout(
                ROOM_OWNERSHIP_CALL_TIMEOUT,
                self.claim_store
                    .fence(&claim_fence.entity, &owner, claim_fence.epoch),
            )
            .await,
            Ok(Ok(true))
        );
        let identity_guard = if still_owned {
            self.node_identity.guard_if_current(&owner).await
        } else {
            None
        };
        let Some(_identity_guard) = identity_guard else {
            actor_ref.kill();
            self.release_room_claim(&room_jid, &claim_fence).await;
            return Err(RoomRegistryError::OwnershipUnavailable(room_jid));
        };
        if !self.publish_room(room_jid.clone(), actor_ref.clone(), claim_fence) {
            return Err(RoomRegistryError::OwnershipReconciliationPending(room_jid));
        }
        Ok(actor_ref)
    }

    /// Spawn and enqueue all durable hydration work without making the actor
    /// discoverable through the registry. Reclaimed rooms use this split so
    /// ownership can be re-fenced after every enqueue await and immediately
    /// before publication.
    async fn prepare_room(
        &self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
    ) -> ActorRef<RoomActor> {
        let room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        let actor_ref = RoomActor::spawn(RoomActor::new(room, self.occupant_id_secret.clone()));
        if let Some(store) = &self.durable_store {
            if let Err(error) = actor_ref
                .tell(RestoreDurableRoomState {
                    store: Arc::clone(store),
                })
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to enqueue durable room-state restore for freshly \
                     spawned/re-claimed room actor"
                );
            }
        }
        if let Some(source) = &self.membership_source {
            if let Err(error) = actor_ref
                .tell(HydrateDurableRecipients {
                    source: Arc::clone(source),
                })
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to enqueue durable-recipient hydration for \
                     freshly spawned room actor"
                );
            }
        }
        actor_ref
    }

    fn publish_room(
        &mut self,
        room_jid: BareJid,
        actor_ref: ActorRef<RoomActor>,
        claim_fence: super::RoomClaimFenceContext,
    ) -> bool {
        if self
            .pending_room_releases
            .contains_key(&(room_jid.clone(), claim_fence.clone()))
        {
            actor_ref.kill();
            return false;
        }
        self.clear_pending_room_acquisition(&room_jid, &claim_fence.owner());
        if let Some(store) = &self.durable_store {
            store.record_claim_fence(&room_jid, claim_fence.clone());
        }
        self.rooms.insert(
            room_jid.clone(),
            RoomEntry {
                actor_ref: actor_ref.clone(),
                claim_fence: claim_fence.clone(),
            },
        );
        self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
        true
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 3 (council-adjudicated): `async` (not
    /// sync) so the dead-actor branch can release the Postgres claim
    /// before returning. Previously this removed the dead entry from
    /// `self.rooms` WITHOUT releasing its Postgres claim at all — an
    /// orphaned claim: Postgres kept attributing the room to this node
    /// (which no longer has a live actor for it, or any record of the
    /// epoch needed to release it, once `self.rooms.remove` ran) until
    /// this node's own liveness lease eventually looked stale to another
    /// node's `OwnerStale` steal. This capture-then-release closes that
    /// gap: the claim epoch is read BEFORE the entry is removed, and
    /// [`Self::release_room_claim`] runs on it — the exact same
    /// best-effort, epoch-gated release [`DestroyRoom`]'s handler already
    /// uses for the graceful-destroy path.
    async fn live_room(
        &mut self,
        room_jid: &BareJid,
    ) -> Result<Option<ActorRef<RoomActor>>, RoomRegistryError> {
        if self.poisoned_rooms.contains(room_jid) {
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        if let Some(entry) = self.rooms.get(room_jid) {
            if entry.actor_ref.is_alive() {
                return Ok(Some(entry.actor_ref.clone()));
            }
            let claim_fence = entry.claim_fence.clone();
            if !self.has_pending_release_capacity(room_jid, &claim_fence) {
                // Saturation must not make a dead map entry immortal. Give
                // the oldest exact responsibility one bounded retry; a
                // successful/NotOwned result frees the slot needed to retire
                // this actor, while persistent backend failure remains
                // bounded and simply defers this dead entry to a later call.
                self.retry_oldest_pending_room_release().await;
                if !self.has_pending_release_capacity(room_jid, &claim_fence) {
                    debug!(room = %room_jid, "Cannot retire dead RoomActor yet: exact-release retry backlog remains full after bounded redrive");
                    return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
                }
            }
            self.rooms.remove(room_jid);
            self.poisoned_rooms.insert(room_jid.clone());
            warn!(
                room = %room_jid,
                "Detected dead RoomActor; failing fast to avoid silent room state loss"
            );
            metrics::record_actor_restart("room_actor", "detected_dead_actor_fail_fast");
            // FIX 3: release BEFORE returning the error — a dead actor
            // whose claim is never released is a genuinely orphaned
            // claim (this node holds it in Postgres but has no way left
            // to act on it), not merely a "fail fast and let the caller
            // retry" situation.
            self.release_room_claim(room_jid, &claim_fence).await;
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        Ok(None)
    }

    /// Best-effort release of `room_jid`'s Postgres claim (dormancy
    /// eviction / explicit destroy, element 7's "graceful release").
    /// Epoch-gated and best-effort per [`ClaimStore::release`]'s own
    /// idempotent contract. A claim already stolen out from under this node
    /// is a successful no-op.
    async fn release_room_claim(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
    ) {
        self.release_room_claim_with_timeout(
            room_jid,
            claim_fence,
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            ClaimReleaseContext::Operational,
        )
        .await;
    }

    async fn release_room_claim_with_timeout(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
        timeout: std::time::Duration,
        context: ClaimReleaseContext,
    ) {
        let owner = claim_fence.owner();
        match tokio::time::timeout(
            timeout,
            self.claim_store
                .release_exact(&claim_fence.entity, &owner, claim_fence.epoch),
        )
        .await
        {
            Ok(Ok(ExactReleaseOutcome::Released | ExactReleaseOutcome::NotOwned)) => {
                self.clear_pending_room_release(room_jid, claim_fence);
                if let Some(store) = &self.durable_store {
                    store.forget_claim_fence(room_jid, claim_fence);
                }
            }
            Ok(Err(error)) => {
                match context {
                    ClaimReleaseContext::Operational => {
                        warn!(room = %room_jid, %error, "failed to release room ownership claim");
                    }
                    ClaimReleaseContext::PreAcquire => {
                        debug!(room = %room_jid, %error, "room claim release remains pending before acquisition");
                    }
                }
                if !self.remember_pending_room_release(room_jid.clone(), claim_fence.clone()) {
                    tracing::error!(room = %room_jid, "exact-release retry backlog saturated despite pre-admission guard; claim remains fenced for node-expiry recovery");
                }
            }
            Err(_) => {
                match context {
                    ClaimReleaseContext::Operational => {
                        warn!(room = %room_jid, "timed out releasing room ownership claim; retaining exact fence for a later retry");
                    }
                    ClaimReleaseContext::PreAcquire => {
                        debug!(room = %room_jid, "room claim release timed out before acquisition; retaining exact fence for background retry");
                    }
                }
                if !self.remember_pending_room_release(room_jid.clone(), claim_fence.clone()) {
                    tracing::error!(room = %room_jid, "exact-release retry backlog saturated despite pre-admission guard; claim remains fenced for node-expiry recovery");
                }
            }
        }
    }

    /// Bounded, observable release for a proactively reclaimed epoch. A
    /// backend error or timeout retains the exact epoch in the pending map;
    /// only a typed confirmation clears the durable fence cache and reports
    /// release or a lost race.
    async fn release_reclaimed_room_claim(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
        previous_owner: &NodeIdentity,
    ) -> ReclaimedRoomOutcome {
        self.release_reclaimed_room_claim_with_timeout(
            room_jid,
            claim_fence,
            previous_owner,
            RECLAIMED_ROOM_RELEASE_TIMEOUT,
        )
        .await
    }

    async fn release_reclaimed_room_claim_with_timeout(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
        previous_owner: &NodeIdentity,
        timeout: std::time::Duration,
    ) -> ReclaimedRoomOutcome {
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        match tokio::time::timeout(
            timeout,
            self.claim_store
                .release_exact(&entity, &claim_fence.owner(), claim_fence.epoch),
        )
        .await
        {
            Ok(Ok(ExactReleaseOutcome::Released)) => {
                self.clear_pending_reclaimed_room(room_jid, claim_fence);
                if let Some(store) = &self.durable_store {
                    store.forget_claim_fence(room_jid, claim_fence);
                }
                ReclaimedRoomOutcome::Released
            }
            Ok(Ok(ExactReleaseOutcome::NotOwned)) => {
                self.clear_pending_reclaimed_room(room_jid, claim_fence);
                if let Some(store) = &self.durable_store {
                    store.forget_claim_fence(room_jid, claim_fence);
                }
                ReclaimedRoomOutcome::LostRace
            }
            Ok(Err(error)) => {
                debug!(room = %room_jid, %error, "reclaimed-room claim release failed; retaining for retry");
                self.remember_pending_reclaimed_room(
                    room_jid.clone(),
                    claim_fence.clone(),
                    previous_owner.clone(),
                );
                ReclaimedRoomOutcome::PendingRetry
            }
            Err(_) => {
                debug!(room = %room_jid, "reclaimed-room claim release timed out; retaining for retry");
                self.remember_pending_reclaimed_room(
                    room_jid.clone(),
                    claim_fence.clone(),
                    previous_owner.clone(),
                );
                ReclaimedRoomOutcome::PendingRetry
            }
        }
    }
}

pub const MAX_PENDING_RECLAIMED_ROOMS: usize = 128;
pub const MAX_PENDING_ROOM_RELEASES: usize = 128;
pub const MAX_PENDING_ROOM_ACQUISITIONS: usize = 128;
const PENDING_ROOM_RETRY_BATCH: usize = 16;
const PENDING_ROOM_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

pub struct RetryPendingRoomReleases {
    pub limit: usize,
}

struct RetryPendingRoomWork {
    generation: u64,
    limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct PendingRoomReleaseBacklog {
    pub depth: usize,
    pub oldest_age_ms: u64,
}

pub struct GetPendingRoomReleaseBacklog;
pub struct ListPendingRoomReleaseJids;
pub struct IsCurrentRoomPendingRelease {
    pub room_jid: BareJid,
}
pub struct IsPendingRoomReleaseOnly {
    pub room_jid: BareJid,
}
pub struct IsCurrentIdentityPendingRoomReleaseOnly {
    pub room_jid: BareJid,
}

impl kameo::message::Message<ListPendingRoomReleaseJids> for RoomRegistryActor {
    type Reply = Vec<BareJid>;

    async fn handle(
        &mut self,
        _msg: ListPendingRoomReleaseJids,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut room_jids = self
            .pending_room_releases
            .keys()
            .map(|(room_jid, _)| room_jid.clone())
            .collect::<Vec<_>>();
        room_jids.sort();
        room_jids.dedup();
        room_jids
    }
}

impl kameo::message::Message<IsCurrentRoomPendingRelease> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsCurrentRoomPendingRelease,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid) else {
            return false;
        };
        self.pending_room_releases
            .contains_key(&(msg.room_jid, entry.claim_fence.clone()))
    }
}

impl kameo::message::Message<IsPendingRoomReleaseOnly> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsPendingRoomReleaseOnly,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.rooms
            .get(&msg.room_jid)
            .is_none_or(|entry| !entry.actor_ref.is_alive())
            && self
                .pending_room_releases
                .keys()
                .any(|(room_jid, _)| room_jid == &msg.room_jid)
    }
}

impl kameo::message::Message<IsCurrentIdentityPendingRoomReleaseOnly> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsCurrentIdentityPendingRoomReleaseOnly,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self
            .rooms
            .get(&msg.room_jid)
            .is_some_and(|entry| entry.actor_ref.is_alive())
        {
            return false;
        }
        let current_identity = self.node_identity.current();
        let mut pending = self
            .pending_room_releases
            .keys()
            .filter(|(room_jid, _)| room_jid == &msg.room_jid)
            .peekable();
        pending.peek().is_some()
            && pending.all(|(_, claim_fence)| claim_fence.owner() == current_identity)
    }
}

impl kameo::message::Message<GetPendingRoomReleaseBacklog> for RoomRegistryActor {
    type Reply = PendingRoomReleaseBacklog;

    async fn handle(
        &mut self,
        _msg: GetPendingRoomReleaseBacklog,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        PendingRoomReleaseBacklog {
            depth: self
                .pending_room_releases
                .len()
                .saturating_add(self.pending_room_acquisitions.len()),
            oldest_age_ms: self
                .pending_room_releases
                .values()
                .map(|pending| pending.first_pending_at.elapsed().as_millis() as u64)
                .chain(
                    self.pending_room_acquisitions
                        .values()
                        .map(|pending| pending.first_pending_at.elapsed().as_millis() as u64),
                )
                .max()
                .unwrap_or(0),
        }
    }
}

impl kameo::message::Message<RetryPendingRoomReleases> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        msg: RetryPendingRoomReleases,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let attempted = self.retry_pending_room_work(msg.limit).await;
        if self.has_pending_room_retry_work() {
            self.schedule_pending_room_retry(ctx.actor_ref());
        }
        attempted
    }
}

impl kameo::message::Message<RetryPendingRoomWork> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        msg: RetryPendingRoomWork,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.scheduled_pending_retry_generation != Some(msg.generation) {
            return 0;
        }
        self.scheduled_pending_retry_generation = None;
        let attempted = self.retry_pending_room_work(msg.limit).await;
        if self.has_pending_room_retry_work() {
            self.schedule_pending_room_retry(ctx.actor_ref());
        }
        attempted
    }
}

/// Wire the real, clustering-backed claim store/identity/durable store
/// into an already-spawned registry (ADR-0017 Phase 3 Slice 7).
///
/// Construction-order note: `clustering::start_if_enabled` (which
/// produces the real `ClaimStore`/`MucDurableStore`) runs *after* the
/// room registry is spawned in `server/mod.rs`, mirroring the exact
/// chicken-and-egg the `local_claims`/`resume_bridge` fill-in-later cells
/// already solve for Slices 5/6 — here realized as a message instead of
/// an `OnceLock`, since a kameo actor's state is only mutable through its
/// own mailbox. Sent once, before any client traffic can reach
/// `GetOrCreateRoom` (the HTTP/WebSocket listeners start after this
/// point in `server/mod.rs`).
pub struct WireClusteringClaims {
    pub claim_store: Arc<dyn ClaimStore>,
    pub node_identity: SharedNodeIdentity,
    pub durable_store: Option<Arc<dyn MucDurableStore>>,
    /// ADR-0017 Phase 3 Slice 10: `None` when clustering wiring predates
    /// this field's introduction — every existing/backward call site simply
    /// omits it via struct-update syntax at its own call site, which is a
    /// no-op behavior change (no backoff, exactly today's default).
    pub rollout_backoff: Option<Arc<dyn RolloutBackoff>>,
}

impl kameo::message::Message<WireClusteringClaims> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: WireClusteringClaims,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.claim_store = msg.claim_store;
        self.node_identity = msg.node_identity;
        self.durable_store = msg.durable_store;
        self.rollout_backoff = msg.rollout_backoff;
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Look up a room actor by JID.
pub struct GetRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<GetRoom> for RoomRegistryActor {
    type Reply = Result<Option<ActorRef<RoomActor>>, RoomRegistryError>;

    async fn handle(&mut self, msg: GetRoom, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.live_room(&msg.room_jid).await
    }
}

/// Whether a get-or-create request spawned the room or found it
/// already registered (#1134). The registry actor's serialized
/// mailbox guarantees exactly one caller per room lifetime observes
/// [`RoomCreation::Created`] — that caller is the XEP-0045 §10.1.1
/// room creator and the only one entitled to the creator Owner grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum RoomCreation {
    /// This request spawned the room actor: the caller created the room.
    Created,
    /// The room actor already existed.
    Existing,
}

/// Reply to [`GetOrCreateRoom`] / [`CreateInstantRoom`]: the room
/// actor plus the authoritative created-bit (#1134).
#[derive(Debug, Clone, kameo::Reply)]
pub struct RoomAcquisition {
    pub actor_ref: ActorRef<RoomActor>,
    pub creation: RoomCreation,
}

/// Result of reconciling a `RoomActor` claim proactively reclaimed by the
/// dead-node sweeper. This is intentionally typed and low-cardinality so the
/// caller can aggregate telemetry without logging once per room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum ReclaimedRoomOutcome {
    /// Durable room state existed and a local actor was spawned at the won
    /// claim epoch.
    Hydrated,
    /// Demand-side creation already installed a live actor under this exact
    /// fenced epoch; no duplicate was spawned.
    AlreadyLive,
    /// No durable room state existed, so the otherwise-unusable orphan claim
    /// was released for ordinary demand-side recreation.
    Released,
    /// The won epoch was no longer owned by this node when the registry
    /// serialized the adoption request.
    LostRace,
    /// This node may still own the epoch, but neither actor installation nor
    /// claim release was confirmed. The registry retained it for a bounded
    /// retry on a later orphan-reaper sweep.
    PendingRetry,
}

/// Adopt or release one exact `RoomActor` epoch won by the dead-node reaper.
/// Keeping this operation inside the registry actor serializes it against
/// every demand-side `GetOrCreateRoom` and prevents duplicate local actors.
pub struct ReconcileReclaimedRoom {
    pub room_jid: BareJid,
    pub claim_fence: super::RoomClaimFenceContext,
    pub previous_owner: NodeIdentity,
}

/// Internal budget for each durable/claim-store read in proactive room
/// adoption. It is shorter than the registry handle's five-second outer
/// reply timeout so the handler completes (including its release fallback)
/// before a caller can abandon an operation that is still mutating state.
const RECLAIMED_ROOM_STORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const RECLAIMED_ROOM_RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const ROOM_OWNERSHIP_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
/// Demand-side convergence gets one shared mailbox budget across reclaimed
/// and ordinary exact fences. Background retries keep the full per-call
/// timeout; admission returns retryably well before the five-second public
/// registry ask timeout.
const PRE_ACQUIRE_CONVERGENCE_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Clone, Copy)]
enum ClaimReleaseContext {
    Operational,
    PreAcquire,
}

/// One typed pending entry returned to the reaper for retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReclaimedRoom {
    pub room_jid: BareJid,
    pub claim_fence: super::RoomClaimFenceContext,
    pub previous_owner: NodeIdentity,
}

/// List a bounded page of won-but-unserved epochs. Selection rotates the
/// returned entries to the back of the retry order so a permanently failing
/// full page cannot starve later rooms. The caller retries each item through
/// [`ReconcileReclaimedRoom`] as a separate mailbox message so no batch can
/// monopolize the registry actor.
pub struct ListPendingReclaimedRooms {
    pub limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct PendingReclaimedRoomBacklog {
    pub depth: usize,
    pub oldest_age_ms: u64,
}

pub struct GetPendingReclaimedRoomBacklog;

/// Terminally release every won-but-unserved room epoch currently registered
/// in this actor, including exact post-CAS handoffs retained by the orphan
/// reaper supervisor when its sweep was cancelled before mailbox delivery.
/// The handler imports those handoffs and disables future room-claim
/// acquisition in the same mailbox turn, so queued demand cannot race an
/// out-of-actor release.
pub struct DrainPendingReclaimedRoomsForShutdown {
    pub pending_handoffs: Vec<PendingReclaimedRoom>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct PendingReclaimedRoomDrainOutcome {
    pub released: usize,
    pub preserved_live: usize,
    pub retained: usize,
}

impl kameo::message::Message<GetPendingReclaimedRoomBacklog> for RoomRegistryActor {
    type Reply = PendingReclaimedRoomBacklog;

    async fn handle(
        &mut self,
        _msg: GetPendingReclaimedRoomBacklog,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let oldest_age_ms = self
            .pending_reclaimed_rooms
            .values()
            .map(|pending| pending.first_pending_at.elapsed().as_millis() as u64)
            .max()
            .unwrap_or(0);
        PendingReclaimedRoomBacklog {
            depth: self.pending_reclaimed_rooms.len() + self.pending_reclaimed_reservations.len(),
            oldest_age_ms,
        }
    }
}

impl kameo::message::Message<DrainPendingReclaimedRoomsForShutdown> for RoomRegistryActor {
    type Reply = PendingReclaimedRoomDrainOutcome;

    async fn handle(
        &mut self,
        msg: DrainPendingReclaimedRoomsForShutdown,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.terminal_claim_acquisition_disabled = true;
        for pending in msg.pending_handoffs {
            self.remember_pending_reclaimed_room(
                pending.room_jid,
                pending.claim_fence,
                pending.previous_owner,
            );
        }
        // A steal CAS can commit while cancellation drops its response
        // future, leaving only the pre-CAS bare-JID reservation. Check those
        // reservations after acquisition is disabled and while the actor
        // mailbox excludes demand-side publication. A positive self-owned
        // snapshot yields an exact fence that can be released or matched to
        // a live actor. An absent/foreign snapshot is not authoritative: the
        // dropped CAS may still commit after this read, so its reservation
        // must survive for node-expiry recovery.
        let reserved_rooms = self
            .pending_reclaimed_reservations
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let owner = self.node_identity.current();
        let claim_store = Arc::clone(&self.claim_store);
        let reservation_claims =
            futures::future::join_all(reserved_rooms.into_iter().map(|room_jid| {
                let claim_store = Arc::clone(&claim_store);
                async move {
                    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
                    let current = tokio::time::timeout(
                        ROOM_OWNERSHIP_CALL_TIMEOUT,
                        claim_store.current_claim_after_pending_writes(&entity),
                    )
                    .await;
                    (room_jid, entity, current)
                }
            }))
            .await;

        let mut preserved_live = 0usize;
        let mut retained = 0usize;
        let mut reservation_owned = Vec::new();
        for (room_jid, entity, current) in reservation_claims {
            match current {
                Ok(Ok(Some(snapshot))) if snapshot.owner.same_incarnation(&owner) => {
                    let claim_owner = snapshot.owner;
                    let claim_fence = super::RoomClaimFenceContext::new(
                        entity,
                        claim_owner,
                        snapshot.claim_epoch,
                    );
                    if self.has_live_room_with_fence(&room_jid, &claim_fence) {
                        self.pending_reclaimed_reservations.remove(&room_jid);
                        preserved_live += 1;
                    } else {
                        self.transfer_reclaimed_reservation_to_pending_release(
                            room_jid.clone(),
                            claim_fence.clone(),
                        );
                        reservation_owned.push((room_jid, claim_fence));
                    }
                }
                Ok(Ok(_)) => {
                    warn!(room = %room_jid, "terminal reclaimed-room reservation remains ambiguous after a non-self snapshot; retaining for node-expiry recovery");
                    retained += 1;
                }
                Ok(Err(error)) => {
                    warn!(room = %room_jid, %error, "terminal reclaimed-room reservation lookup failed; retaining for node-expiry recovery");
                    retained += 1;
                }
                Err(_) => {
                    warn!(room = %room_jid, "terminal reclaimed-room reservation lookup timed out; retaining for node-expiry recovery");
                    retained += 1;
                }
            }
        }
        let duplicate_live = self
            .pending_reclaimed_rooms
            .keys()
            .filter(|(room_jid, claim_fence)| self.has_live_room_with_fence(room_jid, claim_fence))
            .cloned()
            .collect::<Vec<_>>();
        for (room_jid, claim_fence) in &duplicate_live {
            self.clear_pending_reclaimed_room(room_jid, claim_fence);
        }
        preserved_live += duplicate_live.len();
        let mut pending = self
            .pending_reclaimed_rooms
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        pending.extend(reservation_owned);
        let claim_store = Arc::clone(&self.claim_store);
        let outcomes =
            futures::future::join_all(pending.into_iter().map(|(room_jid, claim_fence)| {
                let claim_store = Arc::clone(&claim_store);
                async move {
                    let owner = claim_fence.owner();
                    let outcome = tokio::time::timeout(
                        RECLAIMED_ROOM_RELEASE_TIMEOUT,
                        claim_store.release_exact(&claim_fence.entity, &owner, claim_fence.epoch),
                    )
                    .await;
                    (room_jid, claim_fence, outcome)
                }
            }))
            .await;

        let mut released = 0usize;
        for (room_jid, claim_fence, outcome) in outcomes {
            match outcome {
                Ok(Ok(ExactReleaseOutcome::Released | ExactReleaseOutcome::NotOwned)) => {
                    self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
                    self.clear_pending_room_release(&room_jid, &claim_fence);
                    self.pending_reclaimed_reservations.remove(&room_jid);
                    if let Some(store) = &self.durable_store {
                        store.forget_claim_fence(&room_jid, &claim_fence);
                    }
                    released += 1;
                }
                Ok(Err(error)) => {
                    warn!(room = %room_jid, %error, "terminal reclaimed-room release failed; retaining exact fence until node expiry");
                    retained += 1;
                }
                Err(_) => {
                    warn!(room = %room_jid, "terminal reclaimed-room release timed out; retaining exact fence until node expiry");
                    retained += 1;
                }
            }
        }
        PendingReclaimedRoomDrainOutcome {
            released,
            preserved_live,
            retained,
        }
    }
}

/// Record a newly won epoch before starting any fallible adoption work.
/// Idempotent for repeated delivery of the same `(room, epoch)`.
pub struct RememberPendingReclaimedRoom {
    pub room_jid: BareJid,
    pub claim_fence: super::RoomClaimFenceContext,
    pub previous_owner: NodeIdentity,
}

pub struct ReservePendingReclaimedRoom {
    pub room_jid: BareJid,
}

pub struct CancelPendingReclaimedRoomReservation {
    pub room_jid: BareJid,
}

impl kameo::message::Message<ReservePendingReclaimedRoom> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: ReservePendingReclaimedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.terminal_claim_acquisition_disabled {
            return false;
        }
        if self.pending_reclaimed_reservations.contains(&msg.room_jid) {
            return true;
        }
        if self.pending_reclaimed_rooms.len() + self.pending_reclaimed_reservations.len()
            >= MAX_PENDING_RECLAIMED_ROOMS
        {
            return false;
        }
        self.pending_reclaimed_reservations.insert(msg.room_jid);
        true
    }
}

impl kameo::message::Message<CancelPendingReclaimedRoomReservation> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CancelPendingReclaimedRoomReservation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        self.pending_reclaimed_reservations.remove(&msg.room_jid);
    }
}

impl kameo::message::Message<RememberPendingReclaimedRoom> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RememberPendingReclaimedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.remember_pending_reclaimed_room(msg.room_jid, msg.claim_fence, msg.previous_owner);
    }
}

impl kameo::message::Message<ListPendingReclaimedRooms> for RoomRegistryActor {
    type Reply = Vec<PendingReclaimedRoom>;

    async fn handle(
        &mut self,
        msg: ListPendingReclaimedRooms,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut entries: Vec<_> = self
            .pending_reclaimed_rooms
            .iter()
            .map(|((room_jid, claim_fence), pending)| {
                (room_jid.clone(), claim_fence.clone(), pending.clone())
            })
            .collect();
        entries.sort_by_key(|(_, _, pending)| pending.retry_order);
        entries.truncate(msg.limit);
        let mut selected = Vec::with_capacity(entries.len());
        for (room_jid, claim_fence, pending) in entries {
            self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
            if let Some(current) = self
                .pending_reclaimed_rooms
                .get_mut(&(room_jid.clone(), claim_fence))
            {
                current.retry_order = self.pending_retry_order;
            }
            selected.push(PendingReclaimedRoom {
                room_jid: room_jid.clone(),
                claim_fence: pending.claim_fence,
                previous_owner: pending.previous_owner,
            });
        }
        selected
    }
}

impl kameo::message::Message<ReconcileReclaimedRoom> for RoomRegistryActor {
    type Reply = ReclaimedRoomOutcome;

    async fn handle(
        &mut self,
        msg: ReconcileReclaimedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let entity = Entity::new(EntityType::RoomActor, msg.room_jid.to_string());
        let claim_fence = msg.claim_fence.clone();
        let claim_epoch = claim_fence.epoch;
        let identity = claim_fence.owner();
        if self.terminal_claim_acquisition_disabled {
            if self.has_live_room_with_fence(&msg.room_jid, &claim_fence) {
                self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                return ReclaimedRoomOutcome::AlreadyLive;
            }
            return self
                .release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                .await;
        }
        if self.node_identity.current() != identity {
            return self
                .release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                .await;
        }

        // A delayed or duplicate reclaimed-room message can name an exact
        // generation whose terminal release already timed out. Its database
        // delete may still commit after the caller future was dropped, so
        // never make that same generation live again. Retain both retry
        // responsibilities until the exact release reaches a typed outcome.
        if self
            .pending_room_releases
            .contains_key(&(msg.room_jid.clone(), claim_fence.clone()))
        {
            if self
                .rooms
                .get(&msg.room_jid)
                .is_some_and(|entry| entry.claim_fence == claim_fence)
            {
                if let Some(entry) = self.rooms.remove(&msg.room_jid) {
                    entry.actor_ref.kill();
                }
            }
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ReclaimedRoomOutcome::PendingRetry;
        }

        // Prove the reaper's exact epoch before touching any local actor.
        // A stale adoption message must never depose a newer demand-side
        // actor, and a backend error is uncertainty, not permission.
        let still_owned = match tokio::time::timeout(
            RECLAIMED_ROOM_STORE_TIMEOUT,
            self.claim_store.fence(&entity, &identity, claim_epoch),
        )
        .await
        {
            Ok(Ok(held)) => held,
            Ok(Err(error)) => {
                debug!(room = %msg.room_jid, %error, "reclaimed-room ownership fence failed");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
            Err(_) => {
                debug!(room = %msg.room_jid, "reclaimed-room ownership fence timed out");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
        };
        if !still_owned {
            if self
                .rooms
                .get(&msg.room_jid)
                .is_some_and(|entry| entry.claim_fence == claim_fence)
            {
                if let Some(entry) = self.rooms.remove(&msg.room_jid) {
                    entry.actor_ref.kill();
                }
            }
            if let Some(store) = &self.durable_store {
                store.forget_claim_fence(&msg.room_jid, &claim_fence);
            }
            self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
            return ReclaimedRoomOutcome::LostRace;
        }

        if let Some(entry) = self.rooms.get(&msg.room_jid).cloned() {
            if entry.actor_ref.is_alive() {
                if entry.claim_fence == claim_fence {
                    self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                    return ReclaimedRoomOutcome::AlreadyLive;
                }
                // Never transplant a live actor onto a new epoch. An earlier
                // mailbox mutation may still be running under the old fence;
                // changing only the registry/store fence would let that actor
                // retain memory which was never durably authorized under the
                // new owner. Replace it and hydrate a clean actor from a
                // freshly fenced durable snapshot instead.
                entry.actor_ref.kill();
            }
            self.rooms.remove(&msg.room_jid);
            if let Some(store) = &self.durable_store {
                store.forget_claim_fence(&msg.room_jid, &entry.claim_fence);
            }
        }

        let Some(store) = self.durable_store.clone() else {
            return self
                .release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                .await;
        };
        let snapshot = match tokio::time::timeout(
            RECLAIMED_ROOM_STORE_TIMEOUT,
            store.load_room_state(&msg.room_jid),
        )
        .await
        {
            Ok(Ok(Some(snapshot))) => snapshot,
            Ok(Ok(None)) => {
                return self
                    .release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                    .await;
            }
            Ok(Err(error)) => {
                debug!(
                    room = %msg.room_jid,
                    %error,
                    "failed to load proactively reclaimed room state; retaining for retry"
                );
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
            Err(_) => {
                debug!(room = %msg.room_jid, "proactively reclaimed room-state load timed out");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
        };

        // The durable read above is an await point long enough for another
        // node to expire this node and steal the epoch. Re-prove exact
        // ownership immediately before publishing a live actor; the earlier
        // fence only authorized reading the snapshot, not a later install.
        match tokio::time::timeout(
            RECLAIMED_ROOM_STORE_TIMEOUT,
            self.claim_store.fence(&entity, &identity, claim_epoch),
        )
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                store.forget_claim_fence(&msg.room_jid, &claim_fence);
                self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                return ReclaimedRoomOutcome::LostRace;
            }
            Ok(Err(error)) => {
                debug!(room = %msg.room_jid, %error, "final reclaimed-room ownership fence failed");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
            Err(_) => {
                debug!(room = %msg.room_jid, "final reclaimed-room ownership fence timed out");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
        }

        if self.node_identity.current() != identity {
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ReclaimedRoomOutcome::PendingRetry;
        }

        let actor_ref = self
            .prepare_room(
                msg.room_jid.clone(),
                snapshot.waddle_id,
                snapshot.channel_id,
                snapshot.config,
            )
            .await;
        // `prepare_room` awaits both mailbox enqueues. Fence once more at
        // the actual publication boundary so neither an identity change nor
        // an epoch steal during those awaits can install a stale actor.
        let publish_owned = tokio::time::timeout(
            RECLAIMED_ROOM_STORE_TIMEOUT,
            self.claim_store.fence(&entity, &identity, claim_epoch),
        )
        .await;
        match publish_owned {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                actor_ref.kill();
                if let Some(store) = &self.durable_store {
                    store.forget_claim_fence(&msg.room_jid, &claim_fence);
                }
                self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                return ReclaimedRoomOutcome::LostRace;
            }
            Ok(Err(_)) | Err(_) => {
                actor_ref.kill();
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ReclaimedRoomOutcome::PendingRetry;
            }
        }
        let identity_handle = self.node_identity.clone();
        let Some(_identity_guard) = identity_handle.guard_if_current(&identity).await else {
            actor_ref.kill();
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ReclaimedRoomOutcome::PendingRetry;
        };
        self.poisoned_rooms.remove(&msg.room_jid);
        if !self.publish_room(msg.room_jid.clone(), actor_ref, claim_fence.clone()) {
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ReclaimedRoomOutcome::PendingRetry;
        }
        ReclaimedRoomOutcome::Hydrated
    }
}

/// Get an existing room or create one if it does not exist.
pub struct GetOrCreateRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
}

impl kameo::message::Message<GetOrCreateRoom> for RoomRegistryActor {
    type Reply = Result<RoomAcquisition, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.live_room(&msg.room_jid).await? {
            debug!(room = %msg.room_jid, "Room already exists");
            return Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Existing,
            });
        }

        let claim_fence = self
            .acquire_room_claim(&msg.room_jid, ctx.actor_ref())
            .await?;
        info!(room = %msg.room_jid, "Creating new room via GetOrCreateRoom");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(
                msg.room_jid,
                msg.waddle_id,
                msg.channel_id,
                msg.config,
                claim_fence,
            )
            .await?;
        Ok(RoomAcquisition {
            actor_ref,
            creation: RoomCreation::Created,
        })
    }
}

/// Create an instant room per XEP-0045.
pub struct CreateInstantRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<CreateInstantRoom> for RoomRegistryActor {
    type Reply = Result<RoomAcquisition, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: CreateInstantRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.live_room(&msg.room_jid).await? {
            return Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Existing,
            });
        }

        let room_local = msg
            .room_jid
            .node()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "instant".to_string());
        let waddle_id = format!("instant:{}", room_local);
        let channel_id = room_local.clone();
        let config = RoomConfig {
            name: room_local,
            members_only: false,
            persistent: false,
            ..RoomConfig::default()
        };

        let claim_fence = self
            .acquire_room_claim(&msg.room_jid, ctx.actor_ref())
            .await?;
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(msg.room_jid, waddle_id, channel_id, config, claim_fence)
            .await?;
        Ok(RoomAcquisition {
            actor_ref,
            creation: RoomCreation::Created,
        })
    }
}

/// Create a room. Fails if a room with the same JID already exists.
pub struct CreateRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
}

impl kameo::message::Message<CreateRoom> for RoomRegistryActor {
    type Reply = Result<ActorRef<RoomActor>, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: CreateRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.live_room(&msg.room_jid).await?.is_some() {
            return Err(RoomRegistryError::RoomAlreadyExists(msg.room_jid));
        }

        let claim_fence = self
            .acquire_room_claim(&msg.room_jid, ctx.actor_ref())
            .await?;
        info!(room = %msg.room_jid, "Creating new room");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(
                msg.room_jid,
                msg.waddle_id,
                msg.channel_id,
                msg.config,
                claim_fence,
            )
            .await?;
        Ok(actor_ref)
    }
}

/// Why a room is being removed from the registry — decides whether the
/// durable rows go with it (#1261 vs. the deposed-node eviction race).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroyRoomReason {
    /// XEP-0045 §10.9 destroy or an administrative deletion: the room
    /// ceases to exist, so its durable rows (config, subject,
    /// affiliations incl. bans) are deleted with it.
    Destroy,
    /// A write-adjacent fence proved this process can no longer serve the
    /// room, either because the database claim moved or the local node
    /// identity rotated. Bypass release-backlog admission so the deposed
    /// actor cannot remain registered, preserve durable state, and still
    /// attempt exact release of the old fence.
    DeposedEviction,
}

/// Outcome of a [`DestroyRoom`] ask. Split four ways because callers
/// must distinguish "the room simply was not registered" (fine for
/// admin deletion of a dormant room) from "the fenced durable wipe
/// failed and the room was deliberately kept alive for a retry"
/// (which MUST fail the caller's operation — acknowledging it would
/// leave rows that resurrect the room).
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum DestroyRoomOutcome {
    /// The room existed and was removed (durable rows wiped when the
    /// reason was [`DestroyRoomReason::Destroy`]).
    Destroyed,
    /// No live or poisoned entry for this JID existed.
    NotRegistered,
    /// The epoch-fenced durable delete failed; the registry entry was
    /// restored and nothing was destroyed.
    DurableWipeFailed,
    /// The bounded exact-release retry set is full, so the registry kept the
    /// actor and claim intact rather than losing responsibility for the fence.
    ReleaseBacklogFull,
}

/// Destroy a room, removing it from the registry.
pub struct DestroyRoom {
    pub room_jid: BareJid,
    pub reason: DestroyRoomReason,
}

impl kameo::message::Message<DestroyRoom> for RoomRegistryActor {
    type Reply = DestroyRoomOutcome;

    async fn handle(
        &mut self,
        msg: DestroyRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.reason != DestroyRoomReason::DeposedEviction
            && self.rooms.contains_key(&msg.room_jid)
            && !self
                .has_pending_release_capacity(&msg.room_jid, &self.rooms[&msg.room_jid].claim_fence)
        {
            return DestroyRoomOutcome::ReleaseBacklogFull;
        }
        let removed_entry = self.rooms.remove(&msg.room_jid);
        let removed_room = removed_entry.is_some();
        let removed_poison = self.poisoned_rooms.remove(&msg.room_jid);
        // XEP-0045 §10.9 (#1261): destroy removes the room "even if it
        // was defined as persistent" — wipe the durable rows (config,
        // subject, affiliations incl. bans) so the room cannot
        // resurrect from storage on the next join. Runs BEFORE the
        // claim release below: the delete is epoch-fenced against this
        // node's still-held claim (a fencing loss means another node
        // owns the room now and this node must not wipe the new
        // owner's rows). A failed delete FAILS the destroy: the entry
        // is restored and `false` returned, so a caller never
        // acknowledges a destruction whose durable state survived.
        // `DeposedEviction` never wipes — the
        // room lives on under its new owner.
        if (removed_room || removed_poison) && msg.reason == DestroyRoomReason::Destroy {
            if let Some(store) = &self.durable_store {
                if let Err(error) = store.delete_room_state(&msg.room_jid).await {
                    warn!(
                        room = %msg.room_jid,
                        %error,
                        "Failed to delete durable room state; refusing the destroy so it \
                         can be retried instead of resurrecting from storage"
                    );
                    if let Some(entry) = removed_entry {
                        self.rooms.insert(msg.room_jid.clone(), entry);
                    }
                    if removed_poison {
                        self.poisoned_rooms.insert(msg.room_jid.clone());
                    }
                    return DestroyRoomOutcome::DurableWipeFailed;
                }
            }
        }
        // ADR-0017 Phase 3 Slice 7: release the Postgres claim on every
        // terminal path (explicit destroy, dormancy-eviction sweep) —
        // "graceful release" per element 7. A poisoned-only removal (the
        // actor died and was already detected by `live_room`) has no
        // known epoch to release; the claim is instead reclaimed by
        // another node's `OwnerStale` steal once this node's own liveness
        // lease is what it takes to look stale (bounded residual gap,
        // same class as FIX 6e's fail-open-detach gap).
        if let Some(entry) = removed_entry {
            if msg.reason == DestroyRoomReason::DeposedEviction {
                // Removal from the registry is not itself a terminal signal
                // to a RoomActor. A caller may still hold an ActorRef and the
                // best-effort Demote notification may never arrive, so kill
                // the deposed incarnation before dropping our last entry.
                self.retire_ownership_lost_entry(&msg.room_jid, entry).await;
            } else {
                self.release_room_claim(&msg.room_jid, &entry.claim_fence)
                    .await;
            }
        }
        if removed_room || removed_poison {
            info!(room = %msg.room_jid, "Destroyed room");
            DestroyRoomOutcome::Destroyed
        } else {
            warn!(room = %msg.room_jid, "Attempted to destroy non-existent room");
            DestroyRoomOutcome::NotRegistered
        }
    }
}

/// Destroy a room only if it is still inactive (#1108).
///
/// Replaces the janitor's unconditional [`DestroyRoom`] for eviction
/// paths. Inside this serialized registry handler, the room actor is
/// asked to seal itself if it is still inactive at
/// `expected_occupancy_revision` ([`SealIfInactive`]); the room
/// actor's mailbox serializes that check against joins, so a join that
/// raced the caller's dormancy probe either bumped the revision
/// (→ seal refused) or is queued behind the seal and gets the typed
/// [`RoomActorError::RoomSealed`](super::room_actor::RoomActorError::RoomSealed)
/// refusal, which the join path retries through the registry.
///
/// Returns `true` when the room was sealed and removed.
pub struct DestroyRoomIfInactive {
    pub room_jid: BareJid,
    pub expected_occupancy_revision: u64,
    pub guard: SealGuard,
}

/// Bound for the in-handler seal ask so a wedged room actor cannot
/// wedge the whole registry: shorter than
/// [`ROOM_REGISTRY_REPLY_TIMEOUT`](super::room_registry_handle::ROOM_REGISTRY_REPLY_TIMEOUT)
/// so the outer registry ask still gets a reply.
const SEAL_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl kameo::message::Message<DestroyRoomIfInactive> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DestroyRoomIfInactive,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        if !self.has_pending_release_capacity(&msg.room_jid, &entry.claim_fence) {
            // Ordinary inactivity must remain open when there is nowhere to
            // retain an uncertain release. A previously deposed actor is
            // different: its negative fence is already terminal, so evict it
            // without issuing another release even under saturation.
            let seal_state = entry
                .actor_ref
                .ask(GetRoomSealState)
                .mailbox_timeout(SEAL_ASK_TIMEOUT)
                .reply_timeout(SEAL_ASK_TIMEOUT)
                .await;
            return match seal_state {
                Ok(RoomSealState::OwnershipLost) => {
                    self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                    info!(room = %msg.room_jid, "Evicted deposed room during inactive-room cleanup");
                    true
                }
                Ok(RoomSealState::Open | RoomSealState::Inactive) => {
                    warn!(room = %msg.room_jid, "Skipping inactive-room seal because exact-release retry backlog is full");
                    false
                }
                Err(error) => {
                    warn!(room = %msg.room_jid, error = ?error, "Could not classify room seal while exact-release retry backlog is full");
                    false
                }
            };
        }
        let sealed = entry
            .actor_ref
            .ask(SealIfInactive {
                expected_occupancy_revision: msg.expected_occupancy_revision,
                guard: msg.guard,
            })
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await;
        match sealed {
            Ok(SealIfInactiveOutcome::OwnershipLost) => {
                self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                info!(room = %msg.room_jid, "Evicted deposed room during inactive-room cleanup");
                true
            }
            Ok(SealIfInactiveOutcome::Inactive) => {
                self.rooms.remove(&msg.room_jid);
                self.poisoned_rooms.remove(&msg.room_jid);
                // ADR-0017 Phase 3 Slice 7: this is a terminal removal from
                // `self.rooms` exactly like `DestroyRoom` — release the
                // Postgres claim here too, or every guarded dormancy-evicted
                // room leaks its claim until this node's own liveness lease
                // looks stale to another node's `OwnerStale` steal.
                self.release_room_claim(&msg.room_jid, &entry.claim_fence)
                    .await;
                info!(room = %msg.room_jid, "Destroyed inactive room (guarded)");
                true
            }
            Ok(SealIfInactiveOutcome::Refused) => {
                debug!(
                    room = %msg.room_jid,
                    "Guarded destroy refused: room no longer inactive at expected revision"
                );
                false
            }
            Err(error) => {
                // Never remove on uncertainty. If the seal actually
                // landed but the reply was lost, the seal is idempotent
                // and the next sweep converges (a sealed room reports
                // dormant and re-confirms the seal).
                warn!(
                    room = %msg.room_jid,
                    error = ?error,
                    "Guarded destroy seal ask failed; keeping the room"
                );
                false
            }
        }
    }
}

/// Purge a sealed room actor that is still registered (#1108
/// follow-up): when [`DestroyRoomIfInactive`]'s seal ask times out but
/// the queued [`SealIfInactive`] lands anyway, the actor stays in the
/// map, sealed, refusing every join. The join retry path sends this
/// before re-running get-or-create so the room respawns immediately
/// instead of waiting for the next janitor sweep.
///
/// Returns `true` when a sealed (or dead) actor was removed. Never
/// removes on uncertainty: a timeout or an unsealed reply keeps the
/// room.
pub struct ReapSealedRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<ReapSealedRoom> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: ReapSealedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        if !entry.actor_ref.is_alive() {
            if !self.has_pending_release_capacity(&msg.room_jid, &entry.claim_fence) {
                return false;
            }
            self.rooms.remove(&msg.room_jid);
            self.poisoned_rooms.remove(&msg.room_jid);
            // ADR-0017 Phase 3 Slice 7: same terminal-removal claim release
            // as the `live_room` dead-actor path and `DestroyRoom`.
            self.release_room_claim(&msg.room_jid, &entry.claim_fence)
                .await;
            info!(room = %msg.room_jid, "Reaped dead room actor during sealed-room purge");
            return true;
        }
        let seal_state = entry
            .actor_ref
            .ask(GetRoomSealState)
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await;
        match seal_state {
            Ok(RoomSealState::OwnershipLost) => {
                // A definitive join fence proved this actor is deposed. Its
                // local registration is no longer safe to serve. Remove and
                // kill locally, preserve durable room state, and do not issue
                // a redundant release that could become an untracked late
                // delete while the retry inventory is saturated.
                self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                info!(
                    room = %msg.room_jid,
                    "Evicted room actor after join fence proved ownership loss"
                );
                true
            }
            Ok(RoomSealState::Inactive) => {
                if !self.has_pending_release_capacity(&msg.room_jid, &entry.claim_fence) {
                    return false;
                }
                self.rooms.remove(&msg.room_jid);
                self.poisoned_rooms.remove(&msg.room_jid);
                // ADR-0017 Phase 3 Slice 7: same terminal-removal claim
                // release as the guarded-destroy path above.
                self.release_room_claim(&msg.room_jid, &entry.claim_fence)
                    .await;
                info!(
                    room = %msg.room_jid,
                    "Reaped sealed room actor left by a timed-out guarded destroy"
                );
                true
            }
            Ok(RoomSealState::Open) => false,
            Err(error) => {
                warn!(
                    room = %msg.room_jid,
                    error = ?error,
                    "Sealed-room probe failed; keeping the room"
                );
                false
            }
        }
    }
}

/// Check whether a room exists.
pub struct RoomExists {
    pub room_jid: BareJid,
}

impl kameo::message::Message<RoomExists> for RoomRegistryActor {
    type Reply = Result<bool, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: RoomExists,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.live_room(&msg.room_jid).await?.is_some())
    }
}

/// Check whether a bare JID belongs to this MUC service domain.
pub struct IsMucJid {
    pub jid: BareJid,
}

impl kameo::message::Message<IsMucJid> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsMucJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        msg.jid.domain().as_str() == self.muc_domain
    }
}

/// List all room JIDs.
pub struct ListRooms;

impl kameo::message::Message<ListRooms> for RoomRegistryActor {
    type Reply = Result<Vec<BareJid>, RoomRegistryError>;

    async fn handle(
        &mut self,
        _msg: ListRooms,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_ids: Vec<BareJid> = self.rooms.keys().cloned().collect();
        let mut live_rooms = Vec::with_capacity(room_ids.len());
        for room_jid in room_ids {
            match self.live_room(&room_jid).await {
                Ok(Some(_)) => live_rooms.push(room_jid),
                Ok(None) | Err(RoomRegistryError::RoomActorStateLost(_)) => {
                    // Ignore stale/dead rooms in discovery listing; per-room
                    // operations still fail fast with RoomActorStateLost.
                }
                Err(error) => return Err(error),
            }
        }
        Ok(live_rooms)
    }
}

/// List live or terminal-release room claims belonging to one exact owner.
pub struct ListRoomsOwnedBy {
    pub owner: NodeIdentity,
}

/// Hard-kill and forget a room only while its live entry still belongs to
/// `owner`. The comparison and mutation share the registry mailbox turn, so
/// a fresh same-JID replacement cannot be demoted by a stale owner sweep.
pub struct DemoteRoomIfOwner {
    pub room_jid: BareJid,
    pub owner: NodeIdentity,
}

impl kameo::message::Message<DemoteRoomIfOwner> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DemoteRoomIfOwner,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let matches = self
            .rooms
            .get(&msg.room_jid)
            .is_some_and(|entry| entry.claim_fence.owner() == msg.owner);
        if !matches {
            return false;
        }
        let Some(entry) = self.rooms.remove(&msg.room_jid) else {
            return false;
        };
        entry.actor_ref.kill();
        if let Some(store) = &self.durable_store {
            store.forget_claim_fence(&msg.room_jid, &entry.claim_fence);
        }
        true
    }
}

impl kameo::message::Message<ListRoomsOwnedBy> for RoomRegistryActor {
    type Reply = Result<Vec<BareJid>, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: ListRoomsOwnedBy,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut room_jids = self
            .rooms
            .iter()
            .filter(|(_, entry)| entry.claim_fence.owner() == msg.owner)
            .map(|(jid, _)| jid.clone())
            .collect::<Vec<_>>();
        room_jids.extend(
            self.pending_room_releases
                .keys()
                .filter(|(_, fence)| fence.owner() == msg.owner)
                .map(|(jid, _)| jid.clone()),
        );
        room_jids.extend(
            self.pending_reclaimed_rooms
                .keys()
                .filter(|(_, fence)| fence.owner() == msg.owner)
                .map(|(jid, _)| jid.clone()),
        );
        room_jids.sort();
        room_jids.dedup();
        Ok(room_jids)
    }
}

/// Return the number of active rooms.
pub struct RoomCount;

impl kameo::message::Message<RoomCount> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: RoomCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.rooms.len()
    }
}

/// Test-only message whose handler never returns, used to deterministically
/// exercise the [`RoomRegistry`](crate::muc::room_registry_handle::RoomRegistry)
/// reply-timeout path (the #757 wedge) under `tokio::time` pause/advance.
#[cfg(test)]
pub(crate) struct HangForever;

#[cfg(test)]
impl kameo::message::Message<HangForever> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: HangForever,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Park forever: the registry's single-consumer loop is blocked here,
        // mirroring a wedged handler. The caller must rely on its reply timeout.
        std::future::pending::<()>().await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
