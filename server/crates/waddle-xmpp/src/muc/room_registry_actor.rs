//! MUC Room Registry Actor
//!
//! Kameo actor that manages all MUC room actors. Replaced the DashMap-based
//! legacy registry (deleted in #1136) with a single-writer actor that owns
//! the room map and spawns per-room `RoomActor` instances on demand.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::BoxSendError;
use kameo::message::{BoxReply, Context};
use kameo::reply::{DelegatedReply, ReplySender};
use kameo::{Actor, Reply};
use thiserror::Error;
use tracing::{debug, info, warn};

use super::affiliation::DurableMembershipSource;
use super::durable::{ChannelId, MucDurableStore, WaddleId};
use super::room_actor::{
    DurableRestoreReadiness, DurableRoomOrigin, GetDurableRestoreReadiness, GetRoomSealState,
    GetSnapshot, HydrateDurableRecipients, RestoreDurableRoomState, RestoreLiveRoster, RoomActor,
    RoomSealState, SealForDestroy, SealGuard, SealIfInactive, SealIfInactiveOutcome, UnsealDestroy,
    UnsealInactive,
};
use super::{
    DestroyPassword, DestroyReason, DestroyRecipient, MucOccupantNick, MucRoom, RoomCommitError,
    RoomConfig, RoomDurableMutation, RoomMutationEffects,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomPreparationError {
    ActorUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomPublicationError {
    ClaimLost,
    LocalIdentityChanged,
    OwnershipUnavailable,
    /// Publish may have committed Active despite a lost acknowledgement.
    /// The exact fence must transition into durable unpublished cleanup
    /// rather than be released as an ordinary failed preparation.
    PublishOutcomeUnknown,
    ReconciliationPending,
}

/// Kills a freshly spawned room actor if preparation is cancelled before the
/// actor is deliberately handed back to the registry. This also covers a
/// reclaimed-room deadline cancelling the preparation future.
struct RoomPreparationGuard(Option<ActorRef<RoomActor>>);

impl RoomPreparationGuard {
    fn new(actor_ref: ActorRef<RoomActor>) -> Self {
        Self(Some(actor_ref))
    }

    fn actor_ref(&self) -> &ActorRef<RoomActor> {
        self.0.as_ref().expect("prepared actor remains armed")
    }

    fn disarm(mut self) -> ActorRef<RoomActor> {
        self.0.take().expect("prepared actor remains armed")
    }
}

impl Drop for RoomPreparationGuard {
    fn drop(&mut self) {
        if let Some(actor_ref) = self.0.take() {
            actor_ref.kill();
        }
    }
}

enum RoomPreparationWaiter {
    Lookup {
        reply: ReplySender<Result<Option<ActorRef<RoomActor>>, RoomRegistryError>>,
    },
    Acquisition {
        reply: ReplySender<Result<RoomAcquisition, RoomRegistryError>>,
        creation_spec: Arc<RoomCreationSpec>,
    },
    ExclusiveCreate {
        reply: ReplySender<Result<ActorRef<RoomActor>, RoomRegistryError>>,
        creation_spec: Arc<RoomCreationSpec>,
    },
    Reclaimed {
        reply: ReplySender<ReclaimedRoomOutcome>,
        success: ReclaimedRoomOutcome,
    },
}

#[derive(Clone)]
struct RoomCreationSpec {
    waddle_id: String,
    channel_id: String,
    config: RoomConfig,
    initial_affiliations: Vec<super::durable::AffiliationEntry>,
    live_room_restore: Option<LiveRoomRestore>,
}

impl PartialEq for RoomCreationSpec {
    fn eq(&self, other: &Self) -> bool {
        self.waddle_id == other.waddle_id
            && self.channel_id == other.channel_id
            && self.config == other.config
            && self.initial_affiliations == other.initial_affiliations
    }
}

impl Eq for RoomCreationSpec {}

#[derive(Clone)]
enum RoomPreparationOrigin {
    Demand {
        prepared_spec: Arc<RoomCreationSpec>,
    },
    Reclaimed {
        previous_owner: NodeIdentity,
    },
}

struct PendingRoomPreparation {
    generation: u64,
    claim_fence: super::RoomClaimFenceContext,
    origin: RoomPreparationOrigin,
    guard: RoomPreparationGuard,
    waiters: Vec<RoomPreparationWaiter>,
}

/// Everything needed to construct and prepare one room: identity, config,
/// creation-spec affiliations, and an optional live-room restore snapshot.
struct RoomPreparationSpec {
    waddle_id: String,
    channel_id: String,
    config: RoomConfig,
    initial_affiliations: Vec<super::durable::AffiliationEntry>,
    live_room_restore: Option<LiveRoomRestore>,
}

#[derive(Clone)]
struct LiveRoomRestore {
    room: MucRoom,
    occupancy_revision: u64,
    departures: super::room_actor::DepartureLedger,
}

enum DemandRoomPreparation {
    Published(ActorRef<RoomActor>),
    Pending {
        guard: RoomPreparationGuard,
        claim_fence: super::RoomClaimFenceContext,
    },
}

enum DemandRoomTransition {
    Existing(ActorRef<RoomActor>),
    Created(ActorRef<RoomActor>),
    Pending(Arc<RoomCreationSpec>),
}

enum RoomPreparationReadiness {
    Ready {
        durable_origin: DurableRoomOrigin,
        publication_fence: Result<(), RoomPublicationError>,
    },
    Pending,
    RecreationBlocked,
    ClaimLost,
    Unavailable,
}

struct CompleteRoomPreparation {
    room_jid: BareJid,
    generation: u64,
    readiness: RoomPreparationReadiness,
}

struct ReadyRoomPublication {
    room_jid: BareJid,
    generation: u64,
    durable_origin: DurableRoomOrigin,
}

struct PublishNextReadyRoom;

#[derive(Clone, Copy)]
enum DetachedRoomReleaseOutcome {
    Released,
    NotOwned,
    Retry,
}

struct CompleteDetachedRoomRelease {
    room_jid: BareJid,
    claim_fence: super::RoomClaimFenceContext,
    outcome: DetachedRoomReleaseOutcome,
}

/// Result of terminally deleting a durably-created room whose creator
/// disconnected before the registry could hand the fresh actor to any waiter.
struct CompleteUnpublishedPreparationDestroy {
    room_jid: BareJid,
    claim_fence: super::RoomClaimFenceContext,
    outcome: UnpublishedPreparationDestroyOutcome,
}

#[derive(Clone, Copy)]
enum UnpublishedPreparationDestroyOutcome {
    CleanupMarked,
    Committed,
    CommitOutcomeUnknown,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnpublishedDestroyPhase {
    MarkCleanup,
    Destroy,
    /// A terminal D&R was issued while the actor was still in durable
    /// Preparing. Its acknowledgement is ambiguous, so recovery may use
    /// the Preparing marker to prove terminal absence without mistaking a
    /// foreign claim takeover for success.
    RecoverPreparingDestroy,
}

struct RetryUnpublishedPreparationDestroy {
    room_jid: BareJid,
    claim_fence: super::RoomClaimFenceContext,
}

/// Bounded recovery for a snapshot pre-seal whose owner-IQ caller vanished
/// before registering or aborting its terminal work.
struct ReconcileSnapshotPreseal {
    room_jid: BareJid,
    attempt: super::DestroyAttemptId,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PendingRoomOwnershipResponsibility<'a> {
    Exact {
        room_jid: &'a BareJid,
        claim_fence: &'a super::RoomClaimFenceContext,
    },
    ReclaimedReservation(&'a BareJid),
}

/// See [`RoomRegistryActor::handoff_in_window`].
pub const HANDOFF_PENDING_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// A retired-but-unpublished handoff. Lookups answer
/// `OwnershipReconciliationPending` for the bounded window, and a
/// post-demote transition failure stashes the live-roster creation spec
/// here so the next demand creation inside the window restores the retired
/// actor's roster and departure ledger instead of dropping them with the
/// failed message (#1647).
struct PendingHandoff {
    since: std::time::Instant,
    stashed_spec: Option<Arc<RoomCreationSpec>>,
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
    /// Unpublished room actors whose restore/hydration mailbox prefix is still
    /// running. Keeping these out of `rooms` preserves the publication barrier;
    /// keeping them in registry-owned state lets same-room operations coalesce
    /// and lets terminal operations cancel the exact generation before a late
    /// completion can publish it.
    pending_room_preparations: HashMap<BareJid, PendingRoomPreparation>,
    /// Rooms whose stale actor was retired for a live-roster handoff that
    /// has not (yet) produced a successor: `GetRoom` answers
    /// `OwnershipReconciliationPending` instead of an absence until a room
    /// entry is published again.
    handoff_pending: HashMap<BareJid, PendingHandoff>,
    /// Unpublished demand rooms whose durable Create succeeded but whose
    /// terminal cleanup after a lost creator handoff remains uncertain.
    pending_unpublished_destroys:
        HashMap<(BareJid, super::RoomClaimFenceContext), UnpublishedDestroyPhase>,
    /// Attempt identities whose pre-seal reply was lost or whose durable
    /// destroy commit still needs reconciliation. A retry must reuse the
    /// same token; a new token cannot reopen the actor-local seal. Owner-IQ
    /// destroys additionally retain their typed post-commit work so a lost
    /// registry reply cannot bypass app cleanup or XEP-0045 notifications.
    destroy_attempts: HashMap<BareJid, RetainedDestroyAttempt>,
    /// Owner-IQ destroy work ready for server-owned post-commit cleanup.
    /// Entries remain pending until the server explicitly acknowledges the
    /// exact attempt after successful cleanup.
    pending_destroy_completions: VecDeque<DestroyCompletion>,
    /// Owner-IQ destroy work currently leased to the server. A failed cleanup
    /// must requeue the exact attempt to keep retrying.
    leased_destroy_completions: HashMap<super::DestroyAttemptId, DestroyCompletion>,
    /// Owner-IQ completion data registered before an explicit destroy, keyed
    /// by the caller-supplied typed attempt identity.
    destroy_completions_waiting: HashMap<super::DestroyAttemptId, DestroyCompletion>,
    ready_room_publications: VecDeque<ReadyRoomPublication>,
    ready_room_publication_scheduled: bool,
    next_room_preparation_generation: u64,
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

#[derive(Clone)]
struct RetainedDestroyAttempt {
    attempt: super::DestroyAttemptId,
    phase: DestroyAttemptPhase,
    completion: Option<DestroyCompletion>,
}

/// A pre-seal used only to capture an owner-IQ recipient snapshot must never
/// be mistaken for a requested durable destroy. If completion persistence or
/// registration is lost, reconciliation reopens this exact seal rather than
/// terminally deleting the room without its required outbox work.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DestroyAttemptPhase {
    SnapshotPreseal,
    /// The outbox completion is registered but the terminal registry message
    /// has not been observed. A confirmed non-delivery may still abort it.
    RegisteredPreDestroy,
    DestroyRequested,
}

/// Typed post-commit work for an owner-IQ room destroy. This is retained
/// across ambiguous registry replies and consumed by `waddle-server`, which
/// owns the application database and session-routing effects.
#[derive(Clone)]
pub struct DestroyCompletion {
    pub attempt: super::DestroyAttemptId,
    pub room_jid: BareJid,
    pub room: MucRoom,
    /// Unacknowledged departure receipts at seal time: their holders left
    /// the roster but never saw their leave reply, and the terminal destroy
    /// notification is the effect that settles what they are owed (#1647).
    pub departures: super::room_actor::DepartureLedger,
    pub request: super::DestroyRequest,
}

/// Re-run an explicit destroy using the exact owner-IQ attempt that produced
/// the completion snapshot.
pub struct DestroyRoomWithAttempt {
    pub room_jid: BareJid,
    pub reason: DestroyRoomReason,
    pub attempt: super::DestroyAttemptId,
}

/// Atomically close room admission for an owner-IQ destroy and return the
/// recipient snapshot from that same actor-mailbox boundary.  Callers must
/// either continue with [`DestroyRoomWithAttempt`] for this attempt or abort
/// it through [`AbortDestroyRoomAttempt`] when persisting the completion
/// cannot proceed.
pub struct SealRoomForDestroySnapshot {
    pub room_jid: BareJid,
    pub attempt: super::DestroyAttemptId,
}

/// Reopen a pre-sealed owner-IQ destroy that never reached its durable
/// terminal transition.  This is deliberately attempt-bound so a delayed
/// caller cannot reopen a newer destroy.
pub struct AbortDestroyRoomAttempt {
    pub room_jid: BareJid,
    pub attempt: super::DestroyAttemptId,
}

/// Return a leased destroy completion to the ready queue after server-side
/// cleanup failed.
pub struct RequeueDestroyCompletion {
    pub attempt: super::DestroyAttemptId,
}

/// Acknowledge that server-side destroy cleanup completed successfully.
pub struct AckDestroyCompletion {
    pub attempt: super::DestroyAttemptId,
}

/// Cancel a destroy completion that never started, keyed by the exact
/// owner-IQ attempt that registered it.
pub struct CancelDestroyCompletionAttempt {
    pub attempt: super::DestroyAttemptId,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomRegistryError {
    #[error("room {0} already exists")]
    RoomAlreadyExists(BareJid),
    /// A live-roster handoff named a stale actor that is no longer the
    /// registered one (a successor is already live, or the entry was
    /// retired): the caller's snapshot is not authoritative and must not be
    /// transplanted.
    #[error("room {0}: the stale actor is no longer current")]
    StaleActorNotCurrent(BareJid),
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
    fn destroy_effects(completion: &DestroyCompletion) -> RoomMutationEffects {
        let mut by_nick: std::collections::BTreeMap<MucOccupantNick, Vec<jid::FullJid>> =
            completion
                .room
                .occupants
                .values()
                .map(|occupant| {
                    (
                        MucOccupantNick::new(occupant.nick.clone())
                            .expect("nick was previously accepted"),
                        completion
                            .room
                            .get_occupant_sessions(&occupant.nick)
                            .into_iter()
                            .collect(),
                    )
                })
                .collect();
        // #1647: unacknowledged departure receipts belong to sessions that
        // already left the roster but never saw their leave reply. The
        // terminal destroy effect is the last chance to settle the owed
        // self-unavailable (the retained retry later finds the room absent),
        // so their holders join the durable outbox recipients too.
        for receipt in &completion.departures.receipts {
            let nick = match &receipt.outcome {
                super::room_actor::DepartureReceiptOutcome::Left(outcome) => outcome.nick.clone(),
                super::room_actor::DepartureReceiptOutcome::Suppressed { nick, .. } => nick.clone(),
            };
            let sessions = by_nick.entry(nick).or_default();
            if !sessions.contains(&receipt.jid) {
                sessions.push(receipt.jid.clone());
            }
        }
        let recipients = by_nick
            .into_iter()
            .map(|(nick, sessions)| DestroyRecipient { nick, sessions })
            .collect();
        RoomMutationEffects::destroy(
            completion.room_jid.clone(),
            completion
                .request
                .reason
                .clone()
                .and_then(DestroyReason::new),
            completion.request.alternate_venue.clone(),
            completion
                .request
                .password
                .clone()
                .and_then(DestroyPassword::new),
            recipients,
        )
    }

    async fn destroy_completion_blocks_recreation(&self, room_jid: &BareJid) -> bool {
        if self
            .pending_unpublished_destroys
            .keys()
            .any(|(pending_room_jid, _)| pending_room_jid == room_jid)
        {
            return true;
        }
        let locally_pending = self
            .destroy_completions_waiting
            .values()
            .chain(self.pending_destroy_completions.iter())
            .chain(self.leased_destroy_completions.values())
            .any(|completion| completion.room_jid == *room_jid);
        if locally_pending {
            return true;
        }
        let Some(store) = &self.durable_store else {
            return false;
        };
        match store.destroy_completion_blocks_recreation(room_jid).await {
            Ok(blocks) => blocks,
            Err(error) => {
                // An unavailable durable outbox is not proof that a prior
                // completion is gone.  Refuse recreation until a later
                // request can query it successfully.
                warn!(room = %room_jid, %error, "could not verify durable destroy completion before recreation");
                true
            }
        }
    }

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
            pending_room_preparations: HashMap::new(),
            handoff_pending: HashMap::new(),
            pending_unpublished_destroys: HashMap::new(),
            destroy_attempts: HashMap::new(),
            pending_destroy_completions: VecDeque::new(),
            leased_destroy_completions: HashMap::new(),
            destroy_completions_waiting: HashMap::new(),
            ready_room_publications: VecDeque::new(),
            ready_room_publication_scheduled: false,
            next_room_preparation_generation: 0,
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
            || (self.pending_room_releases.len() < MAX_PENDING_ROOM_RELEASES
                && self.can_admit_room_ownership_responsibility(
                    PendingRoomOwnershipResponsibility::Exact {
                        room_jid,
                        claim_fence,
                    },
                ))
    }

    fn extend_pending_room_ownership_responsibilities_until_full<'a>(
        pending: &mut HashSet<PendingRoomOwnershipResponsibility<'a>>,
        responsibilities: impl IntoIterator<Item = PendingRoomOwnershipResponsibility<'a>>,
    ) -> bool {
        if pending.len() >= MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES {
            return false;
        }
        for responsibility in responsibilities {
            pending.insert(responsibility);
            if pending.len() >= MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES {
                return false;
            }
        }
        true
    }

    fn has_pending_room_ownership_responsibility(
        &self,
        candidate: PendingRoomOwnershipResponsibility<'_>,
    ) -> bool {
        match candidate {
            PendingRoomOwnershipResponsibility::Exact {
                room_jid,
                claim_fence,
            } => {
                let exact_key = ((*room_jid).clone(), (*claim_fence).clone());
                self.pending_room_preparations
                    .get(room_jid)
                    .is_some_and(|pending| pending.claim_fence == *claim_fence)
                    || self.pending_unpublished_destroys.contains_key(&exact_key)
                    || self.pending_room_releases.contains_key(&exact_key)
                    || self.pending_reclaimed_rooms.contains_key(&exact_key)
                    || self.rooms.get(room_jid).is_some_and(|entry| {
                        (!entry.actor_ref.is_alive()
                            || entry.claim_fence.owner != self.node_identity.current())
                            && entry.claim_fence == *claim_fence
                    })
            }
            PendingRoomOwnershipResponsibility::ReclaimedReservation(room_jid) => {
                self.pending_reclaimed_reservations.contains(room_jid)
            }
        }
    }

    fn pending_room_ownership_responsibilities(
        &self,
    ) -> impl Iterator<Item = PendingRoomOwnershipResponsibility<'_>> {
        // Keep this lazy and borrowed: capacity checks stop consuming it at
        // the shared cap instead of cloning every key or walking the rest of
        // a saturated room inventory inside the registry mailbox turn.
        let current_identity = self.node_identity.current();
        self.pending_room_preparations
            .iter()
            .map(
                |(room_jid, preparation)| PendingRoomOwnershipResponsibility::Exact {
                    room_jid,
                    claim_fence: &preparation.claim_fence,
                },
            )
            .chain(
                self.pending_room_releases
                    .keys()
                    .map(
                        |(room_jid, claim_fence)| PendingRoomOwnershipResponsibility::Exact {
                            room_jid,
                            claim_fence,
                        },
                    ),
            )
            .chain(
                self.pending_unpublished_destroys
                    .keys()
                    .map(
                        |(room_jid, claim_fence)| PendingRoomOwnershipResponsibility::Exact {
                            room_jid,
                            claim_fence,
                        },
                    ),
            )
            .chain(
                self.pending_reclaimed_rooms
                    .keys()
                    .map(
                        |(room_jid, claim_fence)| PendingRoomOwnershipResponsibility::Exact {
                            room_jid,
                            claim_fence,
                        },
                    ),
            )
            .chain(
                self.pending_reclaimed_reservations
                    .iter()
                    .map(PendingRoomOwnershipResponsibility::ReclaimedReservation),
            )
            .chain(
                self.rooms
                    .iter()
                    .filter(move |(_, entry)| {
                        !entry.actor_ref.is_alive() || entry.claim_fence.owner != current_identity
                    })
                    .map(
                        |(room_jid, entry)| PendingRoomOwnershipResponsibility::Exact {
                            room_jid,
                            claim_fence: &entry.claim_fence,
                        },
                    ),
            )
    }

    fn has_room_ownership_responsibility_capacity(&self) -> bool {
        let mut pending = HashSet::with_capacity(MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES);
        Self::extend_pending_room_ownership_responsibilities_until_full(
            &mut pending,
            self.pending_room_ownership_responsibilities(),
        )
    }

    fn can_admit_room_ownership_responsibility(
        &self,
        candidate: PendingRoomOwnershipResponsibility<'_>,
    ) -> bool {
        self.has_pending_room_ownership_responsibility(candidate)
            || self.has_room_ownership_responsibility_capacity()
    }

    fn can_admit_new_room_ownership_responsibility(&self) -> bool {
        self.has_room_ownership_responsibility_capacity()
    }

    fn has_pending_preparation_capacity(
        &self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
    ) -> bool {
        self.can_admit_room_ownership_responsibility(PendingRoomOwnershipResponsibility::Exact {
            room_jid,
            claim_fence,
        })
    }

    #[cfg(test)]
    fn pending_room_ownership_responsibility_count_for_test(&self) -> usize {
        self.pending_room_ownership_responsibilities()
            .collect::<HashSet<_>>()
            .len()
    }

    fn preparation_waiter_capacity_available(pending: &PendingRoomPreparation) -> bool {
        pending.waiters.len() < MAX_ROOM_PREPARATION_WAITERS
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
            self.transfer_exact_responsibility_to_pending_release(
                room_jid.clone(),
                entry.claim_fence.clone(),
            );
            self.release_room_claim(room_jid, &entry.claim_fence).await;
        } else if let Some(store) = &self.durable_store {
            store.forget_claim_fence(room_jid, &entry.claim_fence);
        }
    }

    /// Retire an unpublished fence after a terminal ownership result.
    ///
    /// A same-identity database miss proves the exact tuple is already gone.
    /// A different current identity only proves this process can no longer
    /// serve the old incarnation; its exact tuple may still need a safe,
    /// conditional release. Preserve that responsibility until release
    /// succeeds or proves the tuple no longer exists.
    fn finish_unpublished_ownership_loss(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        registry_ref: ActorRef<Self>,
    ) -> ReclaimedRoomOutcome {
        if claim_fence.owner() != self.node_identity.current() {
            self.transfer_exact_responsibility_to_pending_release(
                room_jid.clone(),
                claim_fence.clone(),
            );
            self.start_detached_room_release(room_jid, claim_fence, registry_ref);
            ReclaimedRoomOutcome::PendingRetry
        } else {
            if let Some(store) = &self.durable_store {
                store.forget_claim_fence(&room_jid, &claim_fence);
            }
            ReclaimedRoomOutcome::LostRace
        }
    }

    /// Publish the current room count to the pod-wide rooms gauge.
    /// Called after every `self.rooms` mutation; the actor serializes
    /// those, so the published value is exact (#1415 review — the gauge
    /// was previously wired only into the test-only legacy registry and
    /// never emitted in production).
    /// A live-roster handoff retired this room's actor and no successor is
    /// registered yet. Bounded: a handoff whose successor never materialises
    /// (its preparation failed) degrades to an ordinary absence after
    /// [`HANDOFF_PENDING_WINDOW`] instead of wedging lookups forever.
    fn handoff_in_window(&mut self, room_jid: &BareJid) -> bool {
        if self.rooms.contains_key(room_jid) {
            self.handoff_pending.remove(room_jid);
            return false;
        }
        match self.handoff_pending.get(room_jid) {
            Some(pending) if pending.since.elapsed() < HANDOFF_PENDING_WINDOW => true,
            Some(_) => {
                self.handoff_pending.remove(room_jid);
                tracing::warn!(
                    room = %room_jid,
                    window_secs = HANDOFF_PENDING_WINDOW.as_secs(),
                    "live-roster handoff never published a successor; room now reads as absent"
                );
                false
            }
            None => false,
        }
    }

    /// A live-roster spec stashed by a failed post-demote transition, if
    /// still within the handoff window.
    fn stashed_handoff_spec(&self, room_jid: &BareJid) -> Option<Arc<RoomCreationSpec>> {
        let pending = self.handoff_pending.get(room_jid)?;
        if pending.since.elapsed() >= HANDOFF_PENDING_WINDOW {
            return None;
        }
        pending.stashed_spec.clone()
    }

    fn publish_room_count(&self) {
        crate::metrics::publish_muc_rooms_active(self.rooms.len() as i64);
    }

    async fn evict_ownership_lost_room(&mut self, room_jid: &BareJid, entry: RoomEntry) {
        self.rooms.remove(room_jid);
        self.publish_room_count();
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

    /// Move an already-retained exact fence from a preparation or non-serving
    /// room entry into release state. The caller removes the prior
    /// representation in the same mailbox turn, so the transfer cannot lose
    /// responsibility even when ordinary release admission is saturated.
    fn transfer_exact_responsibility_to_pending_release(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
    ) {
        self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
        let retry_order = self.pending_retry_order;
        self.pending_room_releases
            .entry((room_jid, claim_fence))
            .and_modify(|current| current.retry_order = retry_order)
            .or_insert(PendingRoomReleaseState {
                retry_order,
                first_pending_at: std::time::Instant::now(),
            });
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

    /// Replace one already-bounded uncertain acquisition with the exact
    /// self-owned fence observed after all pending store writes completed.
    /// Removing the acquisition only after installing the fence preserves
    /// uninterrupted ownership responsibility across the representation
    /// change, including during terminal shutdown.
    fn transfer_pending_room_acquisition_to_pending_release(
        &mut self,
        room_jid: BareJid,
        attempted_owner: NodeIdentity,
        claim_fence: super::RoomClaimFenceContext,
    ) {
        debug_assert!(self
            .pending_room_acquisitions
            .contains_key(&(room_jid.clone(), attempted_owner.clone())));
        self.pending_retry_order = self.pending_retry_order.wrapping_add(1);
        let retry_order = self.pending_retry_order;
        self.pending_room_releases
            .entry((room_jid.clone(), claim_fence))
            .and_modify(|current| current.retry_order = retry_order)
            .or_insert(PendingRoomReleaseState {
                retry_order,
                first_pending_at: std::time::Instant::now(),
            });
        self.clear_pending_room_acquisition(&room_jid, &attempted_owner);
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

    fn start_detached_room_release(
        &self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        registry_ref: ActorRef<Self>,
    ) {
        let claim_store = Arc::clone(&self.claim_store);
        tokio::spawn(async move {
            let owner = claim_fence.owner();
            let outcome = match tokio::time::timeout(
                ROOM_OWNERSHIP_CALL_TIMEOUT,
                claim_store.release_exact(&claim_fence.entity, &owner, claim_fence.epoch),
            )
            .await
            {
                Ok(Ok(ExactReleaseOutcome::Released)) => DetachedRoomReleaseOutcome::Released,
                Ok(Ok(ExactReleaseOutcome::NotOwned)) => DetachedRoomReleaseOutcome::NotOwned,
                Ok(Err(_)) | Err(_) => DetachedRoomReleaseOutcome::Retry,
            };
            let _ = registry_ref
                .tell(CompleteDetachedRoomRelease {
                    room_jid,
                    claim_fence,
                    outcome,
                })
                .await;
        });
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
        if claim_fence.entity != Entity::new(EntityType::RoomActor, room_jid.to_string()) {
            warn!(
                room = %room_jid,
                fence_entity = %claim_fence.entity,
                "rejecting pending reclaimed room with a cross-entity claim fence"
            );
            return;
        }
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

    async fn publish_prepared_room(
        &mut self,
        room_jid: BareJid,
        actor_guard: RoomPreparationGuard,
        claim_fence: super::RoomClaimFenceContext,
        durable_origin: DurableRoomOrigin,
    ) -> Result<ActorRef<RoomActor>, RoomPublicationError> {
        let owner = claim_fence.owner();
        let still_owned = match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            self.claim_store
                .fence(&claim_fence.entity, &owner, claim_fence.epoch),
        )
        .await
        {
            Ok(Ok(held)) => held,
            Ok(Err(_)) | Err(_) => return Err(RoomPublicationError::OwnershipUnavailable),
        };
        if !still_owned {
            return Err(RoomPublicationError::ClaimLost);
        }
        self.publish_prepared_room_after_fence(room_jid, actor_guard, claim_fence, durable_origin)
            .await
    }

    async fn publish_prepared_room_after_fence(
        &mut self,
        room_jid: BareJid,
        actor_guard: RoomPreparationGuard,
        claim_fence: super::RoomClaimFenceContext,
        durable_origin: DurableRoomOrigin,
    ) -> Result<ActorRef<RoomActor>, RoomPublicationError> {
        let owner = claim_fence.owner();
        // Acquire the local incarnation guard after the final database fence
        // and retain it only through the synchronous map insertion. A
        // rotation that has already started wins writer preference; a later
        // rotation waits for this exact publication boundary. Never carry
        // this guard in a mailbox message, where registry backlog could delay
        // self-fencing indefinitely.
        let Some(identity_guard) = self.node_identity.guard_if_current(&owner).await else {
            return Err(RoomPublicationError::LocalIdentityChanged);
        };
        if matches!(durable_origin, DurableRoomOrigin::New) {
            if let Some(store) = &self.durable_store {
                match store
                    .commit_room_mutation_with_authority(
                        &room_jid,
                        &claim_fence,
                        RoomDurableMutation::Publish,
                        crate::muc::RoomMutationEffects::none(),
                        &identity_guard,
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(RoomCommitError::NotOwner | RoomCommitError::StateMissing) => {
                        return Err(RoomPublicationError::ClaimLost);
                    }
                    Err(RoomCommitError::CommitOutcomeUnknown) => {
                        warn!(room = %room_jid, "durable Publish outcome is unknown; retaining exact fence for unpublished cleanup");
                        return Err(RoomPublicationError::PublishOutcomeUnknown);
                    }
                    Err(error) => {
                        warn!(room = %room_jid, %error, "failed to promote preparing durable room before publication");
                        return Err(RoomPublicationError::OwnershipUnavailable);
                    }
                }
            }
        }
        let actor_ref = actor_guard.actor_ref().clone();
        if !self.publish_room(room_jid.clone(), actor_ref.clone(), claim_fence) {
            return Err(RoomPublicationError::ReconciliationPending);
        }
        Ok(actor_guard.disarm())
    }

    /// Spawn and enqueue all durable hydration work without making the actor
    /// discoverable through the registry. Reclaimed rooms use this split so
    /// ownership can be re-fenced after every enqueue await and immediately
    /// before publication.
    async fn prepare_room(
        &self,
        room_jid: BareJid,
        spec: RoomPreparationSpec,
        claim_fence: &super::RoomClaimFenceContext,
    ) -> Result<(RoomPreparationGuard, bool), RoomPreparationError> {
        let RoomPreparationSpec {
            waddle_id,
            channel_id,
            config,
            initial_affiliations,
            live_room_restore,
        } = spec;
        // Establish the exact fence before any preparation-time durable I/O.
        // This deliberately does not publish the room-JID fan-out cache: that
        // still waits for the ready actor's registry insertion so an
        // in-flight predecessor cannot borrow successor authority.
        if let Some(store) = &self.durable_store {
            store.establish_claim_fence(&room_jid, claim_fence.clone());
        }
        let mut room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        // Store-less (non-clustered) deployments have no durable `Create`
        // commit and no `RestoreDurableRoomState` round-trip, so the
        // creation spec's initial affiliations must seed actor memory here
        // or they would be silently dropped. With a durable store the
        // fenced `Create` commit followed by the restore installs the
        // authoritative snapshot instead; seeding here would survive a
        // `Restored`-origin hydration (restore never clears entries) and
        // corrupt an existing room's membership, so it stays store-less
        // only.
        if self.durable_store.is_none() {
            for entry in initial_affiliations {
                if let Some(affiliation) = entry.affiliation {
                    room.set_affiliation(entry.jid, affiliation);
                }
            }
        }
        let actor_guard = RoomPreparationGuard::new(RoomActor::spawn(RoomActor::new(
            room,
            self.occupant_id_secret.clone(),
        )));
        let actor_ref = actor_guard.actor_ref();
        if let Some(store) = &self.durable_store {
            if let Err(error) = actor_ref
                .tell(RestoreDurableRoomState {
                    store: Arc::clone(store),
                    claim_fence: claim_fence.clone(),
                })
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to enqueue durable room-state restore for freshly \
                     spawned/re-claimed room actor"
                );
                return Err(RoomPreparationError::ActorUnavailable);
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
                return Err(RoomPreparationError::ActorUnavailable);
            }
        }
        // With a durable store, a new room receives a second durable restore
        // after its Create commit.  Defer this transfer to the pending
        // preparation barrier so that restore cannot overwrite the live
        // roster between preparation and publication.
        if self.durable_store.is_none() {
            if let Some(restore) = live_room_restore {
                if let Err(error) = actor_ref
                    .ask(RestoreLiveRoster {
                        room: restore.room,
                        occupancy_revision: restore.occupancy_revision,
                        departures: restore.departures,
                    })
                    .await
                {
                    warn!(
                        room = %room_jid,
                        %error,
                        "failed to restore live roster for freshly spawned room actor"
                    );
                    return Err(RoomPreparationError::ActorUnavailable);
                }
            }
        }
        let has_async_work = self.durable_store.is_some() || self.membership_source.is_some();
        Ok((actor_guard, has_async_work))
    }

    async fn prepare_demand_room(
        &mut self,
        room_jid: &BareJid,
        spec: RoomPreparationSpec,
        registry_ref: &ActorRef<Self>,
    ) -> Result<DemandRoomPreparation, RoomRegistryError> {
        let has_async_work = self.durable_store.is_some() || self.membership_source.is_some();
        if has_async_work && !self.can_admit_new_room_ownership_responsibility() {
            return Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid.clone(),
            ));
        }
        let claim_fence = self.acquire_room_claim(room_jid, registry_ref).await?;
        self.poisoned_rooms.remove(room_jid);
        let (guard, has_async_work) = match self
            .prepare_room(room_jid.clone(), spec, &claim_fence)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                warn!(room = %room_jid, ?error, "room actor preparation failed before publication");
                self.release_room_claim(room_jid, &claim_fence).await;
                return Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()));
            }
        };
        if has_async_work {
            return Ok(DemandRoomPreparation::Pending { guard, claim_fence });
        }
        match self
            .publish_prepared_room(
                room_jid.clone(),
                guard,
                claim_fence.clone(),
                DurableRoomOrigin::New,
            )
            .await
        {
            Ok(actor_ref) => Ok(DemandRoomPreparation::Published(actor_ref)),
            Err(_) => {
                self.release_room_claim(room_jid, &claim_fence).await;
                Err(RoomRegistryError::OwnershipUnavailable(room_jid.clone()))
            }
        }
    }

    /// A Create is durable before an actor is exposed, so a process loss (or
    /// creator cancellation) can leave a `preparing` lifecycle with no local
    /// registry entry. Recover it only through a newly acquired exact claim;
    /// a room-JID-only cleanup would let a stale node delete a successor.
    async fn reconcile_stranded_preparing_room(
        &mut self,
        room_jid: &BareJid,
        registry_ref: &ActorRef<Self>,
    ) -> Result<(), RoomRegistryError> {
        let Some(store) = self.durable_store.clone() else {
            return Ok(());
        };
        let preparing = match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            store.find_preparing_room(room_jid),
        )
        .await
        {
            Ok(Ok(preparing)) => preparing,
            Ok(Err(error)) => {
                warn!(room = %room_jid, %error, "could not check durable preparing-room recovery state");
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
            Err(_) => {
                warn!(room = %room_jid, "timed out checking durable preparing-room recovery state");
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
        };
        let Some(preparing_coordinates) = preparing else {
            return Ok(());
        };

        let claim_fence = self.acquire_room_claim(room_jid, registry_ref).await?;
        store.establish_claim_fence(room_jid, claim_fence.clone());
        let exact_preparing = match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            store.find_preparing_room(room_jid),
        )
        .await
        {
            Ok(Ok(preparing)) => preparing,
            Ok(Err(error)) => {
                warn!(room = %room_jid, %error, "could not re-check durable preparing-room recovery state after claim acquisition");
                self.release_room_claim(room_jid, &claim_fence).await;
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
            Err(_) => {
                warn!(room = %room_jid, "timed out re-checking durable preparing-room recovery state after claim acquisition");
                self.release_room_claim(room_jid, &claim_fence).await;
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
        };
        match exact_preparing {
            Some(current) if current == preparing_coordinates => {}
            Some(current) => {
                info!(
                    room = %room_jid,
                    observed_lifecycle = %preparing_coordinates.lifecycle,
                    current_lifecycle = %current.lifecycle,
                    "durable preparing lifecycle changed before stranded cleanup could commit"
                );
                self.release_room_claim(room_jid, &claim_fence).await;
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
            None => {
                info!(
                    room = %room_jid,
                    observed_lifecycle = %preparing_coordinates.lifecycle,
                    "durable preparing lifecycle cleared before stranded cleanup could commit"
                );
                self.release_room_claim(room_jid, &claim_fence).await;
                return Ok(());
            }
        }
        match store
            .commit_room_mutation(
                room_jid,
                &claim_fence,
                RoomDurableMutation::DestroyAndReleaseClaim {
                    completion_attempt: None,
                },
                crate::muc::RoomMutationEffects::none(),
            )
            .await
        {
            Ok(_) => {
                // D&R releases in its durable transaction. This exact
                // release is a harmless cache/claim-store no-op when the
                // commit already completed, and retains a retry on an
                // implementation that could not confirm that release.
                self.release_room_claim(room_jid, &claim_fence).await;
                info!(room = %room_jid, "recovered stranded durable preparing room");
                Ok(())
            }
            Err(error) => {
                // Never infer a destroy from claim loss alone. The durable
                // `preparing` marker remains restart-safe and causes this
                // path to retry/reconcile on the next demand attempt.
                warn!(room = %room_jid, %error, "could not terminally recover stranded durable preparing room");
                self.release_room_claim(room_jid, &claim_fence).await;
                Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ))
            }
        }
    }

    async fn reconcile_reclaimed_preparing_room(
        &mut self,
        room_jid: &BareJid,
        claim_fence: &super::RoomClaimFenceContext,
        previous_owner: &NodeIdentity,
        registry_ref: &ActorRef<Self>,
    ) -> Option<ReclaimedRoomOutcome> {
        let store = self.durable_store.clone()?;
        let preparing = match tokio::time::timeout(
            RECLAIMED_ROOM_STORE_TIMEOUT,
            store.find_preparing_room(room_jid),
        )
        .await
        {
            Ok(Ok(preparing)) => preparing,
            Ok(Err(error)) => {
                debug!(
                    room = %room_jid,
                    %error,
                    "failed to classify proactively reclaimed room lifecycle"
                );
                self.remember_pending_reclaimed_room(
                    room_jid.clone(),
                    claim_fence.clone(),
                    previous_owner.clone(),
                );
                return Some(ReclaimedRoomOutcome::PendingRetry);
            }
            Err(_) => {
                debug!(
                    room = %room_jid,
                    "timed out classifying proactively reclaimed room lifecycle"
                );
                self.remember_pending_reclaimed_room(
                    room_jid.clone(),
                    claim_fence.clone(),
                    previous_owner.clone(),
                );
                return Some(ReclaimedRoomOutcome::PendingRetry);
            }
        };
        let preparing_coordinates = preparing?;
        info!(
            room = %room_jid,
            preparing_lifecycle = %preparing_coordinates.lifecycle,
            "proactively reclaimed room still points at a stranded preparing lifecycle"
        );
        match store
            .commit_room_mutation(
                room_jid,
                claim_fence,
                RoomDurableMutation::DestroyAndReleaseClaim {
                    completion_attempt: None,
                },
                crate::muc::RoomMutationEffects::none(),
            )
            .await
        {
            Ok(_) | Err(RoomCommitError::StateMissing) => {
                self.clear_pending_reclaimed_room(room_jid, claim_fence);
                self.poisoned_rooms.remove(room_jid);
                self.release_room_claim(room_jid, claim_fence).await;
                info!(
                    room = %room_jid,
                    "recovered proactively reclaimed stranded preparing room"
                );
                Some(ReclaimedRoomOutcome::Released)
            }
            Err(RoomCommitError::NotOwner) => {
                store.forget_claim_fence(room_jid, claim_fence);
                self.clear_pending_reclaimed_room(room_jid, claim_fence);
                Some(ReclaimedRoomOutcome::LostRace)
            }
            Err(RoomCommitError::CommitOutcomeUnknown) => {
                self.clear_pending_reclaimed_room(room_jid, claim_fence);
                self.begin_preparing_destroy_recovery(
                    room_jid.clone(),
                    claim_fence.clone(),
                    registry_ref.clone(),
                );
                Some(ReclaimedRoomOutcome::PendingRetry)
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "could not terminally recover proactively reclaimed preparing room"
                );
                self.remember_pending_reclaimed_room(
                    room_jid.clone(),
                    claim_fence.clone(),
                    previous_owner.clone(),
                );
                Some(ReclaimedRoomOutcome::PendingRetry)
            }
        }
    }

    /// Resolve the common demand-side state machine exactly once for every
    /// public creation API. Reply-shape differences stay in the message
    /// handlers; claim acquisition, live-room detection, preparation, and
    /// coalescing cannot drift between them.
    async fn transition_demand_room(
        &mut self,
        room_jid: BareJid,
        creation_spec: Arc<RoomCreationSpec>,
        registry_ref: ActorRef<Self>,
    ) -> Result<DemandRoomTransition, RoomRegistryError> {
        if let Some(pending) = self.pending_room_preparations.get(&room_jid) {
            if !Self::preparation_waiter_capacity_available(pending) {
                return Err(RoomRegistryError::OwnershipReconciliationPending(room_jid));
            }
            return Ok(DemandRoomTransition::Pending(creation_spec));
        }
        self.reconcile_stranded_preparing_room(&room_jid, &registry_ref)
            .await?;
        if let Some(actor_ref) = self.live_room(&room_jid).await? {
            return Ok(DemandRoomTransition::Existing(actor_ref));
        }

        // A failed post-demote handoff stashed its live-roster spec on the
        // pending marker. The next demand creation inside the window adopts
        // it, so the retired actor's roster and departure ledger survive the
        // dropped message. A caller that brings its own restore is fresher
        // (a still-live stale actor) and wins. The stash is cleared only by
        // a registered successor or window expiry, so a failed adoption
        // leaves it in place for the next retry.
        let creation_spec = match self.stashed_handoff_spec(&room_jid) {
            Some(stashed) if creation_spec.live_room_restore.is_none() => stashed,
            _ => creation_spec,
        };

        match self
            .prepare_demand_room(
                &room_jid,
                RoomPreparationSpec {
                    waddle_id: creation_spec.waddle_id.clone(),
                    channel_id: creation_spec.channel_id.clone(),
                    config: creation_spec.config.clone(),
                    initial_affiliations: creation_spec.initial_affiliations.clone(),
                    live_room_restore: creation_spec.live_room_restore.clone(),
                },
                &registry_ref,
            )
            .await?
        {
            DemandRoomPreparation::Published(actor_ref) => {
                // A registered successor ends the handoff window (and
                // retires any adopted stash with it).
                self.handoff_pending.remove(&room_jid);
                Ok(DemandRoomTransition::Created(actor_ref))
            }
            DemandRoomPreparation::Pending { guard, claim_fence } => {
                self.start_pending_preparation(
                    room_jid,
                    claim_fence,
                    RoomPreparationOrigin::Demand {
                        prepared_spec: Arc::clone(&creation_spec),
                    },
                    guard,
                    None,
                    registry_ref,
                );
                Ok(DemandRoomTransition::Pending(creation_spec))
            }
        }
    }

    fn attach_preparation_waiter(
        &mut self,
        room_jid: &BareJid,
        waiter: Option<RoomPreparationWaiter>,
    ) {
        let Some(waiter) = waiter else {
            return;
        };
        let pending = self
            .pending_room_preparations
            .get_mut(room_jid)
            .expect("pending demand transition must own a preparation entry");
        debug_assert!(Self::preparation_waiter_capacity_available(pending));
        pending.waiters.push(waiter);
    }

    fn next_preparation_generation(&mut self) -> u64 {
        self.next_room_preparation_generation =
            self.next_room_preparation_generation.wrapping_add(1);
        self.next_room_preparation_generation
    }

    /// Schedule at most one final publication fence at a time. The next
    /// publication message is appended only after the preceding one
    /// completes, so unrelated registry work already queued can run between
    /// bounded database fences instead of sitting behind the whole ready
    /// preparation backlog.
    fn schedule_ready_room_publication(&mut self, registry_ref: &ActorRef<Self>) {
        if self.ready_room_publication_scheduled || self.ready_room_publications.is_empty() {
            return;
        }
        self.ready_room_publication_scheduled = true;
        std::mem::drop(
            registry_ref
                .tell(PublishNextReadyRoom)
                .send_after(std::time::Duration::ZERO),
        );
    }

    fn start_pending_preparation(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        origin: RoomPreparationOrigin,
        guard: RoomPreparationGuard,
        waiter: Option<RoomPreparationWaiter>,
        registry_ref: ActorRef<Self>,
    ) {
        debug_assert!(
            self.pending_room_preparations.contains_key(&room_jid)
                || self.has_pending_preparation_capacity(&room_jid, &claim_fence),
            "pending room preparation inventory must be reserved before claim acquisition"
        );
        let generation = self.next_preparation_generation();
        let actor_ref = guard.actor_ref().clone();
        let claim_store = Arc::clone(&self.claim_store);
        let durable_store = self.durable_store.clone();
        let creation_spec = match &origin {
            RoomPreparationOrigin::Demand { prepared_spec } => Some(Arc::clone(prepared_spec)),
            RoomPreparationOrigin::Reclaimed { .. } => None,
        };
        let live_room_restore = creation_spec
            .as_ref()
            .and_then(|spec| spec.live_room_restore.clone());
        let publication_fence = claim_fence.clone();
        let waiters = waiter.into_iter().collect();
        let replaced = self.pending_room_preparations.insert(
            room_jid.clone(),
            PendingRoomPreparation {
                generation,
                claim_fence,
                origin,
                guard,
                waiters,
            },
        );
        debug_assert!(replaced.is_none(), "same-room preparation must coalesce");
        tokio::spawn(async move {
            let first_readiness = actor_ref
                .ask(GetDurableRestoreReadiness)
                .mailbox_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                .reply_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                .await;
            let readiness = match first_readiness {
                Ok(DurableRestoreReadiness::Pending) => match durable_store.clone() {
                    Some(store) => {
                        if actor_ref
                            .tell(RestoreDurableRoomState {
                                store,
                                claim_fence: publication_fence.clone(),
                            })
                            .mailbox_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                            .await
                            .is_ok()
                        {
                            Some(
                                actor_ref
                                    .ask(GetDurableRestoreReadiness)
                                    .mailbox_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                                    .reply_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                                    .await,
                            )
                        } else {
                            None
                        }
                    }
                    None => None,
                },
                other => Some(other),
            };
            // A fresh actor is not published until its complete initial
            // snapshot is durably committed and then restored into memory.
            // Reclaimed rooms already have an authoritative snapshot and
            // therefore only take the restore path above.
            let readiness = match (readiness, creation_spec, durable_store.clone()) {
                (
                    Some(Ok(DurableRestoreReadiness::Ready(DurableRoomOrigin::New))),
                    Some(spec),
                    Some(store),
                ) => match store
                    .commit_room_mutation(
                        &room_jid,
                        &publication_fence,
                        RoomDurableMutation::Create {
                            waddle_id: WaddleId::new(spec.waddle_id.clone()),
                            channel_id: ChannelId::new(spec.channel_id.clone()),
                            config: spec.config.clone(),
                            initial_affiliations: spec.initial_affiliations.clone(),
                        },
                        crate::muc::RoomMutationEffects::none(),
                    )
                    .await
                {
                    Ok(_) => {
                        if actor_ref
                            .tell(RestoreDurableRoomState {
                                store,
                                claim_fence: publication_fence.clone(),
                            })
                            .await
                            .is_err()
                        {
                            None
                        } else {
                            match actor_ref
                                .ask(GetDurableRestoreReadiness)
                                .mailbox_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                                .reply_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                                .await
                            {
                                Ok(DurableRestoreReadiness::Ready(_)) => {
                                    Some(Ok(DurableRestoreReadiness::Ready(DurableRoomOrigin::New)))
                                }
                                other => Some(other),
                            }
                        }
                    }
                    Err(error) => {
                        if matches!(error, RoomCommitError::RecreationBlocked) {
                            let _ = registry_ref
                                .tell(CompleteRoomPreparation {
                                    room_jid,
                                    generation,
                                    readiness: RoomPreparationReadiness::RecreationBlocked,
                                })
                                .await;
                            return;
                        }
                        warn!(room = %room_jid, %error, "failed to commit initial durable room state before publication");
                        None
                    }
                },
                (readiness, _, _) => readiness,
            };
            // A dormant lifecycle becomes active before the restored actor
            // can be published; an already-active lifecycle activates
            // idempotently. `StateMissing` means no live lifecycle exists —
            // publishing would strand a room no mutation can ever commit to.
            let readiness = match (readiness, durable_store.clone()) {
                (
                    ready @ Some(Ok(DurableRestoreReadiness::Ready(DurableRoomOrigin::Restored))),
                    Some(store),
                ) => match store
                    .commit_room_mutation(
                        &room_jid,
                        &publication_fence,
                        RoomDurableMutation::Activate,
                        crate::muc::RoomMutationEffects::none(),
                    )
                    .await
                {
                    Ok(_) => ready,
                    Err(error) => {
                        warn!(room = %room_jid, %error, "failed to activate durable room before publication");
                        None
                    }
                },
                (readiness, _) => readiness,
            };
            // A durable Create is followed by a fenced restore.  Transfer
            // the predecessor's ephemeral roster only after that final
            // restore (and any activation commit), but before the final
            // ownership fence and registry insertion.  This is the single
            // pre-publication point at which durable and live state are both
            // complete, so callers can never discover a roster-less actor
            // and post-publication joins/leaves are never overwritten.
            let readiness = match (readiness, live_room_restore) {
                (ready @ Some(Ok(DurableRestoreReadiness::Ready(_))), Some(restore)) => {
                    match actor_ref
                        .ask(RestoreLiveRoster {
                            room: restore.room,
                            occupancy_revision: restore.occupancy_revision,
                            departures: restore.departures,
                        })
                        .mailbox_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                        .reply_timeout(ROOM_OWNERSHIP_CALL_TIMEOUT)
                        .await
                    {
                        Ok(()) => ready,
                        Err(error) => {
                            warn!(room = %room_jid, %error, "failed to transfer live roster before room publication");
                            None
                        }
                    }
                }
                (readiness, _) => readiness,
            };
            let readiness = match readiness {
                Some(Ok(DurableRestoreReadiness::Ready(durable_origin))) => {
                    let owner = publication_fence.owner();
                    let publication_fence = match tokio::time::timeout(
                        ROOM_OWNERSHIP_CALL_TIMEOUT,
                        claim_store.fence(
                            &publication_fence.entity,
                            &owner,
                            publication_fence.epoch,
                        ),
                    )
                    .await
                    {
                        Ok(Ok(true)) => Ok(()),
                        Ok(Ok(false)) => Err(RoomPublicationError::ClaimLost),
                        Ok(Err(_)) | Err(_) => Err(RoomPublicationError::OwnershipUnavailable),
                    };
                    RoomPreparationReadiness::Ready {
                        durable_origin,
                        publication_fence,
                    }
                }
                Some(Ok(DurableRestoreReadiness::OwnershipLost)) => {
                    RoomPreparationReadiness::ClaimLost
                }
                Some(Ok(DurableRestoreReadiness::Pending)) => RoomPreparationReadiness::Pending,
                Some(Err(_)) | None => RoomPreparationReadiness::Unavailable,
            };
            let _ = registry_ref
                .tell(CompleteRoomPreparation {
                    room_jid,
                    generation,
                    readiness,
                })
                .await;
        });
    }

    fn reply_preparation_success(
        room_jid: &BareJid,
        waiters: Vec<RoomPreparationWaiter>,
        actor_ref: ActorRef<RoomActor>,
        durable_origin: DurableRoomOrigin,
        preparation_origin: &RoomPreparationOrigin,
    ) -> bool {
        if durable_origin == DurableRoomOrigin::Restored {
            for waiter in waiters {
                match waiter {
                    RoomPreparationWaiter::Lookup { reply } => {
                        reply.send(Ok(Some(actor_ref.clone())));
                    }
                    RoomPreparationWaiter::Acquisition { reply, .. } => {
                        reply.send(Ok(RoomAcquisition {
                            actor_ref: actor_ref.clone(),
                            creation: RoomCreation::Existing,
                        }));
                    }
                    RoomPreparationWaiter::ExclusiveCreate { reply, .. } => {
                        reply.send(Err(RoomRegistryError::RoomAlreadyExists(room_jid.clone())));
                    }
                    RoomPreparationWaiter::Reclaimed { reply, success } => reply.send(success),
                }
            }
            return true;
        }

        let RoomPreparationOrigin::Demand { prepared_spec } = preparation_origin else {
            Self::reply_preparation_failure(room_jid, waiters, ReclaimedRoomOutcome::PendingRetry);
            return false;
        };
        let mut creation_handoff_delivered = false;
        let mut deferred = Vec::new();
        for waiter in waiters {
            match waiter {
                RoomPreparationWaiter::Acquisition {
                    reply,
                    creation_spec,
                } if !creation_handoff_delivered
                    && creation_spec.as_ref() == prepared_spec.as_ref() =>
                {
                    if Self::try_send_reply(
                        reply,
                        Ok(RoomAcquisition {
                            actor_ref: actor_ref.clone(),
                            creation: RoomCreation::Created,
                        }),
                    ) {
                        creation_handoff_delivered = true;
                    }
                }
                RoomPreparationWaiter::ExclusiveCreate {
                    reply,
                    creation_spec,
                } if !creation_handoff_delivered
                    && creation_spec.as_ref() == prepared_spec.as_ref() =>
                {
                    if Self::try_send_reply(reply, Ok(actor_ref.clone())) {
                        creation_handoff_delivered = true;
                    }
                }
                waiter => deferred.push(waiter),
            }
        }

        if !creation_handoff_delivered {
            Self::reply_preparation_failure(room_jid, deferred, ReclaimedRoomOutcome::PendingRetry);
            return false;
        }
        for waiter in deferred {
            match waiter {
                RoomPreparationWaiter::Lookup { reply } => {
                    reply.send(Ok(Some(actor_ref.clone())));
                }
                RoomPreparationWaiter::Acquisition { reply, .. } => {
                    reply.send(Ok(RoomAcquisition {
                        actor_ref: actor_ref.clone(),
                        creation: RoomCreation::Existing,
                    }));
                }
                RoomPreparationWaiter::ExclusiveCreate { reply, .. } => {
                    reply.send(Err(RoomRegistryError::RoomAlreadyExists(room_jid.clone())));
                }
                RoomPreparationWaiter::Reclaimed { reply, success } => reply.send(success),
            }
        }
        true
    }

    fn try_send_reply<R>(reply: ReplySender<R>, value: R) -> bool
    where
        R: Reply,
    {
        let boxed = value
            .to_result()
            .map(|value| Box::new(value) as BoxReply)
            .map_err(|error| BoxSendError::HandlerError(Box::new(error)));
        reply.boxed().send(boxed).is_ok()
    }

    fn reply_preparation_failure(
        room_jid: &BareJid,
        waiters: Vec<RoomPreparationWaiter>,
        reclaimed_outcome: ReclaimedRoomOutcome,
    ) {
        for waiter in waiters {
            match waiter {
                RoomPreparationWaiter::Lookup { reply } => {
                    reply.send(Err(RoomRegistryError::OwnershipUnavailable(
                        room_jid.clone(),
                    )));
                }
                RoomPreparationWaiter::Acquisition { reply, .. } => {
                    reply.send(Err(RoomRegistryError::OwnershipUnavailable(
                        room_jid.clone(),
                    )));
                }
                RoomPreparationWaiter::ExclusiveCreate { reply, .. } => {
                    reply.send(Err(RoomRegistryError::OwnershipUnavailable(
                        room_jid.clone(),
                    )));
                }
                RoomPreparationWaiter::Reclaimed { reply, .. } => reply.send(reclaimed_outcome),
            }
        }
    }

    fn reply_preparation_recreation_blocked(
        room_jid: &BareJid,
        waiters: Vec<RoomPreparationWaiter>,
    ) {
        for waiter in waiters {
            match waiter {
                RoomPreparationWaiter::Lookup { reply } => {
                    reply.send(Err(RoomRegistryError::OwnershipReconciliationPending(
                        room_jid.clone(),
                    )));
                }
                RoomPreparationWaiter::Acquisition { reply, .. } => {
                    reply.send(Err(RoomRegistryError::OwnershipReconciliationPending(
                        room_jid.clone(),
                    )));
                }
                RoomPreparationWaiter::ExclusiveCreate { reply, .. } => {
                    reply.send(Err(RoomRegistryError::OwnershipReconciliationPending(
                        room_jid.clone(),
                    )));
                }
                RoomPreparationWaiter::Reclaimed { reply, .. } => {
                    reply.send(ReclaimedRoomOutcome::PendingRetry);
                }
            }
        }
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
        self.rooms.insert(
            room_jid.clone(),
            RoomEntry {
                actor_ref: actor_ref.clone(),
                claim_fence: claim_fence.clone(),
            },
        );
        self.handoff_pending.remove(&room_jid);
        self.publish_room_count();
        // The cache is a legacy room-JID fan-out fence. Make it visible only
        // after its matching ready actor and immutable fence are in the
        // registry, so a predecessor cannot borrow the successor generation.
        if let Some(store) = &self.durable_store {
            store.record_claim_fence(&room_jid, claim_fence.clone());
        }
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
        if self.destroy_attempts.contains_key(room_jid) {
            if self.destroy_attempts.get(room_jid).is_some_and(|retained| {
                matches!(
                    retained.phase,
                    DestroyAttemptPhase::SnapshotPreseal
                        | DestroyAttemptPhase::RegisteredPreDestroy
                )
            }) {
                // The owner-IQ path has sealed this actor and is persisting
                // its recipient snapshot. Do not reopen it merely because a
                // competing lookup arrived before that bounded protocol can
                // register the completion or issue its abort.
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
            self.reconcile_destroy_attempt(room_jid).await;
            if self.destroy_attempts.contains_key(room_jid) {
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    room_jid.clone(),
                ));
            }
        }
        if self.poisoned_rooms.contains(room_jid) {
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        if self.pending_room_preparations.contains_key(room_jid) {
            return Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid.clone(),
            ));
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
            self.publish_room_count();
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

    /// Reconcile a failed terminal destroy before making the retained room
    /// serviceable again. Re-asking is deliberate: the first pre-seal reply
    /// could have been lost, and the matching token prevents a delayed
    /// recovery from reopening a newer destroy attempt.
    async fn recover_failed_destroy(
        &mut self,
        room_jid: &BareJid,
        entry: &RoomEntry,
        attempt: super::DestroyAttemptId,
    ) {
        let seal = entry
            .actor_ref
            .ask(SealForDestroy { attempt })
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await;
        let matching_attempt = matches!(
            seal,
            Ok(RoomSealState::Destroying { attempt: current }) if current == attempt
        );
        if !matching_attempt {
            warn!(room = %room_jid, "failed destroy could not confirm its matching pre-seal attempt");
            return;
        }

        match entry
            .actor_ref
            .ask(UnsealDestroy { attempt })
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await
        {
            Ok(true) => {
                self.destroy_attempts.remove(room_jid);
                info!(room = %room_jid, "reopened room after durable destroy commit failure");
            }
            Ok(false) | Err(_) => {
                warn!(room = %room_jid, "failed destroy could not unseal its matching attempt");
            }
        }
    }

    /// Resume a destroy that reached the actor but lost its registry reply.
    /// The attempt id is retained until the same actor confirms the seal and
    /// the terminal durable transition either completes or is safely reopened.
    /// This runs on ordinary lookup and reaper touches, not only on another
    /// explicit destroy request, so an ambiguous reply cannot wedge a room.
    async fn reconcile_destroy_attempt(&mut self, room_jid: &BareJid) {
        let Some(retained) = self.destroy_attempts.get(room_jid).cloned() else {
            return;
        };
        let attempt = retained.attempt;
        let Some(entry) = self.rooms.get(room_jid).cloned() else {
            self.destroy_attempts.remove(room_jid);
            return;
        };
        if !entry.actor_ref.is_alive() {
            // Let the normal dead-actor path retire the exact fence.  The
            // attempt belongs to the dead incarnation, never its successor.
            self.destroy_attempts.remove(room_jid);
            return;
        }

        if matches!(retained.phase, DestroyAttemptPhase::SnapshotPreseal) {
            match entry
                .actor_ref
                .ask(UnsealDestroy { attempt })
                .mailbox_timeout(SEAL_ASK_TIMEOUT)
                .reply_timeout(SEAL_ASK_TIMEOUT)
                .await
            {
                Ok(true) => {
                    self.destroy_attempts.remove(room_jid);
                    info!(room = %room_jid, "reopened abandoned destroy snapshot pre-seal");
                }
                Ok(false) | Err(_) => {
                    warn!(room = %room_jid, "could not reconcile abandoned destroy snapshot pre-seal");
                }
            }
            return;
        }
        if matches!(retained.phase, DestroyAttemptPhase::RegisteredPreDestroy) {
            // Once the owner-IQ completion is registered, only a definite
            // non-delivery may reopen the seal. A lost caller/task deadline
            // must keep reconciling toward the terminal destroy instead.
        }

        let sealed = matches!(
            entry
                .actor_ref
                .ask(SealForDestroy { attempt })
                .mailbox_timeout(SEAL_ASK_TIMEOUT)
                .reply_timeout(SEAL_ASK_TIMEOUT)
                .await,
            Ok(RoomSealState::Destroying { attempt: current }) if current == attempt
        );
        if !sealed {
            warn!(room = %room_jid, "destroy reconciliation could not confirm its matching pre-seal attempt");
            return;
        }

        let commit = match &self.durable_store {
            Some(store) => {
                let committed_completion = retained.completion.clone();
                store
                    .commit_room_mutation(
                        room_jid,
                        &entry.claim_fence,
                        RoomDurableMutation::Destroy {
                            completion_attempt: committed_completion
                                .as_ref()
                                .map(|completion| completion.attempt),
                        },
                        committed_completion
                            .as_ref()
                            .map(Self::destroy_effects)
                            .unwrap_or_else(RoomMutationEffects::none),
                    )
                    .await
            }
            None => Ok(super::RoomCommitOutcome {
                coordinates: super::RoomCommittedCoordinates {
                    lifecycle: super::RoomLifecycleId::generate(),
                    revision: super::RoomRevision::initial(),
                },
                reservation: None,
            }),
        };
        match commit {
            Ok(_) => {
                // The durable destroy has committed, but exact claim release
                // can still fail. Reserve its retry slot before removing the
                // sole fence-bearing entry; otherwise a saturated backlog
                // would strand this still-owned generation until node expiry.
                if !self.remember_pending_room_release(room_jid.clone(), entry.claim_fence.clone())
                {
                    warn!(
                        room = %room_jid,
                        "release backlog saturated; retaining room entry and destroy attempt for a later reconciliation"
                    );
                    return;
                }
                self.rooms.remove(room_jid);
                self.publish_room_count();
                self.poisoned_rooms.remove(room_jid);
                if let Some(completion) = retained.completion {
                    self.pending_destroy_completions.push_back(completion);
                }
                self.destroy_attempts.remove(room_jid);
                self.release_room_claim(room_jid, &entry.claim_fence).await;
                info!(room = %room_jid, "completed a previously ambiguous room destroy");
            }
            Err(RoomCommitError::NotOwner) => {
                // A deposed actor must never be reopened. Preserve any
                // old-identity release responsibility while retiring it.
                self.destroy_attempts.remove(room_jid);
                self.evict_ownership_lost_room(room_jid, entry).await;
                info!(room = %room_jid, "evicted room after terminal destroy reconciliation result");
            }
            Err(RoomCommitError::StateMissing) => {
                // This exact fence passed the durable gate before the state
                // miss, so it may still own a claim. Reserve release-retry
                // capacity BEFORE teardown: with a saturated backlog a
                // transient release failure after removal would leave a
                // still-owned fence with no local record. On saturation the
                // entry and attempt are retained so the next registry touch
                // re-enters this reconciliation.
                if !self.remember_pending_room_release(room_jid.clone(), entry.claim_fence.clone())
                {
                    warn!(
                        room = %room_jid,
                        "release backlog saturated; retaining room entry and destroy attempt for a later reconciliation"
                    );
                    return;
                }
                self.rooms.remove(room_jid);
                self.publish_room_count();
                self.poisoned_rooms.remove(room_jid);
                if let Some(completion) = retained.completion {
                    self.pending_destroy_completions.push_back(completion);
                }
                self.destroy_attempts.remove(room_jid);
                entry.actor_ref.kill();
                self.release_room_claim(room_jid, &entry.claim_fence).await;
                info!(room = %room_jid, "released exact claim after terminal destroy reconciliation state miss");
            }
            Err(RoomCommitError::CommitOutcomeUnknown) => {
                // `COMMIT` may have succeeded even though the coordinating
                // read-back failed. Reopening would resurrect a room whose
                // durable lifecycle may already be tombstoned.
                warn!(room = %room_jid, "destroy reconciliation has unknown durable commit outcome; retaining the seal");
            }
            Err(error) => {
                warn!(room = %room_jid, %error, "transient destroy reconciliation failure; reopening matching actor attempt");
                self.recover_failed_destroy(room_jid, &entry, attempt).await;
            }
        }
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
        if claim_fence.entity != entity {
            return ReclaimedRoomOutcome::LostRace;
        }
        match tokio::time::timeout(
            timeout,
            self.claim_store.release_exact(
                &claim_fence.entity,
                &claim_fence.owner(),
                claim_fence.epoch,
            ),
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

impl RoomRegistryActor {
    async fn reconcile_unpublished_preparation_destroy_attempt(
        store: Arc<dyn MucDurableStore>,
        claim_store: Arc<dyn ClaimStore>,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        phase: UnpublishedDestroyPhase,
        completion: Option<DestroyCompletion>,
    ) -> UnpublishedPreparationDestroyOutcome {
        match tokio::time::timeout(
            ROOM_OWNERSHIP_CALL_TIMEOUT,
            claim_store.current_claim_after_pending_writes(&claim_fence.entity),
        )
        .await
        {
            Ok(Ok(Some(snapshot)))
                if snapshot.owner == claim_fence.owner()
                    && snapshot.claim_epoch == claim_fence.epoch =>
            {
                let intent = match phase {
                    UnpublishedDestroyPhase::MarkCleanup => {
                        RoomDurableMutation::MarkUnpublishedCleanup
                    }
                    UnpublishedDestroyPhase::Destroy => {
                        RoomDurableMutation::DestroyAndReleaseClaim {
                            completion_attempt: None,
                        }
                    }
                    UnpublishedDestroyPhase::RecoverPreparingDestroy => {
                        RoomDurableMutation::DestroyAndReleaseClaim {
                            completion_attempt: completion
                                .as_ref()
                                .map(|completion| completion.attempt),
                        }
                    }
                };
                let effects = match phase {
                    UnpublishedDestroyPhase::RecoverPreparingDestroy => completion
                        .as_ref()
                        .map(Self::destroy_effects)
                        .unwrap_or_else(RoomMutationEffects::none),
                    UnpublishedDestroyPhase::MarkCleanup | UnpublishedDestroyPhase::Destroy => {
                        RoomMutationEffects::none()
                    }
                };
                match store
                    .commit_room_mutation(&room_jid, &claim_fence, intent, effects)
                    .await
                {
                    Ok(_) => match phase {
                        UnpublishedDestroyPhase::MarkCleanup => {
                            UnpublishedPreparationDestroyOutcome::CleanupMarked
                        }
                        UnpublishedDestroyPhase::Destroy
                        | UnpublishedDestroyPhase::RecoverPreparingDestroy => {
                            UnpublishedPreparationDestroyOutcome::Committed
                        }
                    },
                    Err(RoomCommitError::CommitOutcomeUnknown) => {
                        UnpublishedPreparationDestroyOutcome::CommitOutcomeUnknown
                    }
                    Err(_) => UnpublishedPreparationDestroyOutcome::Failed,
                }
            }
            Ok(Ok(_))
                if matches!(
                    phase,
                    UnpublishedDestroyPhase::Destroy
                        | UnpublishedDestroyPhase::RecoverPreparingDestroy
                ) =>
            {
                match tokio::time::timeout(
                    ROOM_OWNERSHIP_CALL_TIMEOUT,
                    store.find_preparing_room(&room_jid),
                )
                .await
                {
                    Ok(Ok(None)) => UnpublishedPreparationDestroyOutcome::Committed,
                    Ok(Ok(Some(_))) | Ok(Err(_)) | Err(_) => {
                        UnpublishedPreparationDestroyOutcome::Failed
                    }
                }
            }
            Ok(Ok(_)) => UnpublishedPreparationDestroyOutcome::Failed,
            Ok(Err(_)) | Err(_) => UnpublishedPreparationDestroyOutcome::Failed,
        }
    }

    /// Retain a creator room whose publication/handoff may already be
    /// durable, then durably mark it non-serving before terminal deletion.
    /// This is shared by a definitely lost reply and a Publish acknowledgement
    /// loss: neither may fall back to a bare exact-claim release.
    fn begin_unpublished_cleanup(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        store: Arc<dyn MucDurableStore>,
        registry_ref: ActorRef<Self>,
    ) {
        self.poisoned_rooms.insert(room_jid.clone());
        self.pending_unpublished_destroys
            .entry((room_jid.clone(), claim_fence.clone()))
            .or_insert(UnpublishedDestroyPhase::MarkCleanup);
        tokio::spawn(async move {
            let outcome = match store
                .commit_room_mutation(
                    &room_jid,
                    &claim_fence,
                    RoomDurableMutation::MarkUnpublishedCleanup,
                    crate::muc::RoomMutationEffects::none(),
                )
                .await
            {
                Ok(_) => UnpublishedPreparationDestroyOutcome::CleanupMarked,
                Err(RoomCommitError::CommitOutcomeUnknown) => {
                    UnpublishedPreparationDestroyOutcome::CommitOutcomeUnknown
                }
                Err(_) => UnpublishedPreparationDestroyOutcome::Failed,
            };
            let _ = registry_ref
                .tell(CompleteUnpublishedPreparationDestroy {
                    room_jid,
                    claim_fence,
                    outcome,
                })
                .await;
        });
    }

    fn begin_preparing_destroy_recovery(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        registry_ref: ActorRef<Self>,
    ) {
        self.poisoned_rooms.insert(room_jid.clone());
        self.pending_unpublished_destroys.insert(
            (room_jid.clone(), claim_fence.clone()),
            UnpublishedDestroyPhase::RecoverPreparingDestroy,
        );
        std::mem::drop(
            registry_ref
                .tell(RetryUnpublishedPreparationDestroy {
                    room_jid,
                    claim_fence,
                })
                .send_after(std::time::Duration::ZERO),
        );
    }

    fn abandon_preparation_for_terminal(
        &mut self,
        room_jid: BareJid,
        pending: PendingRoomPreparation,
        registry_ref: ActorRef<Self>,
    ) {
        let claim_fence = pending.claim_fence;
        drop(pending.guard);
        self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
        self.transfer_exact_responsibility_to_pending_release(
            room_jid.clone(),
            claim_fence.clone(),
        );
        self.start_detached_room_release(room_jid.clone(), claim_fence, registry_ref);
        Self::reply_preparation_failure(
            &room_jid,
            pending.waiters,
            ReclaimedRoomOutcome::PendingRetry,
        );
    }

    fn finish_preparation_outcome(
        &mut self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        origin: RoomPreparationOrigin,
        waiters: Vec<RoomPreparationWaiter>,
        publication: Result<(ActorRef<RoomActor>, DurableRoomOrigin), RoomPublicationError>,
        registry_ref: ActorRef<Self>,
    ) {
        // #1647: a demand preparation that fails after an accepted handoff
        // transition drops its prepared spec — the only copy of the retired
        // actor's live roster and departure ledger. Inside an open handoff
        // window, restash it on the pending marker so the next demand
        // creation adopts it instead of hydrating a rosterless successor.
        // (A successful publication removed the marker at registration, so
        // this is a no-op there; adoption is also gated on the room still
        // being absent, so a durably-published room can never regress.)
        if publication.is_err() {
            if let RoomPreparationOrigin::Demand { prepared_spec } = &origin {
                if prepared_spec.live_room_restore.is_some() {
                    if let Some(pending) = self.handoff_pending.get_mut(&room_jid) {
                        pending.stashed_spec = Some(Arc::clone(prepared_spec));
                    }
                }
            }
        }
        match publication {
            Ok((actor_ref, durable_origin)) => {
                let creation_handoff_delivered = Self::reply_preparation_success(
                    &room_jid,
                    waiters,
                    actor_ref.clone(),
                    durable_origin,
                    &origin,
                );
                if !creation_handoff_delivered {
                    let matching_entry = self
                        .rooms
                        .get(&room_jid)
                        .is_some_and(|entry| entry.claim_fence == claim_fence);
                    if matching_entry {
                        self.rooms.remove(&room_jid);
                        self.publish_room_count();
                    }
                    actor_ref.kill();
                    if let Some(store) = self.durable_store.clone() {
                        self.begin_unpublished_cleanup(room_jid, claim_fence, store, registry_ref);
                    } else {
                        self.transfer_exact_responsibility_to_pending_release(
                            room_jid.clone(),
                            claim_fence.clone(),
                        );
                        self.start_detached_room_release(room_jid, claim_fence, registry_ref);
                    }
                }
            }
            Err(error) => {
                if matches!(error, RoomPublicationError::PublishOutcomeUnknown) {
                    if let Some(store) = self.durable_store.clone() {
                        self.begin_unpublished_cleanup(
                            room_jid.clone(),
                            claim_fence,
                            store,
                            registry_ref,
                        );
                    } else {
                        // Store-less publication cannot produce a durable
                        // acknowledgement ambiguity, but retain the normal
                        // exact-release path defensively.
                        self.transfer_exact_responsibility_to_pending_release(
                            room_jid.clone(),
                            claim_fence,
                        );
                    }
                    Self::reply_preparation_failure(
                        &room_jid,
                        waiters,
                        ReclaimedRoomOutcome::PendingRetry,
                    );
                    return;
                }
                let reclaimed_outcome = match origin {
                    RoomPreparationOrigin::Demand { .. } => match error {
                        // A same-identity database miss proves the tuple is
                        // gone. Local identity supersession is also terminal
                        // for the actor, but the old exact row may still need
                        // a conditional release.
                        RoomPublicationError::ClaimLost => {
                            self.clear_pending_room_acquisition(&room_jid, &claim_fence.owner());
                            self.finish_unpublished_ownership_loss(
                                room_jid.clone(),
                                claim_fence.clone(),
                                registry_ref.clone(),
                            )
                        }
                        RoomPublicationError::LocalIdentityChanged
                        | RoomPublicationError::OwnershipUnavailable
                        | RoomPublicationError::ReconciliationPending => {
                            self.transfer_exact_responsibility_to_pending_release(
                                room_jid.clone(),
                                claim_fence.clone(),
                            );
                            self.start_detached_room_release(
                                room_jid.clone(),
                                claim_fence.clone(),
                                registry_ref,
                            );
                            ReclaimedRoomOutcome::PendingRetry
                        }
                        RoomPublicationError::PublishOutcomeUnknown => {
                            unreachable!("handled before preparation-origin dispatch")
                        }
                    },
                    RoomPreparationOrigin::Reclaimed { previous_owner } => match error {
                        RoomPublicationError::ClaimLost => {
                            self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
                            self.finish_unpublished_ownership_loss(
                                room_jid.clone(),
                                claim_fence.clone(),
                                registry_ref.clone(),
                            )
                        }
                        RoomPublicationError::LocalIdentityChanged => {
                            self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
                            self.transfer_exact_responsibility_to_pending_release(
                                room_jid.clone(),
                                claim_fence.clone(),
                            );
                            self.start_detached_room_release(
                                room_jid.clone(),
                                claim_fence,
                                registry_ref,
                            );
                            ReclaimedRoomOutcome::PendingRetry
                        }
                        RoomPublicationError::OwnershipUnavailable
                        | RoomPublicationError::ReconciliationPending => {
                            self.remember_pending_reclaimed_room(
                                room_jid.clone(),
                                claim_fence,
                                previous_owner,
                            );
                            ReclaimedRoomOutcome::PendingRetry
                        }
                        RoomPublicationError::PublishOutcomeUnknown => {
                            unreachable!(
                                "only fresh demand publication can lose Publish acknowledgement"
                            )
                        }
                    },
                };
                Self::reply_preparation_failure(&room_jid, waiters, reclaimed_outcome);
            }
        }
    }
}

impl kameo::message::Message<CompleteRoomPreparation> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CompleteRoomPreparation,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let is_current = self
            .pending_room_preparations
            .get(&msg.room_jid)
            .is_some_and(|pending| pending.generation == msg.generation);
        if !is_current {
            return;
        }
        if self.terminal_claim_acquisition_disabled {
            let pending = self
                .pending_room_preparations
                .remove(&msg.room_jid)
                .expect("generation was checked in the same mailbox turn");
            self.abandon_preparation_for_terminal(msg.room_jid, pending, ctx.actor_ref().clone());
            return;
        }

        if matches!(msg.readiness, RoomPreparationReadiness::RecreationBlocked) {
            let pending = self
                .pending_room_preparations
                .remove(&msg.room_jid)
                .expect("generation was checked in the same mailbox turn");
            let claim_fence = pending.claim_fence.clone();
            drop(pending.guard);
            self.transfer_exact_responsibility_to_pending_release(
                msg.room_jid.clone(),
                claim_fence.clone(),
            );
            self.start_detached_room_release(
                msg.room_jid.clone(),
                claim_fence,
                ctx.actor_ref().clone(),
            );
            Self::reply_preparation_recreation_blocked(&msg.room_jid, pending.waiters);
            return;
        }

        let failure = match msg.readiness {
            RoomPreparationReadiness::Ready {
                durable_origin,
                publication_fence: Ok(()),
            } => {
                self.ready_room_publications
                    .push_back(ReadyRoomPublication {
                        room_jid: msg.room_jid,
                        generation: msg.generation,
                        durable_origin,
                    });
                self.schedule_ready_room_publication(ctx.actor_ref());
                return;
            }
            RoomPreparationReadiness::Ready {
                publication_fence: Err(error),
                ..
            } => error,
            RoomPreparationReadiness::ClaimLost => RoomPublicationError::ClaimLost,
            RoomPreparationReadiness::Pending | RoomPreparationReadiness::Unavailable => {
                RoomPublicationError::OwnershipUnavailable
            }
            RoomPreparationReadiness::RecreationBlocked => unreachable!("handled above"),
        };
        let pending = self
            .pending_room_preparations
            .remove(&msg.room_jid)
            .expect("generation was checked in the same mailbox turn");
        let claim_fence = pending.claim_fence.clone();
        let origin = pending.origin.clone();
        let waiters = pending.waiters;
        drop(pending.guard);
        self.finish_preparation_outcome(
            msg.room_jid,
            claim_fence,
            origin,
            waiters,
            Err(failure),
            ctx.actor_ref().clone(),
        );
    }
}

