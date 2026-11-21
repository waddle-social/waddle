//! The room owner's side of the durable mailbox: finding entries this node
//! owes a decision and preparing one for the owner's commit.
//!
//! A hint or a scan only names a lane. Nothing here decides an entry: the
//! caller runs the owner's ordinary relayed commit, which asserts the room
//! claim and records the decision. Entries are worked from their canonical
//! rows, so the same request is recovered whether the origin's relay, its
//! hint or neither arrived.

use std::time::Duration;

use jid::BareJid;
use waddle_xmpp::ingress::mailbox::{
    MailboxEntryDisposition, MailboxLaneIdentity, MailboxLaneRecipient, MailboxOperationKey,
    MailboxOwnerDecision, MailboxRoomLane,
};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::NodeIdentity;

use super::identity::{IngressCanonicalRef, IngressRelayAdmission};
use super::IngressAuthority;
use crate::ingress_uow::{
    CanonicalMessageRepository, IngressUowError, MailboxDeadLetterReason, MailboxLaneId,
    MailboxRepository,
};

/// Lanes one poll pass examines.
pub const MAILBOX_SCAN_LIMIT: u32 = 128;
/// Lanes one poll pass works at a time; also the number of workers.
pub const MAILBOX_CLAIM_LIMIT: usize = 2;
/// Failed attempts after which an entry is parked for an operator.
pub const MAILBOX_ATTEMPT_LIMIT: u32 = 8;
/// How long after its append a decided entry is left to its origin before
/// the sweep consumes it. Ten times the origin watcher's budget, so a live
/// origin has always finished with the entry first.
pub const MAILBOX_UNCONSUMED_GRACE: Duration = Duration::from_secs(600);

/// A pending room acceptance prepared from canonical rows for the owner's
/// commit.
#[derive(Debug, Clone)]
pub struct ClaimedRoomAcceptance {
    pub lane: MailboxLaneId,
    pub operation: MailboxOperationKey,
    pub room: BareJid,
    pub message: xmpp_parsers::message::Message,
    pub admission: IngressRelayAdmission,
}

/// What a worker found at the head of a lane.
#[derive(Debug, Clone)]
pub enum MailboxLaneHead {
    /// Nothing to do now: the lane is held by another worker, drained,
    /// targeted elsewhere, dead-lettered, or its next attempt is deferred.
    Idle,
    /// The head entry, ready for the owner's commit.
    Ready(Box<ClaimedRoomAcceptance>),
    /// The head entry carries a room request, ready for the owner to run
    /// against its room and decide.
    #[cfg(feature = "clustering")]
    ReadyRequest(Box<super::room_request::ClaimedRoomRequest>),
    /// The head entry carries a direct request, ready for the owner of the
    /// recipient's account to take custody of and decide.
    #[cfg(feature = "clustering")]
    ReadyDirect(Box<super::direct_request::ClaimedDirectRequest>),
    /// The head entry's canonical evidence cannot be used. It was parked for
    /// an operator in the same transaction.
    DeadLettered,
}

/// How an attempt at a lane head was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxAttemptRecord {
    /// The attempt was counted and the next one deferred.
    Deferred { attempts: u32 },
    /// The retry budget is spent; the entry was parked for an operator.
    DeadLettered,
    /// A session-scoped room request exhausted retries and was settled as
    /// rejected to release its lane.
    ExhaustedSettled,
    /// This node does not hold the authoritative room claim needed to settle a
    /// retry-exhausted request; the entry stays pending (with its next attempt
    /// deferred) so the current or future room owner can adopt and work it.
    ClaimNotHeld,
    /// The entry was decided or the lane changed hands meanwhile.
    Settled,
}

/// What one adoption pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxLaneAdoption {
    /// Lanes that now record this node's room claim.
    pub adopted: usize,
    /// Where the next pass resumes; `None` starts over.
    pub resume_after: Option<MailboxLaneId>,
}

/// What one sweep of unconsumed decisions did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MailboxDecisionSweep {
    pub consumed: usize,
    /// Entries whose canonical message does not prove their decision.
    pub unproven: usize,
    /// Where the next pass resumes; `None` starts over.
    pub resume_after: Option<MessageKey>,
}

