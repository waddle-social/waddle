//! Room requests through the durable mailbox: a MUC join, occupant presence
//! update, leave or relayed IQ for a room that another node owns.
//!
//! The origin commits the request and its pending mailbox entry in one
//! ingress transaction, then waits for the room owner's decision the way it
//! waits for a room acceptance. The owner decides the request in one
//! transaction that asserts its room claim, latches the decision, freezes
//! the response, writes durable occupancy and moves the lane's cursor. The
//! origin reads the response in the transaction that consumes the entry and
//! settles the request; that transaction is the single hand-over, so no
//! stanza travels back on a lane or a relay.
//!
//! A hint, a relay reply or a timeout proves nothing here either: the only
//! evidence is the entry's disposition checked against the request's
//! `owner_decision`.

use std::sync::Arc;
use std::time::Duration;

use jid::{BareJid, FullJid};
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::mailbox::{
    MailboxAdmissionLimits, MailboxAdmissionRefusal, MailboxEntryDisposition, MailboxLaneFence,
    MailboxLaneIdentity, MailboxLaneOrigin, MailboxLaneRecipient, MailboxOperationKey,
    MailboxOwnerDecision, MailboxRoomLane, RoomRequestKey, RoomRequestKind,
};
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::{Affiliation, OccupancySessionGeneration, Role};
use xmpp_parsers::presence::Presence;

use super::origin_gate::{RoomAcceptanceUnresolved, RoomAcceptanceWatch};
use super::IngressAuthority;
use crate::ingress_uow::{
    ClaimRepository, IngressUnitOfWork, IngressUowError, MailboxError, MailboxLaneId,
    MailboxRepository, MailboxTargetFence, NewRoomRequest, RoomDecisionLatch, RoomOccupancyLoad,
    RoomOccupantRecord, RoomOccupantRepository, RoomRequestError, RoomRequestRepository,
    RoomRequestSource, RoomResponseRepository, StoredRoomRequest,
};

/// Pending-entry caps for room requests. A lane is one connection's requests
/// to one room, so its backlog is small unless the owner has stalled.
pub(crate) const ROOM_REQUEST_LIMITS: MailboxAdmissionLimits = MailboxAdmissionLimits {
    lane_pending: 64,
    node_pending: 16_384,
};

/// The claim epoch a bound resource presents as a lane's origin. A
/// connection without stream management holds no claim, and the lane is
/// already specific to the connection through its occupancy generation.
const BOUND_RESOURCE_ORIGIN_EPOCH: ClaimEpoch = ClaimEpoch(0);

/// A room request as a connection's handler submits it.
#[derive(Debug, Clone)]
pub struct RoomRequestSubmission {
    /// Minted by the handler before its first attempt and kept for every
    /// retry of that attempt.
    pub key: RoomRequestKey,
    pub kind: RoomRequestKind,
    pub room: BareJid,
    pub occupant: FullJid,
    pub generation: OccupancySessionGeneration,
    pub principal: AuthenticatedPrincipalRef,
    pub stanza: Stanza,
}

/// A committed request whose owner decision the origin waits for.
#[derive(Debug, Clone)]
pub struct PendingRoomRequest {
    pub key: RoomRequestKey,
    pub operation: MailboxOperationKey,
    pub lane: MailboxLaneId,
    pub room: BareJid,
    pub occupant: FullJid,
    pub generation: OccupancySessionGeneration,
    /// Owner incarnation the entry was appended for; where the hint goes.
    pub target_owner: NodeIdentity,
}

/// Result of the origin's commit.
#[derive(Debug, Clone)]
pub enum RoomRequestCommit {
    /// The mailbox does not carry this request: it is not selected, the
    /// protocol epoch is below the mailbox epoch, or the room has no claim.
    /// Nothing was written and the caller keeps its other path.
    NotCarried,
    /// The lane or the owner's node is at its pending cap. Nothing was
    /// written; the requester is answered with `resource-constraint`.
    Refused(MailboxAdmissionRefusal),
    Committed(PendingRoomRequest),
}