impl kameo::message::Message<PublishNextReadyRoom> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: PublishNextReadyRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.ready_room_publication_scheduled = false;
        let Some(ready) = self.ready_room_publications.pop_front() else {
            return;
        };
        let is_current = self
            .pending_room_preparations
            .get(&ready.room_jid)
            .is_some_and(|pending| pending.generation == ready.generation);
        if is_current {
            let pending = self
                .pending_room_preparations
                .remove(&ready.room_jid)
                .expect("generation was checked in the same mailbox turn");
            if self.terminal_claim_acquisition_disabled {
                self.abandon_preparation_for_terminal(
                    ready.room_jid,
                    pending,
                    ctx.actor_ref().clone(),
                );
            } else {
                let claim_fence = pending.claim_fence.clone();
                let origin = pending.origin.clone();
                let waiters = pending.waiters;
                let publication = self
                    .publish_prepared_room(
                        ready.room_jid.clone(),
                        pending.guard,
                        claim_fence.clone(),
                        ready.durable_origin,
                    )
                    .await
                    .map(|actor_ref| (actor_ref, ready.durable_origin));
                self.finish_preparation_outcome(
                    ready.room_jid,
                    claim_fence,
                    origin,
                    waiters,
                    publication,
                    ctx.actor_ref().clone(),
                );
            }
        }
        self.schedule_ready_room_publication(ctx.actor_ref());
    }
}