/// Delay before the next attempt at an entry that has failed `attempts`
/// times: 250 ms doubling to at most 30 s.
pub fn mailbox_retry_delay(attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(7);
    Duration::from_millis(250u64.saturating_mul(1u64 << exponent)).min(Duration::from_secs(30))
}

impl IngressAuthority {
    /// Whether this node carries remote room acceptance through the mailbox.
    pub fn mailbox_acceptance_enabled(&self) -> bool {
        self.uow.mailbox_acceptance_enabled()
    }

    /// Lanes that owe `target` a disposition, in lane order after `after`,
    /// at most [`MAILBOX_SCAN_LIMIT`]. A caller that passes the last lane of
    /// a full page reaches lanes behind heads that cannot be worked now.
    pub async fn scan_mailbox_backlog(
        &self,
        target: &NodeIdentity,
        after: Option<MailboxLaneId>,
    ) -> Result<Vec<MailboxLaneId>, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let lanes =
            MailboxRepository::scan_backlog(&mut tx, target, after, MAILBOX_SCAN_LIMIT).await?;
        tx.commit().await?;
        Ok(lanes)
    }

    /// Read the head of `lane` for `target` and prepare it for the owner's
    /// commit. The lane lock is held only for this read; the commit takes it
    /// again under the room claim.
    pub async fn claim_room_acceptance(
        &self,
        lane: MailboxLaneId,
        target: &NodeIdentity,
    ) -> Result<MailboxLaneHead, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let Some(locked) = MailboxRepository::claim_backlogged_lane(&mut tx, lane, target).await?
        else {
            return Ok(MailboxLaneHead::Idle);
        };
        let Some(entry) = MailboxRepository::read_after_cursor(&mut tx, &locked, 1)
            .await?
            .into_iter()
            .next()
        else {
            return Ok(MailboxLaneHead::Idle);
        };
        if entry.disposition != MailboxEntryDisposition::Pending
            || entry.dead_letter.is_some()
            || MailboxRepository::attempt_deferred(&mut tx, &entry).await?
        {
            return Ok(MailboxLaneHead::Idle);
        }
        // An entry whose parent is a direct request is prepared from that
        // request; one whose request cannot be read is parked for an
        // operator.
        #[cfg(feature = "clustering")]
        if entry.operation.direct().is_some() {
            let Some(claimed) = Self::load_claimed_direct(&mut tx, lane, entry.operation).await?
            else {
                MailboxRepository::dead_letter(
                    &mut tx,
                    &locked,
                    entry.operation,
                    MailboxDeadLetterReason::CanonicalEvidenceInvalid,
                )
                .await?;
                tx.commit().await?;
                return Ok(MailboxLaneHead::DeadLettered);
            };
            tx.commit().await?;
            return Ok(MailboxLaneHead::ReadyDirect(Box::new(claimed)));
        }
        // An entry whose parent is a room request is prepared from that
        // request; one whose request cannot be read is parked for an
        // operator. Room requests exist only with clustering.
        let Some(key) = entry.operation.message_key() else {
            #[cfg(feature = "clustering")]
            {
                let claimed = Self::load_claimed_request(&mut tx, lane, entry.operation).await?;
                let Some(claimed) = claimed else {
                    MailboxRepository::dead_letter(
                        &mut tx,
                        &locked,
                        entry.operation,
                        MailboxDeadLetterReason::CanonicalEvidenceInvalid,
                    )
                    .await?;
                    tx.commit().await?;
                    return Ok(MailboxLaneHead::DeadLettered);
                };
                tx.commit().await?;
                return Ok(MailboxLaneHead::ReadyRequest(Box::new(claimed)));
            }
            #[cfg(not(feature = "clustering"))]
            return Ok(MailboxLaneHead::Idle);
        };
        let room = match locked.key().encoding().decode() {
            Ok(MailboxLaneIdentity {
                recipient:
                    MailboxLaneRecipient::Room {
                        room,
                        lane: MailboxRoomLane::MucStanza,
                    },
                ..
            }) => Some(room),
            _ => None,
        };
        let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key).await?;
        let source = CanonicalMessageRepository::acceptance_source(&mut tx, key).await?;
        let (Some(room), Some(envelope), Some((principal, origin_id))) = (room, envelope, source)
        else {
            MailboxRepository::dead_letter(
                &mut tx,
                &locked,
                entry.operation,
                MailboxDeadLetterReason::CanonicalEvidenceInvalid,
            )
            .await?;
            tx.commit().await?;
            return Ok(MailboxLaneHead::DeadLettered);
        };
        let stanza_lang = MailboxRepository::request_language(&mut tx, &entry)
            .await?
            .map(xmpp_parsers::message::Lang);
        tx.commit().await?;
        Ok(MailboxLaneHead::Ready(Box::new(ClaimedRoomAcceptance {
            lane,
            operation: entry.operation,
            room,
            message: envelope.message().clone(),
            admission: IngressRelayAdmission {
                canonical: IngressCanonicalRef {
                    message_key: key,
                    sender_bare: principal.bare_jid().clone(),
                    origin_id,
                },
                principal,
                stanza_lang,
            },
        })))
    }

    /// Deliver what the owner's commit of `operation` still owes, from this
    /// node, as soon as the commit has executed.
    ///
    /// The worker has no connection to answer: the sender's reflection and
    /// any copy its live execution could not hand over are left as
    /// unreceipted obligations on the canonical message. This runs the
    /// ingress recovery of that one message now, which rebuilds exactly the
    /// frozen copies and routes each to the node that hosts its recipient,
    /// instead of leaving them to the maintenance pass a minute later. A
    /// failure here loses nothing; the maintenance pass retries the same
    /// rows.
    pub async fn deliver_owner_obligations(&self, operation: MailboxOperationKey) -> bool {
        const BUDGET: Duration = Duration::from_secs(5);
        // A room request leaves no obligation on a canonical message: its
        // response is frozen with the decision and read by its origin.
        let Some(message_key) = operation.message_key() else {
            return true;
        };
        let Some(environment) = self.recovery.environment() else {
            return false;
        };
        let deps = environment.recovery_deps();
        let deadline = tokio::time::Instant::now() + BUDGET;
        let recovered = tokio::time::timeout_at(
            deadline,
            super::recovery_executor::recover_row(
                &self.database,
                &self.uow,
                &deps,
                message_key,
                deadline,
            ),
        )
        .await;
        match recovered {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                tracing::warn!(%error, "owner obligations of a mailbox entry not delivered yet");
                false
            }
            Err(_) => false,
        }
    }

    /// Count a failed attempt at `operation` and defer the next one, or once
    /// [`MAILBOX_ATTEMPT_LIMIT`] is reached:
    /// - for a session-scoped room request (`RoomJoin`, `RoomPresence`,
    ///   `RoomLeave`, `RoomIq`), verify this node still holds the room claim
    ///   and atomically reject, consume, and settle the request in canonical
    ///   lock order (`epoch -> claim -> lane -> request -> entry`) so the lane
    ///   cursor advances instead of parking forever;
    /// - for a durable room acceptance or direct request, park the entry for
    ///   an operator without moving the lane's cursor.
    pub async fn record_room_acceptance_failure(
        &self,
        lane: MailboxLaneId,
        operation: MailboxOperationKey,
        target: &NodeIdentity,
    ) -> Result<MailboxAttemptRecord, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        #[cfg(feature = "clustering")]
        if let Some((kind, request_key)) = operation.request() {
            let Some(peek) = MailboxRepository::lookup(&mut tx, operation).await? else {
                return Ok(MailboxAttemptRecord::Settled);
            };
            if peek.disposition != MailboxEntryDisposition::Pending || peek.dead_letter.is_some() {
                return Ok(MailboxAttemptRecord::Settled);
            }
            if peek.lane == lane && peek.attempts.saturating_add(1) >= MAILBOX_ATTEMPT_LIMIT {
                return Self::settle_exhausted_room_request(
                    tx,
                    lane,
                    operation,
                    kind,
                    request_key,
                    target,
                )
                .await;
            }
        }
        let Some(locked) = MailboxRepository::claim_backlogged_lane(&mut tx, lane, target).await?
        else {
            return Ok(MailboxAttemptRecord::Settled);
        };
        let Some(entry) = MailboxRepository::lookup(&mut tx, operation).await? else {
            return Ok(MailboxAttemptRecord::Settled);
        };
        if entry.disposition != MailboxEntryDisposition::Pending || entry.dead_letter.is_some() {
            return Ok(MailboxAttemptRecord::Settled);
        }
        #[cfg(feature = "clustering")]
        if let Some((kind, request_key)) = operation.request() {
            if entry.attempts.saturating_add(1) >= MAILBOX_ATTEMPT_LIMIT {
                // Another worker bumped `attempts` to the limit between our
                // unlocked peek and `claim_backlogged_lane`. Drop `tx` to
                // release the lane lock before re-entering in canonical
                // `claim -> lane -> request -> entry` order.
                drop(tx);
                let fresh_tx = self
                    .uow
                    .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
                    .await?;
                return Self::settle_exhausted_room_request(
                    fresh_tx,
                    lane,
                    operation,
                    kind,
                    request_key,
                    target,
                )
                .await;
            }
        }
        let attempts = MailboxRepository::record_attempt(
            &mut tx,
            &locked,
            operation,
            mailbox_retry_delay(entry.attempts.saturating_add(1)),
        )
        .await?;
        let record = if attempts >= MAILBOX_ATTEMPT_LIMIT {
            MailboxRepository::dead_letter(
                &mut tx,
                &locked,
                operation,
                MailboxDeadLetterReason::RetriesExhausted,
            )
            .await?;
            MailboxAttemptRecord::DeadLettered
        } else {
            MailboxAttemptRecord::Deferred { attempts }
        };
        tx.commit().await?;
        Ok(record)
    }

    /// Settle a retry-exhausted session-scoped room request inside `tx` in
    /// exact canonical lock order:
    /// `ingress_protocol_epoch (held by tx) -> clustering_claims (FOR SHARE)
    /// -> ingress_mailbox_lanes (FOR UPDATE) -> ingress_room_requests (FOR UPDATE)
    /// -> ingress_mailbox_entries (FOR UPDATE)`.
    #[cfg(feature = "clustering")]
    async fn settle_exhausted_room_request(
        mut tx: crate::ingress_uow::IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        operation: MailboxOperationKey,
        kind: waddle_xmpp::ingress::mailbox::RoomRequestKind,
        request_key: waddle_xmpp::ingress::mailbox::RoomRequestKey,
        target: &NodeIdentity,
    ) -> Result<MailboxAttemptRecord, IngressUowError> {
        use crate::ingress_uow::{
            ClaimRepository, MailboxError, MailboxTargetFence, RoomRequestError,
            RoomRequestRepository,
        };
        // Unlocked read of the parent request to discover the room JID before
        // taking any row locks. Missing or malformed parent rows park as
        // `CanonicalEvidenceInvalid` so corrupt state is never silently dropped.
        let room = match RoomRequestRepository::read(&mut tx, request_key).await {
            Ok(Some(request)) if request.kind == kind => request.room,
            Ok(_) | Err(IngressUowError::RoomRequest(RoomRequestError::InvalidStored)) => {
                let Some(locked) =
                    MailboxRepository::claim_backlogged_lane(&mut tx, lane, target).await?
                else {
                    return Ok(MailboxAttemptRecord::Settled);
                };
                MailboxRepository::dead_letter(
                    &mut tx,
                    &locked,
                    operation,
                    MailboxDeadLetterReason::CanonicalEvidenceInvalid,
                )
                .await?;
                tx.commit().await?;
                return Ok(MailboxAttemptRecord::DeadLettered);
            }
            Err(error) => return Err(error),
        };
        // 1. Assert room claim first (canonical lock order). If the room claim
        //    is gone or moved elsewhere, do not settle the request — defer our
        //    retry without consuming an attempt and report `ClaimNotHeld` so
        //    `work_lane` stops immediately without spinning or emitting
        //    `Decided`, while the current or next room owner can adopt the
        //    lane (which clears `next_attempt_at`) and work it cleanly.
        let Some((claim_owner, claim_epoch)) = ClaimRepository::room_claim(&mut tx, &room).await?
        else {
            return Self::defer_exhausted_without_claim(tx, lane, operation, target).await;
        };
        if claim_owner != *target {
            return Self::defer_exhausted_without_claim(tx, lane, operation, target).await;
        }
        match ClaimRepository::assert_room_claim(&mut tx, &room, target, claim_epoch).await {
            Ok(_) => {}
            Err(IngressUowError::ClaimFenceMissing | IngressUowError::RoomGenerationStale) => {
                return Self::defer_exhausted_without_claim(tx, lane, operation, target).await;
            }
            Err(error) => return Err(error),
        }
        // 2. Lock the lane as its target.
        let mut locked = match MailboxRepository::lock_lane_for_target(
            &mut tx,
            lane,
            &MailboxTargetFence {
                target_epoch: claim_epoch,
                target_owner: claim_owner,
            },
        )
        .await
        {
            Ok(locked) => locked,
            Err(IngressUowError::Mailbox(MailboxError::LaneMissing)) => {
                return Ok(MailboxAttemptRecord::Settled);
            }
            Err(IngressUowError::Mailbox(MailboxError::StaleFence(_))) => {
                return Ok(MailboxAttemptRecord::ClaimNotHeld);
            }
            Err(error) => return Err(error),
        };
        if locked.pending() == 0 {
            return Ok(MailboxAttemptRecord::Settled);
        }
        // 3. Lock the room request row.
        let Some(stored) = RoomRequestRepository::lock(&mut tx, request_key).await? else {
            MailboxRepository::dead_letter(
                &mut tx,
                &locked,
                operation,
                MailboxDeadLetterReason::CanonicalEvidenceInvalid,
            )
            .await?;
            tx.commit().await?;
            return Ok(MailboxAttemptRecord::DeadLettered);
        };
        // Re-verify the mailbox entry under the held locks.
        let Some(entry) = MailboxRepository::lookup(&mut tx, operation).await? else {
            return Ok(MailboxAttemptRecord::Settled);
        };
        if entry.lane != locked.id()
            || entry.disposition != MailboxEntryDisposition::Pending
            || entry.dead_letter.is_some()
        {
            return Ok(MailboxAttemptRecord::Settled);
        }
        if locked.cursor().advance_to(entry.sequence).is_err() {
            return Ok(MailboxAttemptRecord::Settled);
        }
        if entry.attempts.saturating_add(1) < MAILBOX_ATTEMPT_LIMIT {
            let attempts = MailboxRepository::record_attempt(
                &mut tx,
                &locked,
                operation,
                mailbox_retry_delay(entry.attempts.saturating_add(1)),
            )
            .await?;
            tx.commit().await?;
            return Ok(MailboxAttemptRecord::Deferred { attempts });
        }
        // Defensive guard: if the request already carries a latched decision,
        // only advance the mailbox entry without consuming/settling responses
        // the origin may still need to read.
        if let Some(recorded) = stored.decision {
            MailboxRepository::decide(&mut tx, &mut locked, operation, recorded).await?;
            tx.commit().await?;
            return Ok(MailboxAttemptRecord::Settled);
        }
        // 4. Record the final attempt, latch Rejected, advance the lane cursor,
        //    consume the entry, and settle the request so collection cleans it
        //    up and `room_request_unconsumed_decisions` does not stay nonzero.
        MailboxRepository::record_attempt(&mut tx, &locked, operation, Duration::ZERO).await?;
        RoomRequestRepository::latch_decision(&mut tx, request_key, MailboxOwnerDecision::Rejected)
            .await?;
        MailboxRepository::decide(
            &mut tx,
            &mut locked,
            operation,
            MailboxOwnerDecision::Rejected,
        )
        .await?;
        MailboxRepository::consume(&mut tx, &locked, operation).await?;
        RoomRequestRepository::settle(&mut tx, request_key).await?;
        tx.commit().await?;
        tracing::warn!(
            room = %room,
            ?kind,
            lane = %lane.as_uuid(),
            attempts = MAILBOX_ATTEMPT_LIMIT,
            "room request exhausted retries; settled as rejected to unblock mailbox lane"
        );
        Ok(MailboxAttemptRecord::ExhaustedSettled)
    }

    /// Defer the next attempt of an entry whose room claim is absent, held by
    /// another node, or stale — without incrementing `attempt_count` or
    /// settling the entry — so a deposed or unclaimed node does not hot-spin
    /// `work_lane` or poll every second while waiting for claim adoption.
    #[cfg(feature = "clustering")]
    async fn defer_exhausted_without_claim(
        mut tx: crate::ingress_uow::IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        operation: MailboxOperationKey,
        target: &NodeIdentity,
    ) -> Result<MailboxAttemptRecord, IngressUowError> {
        if let Some(locked) =
            MailboxRepository::claim_backlogged_lane(&mut tx, lane, target).await?
        {
            MailboxRepository::defer_attempt(
                &mut tx,
                &locked,
                operation,
                mailbox_retry_delay(MAILBOX_ATTEMPT_LIMIT),
            )
            .await?;
            tx.commit().await?;
        }
        Ok(MailboxAttemptRecord::ClaimNotHeld)
    }

    /// Adopt backlogged lanes whose work this node now owns: lanes of a
    /// room whose claim it holds, and lanes of a bare or full JID whose
    /// account's `UserActor` claim it holds.
    ///
    /// A lane records the owner incarnation its origin last proved, and the
    /// backlog scan reads only lanes recorded for this node. After a claim
    /// moves (to another node, or to a new incarnation of the same node)
    /// its lanes still name the predecessor. This pass reads at most
    /// [`MAILBOX_SCAN_LIMIT`] such lanes after `after` and, for each whose
    /// room the claims table assigns to `target`, writes that claim onto the
    /// lane in a transaction that reads the claim and locks the lane. A lane
    /// whose room has no claim, or a claim held elsewhere, is left alone:
    /// its entry stays pending until some node owns the room.
    ///
    /// Before a room's lanes are adopted, `seat_occupancy` loads the room's
    /// durable occupancy and seats it in this node's room actor; a room for
    /// which it reports `false` keeps its lanes unadopted until a later pass.
    ///
    /// Adoption only makes the lane visible to this node's backlog scan. The
    /// owner's commit asserts the room claim again before it decides.
    #[cfg(feature = "clustering")]
    pub async fn adopt_transferred_lanes<Seat, Seated>(
        &self,
        target: &NodeIdentity,
        after: Option<MailboxLaneId>,
        seat_occupancy: Seat,
    ) -> Result<MailboxLaneAdoption, IngressUowError>
    where
        Seat: Fn(BareJid) -> Seated,
        Seated: std::future::Future<Output = bool>,
    {
        use crate::ingress_uow::{ClaimRepository, MailboxTargetFence};
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let lanes =
            MailboxRepository::scan_foreign_backlog(&mut tx, target, after, MAILBOX_SCAN_LIMIT)
                .await?;
        // The claim that owns a lane's work: the room's for a room lane, the
        // recipient account's `UserActor` claim for a bare-JID or full-JID
        // lane.
        #[derive(Clone, PartialEq, Eq, Hash)]
        enum LaneOwner {
            Room(BareJid),
            Account(BareJid),
        }
        async fn owner_claim(
            tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
            owner: &LaneOwner,
        ) -> Result<Option<(NodeIdentity, waddle_xmpp::ownership::ClaimEpoch)>, IngressUowError>
        {
            match owner {
                LaneOwner::Room(room) => ClaimRepository::room_claim(tx, room).await,
                LaneOwner::Account(account) => ClaimRepository::user_claim(tx, account).await,
            }
        }
        let mut owned = Vec::new();
        let mut claims: std::collections::HashMap<LaneOwner, bool> =
            std::collections::HashMap::new();
        for (lane, key) in &lanes {
            let owner = match key.encoding().decode() {
                Ok(MailboxLaneIdentity {
                    recipient: MailboxLaneRecipient::Room { room, .. },
                    ..
                }) => LaneOwner::Room(room),
                Ok(MailboxLaneIdentity {
                    recipient: MailboxLaneRecipient::BareJid(account),
                    ..
                }) => LaneOwner::Account(account),
                Ok(MailboxLaneIdentity {
                    recipient: MailboxLaneRecipient::FullJid(resource),
                    ..
                }) => LaneOwner::Account(resource.to_bare()),
                Err(_) => continue,
            };
            let here = match claims.get(&owner) {
                Some(here) => *here,
                None => {
                    let here = owner_claim(&mut tx, &owner)
                        .await?
                        .is_some_and(|(holder, _)| holder == *target);
                    claims.insert(owner.clone(), here);
                    here
                }
            };
            if here {
                owned.push((*lane, owner));
            }
        }
        tx.commit().await?;
        let mut adopted = 0;
        let mut seated: std::collections::HashMap<BareJid, bool> = std::collections::HashMap::new();
        for (lane, owner) in owned {
            // A room lane becomes visible to this node's backlog scan only
            // after the room's durable occupancy is seated in its room
            // actor, so no entry of the room is decided against an empty
            // roster. An account lane has nothing to seat: the account's
            // sessions are read when an entry is worked.
            if let LaneOwner::Room(room) = &owner {
                let ready = match seated.get(room) {
                    Some(ready) => *ready,
                    None => {
                        let ready = seat_occupancy(room.clone()).await;
                        seated.insert(room.clone(), ready);
                        ready
                    }
                };
                if !ready {
                    continue;
                }
            }
            let mut tx = self
                .uow
                .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
                .await?;
            // The claim is read again in the transaction that writes it onto
            // the lane, so a claim that moved since the scan is not adopted.
            let Some((holder, epoch)) = owner_claim(&mut tx, &owner).await? else {
                continue;
            };
            if holder != *target {
                continue;
            }
            let fence = MailboxTargetFence {
                target_epoch: epoch,
                target_owner: holder,
            };
            if MailboxRepository::adopt_backlogged_lane(&mut tx, lane, &fence).await? {
                tx.commit().await?;
                adopted += 1;
            }
        }
        let full_page = lanes.len() >= MAILBOX_SCAN_LIMIT as usize;
        Ok(MailboxLaneAdoption {
            adopted,
            resume_after: lanes.last().map(|(lane, _)| *lane).filter(|_| full_page),
        })
    }

    /// Consume decided entries whose origin never read the decision.
    ///
    /// The origin's watcher or a retransmission normally consumes a decided
    /// entry. When the origin node stopped in between and the sender never
    /// sends the stanza again, nothing would, and the entry would keep its
    /// canonical message from collection indefinitely. This pass reads at
    /// most [`MAILBOX_SCAN_LIMIT`] entries appended at least `grace` ago
    /// (the poller passes [`MAILBOX_UNCONSUMED_GRACE`]) and consumes each
    /// one whose canonical message proves the same decision, under the lane
    /// and canonical locks.
    ///
    /// Consumption discards nothing a sender can still need: the terminal
    /// entry keeps the decision until its message is collected, so a later
    /// retransmission reads it and is counted as handled. An entry whose
    /// evidence does not prove its decision is left in place and reported.
    pub async fn sweep_unconsumed_decisions(
        &self,
        grace: Duration,
        after: Option<MessageKey>,
    ) -> Result<MailboxDecisionSweep, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let entries =
            MailboxRepository::scan_unconsumed_decisions(&mut tx, grace, after, MAILBOX_SCAN_LIMIT)
                .await?;
        tx.commit().await?;
        let mut sweep = MailboxDecisionSweep::default();
        for entry in &entries {
            let decision = match entry.disposition {
                MailboxEntryDisposition::OwnerAccepted => MailboxOwnerDecision::Accepted,
                MailboxEntryDisposition::OwnerRejected => MailboxOwnerDecision::Rejected,
                MailboxEntryDisposition::Pending | MailboxEntryDisposition::Consumed(_) => continue,
            };
            let mut tx = self
                .uow
                .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
                .await?;
            let Some(lane) = MailboxRepository::claim_lane_for_sweep(&mut tx, entry.lane).await?
            else {
                continue;
            };
            match super::origin_gate::consume_proven_decision(
                &mut tx,
                &lane,
                entry.operation,
                decision,
            )
            .await?
            {
                None => {
                    tx.commit().await?;
                    sweep.consumed += 1;
                }
                Some(reason) => {
                    tracing::warn!(
                        %reason,
                        "decided mailbox entry is not proven by its canonical message; left unconsumed"
                    );
                    sweep.unproven += 1;
                }
            }
        }
        let full_page = entries.len() >= MAILBOX_SCAN_LIMIT as usize;
        sweep.resume_after = entries
            .last()
            .and_then(|entry| entry.operation.message_key())
            .filter(|_| full_page);
        Ok(sweep)
    }

    /// Whether a room request sent now is carried by the mailbox: the
    /// deployment selects it and the protocol epoch has reached the mailbox
    /// epoch. A storage failure reads as `false`.
    pub async fn room_requests_carried_now(&self) -> bool {
        self.uow
            .mailbox_acceptance_active_now()
            .await
            .unwrap_or(false)
    }

    /// Count the room requests whose owner decided at least `older_than`
    /// ago and whose origin has not consumed the decision.
    ///
    /// A room request's response belongs to the connection that sent it, so
    /// nothing consumes it on that connection's behalf: unlike a decided
    /// room acceptance, such a request is only reported. It stays until its
    /// requester resumes and consumes it or an operator removes it.
    #[cfg(feature = "clustering")]
    pub async fn count_unconsumed_request_decisions(
        &self,
        older_than: Duration,
    ) -> Result<u64, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let count = crate::ingress_uow::RoomRequestRepository::count_unsettled_decisions(
            &mut tx, older_than,
        )
        .await?;
        tx.commit().await?;
        Ok(count)
    }

    /// Operator repair: return a dead-lettered entry to pending work with a
    /// fresh retry budget. Nothing else moves a lane past a dead letter.
    pub async fn repair_mailbox_dead_letter(
        &self,
        lane: MailboxLaneId,
        operation: MailboxOperationKey,
        target: &NodeIdentity,
    ) -> Result<bool, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let Some(locked) = MailboxRepository::claim_backlogged_lane(&mut tx, lane, target).await?
        else {
            return Ok(false);
        };
        MailboxRepository::repair_dead_letter(&mut tx, &locked, operation).await?;
        tx.commit().await?;
        Ok(true)
    }
}