/// The owner's decision and frozen response, handed to the origin once.
#[derive(Debug, Clone)]
pub struct RoomRequestResolution {
    pub decision: MailboxOwnerDecision,
    /// The response in the order the requester must receive it. Empty when
    /// an earlier reader already consumed the entry: the hand-over happened
    /// then, and this reader delivers nothing.
    pub responses: Vec<Stanza>,
}

enum RoomRequestPoll {
    Pending,
    Decided(RoomRequestResolution),
    Unresolved(RoomAcceptanceUnresolved),
}

/// What the owner's decision does to the room's durable occupancy.
#[derive(Debug, Clone)]
pub enum RoomOccupancyChange {
    /// The decision leaves occupancy as it is: a refusal, a stale request or
    /// an IQ.
    Unchanged,
    /// An accepted join completes the row the room actor recorded for the
    /// requester with the join request and the broadcast presence.
    Seat {
        nick: String,
        role: Role,
        affiliation: Affiliation,
        presence: Option<Presence>,
    },
    /// An accepted presence update rewrites the requester's row.
    Update {
        nick: String,
        role: Role,
        affiliation: Affiliation,
        presence: Option<Presence>,
    },
    /// An accepted leave removes the requester's row.
    Remove,
}

/// The decision the owner reached for one request.
#[derive(Debug, Clone)]
pub struct RoomRequestOutcome {
    pub decision: MailboxOwnerDecision,
    pub responses: Vec<Stanza>,
    pub occupancy: RoomOccupancyChange,
}

fn room_lane(kind: RoomRequestKind) -> MailboxRoomLane {
    match kind {
        RoomRequestKind::Join | RoomRequestKind::Presence | RoomRequestKind::Leave => {
            MailboxRoomLane::MucStanza
        }
        RoomRequestKind::Iq => MailboxRoomLane::MujiSignaling,
    }
}

/// One authoritative read of a pending request. A decided entry is verified
/// against the request's `owner_decision` under the lane and request locks,
/// its responses are read in order, the entry is consumed and the request
/// settled, all in this transaction.
async fn resolve_room_request(
    uow: &IngressUnitOfWork,
    pending: &PendingRoomRequest,
) -> Result<RoomRequestPoll, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(250), Duration::from_millis(500))
        .await?;
    let Some(entry) = MailboxRepository::lookup(&mut tx, pending.operation).await? else {
        return Ok(RoomRequestPoll::Unresolved(
            RoomAcceptanceUnresolved::EntryMissing,
        ));
    };
    let decision = match entry.disposition {
        MailboxEntryDisposition::Pending if entry.dead_letter.is_some() => {
            return Ok(RoomRequestPoll::Unresolved(
                RoomAcceptanceUnresolved::DeadLettered,
            ));
        }
        MailboxEntryDisposition::Pending => return Ok(RoomRequestPoll::Pending),
        MailboxEntryDisposition::OwnerAccepted => MailboxOwnerDecision::Accepted,
        MailboxEntryDisposition::OwnerRejected => MailboxOwnerDecision::Rejected,
        // Another reader of this request already took the response.
        MailboxEntryDisposition::Consumed(decision) => {
            return Ok(RoomRequestPoll::Decided(RoomRequestResolution {
                decision,
                responses: Vec::new(),
            }));
        }
    };
    let lane = match MailboxRepository::lock_lane_for_origin(
        &mut tx,
        entry.lane,
        BOUND_RESOURCE_ORIGIN_EPOCH,
    )
    .await
    {
        Ok(lane) => lane,
        Err(IngressUowError::Mailbox(MailboxError::StaleFence(_))) => {
            return Ok(RoomRequestPoll::Unresolved(
                RoomAcceptanceUnresolved::OriginSuperseded,
            ));
        }
        Err(error) => return Err(error),
    };
    let Some(request) = RoomRequestRepository::lock(&mut tx, pending.key).await? else {
        return Ok(RoomRequestPoll::Unresolved(
            RoomAcceptanceUnresolved::EntryMissing,
        ));
    };
    if request.decision != Some(decision) || request.generation != pending.generation {
        return Ok(RoomRequestPoll::Unresolved(
            RoomAcceptanceUnresolved::EvidenceMismatch,
        ));
    }
    let responses = RoomResponseRepository::read(&mut tx, pending.key).await?;
    MailboxRepository::consume(&mut tx, &lane, pending.operation).await?;
    RoomRequestRepository::settle(&mut tx, pending.key).await?;
    tx.commit().await?;
    Ok(RoomRequestPoll::Decided(RoomRequestResolution {
        decision,
        responses,
    }))
}