impl kameo::message::Message<CompleteDetachedRoomRelease> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CompleteDetachedRoomRelease,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self
            .pending_room_releases
            .contains_key(&(msg.room_jid.clone(), msg.claim_fence.clone()))
        {
            return;
        }
        match msg.outcome {
            DetachedRoomReleaseOutcome::Released | DetachedRoomReleaseOutcome::NotOwned => {
                self.clear_pending_room_release(&msg.room_jid, &msg.claim_fence);
                if let Some(store) = &self.durable_store {
                    store.forget_claim_fence(&msg.room_jid, &msg.claim_fence);
                }
            }
            DetachedRoomReleaseOutcome::Retry => {
                self.schedule_pending_room_retry(ctx.actor_ref());
            }
        }
    }
}

impl kameo::message::Message<CompleteUnpublishedPreparationDestroy> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CompleteUnpublishedPreparationDestroy,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match msg.outcome {
            UnpublishedPreparationDestroyOutcome::CleanupMarked => {
                let key = (msg.room_jid.clone(), msg.claim_fence.clone());
                if let Some(phase) = self.pending_unpublished_destroys.get_mut(&key) {
                    *phase = UnpublishedDestroyPhase::Destroy;
                    std::mem::drop(
                        ctx.actor_ref()
                            .tell(RetryUnpublishedPreparationDestroy {
                                room_jid: msg.room_jid,
                                claim_fence: msg.claim_fence,
                            })
                            .send_after(std::time::Duration::ZERO),
                    );
                }
            }
            UnpublishedPreparationDestroyOutcome::Committed => {
                let phase = self
                    .pending_unpublished_destroys
                    .remove(&(msg.room_jid.clone(), msg.claim_fence.clone()));
                if phase == Some(UnpublishedDestroyPhase::RecoverPreparingDestroy) {
                    if let Some(RetainedDestroyAttempt {
                        completion: Some(completion),
                        ..
                    }) = self.destroy_attempts.remove(&msg.room_jid)
                    {
                        self.pending_destroy_completions.push_back(completion);
                    }
                }
                self.poisoned_rooms.remove(&msg.room_jid);
                self.release_room_claim(&msg.room_jid, &msg.claim_fence)
                    .await;
                info!(room = %msg.room_jid, "terminally deleted unpublished room after creator handoff loss");
            }
            UnpublishedPreparationDestroyOutcome::CommitOutcomeUnknown
            | UnpublishedPreparationDestroyOutcome::Failed => {
                // `CommitOutcomeUnknown` is intentionally not folded into a
                // normal write failure. Its delayed retry first reads the
                // exact claim after pending writes: an absent/foreign fence
                // proves this local responsibility has already converged,
                // while the same fence proves a retry remains authorized.
                warn!(room = %msg.room_jid, "could not confirm terminal delete of unpublished durable room after creator handoff loss");
                std::mem::drop(
                    ctx.actor_ref()
                        .tell(RetryUnpublishedPreparationDestroy {
                            room_jid: msg.room_jid,
                            claim_fence: msg.claim_fence,
                        })
                        .send_after(PENDING_ROOM_RETRY_DELAY),
                );
            }
        }
    }
}