/// Result of an operator-initiated repair attempt on one dead-lettered mailbox
/// entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorMailboxRepairOutcome {
    /// Entry was returned to pending work with a fresh retry budget.
    Repaired {
        previous_reason: MailboxDeadLetterReason,
    },
    /// No lane exists with the requested ID.
    LaneMissing,
    /// The entry does not exist on that lane or is no longer dead-lettered.
    NotDeadLettered,
    /// Entry was parked because its canonical parent evidence is missing or
    /// malformed; requeueing without `--force` is refused.
    RefusedCanonicalEvidenceInvalid,
}

/// List pending dead-lettered mailbox entries, oldest first, at most `limit`.
pub async fn list_mailbox_dead_letters(
    uow: &crate::ingress_uow::IngressUnitOfWork,
    limit: u32,
) -> Result<Vec<crate::ingress_uow::DeadLetteredMailboxEntry>, IngressUowError> {
    let mut tx = uow
        .begin_read_with_timeouts(Duration::from_secs(2), Duration::from_secs(5))
        .await?;
    let entries = MailboxRepository::list_dead_letters(&mut tx, limit).await?;
    tx.commit().await?;
    Ok(entries)
}

/// Operator repair of one dead-lettered mailbox entry without requiring the
/// caller to match the lane's recorded `target_owner` (which may name a
/// stopped node incarnation awaiting claim adoption).
pub async fn operator_repair_mailbox_dead_letter(
    uow: &crate::ingress_uow::IngressUnitOfWork,
    lane: MailboxLaneId,
    operation: MailboxOperationKey,
    force_invalid_evidence: bool,
) -> Result<OperatorMailboxRepairOutcome, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(Duration::from_secs(2), Duration::from_secs(5))
        .await?;
    let Some(locked) = MailboxRepository::lock_lane_for_operator(&mut tx, lane).await? else {
        return Ok(OperatorMailboxRepairOutcome::LaneMissing);
    };
    let Some(entry) = MailboxRepository::lookup(&mut tx, operation).await? else {
        return Ok(OperatorMailboxRepairOutcome::NotDeadLettered);
    };
    if entry.lane != locked.id() || entry.disposition != MailboxEntryDisposition::Pending {
        return Ok(OperatorMailboxRepairOutcome::NotDeadLettered);
    }
    let Some(reason) = entry.dead_letter else {
        return Ok(OperatorMailboxRepairOutcome::NotDeadLettered);
    };
    if reason == MailboxDeadLetterReason::CanonicalEvidenceInvalid && !force_invalid_evidence {
        return Ok(OperatorMailboxRepairOutcome::RefusedCanonicalEvidenceInvalid);
    }
    MailboxRepository::repair_dead_letter(&mut tx, &locked, operation).await?;
    tx.commit().await?;
    Ok(OperatorMailboxRepairOutcome::Repaired {
        previous_reason: reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_doubles_from_a_quarter_second_and_is_capped() {
        assert_eq!(mailbox_retry_delay(1), Duration::from_millis(250));
        assert_eq!(mailbox_retry_delay(2), Duration::from_millis(500));
        assert_eq!(mailbox_retry_delay(5), Duration::from_secs(4));
        assert_eq!(mailbox_retry_delay(8), Duration::from_secs(30));
        assert_eq!(mailbox_retry_delay(u32::MAX), Duration::from_secs(30));
    }
}