/// A head entry that carries a room request, prepared for the owner.
#[derive(Debug, Clone)]
pub struct ClaimedRoomRequest {
    pub lane: MailboxLaneId,
    pub operation: MailboxOperationKey,
    pub request: StoredRoomRequest,
}

impl IngressAuthority {
    /// Commit a room request and its pending mailbox entry as the origin.
    ///
    /// One ingress transaction reads the room's claim, locks the lane of the
    /// requesting connection, checks admission, inserts the request with its
    /// source binding and appends the entry. A retry with the same key finds
    /// the stored request and its entry and writes nothing. After the commit
    /// the room owner is hinted; its poller finds the entry without the hint.
    pub async fn commit_room_request(
        &self,
        submission: RoomRequestSubmission,
    ) -> Result<RoomRequestCommit, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        if !self.uow.mailbox_acceptance_active(&tx) {
            return Ok(RoomRequestCommit::NotCarried);
        }
        let Some((target_owner, target_epoch)) =
            ClaimRepository::room_claim(&mut tx, &submission.room).await?
        else {
            return Ok(RoomRequestCommit::NotCarried);
        };
        let identity = MailboxLaneIdentity {
            origin: MailboxLaneOrigin::BoundResource {
                jid: submission.occupant.clone(),
                generation: submission.generation,
            },
            recipient: MailboxLaneRecipient::Room {
                room: submission.room.clone(),
                lane: room_lane(submission.kind),
            },
        };
        let fence = MailboxLaneFence {
            origin_epoch: BOUND_RESOURCE_ORIGIN_EPOCH,
            target_epoch,
            target_owner: target_owner.clone(),
        };
        let operation = MailboxOperationKey::for_request(submission.kind, submission.key);
        let mut lane = MailboxRepository::lock_lane(&mut tx, &identity, &fence).await?;
        // A retry of an attempt that already has its entry is never refused
        // for a backlog it is itself part of.
        if MailboxRepository::lookup(&mut tx, operation)
            .await?
            .is_none()
        {
            let load = MailboxRepository::pending_load(&mut tx, &lane).await?;
            if let Err(refusal) = ROOM_REQUEST_LIMITS.admit(load) {
                return Ok(RoomRequestCommit::Refused(refusal));
            }
        }
        RoomRequestRepository::insert(
            &mut tx,
            &NewRoomRequest {
                key: submission.key,
                kind: submission.kind,
                room: submission.room.clone(),
                occupant: submission.occupant.clone(),
                generation: submission.generation,
                source: RoomRequestSource {
                    principal: submission.principal,
                    sm_occurrence: None,
                },
                stanza: submission.stanza,
            },
        )
        .await?;
        let appended =
            MailboxRepository::append(&mut tx, &mut lane, operation, &ROOM_REQUEST_LIMITS).await?;
        let lane = appended.entry().lane;
        tx.commit().await?;
        if let Some(hints) = self.mailbox_hints.get() {
            hints.hint(target_owner.clone());
        }
        Ok(RoomRequestCommit::Committed(PendingRoomRequest {
            key: submission.key,
            operation,
            lane,
            room: submission.room,
            occupant: submission.occupant,
            generation: submission.generation,
            target_owner,
        }))
    }

    /// Look for the owner's decision on `pending` until it is read and
    /// consumed, proven impossible, the budget ends, the authority stops or
    /// `abandoned` reports that nobody waits any more. A storage failure is
    /// retried: it is not evidence either way.
    pub async fn await_room_request(
        &self,
        pending: &PendingRoomRequest,
        abandoned: impl Fn() -> bool,
    ) -> Result<RoomRequestResolution, RoomAcceptanceUnresolved> {
        let watch = RoomAcceptanceWatch::default();
        let deadline = tokio::time::Instant::now() + watch.budget;
        let mut interval = watch.first_interval;
        let _undecided = self.open_room_requests.opened(pending);
        loop {
            if self.cancellation.is_cancelled() {
                return Err(RoomAcceptanceUnresolved::AuthorityStopped);
            }
            if abandoned() {
                return Err(RoomAcceptanceUnresolved::BudgetExhausted);
            }
            match resolve_room_request(&self.uow, pending).await {
                Ok(RoomRequestPoll::Decided(resolution)) => return Ok(resolution),
                Ok(RoomRequestPoll::Unresolved(reason)) => return Err(reason),
                Ok(RoomRequestPoll::Pending) | Err(_) => {}
            }
            if tokio::time::Instant::now() + interval > deadline {
                return Err(RoomAcceptanceUnresolved::BudgetExhausted);
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(watch.max_interval);
        }
    }

    /// Hold a groupchat stanza of the connection `generation` of `sender`
    /// while that connection's join of `room` is undecided.
    ///
    /// A groupchat entry of a resumable stream sits on the session's lane
    /// and the join on the connection's lane, so no cursor orders them. The
    /// owner refuses a sender it has not seated; this gate keeps the origin
    /// from sending the stanza before its own join was decided. It returns
    /// when no join of that connection for the room is undecided, or after
    /// the budget an origin gives a decision, whichever is first.
    pub async fn hold_groupchat_for_undecided_join(
        &self,
        room: &BareJid,
        sender: &FullJid,
        generation: OccupancySessionGeneration,
    ) {
        let deadline = tokio::time::Instant::now() + RoomAcceptanceWatch::default().budget;
        let mut interval = Duration::from_millis(10);
        while self
            .open_room_requests
            .join_undecided(room, sender, generation)
        {
            if self.cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(Duration::from_millis(250));
        }
    }

    /// Commit a leave for an occupant whose connection is ending.
    ///
    /// The connection's cleanup calls this before it drops the connection's
    /// state, with the occupancy generation of that connection, so the room
    /// owner removes exactly that connection's occupancy row. Nobody is left
    /// to receive the response: a background task reads the decision and
    /// settles the request so that retention can collect it.
    pub async fn commit_room_leave_at_connection_end(
        self: &Arc<Self>,
        room: BareJid,
        occupant: FullJid,
        nick: &str,
        generation: OccupancySessionGeneration,
        principal: AuthenticatedPrincipalRef,
    ) -> Result<RoomRequestCommit, IngressUowError> {
        let mut presence = Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(occupant.clone().into());
        presence.to = room.with_resource_str(nick).ok().map(jid::Jid::from);
        let commit = self
            .commit_room_request(RoomRequestSubmission {
                key: RoomRequestKey::mint(),
                kind: RoomRequestKind::Leave,
                room,
                occupant,
                generation,
                principal,
                stanza: Stanza::Presence(presence),
            })
            .await?;
        if let RoomRequestCommit::Committed(pending) = &commit {
            let authority = Arc::clone(self);
            let pending = pending.clone();
            tokio::spawn(async move {
                if let Err(reason) = authority.await_room_request(&pending, || false).await {
                    tracing::warn!(%reason, "leave at connection end was not settled");
                }
            });
        }
        Ok(commit)
    }

    /// Load the request a lane head carries, for the owner's worker.
    /// `Ok(None)` when the stored request cannot be used, which the worker
    /// parks for an operator.
    pub(super) async fn load_claimed_request(
        tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        operation: MailboxOperationKey,
    ) -> Result<Option<ClaimedRoomRequest>, IngressUowError> {
        let Some((kind, key)) = operation.request() else {
            return Ok(None);
        };
        let request = match RoomRequestRepository::read(tx, key).await {
            Ok(Some(request)) if request.kind == kind => request,
            Ok(_) | Err(IngressUowError::RoomRequest(RoomRequestError::InvalidStored)) => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        Ok(Some(ClaimedRoomRequest {
            lane,
            operation,
            request,
        }))
    }

    /// Whether this node has seated `room`'s durable occupancy under the
    /// claim `epoch`.
    pub fn room_occupancy_seated(&self, room: &BareJid, epoch: ClaimEpoch) -> bool {
        self.uow.room_occupancy_seated(room, epoch)
    }

    /// This node's current identity, when the ingress authority is fenced
    /// by cluster claims.
    pub fn node_identity(&self) -> Option<NodeIdentity> {
        match self.uow.fencing() {
            crate::ingress_uow::IngressFencing::Clustered(identity) => Some(identity.current()),
            crate::ingress_uow::IngressFencing::SingleNode => None,
        }
    }

    /// Record that `room`'s durable occupancy was seated under `epoch`.
    pub fn mark_room_occupancy_seated(&self, room: BareJid, epoch: ClaimEpoch) {
        self.uow.mark_room_occupancy_seated(room, epoch);
    }

    /// This node's undecided joins.
    pub fn open_room_requests(&self) -> &OpenRoomRequests {
        &self.open_room_requests
    }

    /// The durable occupancy row of the requester of `request`, read before
    /// the owner runs the request against its room actor.
    pub async fn room_request_occupancy(
        &self,
        request: &StoredRoomRequest,
    ) -> Result<crate::ingress_uow::RoomOccupantLookup, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let lookup =
            RoomOccupantRepository::lookup(&mut tx, &request.room, &request.occupant).await?;
        tx.commit().await?;
        Ok(lookup)
    }

    /// Decide a room request as the room's owner, in one transaction.
    ///
    /// The transaction reads the room's claim, requires it to be held by
    /// `owner` and asserts it, locks the lane as its target, locks the
    /// request, latches the decision, freezes the response, applies the
    /// occupancy change and moves the lane's cursor past the entry. A
    /// request that already records a decision keeps it and its stored
    /// response: the replay only moves the entry, and neither the response
    /// nor occupancy is written again.
    pub async fn decide_room_request(
        &self,
        claimed: &ClaimedRoomRequest,
        owner: &NodeIdentity,
        outcome: RoomRequestOutcome,
    ) -> Result<MailboxEntryDisposition, IngressUowError> {
        let request = &claimed.request;
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let Some((claim_owner, claim_epoch)) =
            ClaimRepository::room_claim(&mut tx, &request.room).await?
        else {
            return Err(IngressUowError::ClaimFenceMissing);
        };
        if claim_owner != *owner {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        let _room_claim =
            ClaimRepository::assert_room_claim(&mut tx, &request.room, owner, claim_epoch).await?;
        let mut lane = MailboxRepository::lock_lane_for_target(
            &mut tx,
            claimed.lane,
            &MailboxTargetFence {
                target_epoch: claim_epoch,
                target_owner: claim_owner,
            },
        )
        .await?;
        let stored = RoomRequestRepository::lock(&mut tx, request.key)
            .await?
            .ok_or(RoomRequestError::RequestMissing)?;
        let decision = match stored.decision {
            Some(recorded) => recorded,
            None => {
                match RoomRequestRepository::latch_decision(&mut tx, request.key, outcome.decision)
                    .await?
                {
                    RoomDecisionLatch::Applied(latched) => {
                        RoomResponseRepository::freeze(&mut tx, &latched, &outcome.responses)
                            .await?;
                        apply_occupancy(&mut tx, request, claim_epoch, outcome.occupancy).await?;
                        latched.decision()
                    }
                    RoomDecisionLatch::Unchanged(recorded) => recorded,
                }
            }
        };
        let change =
            MailboxRepository::decide(&mut tx, &mut lane, claimed.operation, decision).await?;
        tx.commit().await?;
        Ok(change.disposition())
    }

    /// The durable occupancy of `room`, for the node that has taken its
    /// claim. The read asserts nothing; the caller seeds its room actor and
    /// the owner's commit still asserts the claim before it decides.
    pub async fn load_room_occupancy(
        &self,
        room: &BareJid,
    ) -> Result<(RoomOccupancyLoad, Option<(NodeIdentity, ClaimEpoch)>), IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let claim = ClaimRepository::room_claim(&mut tx, room).await?;
        let load = RoomOccupantRepository::load_room(&mut tx, room).await?;
        tx.commit().await?;
        Ok((load, claim))
    }
}