impl kameo::message::Message<RetryUnpublishedPreparationDestroy> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RetryUnpublishedPreparationDestroy,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(phase) = self
            .pending_unpublished_destroys
            .get(&(msg.room_jid.clone(), msg.claim_fence.clone()))
            .copied()
        else {
            return;
        };
        let Some(store) = self.durable_store.clone() else {
            return;
        };
        let room_jid = msg.room_jid.clone();
        let claim_fence = msg.claim_fence.clone();
        let registry_ref = ctx.actor_ref().clone();
        let claim_store = Arc::clone(&self.claim_store);
        let completion = self
            .destroy_attempts
            .get(&room_jid)
            .and_then(|retained| retained.completion.clone());
        tokio::spawn(async move {
            let outcome = RoomRegistryActor::reconcile_unpublished_preparation_destroy_attempt(
                store,
                claim_store,
                room_jid.clone(),
                claim_fence.clone(),
                phase,
                completion,
            )
            .await;
            let _ = registry_ref
                .tell(CompleteUnpublishedPreparationDestroy {
                    room_jid,
                    claim_fence,
                    outcome,
                })
                .await;
        });
    }
}

pub const MAX_PENDING_RECLAIMED_ROOMS: usize = 128;
pub const MAX_PENDING_ROOM_RELEASES: usize = 128;
pub const MAX_PENDING_ROOM_ACQUISITIONS: usize = 128;
/// Combined admission cap for unique unpublished-room, exact terminal, and
/// unfenced reclaimed-reservation responsibilities. The same exact fence can
/// temporarily appear in both reclaimed and preparation/release state during
/// a representation transfer; it consumes one slot, while different fences
/// remain distinct.
pub const MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES: usize =
    MAX_PENDING_ROOM_RELEASES + MAX_PENDING_RECLAIMED_ROOMS;
pub const MAX_ROOM_PREPARATION_WAITERS: usize = 64;
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
                .saturating_add(self.pending_room_acquisitions.len())
                .saturating_add(self.pending_unpublished_destroys.len()),
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

/// Test seam: set (or age) the handoff marker for a room as if a handoff had
/// retired its actor `age` ago without publishing a successor.
#[cfg(test)]
struct MarkHandoffPendingForTest {
    room_jid: BareJid,
    age: std::time::Duration,
    restore: Option<Arc<RoomCreationSpec>>,
}

#[cfg(test)]
impl kameo::message::Message<MarkHandoffPendingForTest> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: MarkHandoffPendingForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.handoff_pending.insert(
            msg.room_jid,
            PendingHandoff {
                since: std::time::Instant::now() - msg.age,
                stashed_spec: msg.restore,
            },
        );
    }
}

impl kameo::message::Message<GetRoom> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<Option<ActorRef<RoomActor>>, RoomRegistryError>>;

    async fn handle(&mut self, msg: GetRoom, ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        // Creator-handoff cleanup has deliberately removed the local actor
        // and retained a durable terminal marker. It is not a dead actor:
        // probes observe no room while demand retries wait for cleanup,
        // reserving RoomActorStateLost for poison-only failures.
        if self
            .pending_unpublished_destroys
            .keys()
            .any(|(room_jid, _)| room_jid == &msg.room_jid)
        {
            return ctx.reply(Ok(None));
        }
        if let Some(pending) = self.pending_room_preparations.get_mut(&msg.room_jid) {
            if !Self::preparation_waiter_capacity_available(pending) {
                return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                    msg.room_jid,
                )));
            }
            let (delegated, reply) = ctx.reply_sender();
            if let Some(reply) = reply {
                pending
                    .waiters
                    .push(RoomPreparationWaiter::Lookup { reply });
            }
            return delegated;
        }
        if self.handoff_in_window(&msg.room_jid) {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                msg.room_jid,
            )));
        }
        ctx.reply(self.live_room(&msg.room_jid).await)
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
pub struct DrainRoomOwnershipForShutdown {
    pub pending_handoffs: Vec<PendingReclaimedRoom>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub struct RoomOwnershipDrainOutcome {
    pub released: usize,
    pub preserved_live: usize,
    pub retained: usize,
}

struct RoomOwnershipShutdownReconciliation {
    preserved_live: usize,
    retained: usize,
    reservation_owned: Vec<(BareJid, super::RoomClaimFenceContext)>,
}

impl RoomRegistryActor {
    fn begin_room_ownership_shutdown(&mut self, pending_handoffs: Vec<PendingReclaimedRoom>) {
        self.terminal_claim_acquisition_disabled = true;
        self.ready_room_publications.clear();
        self.ready_room_publication_scheduled = false;
        let pending_preparations = std::mem::take(&mut self.pending_room_preparations);
        for (room_jid, pending) in pending_preparations {
            let claim_fence = pending.claim_fence.clone();
            Self::reply_preparation_failure(
                &room_jid,
                pending.waiters,
                ReclaimedRoomOutcome::PendingRetry,
            );
            drop(pending.guard);
            self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
            self.transfer_exact_responsibility_to_pending_release(room_jid, claim_fence);
        }
        for pending in pending_handoffs {
            self.remember_pending_reclaimed_room(
                pending.room_jid,
                pending.claim_fence,
                pending.previous_owner,
            );
        }
    }

    async fn reconcile_pending_unpublished_destroys_for_shutdown(&mut self) -> (usize, usize) {
        let Some(store) = self.durable_store.clone() else {
            return (0, self.pending_unpublished_destroys.len());
        };
        let claim_store = Arc::clone(&self.claim_store);
        let mut released = 0usize;
        let mut pending = self
            .pending_unpublished_destroys
            .keys()
            .cloned()
            .collect::<VecDeque<_>>();
        while let Some((room_jid, claim_fence)) = pending.pop_front() {
            let Some(phase) = self
                .pending_unpublished_destroys
                .get(&(room_jid.clone(), claim_fence.clone()))
                .copied()
            else {
                continue;
            };
            let completion = self
                .destroy_attempts
                .get(&room_jid)
                .and_then(|retained| retained.completion.clone());
            match Self::reconcile_unpublished_preparation_destroy_attempt(
                Arc::clone(&store),
                Arc::clone(&claim_store),
                room_jid.clone(),
                claim_fence.clone(),
                phase,
                completion,
            )
            .await
            {
                UnpublishedPreparationDestroyOutcome::CleanupMarked => {
                    if let Some(current_phase) = self
                        .pending_unpublished_destroys
                        .get_mut(&(room_jid.clone(), claim_fence.clone()))
                    {
                        *current_phase = UnpublishedDestroyPhase::Destroy;
                        pending.push_front((room_jid, claim_fence));
                    }
                }
                UnpublishedPreparationDestroyOutcome::Committed => {
                    self.transfer_exact_responsibility_to_pending_release(
                        room_jid.clone(),
                        claim_fence.clone(),
                    );
                    let phase = self
                        .pending_unpublished_destroys
                        .remove(&(room_jid.clone(), claim_fence.clone()));
                    if phase == Some(UnpublishedDestroyPhase::RecoverPreparingDestroy) {
                        if let Some(RetainedDestroyAttempt {
                            completion: Some(completion),
                            ..
                        }) = self.destroy_attempts.remove(&room_jid)
                        {
                            self.pending_destroy_completions.push_back(completion);
                        }
                    }
                    self.poisoned_rooms.remove(&room_jid);
                    self.release_room_claim(&room_jid, &claim_fence).await;
                    if !self
                        .pending_room_releases
                        .contains_key(&(room_jid.clone(), claim_fence.clone()))
                    {
                        released += 1;
                    }
                }
                UnpublishedPreparationDestroyOutcome::CommitOutcomeUnknown
                | UnpublishedPreparationDestroyOutcome::Failed => {}
            }
        }
        (released, self.pending_unpublished_destroys.len())
    }

    /// Resolve demand-side claim CAS calls that may have committed after
    /// their response future timed out. Terminal shutdown cannot rely on the
    /// normal retry timer, so this waits behind already-issued writes and
    /// transfers every observed self-owned epoch into exact release state.
    async fn reconcile_uncertain_room_acquisitions_for_shutdown(
        &mut self,
    ) -> RoomOwnershipShutdownReconciliation {
        let pending_acquisitions = self
            .pending_room_acquisitions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let claim_store = Arc::clone(&self.claim_store);
        let acquisition_claims = futures::future::join_all(pending_acquisitions.into_iter().map(
            |(room_jid, attempted_owner)| {
                let claim_store = Arc::clone(&claim_store);
                async move {
                    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
                    let current = tokio::time::timeout(
                        ROOM_OWNERSHIP_CALL_TIMEOUT,
                        claim_store.current_claim_after_pending_writes(&entity),
                    )
                    .await;
                    (room_jid, attempted_owner, entity, current)
                }
            },
        ))
        .await;

        let mut preserved_live = 0usize;
        let mut retained = 0usize;
        for (room_jid, attempted_owner, entity, current) in acquisition_claims {
            match current {
                Ok(Ok(Some(snapshot))) if snapshot.owner == attempted_owner => {
                    let claim_fence = super::RoomClaimFenceContext::new(
                        entity,
                        snapshot.owner,
                        snapshot.claim_epoch,
                    );
                    if self.has_live_room_with_fence(&room_jid, &claim_fence) {
                        self.clear_pending_room_acquisition(&room_jid, &attempted_owner);
                        preserved_live += 1;
                    } else {
                        self.transfer_pending_room_acquisition_to_pending_release(
                            room_jid,
                            attempted_owner,
                            claim_fence,
                        );
                    }
                }
                Ok(Ok(_)) => {
                    self.clear_pending_room_acquisition(&room_jid, &attempted_owner);
                }
                Ok(Err(error)) => {
                    warn!(room = %room_jid, %error, "terminal room-acquisition lookup failed; retaining uncertain acquisition until node expiry");
                    retained += 1;
                }
                Err(_) => {
                    warn!(room = %room_jid, "terminal room-acquisition lookup timed out; retaining uncertain acquisition until node expiry");
                    retained += 1;
                }
            }
        }
        RoomOwnershipShutdownReconciliation {
            preserved_live,
            retained,
            reservation_owned: Vec::new(),
        }
    }