/// A seat the room actor granted or removed, for the room's durable
/// occupancy.
#[derive(Debug, Clone)]
pub enum RoomSeatChange {
    Seat {
        occupant: FullJid,
        nick: String,
        role: Role,
        affiliation: Affiliation,
        generation: OccupancySessionGeneration,
    },
    Unseat {
        occupant: FullJid,
    },
}

impl IngressAuthority {
    /// Write seats the room actor of `room` granted or removed, as the
    /// room's owner at `epoch`.
    ///
    /// One ingress transaction asserts the room claim and applies the
    /// changes in order. This is the nearest the actor's own durable commit
    /// has to a shared transaction: that commit runs on the room store's
    /// connection, which cannot write an epoch-guarded ingress relation, so
    /// the actor writes a seat here before the join's projection commits
    /// and a removal after it applied one. `false` when this deployment
    /// keeps no durable occupancy (the mailbox is not selected, or the
    /// protocol epoch is below the mailbox epoch) and nothing was written.
    pub async fn record_room_seats(
        &self,
        room: &BareJid,
        owner: &NodeIdentity,
        epoch: ClaimEpoch,
        changes: Vec<RoomSeatChange>,
    ) -> Result<bool, IngressUowError> {
        if !self.uow.mailbox_acceptance_enabled() {
            return Ok(false);
        }
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        if !self.uow.mailbox_acceptance_active(&tx) {
            return Ok(false);
        }
        let _room_claim = ClaimRepository::assert_room_claim(&mut tx, room, owner, epoch).await?;
        for change in changes {
            match change {
                RoomSeatChange::Seat {
                    occupant,
                    nick,
                    role,
                    affiliation,
                    generation,
                } => {
                    RoomOccupantRepository::record_seat(
                        &mut tx,
                        &RoomOccupantRecord {
                            room: room.clone(),
                            occupant,
                            nick,
                            role,
                            affiliation,
                            generation,
                            join_request: None,
                            presence: None,
                            room_epoch: epoch,
                        },
                    )
                    .await?;
                }
                RoomSeatChange::Unseat { occupant } => {
                    RoomOccupantRepository::remove(&mut tx, room, &occupant, None).await?;
                }
            }
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// The room store's writer of durable occupancy: the ingress authority,
/// which owns the epoch-guarded relation.
pub struct IngressOccupancyLedger {
    authority: Arc<IngressAuthority>,
}

impl IngressOccupancyLedger {
    pub fn new(authority: Arc<IngressAuthority>) -> Self {
        Self { authority }
    }
}

impl waddle_xmpp::muc::durable::RoomOccupancyLedger for IngressOccupancyLedger {
    fn record<'a>(
        &'a self,
        room_jid: &'a BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        changes: Vec<waddle_xmpp::muc::durable::RoomOccupancyChange>,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, waddle_xmpp::muc::durable::RoomOccupancyRecord>
    {
        use waddle_xmpp::muc::durable::{RoomOccupancyChange as Change, RoomOccupancyRecord};
        Box::pin(async move {
            let changes = changes
                .into_iter()
                .map(|change| match change {
                    Change::Seat(seat) => RoomSeatChange::Seat {
                        occupant: seat.occupant,
                        nick: seat.nick,
                        role: seat.role,
                        affiliation: seat.affiliation,
                        generation: seat.session,
                    },
                    Change::Unseat { occupant } => RoomSeatChange::Unseat { occupant },
                })
                .collect();
            match self
                .authority
                .record_room_seats(room_jid, &fence.owner, fence.epoch, changes)
                .await
            {
                Ok(true) => Ok(RoomOccupancyRecord::Written),
                Ok(false) => Ok(RoomOccupancyRecord::Inactive),
                Err(error) => {
                    tracing::warn!(%error, room = %room_jid, "room occupancy not recorded");
                    Err(waddle_xmpp::XmppError::internal(
                        "room occupancy record is unavailable",
                    ))
                }
            }
        })
    }
}

async fn apply_occupancy(
    tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    request: &StoredRoomRequest,
    room_epoch: ClaimEpoch,
    change: RoomOccupancyChange,
) -> Result<(), IngressUowError> {
    let record = |nick, role, affiliation, presence, join_request| RoomOccupantRecord {
        room: request.room.clone(),
        occupant: request.occupant.clone(),
        nick,
        role,
        affiliation,
        generation: request.generation,
        join_request,
        presence,
        room_epoch,
    };
    match change {
        RoomOccupancyChange::Unchanged => {}
        RoomOccupancyChange::Seat {
            nick,
            role,
            affiliation,
            presence,
        } => {
            RoomOccupantRepository::confirm_join(
                tx,
                &record(nick, role, affiliation, presence, Some(request.key)),
            )
            .await?;
        }
        RoomOccupancyChange::Update {
            nick,
            role,
            affiliation,
            presence,
        } => {
            RoomOccupantRepository::update_presence(
                tx,
                &record(nick, role, affiliation, presence, None),
            )
            .await?;
        }
        RoomOccupancyChange::Remove => {
            RoomOccupantRepository::remove(
                tx,
                &request.room,
                &request.occupant,
                Some(request.generation),
            )
            .await?;
        }
    }
    Ok(())
}

/// The room requests this node's connections have committed and not yet
/// seen decided.
#[derive(Debug, Default)]
pub struct OpenRoomRequests {
    undecided_joins: std::sync::Mutex<std::collections::HashMap<RoomRequestKey, UndecidedJoin>>,
}

#[derive(Debug)]
struct UndecidedJoin {
    room: BareJid,
    occupant: FullJid,
    generation: OccupancySessionGeneration,
}

/// Removes an undecided join when its wait ends, also when its future is
/// dropped.
pub(super) struct OpenJoinWait<'a> {
    requests: &'a OpenRoomRequests,
    key: Option<RoomRequestKey>,
}

impl Drop for OpenJoinWait<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            self.requests
                .undecided_joins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
        }
    }
}

impl OpenRoomRequests {
    /// Record that the wait on `pending` began. Only a join is tracked: it
    /// is the request a groupchat stanza of the same connection waits for.
    fn opened(&self, pending: &PendingRoomRequest) -> OpenJoinWait<'_> {
        let join = matches!(pending.operation, MailboxOperationKey::RoomJoin(_));
        if join {
            self.undecided_joins
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    pending.key,
                    UndecidedJoin {
                        room: pending.room.clone(),
                        occupant: pending.occupant.clone(),
                        generation: pending.generation,
                    },
                );
        }
        OpenJoinWait {
            requests: self,
            key: join.then_some(pending.key),
        }
    }

    fn join_undecided(
        &self,
        room: &BareJid,
        occupant: &FullJid,
        generation: OccupancySessionGeneration,
    ) -> bool {
        self.undecided_joins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|join| {
                join.room == *room && join.occupant == *occupant && join.generation == generation
            })
    }
}