    /// Convert ambiguous orphan-reaper reservations into exact fences after
    /// all already-issued steal writes settle. Unlike demand acquisitions, a
    /// non-self snapshot stays ambiguous because the dropped steal may still
    /// be the writer ordered behind that observation.
    async fn reconcile_reclaimed_room_reservations_for_shutdown(
        &mut self,
    ) -> RoomOwnershipShutdownReconciliation {
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
                    let claim_fence = super::RoomClaimFenceContext::new(
                        entity,
                        snapshot.owner,
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
        RoomOwnershipShutdownReconciliation {
            preserved_live,
            retained,
            reservation_owned,
        }
    }

    /// Drain every exact ordinary/reclaimed release responsibility in one
    /// concurrent terminal batch after ambiguous state has been reconciled.
    async fn release_exact_room_ownership_for_shutdown(
        &mut self,
        reservation_owned: Vec<(BareJid, super::RoomClaimFenceContext)>,
    ) -> RoomOwnershipDrainOutcome {
        let duplicate_live = self
            .pending_reclaimed_rooms
            .keys()
            .filter(|(room_jid, claim_fence)| self.has_live_room_with_fence(room_jid, claim_fence))
            .cloned()
            .collect::<Vec<_>>();
        for (room_jid, claim_fence) in &duplicate_live {
            self.clear_pending_reclaimed_room(room_jid, claim_fence);
        }
        let preserved_live = duplicate_live.len();

        let mut pending = self
            .pending_room_releases
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        pending.extend(self.pending_reclaimed_rooms.keys().cloned());
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
        let mut retained = 0usize;
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
                    warn!(room = %room_jid, %error, "terminal room-ownership release failed; retaining exact fence until node expiry");
                    retained += 1;
                }
                Err(_) => {
                    warn!(room = %room_jid, "terminal room-ownership release timed out; retaining exact fence until node expiry");
                    retained += 1;
                }
            }
        }
        RoomOwnershipDrainOutcome {
            released,
            preserved_live,
            retained,
        }
    }
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

impl kameo::message::Message<DrainRoomOwnershipForShutdown> for RoomRegistryActor {
    type Reply = RoomOwnershipDrainOutcome;

    async fn handle(
        &mut self,
        msg: DrainRoomOwnershipForShutdown,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.begin_room_ownership_shutdown(msg.pending_handoffs);
        let acquisition = self
            .reconcile_uncertain_room_acquisitions_for_shutdown()
            .await;
        let reservations = self
            .reconcile_reclaimed_room_reservations_for_shutdown()
            .await;
        let (unpublished_released, unpublished_retained) = self
            .reconcile_pending_unpublished_destroys_for_shutdown()
            .await;
        let mut outcome = self
            .release_exact_room_ownership_for_shutdown(reservations.reservation_owned)
            .await;
        outcome.released += unpublished_released;
        outcome.preserved_live += acquisition.preserved_live + reservations.preserved_live;
        outcome.retained += acquisition.retained + reservations.retained + unpublished_retained;
        outcome
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
        if self.pending_reclaimed_reservations.contains(&msg.room_jid)
            || self
                .pending_reclaimed_rooms
                .keys()
                .any(|(room_jid, _)| room_jid == &msg.room_jid)
        {
            return true;
        }
        if self.pending_reclaimed_rooms.len() + self.pending_reclaimed_reservations.len()
            >= MAX_PENDING_RECLAIMED_ROOMS
            || !self.can_admit_room_ownership_responsibility(
                PendingRoomOwnershipResponsibility::ReclaimedReservation(&msg.room_jid),
            )
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
    type Reply = DelegatedReply<ReclaimedRoomOutcome>;

    async fn handle(
        &mut self,
        msg: ReconcileReclaimedRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let entity = Entity::new(EntityType::RoomActor, msg.room_jid.to_string());
        if msg.claim_fence.entity != entity {
            return ctx.reply(ReclaimedRoomOutcome::LostRace);
        }
        let claim_fence = msg.claim_fence.clone();
        let claim_epoch = claim_fence.epoch;
        let identity = claim_fence.owner();
        if let Some(pending) = self.pending_room_preparations.get_mut(&msg.room_jid) {
            if pending.claim_fence == claim_fence {
                if !Self::preparation_waiter_capacity_available(pending) {
                    return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
                }
                let (delegated, reply) = ctx.reply_sender();
                if let Some(reply) = reply {
                    pending.waiters.push(RoomPreparationWaiter::Reclaimed {
                        reply,
                        success: ReclaimedRoomOutcome::AlreadyLive,
                    });
                }
                return delegated;
            }
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
        }
        if self.terminal_claim_acquisition_disabled {
            if self.has_live_room_with_fence(&msg.room_jid, &claim_fence) {
                self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                return ctx.reply(ReclaimedRoomOutcome::AlreadyLive);
            }
            return ctx.reply(
                self.release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                    .await,
            );
        }
        if self.node_identity.current() != identity {
            return ctx.reply(
                self.release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                    .await,
            );
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
                    self.publish_room_count();
                }
            }
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
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
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
            }
            Err(_) => {
                debug!(room = %msg.room_jid, "reclaimed-room ownership fence timed out");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
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
                    self.publish_room_count();
                }
            }
            if let Some(store) = &self.durable_store {
                store.forget_claim_fence(&msg.room_jid, &claim_fence);
            }
            self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
            return ctx.reply(ReclaimedRoomOutcome::LostRace);
        }

        if let Some(entry) = self.rooms.get(&msg.room_jid).cloned() {
            if entry.actor_ref.is_alive() {
                if entry.claim_fence == claim_fence {
                    self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                    return ctx.reply(ReclaimedRoomOutcome::AlreadyLive);
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
            self.publish_room_count();
            if let Some(store) = &self.durable_store {
                store.forget_claim_fence(&msg.room_jid, &entry.claim_fence);
            }
        }

        let Some(store) = self.durable_store.clone() else {
            return ctx.reply(
                self.release_reclaimed_room_claim(&msg.room_jid, &claim_fence, &msg.previous_owner)
                    .await,
            );
        };
        // The reaper won this claim outside the demand-side acquisition
        // path. The fenced read below receives that exact generation
        // directly. The shared fan-out cache is updated only by
        // `publish_room`, after the replacement actor is ready and inserted.
        let snapshot = match tokio::time::timeout(
            RECLAIMED_ROOM_STORE_TIMEOUT,
            store.load_room_state_fenced(&msg.room_jid, &claim_fence),
        )
        .await
        {
            Ok(Ok(Some(snapshot))) => snapshot,
            Ok(Ok(None)) => {
                return ctx.reply(
                    self.release_reclaimed_room_claim(
                        &msg.room_jid,
                        &claim_fence,
                        &msg.previous_owner,
                    )
                    .await,
                );
            }
            Ok(Err(crate::XmppError::OwnershipLost { entity })) => {
                warn!(
                    room = %msg.room_jid,
                    %entity,
                    "proactively reclaimed room-state load lost the exact claim"
                );
                self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                let outcome = self.finish_unpublished_ownership_loss(
                    msg.room_jid,
                    claim_fence,
                    ctx.actor_ref().clone(),
                );
                return ctx.reply(outcome);
            }
            Ok(Err(error)) => {
                debug!(
                    room = %msg.room_jid,
                    %error,
                    "failed to load proactively reclaimed room state; retaining for retry"
                );
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
            }
            Err(_) => {
                debug!(room = %msg.room_jid, "proactively reclaimed room-state load timed out");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
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
                return ctx.reply(ReclaimedRoomOutcome::LostRace);
            }
            Ok(Err(error)) => {
                debug!(room = %msg.room_jid, %error, "final reclaimed-room ownership fence failed");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
            }
            Err(_) => {
                debug!(room = %msg.room_jid, "final reclaimed-room ownership fence timed out");
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
            }
        }

        if self.node_identity.current() != identity {
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
        }

        if let Some(outcome) = self
            .reconcile_reclaimed_preparing_room(
                &msg.room_jid,
                &claim_fence,
                &msg.previous_owner,
                ctx.actor_ref(),
            )
            .await
        {
            return ctx.reply(outcome);
        }

        if !self.has_pending_preparation_capacity(&msg.room_jid, &claim_fence) {
            self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
            return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
        }

        let (guard, has_async_work) = match self
            .prepare_room(
                msg.room_jid.clone(),
                RoomPreparationSpec {
                    waddle_id: snapshot.waddle_id,
                    channel_id: snapshot.channel_id,
                    config: snapshot.config,
                    initial_affiliations: Vec::new(),
                    live_room_restore: None,
                },
                &claim_fence,
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                debug!(
                    room = %msg.room_jid,
                    ?error,
                    "reclaimed room restore did not become ready; retaining exact epoch"
                );
                self.remember_pending_reclaimed_room(msg.room_jid, claim_fence, msg.previous_owner);
                return ctx.reply(ReclaimedRoomOutcome::PendingRetry);
            }
        };
        self.poisoned_rooms.remove(&msg.room_jid);
        if !has_async_work {
            return match self
                .publish_prepared_room(
                    msg.room_jid.clone(),
                    guard,
                    claim_fence.clone(),
                    DurableRoomOrigin::Restored,
                )
                .await
            {
                Ok(_) => ctx.reply(ReclaimedRoomOutcome::Hydrated),
                Err(RoomPublicationError::ClaimLost) => {
                    if let Some(store) = &self.durable_store {
                        store.forget_claim_fence(&msg.room_jid, &claim_fence);
                    }
                    self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                    ctx.reply(ReclaimedRoomOutcome::LostRace)
                }
                Err(RoomPublicationError::LocalIdentityChanged) => {
                    self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
                    self.transfer_exact_responsibility_to_pending_release(
                        msg.room_jid.clone(),
                        claim_fence.clone(),
                    );
                    self.start_detached_room_release(
                        msg.room_jid,
                        claim_fence,
                        ctx.actor_ref().clone(),
                    );
                    ctx.reply(ReclaimedRoomOutcome::PendingRetry)
                }
                Err(_) => {
                    self.remember_pending_reclaimed_room(
                        msg.room_jid,
                        claim_fence,
                        msg.previous_owner,
                    );
                    ctx.reply(ReclaimedRoomOutcome::PendingRetry)
                }
            };
        }
        let room_jid = msg.room_jid;
        let previous_owner = msg.previous_owner;
        let (delegated, reply) = ctx.reply_sender();
        self.start_pending_preparation(
            room_jid.clone(),
            claim_fence,
            RoomPreparationOrigin::Reclaimed { previous_owner },
            guard,
            reply.map(|reply| RoomPreparationWaiter::Reclaimed {
                reply,
                success: ReclaimedRoomOutcome::Hydrated,
            }),
            ctx.actor_ref().clone(),
        );
        delegated
    }
}

/// Get an existing room or create one if it does not exist.
pub struct GetOrCreateRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
}

pub struct GetOrCreateRoomWithLiveRoster {
    pub room_jid: BareJid,
    pub waddle_id: WaddleId,
    pub channel_id: ChannelId,
    pub config: RoomConfig,
    pub live_room_restore: MucRoom,
    pub occupancy_revision: u64,
    /// The predecessor's unacknowledged departure receipts (see
    /// [`super::room_actor::RestoreLiveRoster`]).
    pub departures: super::room_actor::DepartureLedger,
    /// Demote this exact stale actor in the SAME registry turn as the
    /// successor's publication, so no `GetRoom` can observe a gap in which
    /// the room appears absent (cleanup and the departure janitor treat a
    /// definitive absence as convergence). `Err(StaleActorNotCurrent)` when
    /// the registered actor is a different one.
    pub demote_first: Option<ActorRef<RoomActor>>,
}

impl kameo::message::Message<GetOrCreateRoom> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<RoomAcquisition, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_jid = msg.room_jid;
        if self.destroy_completion_blocks_recreation(&room_jid).await {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid,
            )));
        }
        let creation_spec = Arc::new(RoomCreationSpec {
            waddle_id: msg.waddle_id,
            channel_id: msg.channel_id,
            config: msg.config,
            initial_affiliations: Vec::new(),
            live_room_restore: None,
        });
        match self
            .transition_demand_room(room_jid.clone(), creation_spec, ctx.actor_ref().clone())
            .await
        {
            Ok(DemandRoomTransition::Existing(actor_ref)) => {
                debug!(room = %room_jid, "Room already exists");
                ctx.reply(Ok(RoomAcquisition {
                    actor_ref,
                    creation: RoomCreation::Existing,
                }))
            }
            Ok(DemandRoomTransition::Created(actor_ref)) => ctx.reply(Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Created,
            })),
            Ok(DemandRoomTransition::Pending(creation_spec)) => {
                let (delegated, reply) = ctx.reply_sender();
                self.attach_preparation_waiter(
                    &room_jid,
                    reply.map(|reply| RoomPreparationWaiter::Acquisition {
                        reply,
                        creation_spec,
                    }),
                );
                delegated
            }
            Err(error) => ctx.reply(Err(error)),
        }
    }
}

impl kameo::message::Message<GetOrCreateRoomWithLiveRoster> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<RoomAcquisition, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoomWithLiveRoster,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_jid = msg.room_jid;
        if let Some(stale_actor) = &msg.demote_first {
            let is_current = self
                .rooms
                .get(&room_jid)
                .is_some_and(|entry| entry.actor_ref.id() == stale_actor.id());
            if !is_current {
                return ctx.reply(Err(RoomRegistryError::StaleActorNotCurrent(room_jid)));
            }
        }
        // Every gate that can refuse runs BEFORE the stale actor is retired:
        // a refused handoff must leave the stale entry (and its ledger) in
        // place, never an absent room.
        if self.destroy_completion_blocks_recreation(&room_jid).await {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid,
            )));
        }
        if msg.demote_first.is_some() {
            if let Some(entry) = self.rooms.remove(&room_jid) {
                self.publish_room_count();
                // From here until the successor is registered (or a
                // preparation is pending) the room must not look absent.
                self.handoff_pending
                    .retain(|_, pending| pending.since.elapsed() < HANDOFF_PENDING_WINDOW);
                self.handoff_pending.insert(
                    room_jid.clone(),
                    PendingHandoff {
                        since: std::time::Instant::now(),
                        stashed_spec: None,
                    },
                );
                self.retire_ownership_lost_entry(&room_jid, entry).await;
            }
        }
        let creation_spec = Arc::new(RoomCreationSpec {
            waddle_id: msg.waddle_id.into_string(),
            channel_id: msg.channel_id.into_string(),
            config: msg.config,
            initial_affiliations: Vec::new(),
            live_room_restore: Some(LiveRoomRestore {
                room: msg.live_room_restore,
                occupancy_revision: msg.occupancy_revision,
                departures: msg.departures,
            }),
        });
        let transition = self
            .transition_demand_room(
                room_jid.clone(),
                Arc::clone(&creation_spec),
                ctx.actor_ref().clone(),
            )
            .await;
        if transition.is_err() {
            // The demand failed after the stale actor was retired: this
            // message held the only copy of the retired actor's live roster
            // and departure ledger. Stash it on the pending marker so the
            // next demand creation inside the window restores it instead of
            // hydrating a rosterless room after the marker expires.
            if let Some(pending) = self.handoff_pending.get_mut(&room_jid) {
                pending.stashed_spec = Some(Arc::clone(&creation_spec));
            }
        }
        // A registered successor clears the marker (so does any later
        // publication). A PENDING preparation keeps it: the preparation may
        // still fail without publishing, and lookups after that must stay
        // retryable for the bounded window rather than read as an absence.
        if matches!(
            transition,
            Ok(DemandRoomTransition::Existing(_)) | Ok(DemandRoomTransition::Created(_))
        ) {
            self.handoff_pending.remove(&room_jid);
        }
        match transition {
            // A live-roster handoff is only valid while the target actor is
            // still unpublished.  Merging into an already-live actor could
            // erase a join or leave that arrived after its publication.
            Ok(DemandRoomTransition::Existing(_)) => ctx.reply(Err(
                RoomRegistryError::OwnershipReconciliationPending(room_jid),
            )),
            Ok(DemandRoomTransition::Created(actor_ref)) => ctx.reply(Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Created,
            })),
            Ok(DemandRoomTransition::Pending(creation_spec)) => {
                let pending_has_live_roster = self
                    .pending_room_preparations
                    .get(&room_jid)
                    .is_some_and(|pending| {
                        matches!(
                            &pending.origin,
                            RoomPreparationOrigin::Demand { prepared_spec }
                                if prepared_spec.live_room_restore.is_some()
                        )
                    });
                if !pending_has_live_roster {
                    return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                        room_jid,
                    )));
                }
                let (delegated, reply) = ctx.reply_sender();
                self.attach_preparation_waiter(
                    &room_jid,
                    reply.map(|reply| RoomPreparationWaiter::Acquisition {
                        reply,
                        creation_spec,
                    }),
                );
                delegated
            }
            Err(error) => ctx.reply(Err(error)),
        }
    }
}

/// Get or create a room with the complete affiliation snapshot required by
/// its first observable incarnation.
pub struct GetOrCreateRoomWithInitialAffiliations {
    pub room_jid: BareJid,
    pub waddle_id: WaddleId,
    pub channel_id: ChannelId,
    pub config: RoomConfig,
    pub initial_affiliations: Vec<super::durable::AffiliationEntry>,
}

impl kameo::message::Message<GetOrCreateRoomWithInitialAffiliations> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<RoomAcquisition, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoomWithInitialAffiliations,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_jid = msg.room_jid;
        if self.destroy_completion_blocks_recreation(&room_jid).await {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid,
            )));
        }
        let creation_spec = Arc::new(RoomCreationSpec {
            waddle_id: msg.waddle_id.into_string(),
            channel_id: msg.channel_id.into_string(),
            config: msg.config,
            initial_affiliations: msg.initial_affiliations,
            live_room_restore: None,
        });
        match self
            .transition_demand_room(room_jid.clone(), creation_spec, ctx.actor_ref().clone())
            .await
        {
            Ok(DemandRoomTransition::Existing(actor_ref)) => ctx.reply(Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Existing,
            })),
            Ok(DemandRoomTransition::Created(actor_ref)) => ctx.reply(Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Created,
            })),
            Ok(DemandRoomTransition::Pending(creation_spec)) => {
                let (delegated, reply) = ctx.reply_sender();
                self.attach_preparation_waiter(
                    &room_jid,
                    reply.map(|reply| RoomPreparationWaiter::Acquisition {
                        reply,
                        creation_spec,
                    }),
                );
                delegated
            }
            Err(error) => ctx.reply(Err(error)),
        }
    }
}

/// Create an instant room per XEP-0045.
pub struct CreateInstantRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<CreateInstantRoom> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<RoomAcquisition, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: CreateInstantRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self
            .destroy_completion_blocks_recreation(&msg.room_jid)
            .await
        {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                msg.room_jid,
            )));
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
        let creation_spec = Arc::new(RoomCreationSpec {
            waddle_id,
            channel_id,
            config,
            initial_affiliations: Vec::new(),
            live_room_restore: None,
        });

        let room_jid = msg.room_jid;
        match self
            .transition_demand_room(room_jid.clone(), creation_spec, ctx.actor_ref().clone())
            .await
        {
            Ok(DemandRoomTransition::Existing(actor_ref)) => ctx.reply(Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Existing,
            })),
            Ok(DemandRoomTransition::Created(actor_ref)) => ctx.reply(Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Created,
            })),
            Ok(DemandRoomTransition::Pending(creation_spec)) => {
                let (delegated, reply) = ctx.reply_sender();
                self.attach_preparation_waiter(
                    &room_jid,
                    reply.map(|reply| RoomPreparationWaiter::Acquisition {
                        reply,
                        creation_spec,
                    }),
                );
                delegated
            }
            Err(error) => ctx.reply(Err(error)),
        }
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
    type Reply = DelegatedReply<Result<ActorRef<RoomActor>, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: CreateRoom,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_jid = msg.room_jid;
        if self.destroy_completion_blocks_recreation(&room_jid).await {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid,
            )));
        }
        let creation_spec = Arc::new(RoomCreationSpec {
            waddle_id: msg.waddle_id,
            channel_id: msg.channel_id,
            config: msg.config,
            initial_affiliations: Vec::new(),
            live_room_restore: None,
        });
        match self
            .transition_demand_room(room_jid.clone(), creation_spec, ctx.actor_ref().clone())
            .await
        {
            Ok(DemandRoomTransition::Existing(_)) => {
                ctx.reply(Err(RoomRegistryError::RoomAlreadyExists(room_jid)))
            }
            Ok(DemandRoomTransition::Created(actor_ref)) => ctx.reply(Ok(actor_ref)),
            Ok(DemandRoomTransition::Pending(creation_spec)) => {
                let (delegated, reply) = ctx.reply_sender();
                self.attach_preparation_waiter(
                    &room_jid,
                    reply.map(|reply| RoomPreparationWaiter::ExclusiveCreate {
                        reply,
                        creation_spec,
                    }),
                );
                delegated
            }
            Err(error) => ctx.reply(Err(error)),
        }
    }
}

/// Create an administrator-provisioned room with the affiliations that must
/// be visible in its first published durable snapshot.
pub struct CreateRoomWithInitialAffiliations {
    pub room_jid: BareJid,
    pub waddle_id: WaddleId,
    pub channel_id: ChannelId,
    pub config: RoomConfig,
    pub initial_affiliations: Vec<super::durable::AffiliationEntry>,
}

impl kameo::message::Message<CreateRoomWithInitialAffiliations> for RoomRegistryActor {
    type Reply = DelegatedReply<Result<ActorRef<RoomActor>, RoomRegistryError>>;

    async fn handle(
        &mut self,
        msg: CreateRoomWithInitialAffiliations,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_jid = msg.room_jid;
        if self.destroy_completion_blocks_recreation(&room_jid).await {
            return ctx.reply(Err(RoomRegistryError::OwnershipReconciliationPending(
                room_jid,
            )));
        }
        let creation_spec = Arc::new(RoomCreationSpec {
            waddle_id: msg.waddle_id.into_string(),
            channel_id: msg.channel_id.into_string(),
            config: msg.config,
            initial_affiliations: msg.initial_affiliations,
            live_room_restore: None,
        });
        match self
            .transition_demand_room(room_jid.clone(), creation_spec, ctx.actor_ref().clone())
            .await
        {
            Ok(DemandRoomTransition::Existing(_)) => {
                ctx.reply(Err(RoomRegistryError::RoomAlreadyExists(room_jid)))
            }
            Ok(DemandRoomTransition::Created(actor_ref)) => ctx.reply(Ok(actor_ref)),
            Ok(DemandRoomTransition::Pending(creation_spec)) => {
                let (delegated, reply) = ctx.reply_sender();
                self.attach_preparation_waiter(
                    &room_jid,
                    reply.map(|reply| RoomPreparationWaiter::ExclusiveCreate {
                        reply,
                        creation_spec,
                    }),
                );
                delegated
            }
            Err(error) => ctx.reply(Err(error)),
        }
    }
}

/// Why a room is being removed from the registry — decides whether the
/// durable rows go with it (#1261 vs. non-serving actor eviction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroyRoomReason {
    /// XEP-0045 §10.9 destroy or an administrative deletion: the room
    /// ceases to exist, so its durable rows (config, subject,
    /// affiliations incl. bans) are deleted with it.
    Destroy,
    /// A write-adjacent fence proved this process can no longer serve the
    /// room, either because the database claim moved or the local node
    /// identity rotated. Bypass release-backlog admission so the non-serving
    /// actor cannot remain registered, preserve durable state, and still
    /// attempt exact release of the old fence.
    DeposedEviction,
}

/// Outcome of a [`DestroyRoom`] ask. Split four ways because callers
/// must distinguish "the room simply was not registered" (fine for
/// admin deletion of a dormant room) from "the fenced durable wipe did
/// not commit" (which MUST fail the caller's operation — a local actor
/// may have been evicted after ownership moved, but the durable room can
/// still be live elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum DestroyRoomOutcome {
    /// The room existed and was removed (durable rows wiped when the
    /// reason was [`DestroyRoomReason::Destroy`]).
    Destroyed,
    /// No live or poisoned entry for this JID existed.
    NotRegistered,
    /// The epoch-fenced durable delete did not commit. The registry either
    /// retained the room for retry or evicted a now-deposed local actor, but
    /// callers must not perform application-level destroy cleanup.
    DurableWipeFailed,
    /// The bounded exact-release retry set is full, so the registry kept the
    /// actor and claim intact rather than losing responsibility for the fence.
    ReleaseBacklogFull,
}

/// Attach typed owner-IQ post-commit work to its exact destroy for this room.
/// If a destroy is already retained after a lost reply, it is attached to that
/// same attempt instead of creating a new actor-local seal generation.
pub struct RegisterDestroyCompletion {
    pub completion: DestroyCompletion,
}

impl RoomRegistryActor {
    fn take_waiting_destroy_completion(
        &mut self,
        room_jid: &BareJid,
        attempt: Option<super::DestroyAttemptId>,
    ) -> Option<DestroyCompletion> {
        match attempt {
            Some(attempt) => self
                .destroy_completions_waiting
                .remove(&attempt)
                .filter(|completion| completion.room_jid == *room_jid),
            // A plain destroy has no authority to consume an owner-IQ
            // snapshot. It might run after a timed-out registration, and
            // attaching that stale request would emit incorrect XEP-0045
            // presence and clean members from the wrong snapshot.
            None => None,
        }
    }

    fn drop_waiting_destroy_completions_for_room(&mut self, room_jid: &BareJid) {
        self.destroy_completions_waiting
            .retain(|_, completion| completion.room_jid != *room_jid);
    }

    async fn destroy_durable_room_without_local_entry(
        &mut self,
        room_jid: &BareJid,
        ctx: &mut Context<Self, DestroyRoomOutcome>,
    ) -> DestroyRoomOutcome {
        let Some(store) = self.durable_store.clone() else {
            return DestroyRoomOutcome::NotRegistered;
        };
        let claim_fence = match self.acquire_room_claim(room_jid, ctx.actor_ref()).await {
            Ok(claim_fence) => claim_fence,
            Err(RoomRegistryError::OwnershipUnavailable(_))
            | Err(RoomRegistryError::OwnershipReconciliationPending(_))
            | Err(RoomRegistryError::ClaimHeldByAnotherNode(_)) => {
                return DestroyRoomOutcome::DurableWipeFailed;
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to acquire exact claim for explicit destroy without a local room entry"
                );
                return DestroyRoomOutcome::DurableWipeFailed;
            }
        };
        store.establish_claim_fence(room_jid, claim_fence.clone());
        let outcome = match store
            .commit_room_mutation(
                room_jid,
                &claim_fence,
                RoomDurableMutation::Destroy {
                    completion_attempt: None,
                },
                crate::muc::RoomMutationEffects::none(),
            )
            .await
        {
            Ok(_) => DestroyRoomOutcome::Destroyed,
            Err(RoomCommitError::StateMissing) => DestroyRoomOutcome::NotRegistered,
            Err(RoomCommitError::NotOwner) => DestroyRoomOutcome::DurableWipeFailed,
            Err(RoomCommitError::CommitOutcomeUnknown) => {
                warn!(
                    room = %room_jid,
                    "explicit destroy without a local room entry has unknown durable commit outcome"
                );
                self.begin_preparing_destroy_recovery(
                    room_jid.clone(),
                    claim_fence,
                    ctx.actor_ref().clone(),
                );
                return DestroyRoomOutcome::DurableWipeFailed;
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    %error,
                    "explicit destroy without a local room entry failed its durable commit"
                );
                DestroyRoomOutcome::DurableWipeFailed
            }
        };
        self.release_room_claim(room_jid, &claim_fence).await;
        outcome
    }

    async fn handle_destroy_room_message(
        &mut self,
        room_jid: BareJid,
        reason: DestroyRoomReason,
        completion_attempt: Option<super::DestroyAttemptId>,
        ctx: &mut Context<Self, DestroyRoomOutcome>,
    ) -> DestroyRoomOutcome {
        let mut terminal_outcome = None;
        let mut completed_pending_preparation = None;
        if reason != DestroyRoomReason::DeposedEviction {
            let claim_fence = self.rooms.get(&room_jid).map(|entry| &entry.claim_fence);
            if claim_fence.is_some_and(|claim_fence| {
                !self.has_pending_release_capacity(&room_jid, claim_fence)
            }) {
                return DestroyRoomOutcome::ReleaseBacklogFull;
            }
        }
        if reason == DestroyRoomReason::Destroy {
            if let Some(requested_attempt) = completion_attempt {
                if let Some(retained) = self.destroy_attempts.get(&room_jid) {
                    if retained.attempt != requested_attempt {
                        self.reconcile_destroy_attempt(&room_jid).await;
                        if self
                            .destroy_attempts
                            .get(&room_jid)
                            .is_some_and(|retained| retained.attempt != requested_attempt)
                        {
                            return DestroyRoomOutcome::DurableWipeFailed;
                        }
                    }
                }
            }
            let pending_fence = self
                .pending_room_preparations
                .get(&room_jid)
                .map(|pending| pending.claim_fence.clone());
            if let Some(entry) = self.rooms.get(&room_jid).cloned() {
                let completion =
                    self.take_waiting_destroy_completion(&room_jid, completion_attempt);
                let retained = self
                    .destroy_attempts
                    .entry(room_jid.clone())
                    .or_insert_with(|| RetainedDestroyAttempt {
                        attempt: completion_attempt
                            .unwrap_or_else(super::DestroyAttemptId::generate),
                        phase: DestroyAttemptPhase::DestroyRequested,
                        completion: None,
                    });
                if let Some(requested_attempt) = completion_attempt {
                    if retained.attempt != requested_attempt {
                        return DestroyRoomOutcome::DurableWipeFailed;
                    }
                }
                if retained.completion.is_none() {
                    retained.completion = completion;
                }
                retained.phase = DestroyAttemptPhase::DestroyRequested;
                let attempt = retained.attempt;
                let seal = entry
                    .actor_ref
                    .ask(SealForDestroy { attempt })
                    .mailbox_timeout(SEAL_ASK_TIMEOUT)
                    .reply_timeout(SEAL_ASK_TIMEOUT)
                    .await;
                match seal {
                    Ok(RoomSealState::Destroying { attempt: current }) if current == attempt => {}
                    Ok(RoomSealState::OwnershipLost) => {
                        self.destroy_attempts.remove(&room_jid);
                        self.evict_ownership_lost_room(&room_jid, entry).await;
                        return DestroyRoomOutcome::DurableWipeFailed;
                    }
                    Ok(state) => {
                        // A definitive non-matching state cannot be repaired
                        // by retrying this retained token.  Drop it rather
                        // than making every later lookup reconcile forever.
                        self.destroy_attempts.remove(&room_jid);
                        warn!(room = %room_jid, ?state, "room destroy pre-seal refused the retained attempt");
                        return DestroyRoomOutcome::DurableWipeFailed;
                    }
                    Err(error) => {
                        // The actor may have accepted the seal after this
                        // reply timed out; keep the attempt for the ordinary
                        // reconciliation path.
                        warn!(room = %room_jid, ?error, "room destroy pre-seal acknowledgement was lost");
                        return DestroyRoomOutcome::DurableWipeFailed;
                    }
                }
                if let Some(store) = &self.durable_store {
                    let committed_completion = retained.completion.clone();
                    if let Err(error) = store
                        .commit_room_mutation(
                            &room_jid,
                            &entry.claim_fence,
                            RoomDurableMutation::Destroy {
                                completion_attempt: committed_completion
                                    .as_ref()
                                    .map(|completion| completion.attempt),
                            },
                            committed_completion
                                .as_ref()
                                .map(Self::destroy_effects)
                                .unwrap_or_else(RoomMutationEffects::none),
                        )
                        .await
                    {
                        match error {
                            RoomCommitError::NotOwner => {
                                self.destroy_attempts.remove(&room_jid);
                                self.evict_ownership_lost_room(&room_jid, entry).await;
                                return DestroyRoomOutcome::DurableWipeFailed;
                            }
                            RoomCommitError::StateMissing => {
                                entry.actor_ref.kill();
                            }
                            RoomCommitError::CommitOutcomeUnknown => {
                                warn!(room = %room_jid, "durable room destroy has unknown commit outcome; retaining the sealed attempt");
                                return DestroyRoomOutcome::DurableWipeFailed;
                            }
                            error => {
                                warn!(room = %room_jid, %error, "durable room destroy commit failed; reconciling the sealed attempt");
                                self.recover_failed_destroy(&room_jid, &entry, attempt)
                                    .await;
                                return DestroyRoomOutcome::DurableWipeFailed;
                            }
                        }
                    }
                }
            } else if let (Some(store), Some(claim_fence)) =
                (self.durable_store.clone(), pending_fence)
            {
                if let Some(pending) = self.pending_room_preparations.get_mut(&room_jid) {
                    let waiters = std::mem::take(&mut pending.waiters);
                    Self::reply_preparation_failure(
                        &room_jid,
                        waiters,
                        ReclaimedRoomOutcome::PendingRetry,
                    );
                }
                let retained_completion = if let Some(requested_attempt) = completion_attempt {
                    match self.destroy_attempts.get_mut(&room_jid) {
                        Some(retained) => {
                            if retained.attempt != requested_attempt {
                                return DestroyRoomOutcome::DurableWipeFailed;
                            }
                            // This mailbox delivery is the first definite evidence
                            // that the terminal destroy began; cancellation is no
                            // longer permitted even though the prepared actor was
                            // never published.
                            retained.phase = DestroyAttemptPhase::DestroyRequested;
                            retained.completion.clone()
                        }
                        None => None,
                    }
                } else {
                    None
                };
                let completion_from_retained = retained_completion.is_some();
                let waiting_completion = if retained_completion.is_none() {
                    self.take_waiting_destroy_completion(&room_jid, completion_attempt)
                } else {
                    None
                };
                let completion = retained_completion.or(waiting_completion);
                if completion_attempt.is_some() && completion.is_none() {
                    // An owner-IQ attempt without its exact registered
                    // completion may not report a terminal destroy: doing so
                    // would leave its persisted outbox record inert.
                    return DestroyRoomOutcome::DurableWipeFailed;
                }
                let effects = completion
                    .as_ref()
                    .map(Self::destroy_effects)
                    .unwrap_or_else(RoomMutationEffects::none);
                let commit = store
                    .commit_room_mutation(
                        &room_jid,
                        &claim_fence,
                        RoomDurableMutation::DestroyAndReleaseClaim {
                            completion_attempt: completion
                                .as_ref()
                                .map(|completion| completion.attempt),
                        },
                        effects,
                    )
                    .await;
                if let Err(error) = commit {
                    if matches!(error, RoomCommitError::CommitOutcomeUnknown) {
                        // Do not leave the unpublished preparation in the
                        // normal preparation map: every later lookup would
                        // report reconciliation-pending forever while the
                        // original D&R may already have released its fence.
                        // Its durable Preparing marker is the recovery proof.
                        if let Some(pending) = self.pending_room_preparations.remove(&room_jid) {
                            drop(pending.guard);
                            self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
                        }
                        self.begin_preparing_destroy_recovery(
                            room_jid,
                            claim_fence,
                            ctx.actor_ref().clone(),
                        );
                        return DestroyRoomOutcome::DurableWipeFailed;
                    }
                    if matches!(error, RoomCommitError::StateMissing) {
                        self.destroy_attempts.remove(&room_jid);
                        if completion.is_some() {
                            terminal_outcome = Some(DestroyRoomOutcome::DurableWipeFailed);
                        }
                    } else if matches!(error, RoomCommitError::NotOwner) {
                        self.destroy_attempts.remove(&room_jid);
                        terminal_outcome = Some(DestroyRoomOutcome::DurableWipeFailed);
                    } else {
                        warn!(room = %room_jid, %error, "durable pending-room destroy commit failed; keeping the preparation intact");
                        return DestroyRoomOutcome::DurableWipeFailed;
                    }
                } else {
                    if !completion_from_retained {
                        completed_pending_preparation = completion;
                    }
                }
            } else if completion_attempt.is_none() && !self.poisoned_rooms.contains(&room_jid) {
                return self
                    .destroy_durable_room_without_local_entry(&room_jid, ctx)
                    .await;
            }
        }
        let removed_entry = self.rooms.remove(&room_jid);
        let completed_attempt = self.destroy_attempts.remove(&room_jid);
        let removed_room = removed_entry.is_some();
        let removed_preparation = self.pending_room_preparations.remove(&room_jid);
        let removed_pending_room = removed_preparation.is_some();
        let removed_poison = self.poisoned_rooms.remove(&room_jid);
        if let Some(pending) = removed_preparation {
            let claim_fence = pending.claim_fence.clone();
            Self::reply_preparation_failure(
                &room_jid,
                pending.waiters,
                ReclaimedRoomOutcome::PendingRetry,
            );
            drop(pending.guard);
            self.clear_pending_reclaimed_room(&room_jid, &claim_fence);
            self.transfer_exact_responsibility_to_pending_release(
                room_jid.clone(),
                claim_fence.clone(),
            );
            self.start_detached_room_release(
                room_jid.clone(),
                claim_fence,
                ctx.actor_ref().clone(),
            );
        }
        if let Some(entry) = removed_entry {
            if reason == DestroyRoomReason::DeposedEviction {
                self.retire_ownership_lost_entry(&room_jid, entry).await;
            } else {
                self.release_room_claim(&room_jid, &entry.claim_fence).await;
            }
        }
        let outcome = if let Some(outcome) = terminal_outcome {
            outcome
        } else if removed_room || removed_pending_room {
            info!(room = %room_jid, "Destroyed room");
            DestroyRoomOutcome::Destroyed
        } else if removed_poison {
            // A poison marker records a dead local actor whose exact durable
            // fence was already released.  Without a live or pending entry
            // there is no authority to wipe its durable room state, so an
            // explicit destroy must fail closed instead of falsely claiming
            // success and allowing a later restore.
            self.poisoned_rooms.insert(room_jid.clone());
            warn!(room = %room_jid, "refusing poisoned-only destroy without an exact durable wipe");
            DestroyRoomOutcome::DurableWipeFailed
        } else {
            warn!(room = %room_jid, "Attempted to destroy non-existent room");
            DestroyRoomOutcome::NotRegistered
        };
        if outcome == DestroyRoomOutcome::Destroyed {
            if let Some(RetainedDestroyAttempt {
                completion: Some(completion),
                ..
            }) = completed_attempt
            {
                self.pending_destroy_completions.push_back(completion);
            }
            if let Some(completion) = completed_pending_preparation {
                self.pending_destroy_completions.push_back(completion);
            }
        } else if let Some(attempt) = completion_attempt {
            self.destroy_completions_waiting.remove(&attempt);
        } else {
            self.drop_waiting_destroy_completions_for_room(&room_jid);
        }
        outcome
    }
}

impl kameo::message::Message<SealRoomForDestroySnapshot> for RoomRegistryActor {
    type Reply = Result<super::room_actor::RoomSnapshot, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: SealRoomForDestroySnapshot,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return Err(RoomRegistryError::RoomActorStateLost(msg.room_jid));
        };
        let sealed = entry
            .actor_ref
            .ask(SealForDestroy {
                attempt: msg.attempt,
            })
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await;
        if !matches!(sealed, Ok(RoomSealState::Destroying { attempt }) if attempt == msg.attempt) {
            return Err(RoomRegistryError::OwnershipUnavailable(msg.room_jid));
        }
        match self.destroy_attempts.get(&msg.room_jid) {
            Some(retained) if retained.attempt != msg.attempt => {
                return Err(RoomRegistryError::OwnershipReconciliationPending(
                    msg.room_jid,
                ));
            }
            Some(_) => {}
            None => {
                self.destroy_attempts.insert(
                    msg.room_jid.clone(),
                    RetainedDestroyAttempt {
                        attempt: msg.attempt,
                        phase: DestroyAttemptPhase::SnapshotPreseal,
                        completion: None,
                    },
                );
                std::mem::drop(
                    ctx.actor_ref()
                        .tell(ReconcileSnapshotPreseal {
                            room_jid: msg.room_jid.clone(),
                            attempt: msg.attempt,
                        })
                        .send_after(SNAPSHOT_PRESEAL_RECONCILE_DELAY),
                );
            }
        }
        entry
            .actor_ref
            .ask(GetSnapshot)
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await
            .map_err(|_| RoomRegistryError::OwnershipUnavailable(msg.room_jid))
    }
}

impl kameo::message::Message<AbortDestroyRoomAttempt> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: AbortDestroyRoomAttempt,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(retained) = self.destroy_attempts.get(&msg.room_jid) else {
            return false;
        };
        if retained.attempt != msg.attempt
            || !matches!(
                retained.phase,
                DestroyAttemptPhase::SnapshotPreseal | DestroyAttemptPhase::RegisteredPreDestroy
            )
        {
            return false;
        }
        let Some(entry) = self.rooms.get(&msg.room_jid) else {
            return false;
        };
        let unsealed = entry
            .actor_ref
            .ask(UnsealDestroy {
                attempt: msg.attempt,
            })
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await
            .unwrap_or(false);
        if unsealed {
            self.destroy_attempts.remove(&msg.room_jid);
        }
        unsealed
    }
}

impl kameo::message::Message<ReconcileSnapshotPreseal> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ReconcileSnapshotPreseal,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self
            .destroy_attempts
            .get(&msg.room_jid)
            .is_some_and(|retained| {
                retained.attempt == msg.attempt
                    && matches!(
                        retained.phase,
                        DestroyAttemptPhase::SnapshotPreseal
                            | DestroyAttemptPhase::RegisteredPreDestroy
                    )
            })
        {
            self.reconcile_destroy_attempt(&msg.room_jid).await;
        }
    }
}

impl kameo::message::Message<RegisterDestroyCompletion> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RegisterDestroyCompletion,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(retained) = self.destroy_attempts.get_mut(&msg.completion.room_jid) {
            if retained.attempt == msg.completion.attempt && retained.completion.is_none() {
                retained.completion = Some(msg.completion);
                retained.phase = DestroyAttemptPhase::RegisteredPreDestroy;
            }
            return;
        }
        self.destroy_completions_waiting
            .insert(msg.completion.attempt, msg.completion);
    }
}

/// Discard owner-IQ work when a destroy was conclusively refused before it
/// began. Retained attempts are intentionally left alone because their
/// terminal durable status is still ambiguous.
pub struct CancelDestroyCompletion {
    pub room_jid: BareJid,
}

impl kameo::message::Message<CancelDestroyCompletion> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CancelDestroyCompletion,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.drop_waiting_destroy_completions_for_room(&msg.room_jid);
    }
}

impl kameo::message::Message<CancelDestroyCompletionAttempt> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: CancelDestroyCompletionAttempt,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.destroy_completions_waiting.remove(&msg.attempt);
    }
}

/// Take completed owner-IQ destroys for execution by the server layer.
pub struct TakeDestroyCompletions;

pub struct TakeDestroyCompletionAttempt {
    pub attempt: super::DestroyAttemptId,
}

impl kameo::message::Message<TakeDestroyCompletions> for RoomRegistryActor {
    type Reply = DelegatedReply<Vec<DestroyCompletion>>;

    async fn handle(
        &mut self,
        _msg: TakeDestroyCompletions,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let completions: Vec<_> = self.pending_destroy_completions.drain(..).collect();
        let (delegated, reply) = ctx.reply_sender();
        if let Some(reply) = reply {
            if Self::try_send_reply(reply, completions.clone()) {
                for completion in &completions {
                    self.leased_destroy_completions
                        .insert(completion.attempt, completion.clone());
                }
            } else {
                self.pending_destroy_completions.extend(completions);
            }
        } else {
            self.pending_destroy_completions.extend(completions);
        }
        delegated
    }
}

impl kameo::message::Message<TakeDestroyCompletionAttempt> for RoomRegistryActor {
    type Reply = DelegatedReply<Option<DestroyCompletion>>;

    async fn handle(
        &mut self,
        msg: TakeDestroyCompletionAttempt,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(index) = self
            .pending_destroy_completions
            .iter()
            .position(|completion| completion.attempt == msg.attempt)
        else {
            return ctx.reply(None);
        };
        let completion = self
            .pending_destroy_completions
            .remove(index)
            .expect("completion index came from this queue");
        let (delegated, reply) = ctx.reply_sender();
        if let Some(reply) = reply {
            if Self::try_send_reply(reply, Some(completion.clone())) {
                self.leased_destroy_completions
                    .insert(completion.attempt, completion);
            } else {
                self.pending_destroy_completions.insert(index, completion);
            }
        } else {
            self.pending_destroy_completions.insert(index, completion);
        }
        delegated
    }
}

impl kameo::message::Message<RequeueDestroyCompletion> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RequeueDestroyCompletion,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(completion) = self.leased_destroy_completions.remove(&msg.attempt) else {
            return false;
        };
        self.pending_destroy_completions.push_back(completion);
        true
    }
}

impl kameo::message::Message<AckDestroyCompletion> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: AckDestroyCompletion,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.leased_destroy_completions
            .remove(&msg.attempt)
            .is_some()
    }
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
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.handle_destroy_room_message(msg.room_jid, msg.reason, None, ctx)
            .await
    }
}

impl kameo::message::Message<DestroyRoomWithAttempt> for RoomRegistryActor {
    type Reply = DestroyRoomOutcome;

    async fn handle(
        &mut self,
        msg: DestroyRoomWithAttempt,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.handle_destroy_room_message(msg.room_jid, msg.reason, Some(msg.attempt), ctx)
            .await
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
/// Answers a typed [`GuardedDestroyOutcome`].
pub struct DestroyRoomIfInactive {
    pub room_jid: BareJid,
    pub expected_occupancy_revision: u64,
    pub guard: SealGuard,
}

/// Typed answer of a guarded destroy, so callers can tell a DEFINITIVE
/// outcome (destroyed, absent, or refused because occupancy moved) from a
/// TRANSIENT one that leaves the destroy owed (uncertain durable commit,
/// release backlog, seal ask failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum GuardedDestroyOutcome {
    Destroyed,
    Absent,
    Refused,
    Retained,
}

impl GuardedDestroyOutcome {
    pub fn is_definitive(self) -> bool {
        !matches!(self, Self::Retained)
    }

    pub fn destroyed(self) -> bool {
        matches!(self, Self::Destroyed)
    }
}

/// Bound for the in-handler seal ask so a wedged room actor cannot
/// wedge the whole registry: shorter than
/// [`ROOM_REGISTRY_REPLY_TIMEOUT`](super::room_registry_handle::ROOM_REGISTRY_REPLY_TIMEOUT)
/// so the outer registry ask still gets a reply.
const SEAL_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// A snapshot pre-seal must outlive the websocket stanza wedge backstop (15s)
/// so a slow but still-live owner-IQ destroy can persist/register its
/// completion before the registry attempts bounded recovery. Definite
/// non-delivery still reopens immediately through `AbortDestroyRoomAttempt`.
const SNAPSHOT_PRESEAL_RECONCILE_DELAY: std::time::Duration = std::time::Duration::from_secs(20);

impl kameo::message::Message<DestroyRoomIfInactive> for RoomRegistryActor {
    type Reply = GuardedDestroyOutcome;

    async fn handle(
        &mut self,
        msg: DestroyRoomIfInactive,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Preparation is not yet published and therefore cannot have been
        // observed inactive at the supplied occupancy revision.
        if self.pending_room_preparations.contains_key(&msg.room_jid) {
            return GuardedDestroyOutcome::Refused;
        }
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return GuardedDestroyOutcome::Absent;
        };
        if !self.has_pending_release_capacity(&msg.room_jid, &entry.claim_fence) {
            // Ordinary inactivity must remain open when there is nowhere to
            // retain an uncertain release. A terminally non-serving actor is
            // different: evict it immediately, while owner-sensitive cleanup
            // retains an exact release only when local identity supersession
            // means the old tuple may still exist.
            let seal_state = entry
                .actor_ref
                .ask(GetRoomSealState)
                .mailbox_timeout(SEAL_ASK_TIMEOUT)
                .reply_timeout(SEAL_ASK_TIMEOUT)
                .await;
            return match seal_state {
                Ok(RoomSealState::OwnershipLost) => {
                    self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                    info!(room = %msg.room_jid, "Evicted non-serving room during inactive-room cleanup");
                    GuardedDestroyOutcome::Destroyed
                }
                Ok(
                    RoomSealState::Open
                    | RoomSealState::Inactive
                    | RoomSealState::Destroying { .. },
                ) => {
                    warn!(room = %msg.room_jid, "Skipping inactive-room seal because exact-release retry backlog is full");
                    GuardedDestroyOutcome::Retained
                }
                Err(error) => {
                    warn!(room = %msg.room_jid, error = ?error, "Could not classify room seal while exact-release retry backlog is full");
                    GuardedDestroyOutcome::Retained
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
                info!(room = %msg.room_jid, "Evicted non-serving room during inactive-room cleanup");
                GuardedDestroyOutcome::Destroyed
            }
            Ok(SealIfInactiveOutcome::Inactive) => {
                if let Some(store) = &self.durable_store {
                    let intent = match msg.guard {
                        // XEP-0045 instant rooms are terminally destroyed on
                        // their last departure.  Keeping their durable
                        // lifecycle dormant would resurrect the old creator
                        // and subject on a later create.
                        SealGuard::EmptyNonPersistent => RoomDurableMutation::Destroy {
                            completion_attempt: None,
                        },
                        SealGuard::Dormant => RoomDurableMutation::Dormancy,
                    };
                    if let Err(error) = store
                        .commit_room_mutation(
                            &msg.room_jid,
                            &entry.claim_fence,
                            intent,
                            crate::muc::RoomMutationEffects::none(),
                        )
                        .await
                    {
                        if matches!(error, RoomCommitError::NotOwner) {
                            self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                            info!(room = %msg.room_jid, "evicted room after ownership loss during dormancy commit");
                            return GuardedDestroyOutcome::Destroyed;
                        }
                        if matches!(error, RoomCommitError::StateMissing) {
                            info!(room = %msg.room_jid, "evicting room after terminal dormancy state miss");
                        } else if matches!(error, RoomCommitError::CommitOutcomeUnknown) {
                            warn!(room = %msg.room_jid, "inactive-room transition has unknown durable commit outcome; retaining seal for reaping");
                            return GuardedDestroyOutcome::Retained;
                        } else {
                            let _ = entry.actor_ref.tell(UnsealInactive).await;
                            warn!(room = %msg.room_jid, %error, "durable dormancy commit failed; keeping room active");
                            return GuardedDestroyOutcome::Retained;
                        }
                    }
                }
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
                GuardedDestroyOutcome::Destroyed
            }
            Ok(SealIfInactiveOutcome::Refused) => {
                debug!(
                    room = %msg.room_jid,
                    "Guarded destroy refused: room no longer inactive at expected revision"
                );
                GuardedDestroyOutcome::Refused
            }
            Ok(SealIfInactiveOutcome::EffectsOwed) => {
                debug!(
                    room = %msg.room_jid,
                    "Guarded destroy retained: departure receipts still owed"
                );
                GuardedDestroyOutcome::Retained
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
                GuardedDestroyOutcome::Retained
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
        // A stale reap from a previous incarnation must not cancel or race
        // publication of a newly restoring incarnation.
        if self.pending_room_preparations.contains_key(&msg.room_jid) {
            return false;
        }
        if self.destroy_attempts.contains_key(&msg.room_jid) {
            if self
                .destroy_attempts
                .get(&msg.room_jid)
                .is_some_and(|retained| {
                    matches!(
                        retained.phase,
                        DestroyAttemptPhase::SnapshotPreseal
                            | DestroyAttemptPhase::RegisteredPreDestroy
                    )
                })
            {
                return false;
            }
            self.reconcile_destroy_attempt(&msg.room_jid).await;
            if !self.rooms.contains_key(&msg.room_jid) {
                return true;
            }
            if self.destroy_attempts.contains_key(&msg.room_jid) {
                return false;
            }
        }
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        if !entry.actor_ref.is_alive() {
            if !self.has_pending_release_capacity(&msg.room_jid, &entry.claim_fence) {
                return false;
            }
            self.rooms.remove(&msg.room_jid);
            // A dead actor cannot complete a retained destroy reconciliation.
            // Its exact claim is being released below, so no later recovery
            // can safely apply this attempt to a successor incarnation.
            self.destroy_attempts.remove(&msg.room_jid);
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
                // A terminal ownership result made this actor non-serving.
                // Remove and kill it locally while preserving durable room
                // state. Owner-sensitive cleanup skips a redundant release
                // after a same-identity database miss, but retains one exact
                // release when local identity supersession may have left the
                // old tuple behind.
                self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                info!(
                    room = %msg.room_jid,
                    "Evicted room actor after a terminal ownership result"
                );
                true
            }
            Ok(RoomSealState::Inactive) => {
                if !self.has_pending_release_capacity(&msg.room_jid, &entry.claim_fence) {
                    return false;
                }
                // A timed-out last-leave request is observed here as the
                // generic `Inactive` seal. Read the frozen room state before
                // removing it so its retry follows the same terminal-destroy
                // rule as the original EmptyNonPersistent transition.
                let intent = match entry
                    .actor_ref
                    .ask(GetSnapshot)
                    .mailbox_timeout(SEAL_ASK_TIMEOUT)
                    .reply_timeout(SEAL_ASK_TIMEOUT)
                    .await
                {
                    Ok(snapshot) if !snapshot.room.config.persistent => {
                        RoomDurableMutation::Destroy {
                            completion_attempt: None,
                        }
                    }
                    Ok(_) => RoomDurableMutation::Dormancy,
                    Err(error) => {
                        warn!(room = %msg.room_jid, ?error, "could not read sealed room state before reaping");
                        return false;
                    }
                };
                if let Some(store) = &self.durable_store {
                    if let Err(error) = store
                        .commit_room_mutation(
                            &msg.room_jid,
                            &entry.claim_fence,
                            intent,
                            crate::muc::RoomMutationEffects::none(),
                        )
                        .await
                    {
                        if matches!(error, RoomCommitError::NotOwner) {
                            self.evict_ownership_lost_room(&msg.room_jid, entry).await;
                            info!(room = %msg.room_jid, "evicted room after ownership loss during dormancy recovery");
                            return true;
                        }
                        if matches!(error, RoomCommitError::StateMissing) {
                            info!(room = %msg.room_jid, "reaping room after terminal dormancy state miss");
                        } else if matches!(error, RoomCommitError::CommitOutcomeUnknown) {
                            warn!(room = %msg.room_jid, "sealed-room reap has unknown durable commit outcome; retaining seal");
                            return false;
                        } else {
                            let _ = entry.actor_ref.tell(UnsealInactive).await;
                            warn!(room = %msg.room_jid, %error, "durable dormancy recovery commit failed; keeping room active");
                            return false;
                        }
                    }
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
            Ok(RoomSealState::Open | RoomSealState::Destroying { .. }) => false,
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
                // Listing remains useful while another room completes its
                // bounded destroy reconciliation. Per-room access stays
                // fail-closed through `live_room`, but an unrelated room
                // must not poison the global inventory.
                Err(RoomRegistryError::OwnershipReconciliationPending(_)) => {}
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

/// Hard-kill and forget a room only while the registry still points at the
/// exact actor incarnation named by the caller. This is the safe response to
/// an actor-local fence rejection: a same-JID successor published before this
/// mailbox turn must never be evicted by the stale actor's failure.
pub struct DemoteRoomIfExactActor {
    pub room_jid: BareJid,
    pub actor_ref: ActorRef<RoomActor>,
}

impl kameo::message::Message<DemoteRoomIfExactActor> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DemoteRoomIfExactActor,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let matches = self
            .rooms
            .get(&msg.room_jid)
            .is_some_and(|entry| entry.actor_ref.id() == msg.actor_ref.id());
        if !matches {
            return false;
        }
        let Some(entry) = self.rooms.remove(&msg.room_jid) else {
            return false;
        };
        self.retire_ownership_lost_entry(&msg.room_jid, entry).await;
        true
    }
}

impl kameo::message::Message<DemoteRoomIfOwner> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DemoteRoomIfOwner,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let pending_matches = self
            .pending_room_preparations
            .get(&msg.room_jid)
            .is_some_and(|pending| pending.claim_fence.owner() == msg.owner);
        if pending_matches {
            let pending = self
                .pending_room_preparations
                .remove(&msg.room_jid)
                .expect("pending owner was checked in the same mailbox turn");
            let claim_fence = pending.claim_fence.clone();
            Self::reply_preparation_failure(
                &msg.room_jid,
                pending.waiters,
                ReclaimedRoomOutcome::LostRace,
            );
            drop(pending.guard);
            self.clear_pending_reclaimed_room(&msg.room_jid, &claim_fence);
            self.transfer_exact_responsibility_to_pending_release(
                msg.room_jid.clone(),
                claim_fence.clone(),
            );
            self.start_detached_room_release(
                msg.room_jid.clone(),
                claim_fence,
                ctx.actor_ref().clone(),
            );
            return true;
        }
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
        self.retire_ownership_lost_entry(&msg.room_jid, entry).await;
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
            self.pending_room_preparations
                .iter()
                .filter(|(_, pending)| pending.claim_fence.owner() == msg.owner)
                .map(|(jid, _)| jid.clone()),
        );
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

/// Bare JIDs of every room this node currently holds an actor for.
///
/// A room actor is claimed by exactly one node, so iterating this on
/// every node covers each room exactly once cluster-wide — which is what
/// lets the SFU voice-reconciliation backstop run without any cross-node
/// request.
pub struct LocalRoomJids;

impl kameo::message::Message<LocalRoomJids> for RoomRegistryActor {
    type Reply = Vec<BareJid>;

    async fn handle(
        &mut self,
        _msg: LocalRoomJids,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.rooms.keys().cloned().collect()
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

/// Test-only mailbox marker used to prove that queued work runs between
/// serialized publication-boundary fences.
#[cfg(test)]
pub(crate) struct MarkRegistryProgress(pub std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(test)]
impl kameo::message::Message<MarkRegistryProgress> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: MarkRegistryProgress,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        msg.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
