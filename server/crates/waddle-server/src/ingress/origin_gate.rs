//! The origin's gate on its XEP-0198 handled count for a groupchat stanza
//! whose room is owned by another node.
//!
//! The origin's commit leaves such a stanza out of the handled count (see
//! [`PendingRoomAcceptance`]). This module decides when it may join it. The
//! only proof is the owner's durable decision, read from the database: the
//! mailbox entry's disposition, checked against `owner_decision` on the
//! canonical message and, for a refusal, the rejection child. A relay reply,
//! a relay failure, a hint or a timeout proves nothing, so the same lookup
//! serves a lost reply and an indeterminate owner commit.
//!
//! [`watch_room_acceptance`] is the hook a connection calls after such a
//! commit. It settles the stanza's reserved inbound slot exactly once: with a
//! completion when the owner's decision was read and consumed, or by
//! withholding it when no decision can be proven within the budget.

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use waddle_xmpp::ingress::mailbox::{
    MailboxEntryDisposition, MailboxOperationKey, MailboxOwnerDecision,
};
#[cfg(feature = "clustering")]
use waddle_xmpp::telemetry::attributes::RemoteDispatchOutcome;

use super::decision::PendingRoomAcceptance;
use super::IngressAuthority;
use crate::ingress_uow::{
    CanonicalMessageRepository, IngressUnitOfWork, IngressUowError, IngressUowTransaction,
    LockedMailboxLane, MailboxRepository, RoomOwnerDecision,
};

/// Why a pending acceptance can never be proven by this origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RoomAcceptanceUnresolved {
    #[error("the mailbox entry is gone")]
    EntryMissing,
    #[error("the mailbox entry is dead-lettered and awaits operator repair")]
    DeadLettered,
    #[error("the entry's decision and the canonical owner decision disagree")]
    EvidenceMismatch,
    #[error("the origin's stream claim was superseded")]
    OriginSuperseded,
    #[error("no owner decision was recorded within the budget")]
    BudgetExhausted,
    #[error("the ingress authority stopped")]
    AuthorityStopped,
}

/// One authoritative read of a pending acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomAcceptancePoll {
    /// The owner has not decided.
    Pending,
    /// The owner's decision was read, verified and consumed.
    Decided(MailboxOwnerDecision),
    Unresolved(RoomAcceptanceUnresolved),
}

/// How long and how often the origin looks for the owner's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomAcceptanceWatch {
    pub budget: Duration,
    pub first_interval: Duration,
    pub max_interval: Duration,
}

impl Default for RoomAcceptanceWatch {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(60),
            first_interval: Duration::from_millis(25),
            max_interval: Duration::from_millis(500),
        }
    }
}

/// Read the owner's decision for `pending` and, when one is recorded,
/// consume the entry in the same transaction.
///
/// The entry is read first without a lock. A decided entry is then verified
/// under the lane and canonical locks: `owner_decision` must record the same
/// decision, and a refusal must have its rejection child. Only then is the
/// entry consumed. Any other combination is reported as unresolved, never as
/// a decision.
pub async fn resolve_room_acceptance(
    uow: &IngressUnitOfWork,
    pending: &PendingRoomAcceptance,
) -> Result<RoomAcceptancePoll, IngressUowError> {
    use crate::ingress_uow::MailboxError;
    let mut tx = uow
        .begin_with_timeouts(Duration::from_millis(250), Duration::from_millis(500))
        .await?;
    let Some(entry) = MailboxRepository::lookup(&mut tx, pending.operation).await? else {
        return Ok(RoomAcceptancePoll::Unresolved(
            RoomAcceptanceUnresolved::EntryMissing,
        ));
    };
    let decision = match entry.disposition {
        MailboxEntryDisposition::Pending if entry.dead_letter.is_some() => {
            return Ok(RoomAcceptancePoll::Unresolved(
                RoomAcceptanceUnresolved::DeadLettered,
            ));
        }
        MailboxEntryDisposition::Pending => return Ok(RoomAcceptancePoll::Pending),
        MailboxEntryDisposition::OwnerAccepted => MailboxOwnerDecision::Accepted,
        MailboxEntryDisposition::OwnerRejected => MailboxOwnerDecision::Rejected,
        MailboxEntryDisposition::Consumed(decision) => decision,
    };
    let lane =
        match MailboxRepository::lock_lane_for_origin(&mut tx, entry.lane, pending.origin_epoch)
            .await
        {
            Ok(lane) => lane,
            Err(IngressUowError::Mailbox(MailboxError::StaleFence(_))) => {
                return Ok(RoomAcceptancePoll::Unresolved(
                    RoomAcceptanceUnresolved::OriginSuperseded,
                ));
            }
            Err(error) => return Err(error),
        };
    if let Some(reason) =
        consume_proven_decision(&mut tx, &lane, pending.operation, decision).await?
    {
        return Ok(RoomAcceptancePoll::Unresolved(reason));
    }
    tx.commit().await?;
    Ok(RoomAcceptancePoll::Decided(decision))
}

/// Consume a decided entry once the canonical message proves the same
/// decision. The caller holds the entry's lane; this locks the canonical
/// message, requires `owner_decision` to record `decision` (and a refusal to
/// have its rejection child, an acceptance to have none) and only then marks
/// the entry consumed. `Some` names why the decision is not proven; nothing
/// is written in that case.
pub(super) async fn consume_proven_decision(
    tx: &mut IngressUowTransaction<'_>,
    lane: &LockedMailboxLane,
    operation: MailboxOperationKey,
    decision: MailboxOwnerDecision,
) -> Result<Option<RoomAcceptanceUnresolved>, IngressUowError> {
    let key = operation.message_key();
    if !CanonicalMessageRepository::lock(tx, key).await? {
        return Ok(Some(RoomAcceptanceUnresolved::EntryMissing));
    }
    let recorded = CanonicalMessageRepository::room_owner_state(tx, key)
        .await?
        .and_then(|state| state.decision);
    let rejection_child = CanonicalMessageRepository::rejection_child(tx, key).await?;
    let proven = match (decision, recorded) {
        (MailboxOwnerDecision::Accepted, Some(RoomOwnerDecision::Accepted)) => {
            rejection_child.is_none()
        }
        (MailboxOwnerDecision::Rejected, Some(RoomOwnerDecision::Rejected)) => {
            rejection_child.is_some()
        }
        _ => false,
    };
    if !proven {
        return Ok(Some(RoomAcceptanceUnresolved::EvidenceMismatch));
    }
    MailboxRepository::consume(tx, lane, operation).await?;
    Ok(None)
}

/// Look for the owner's decision until it is read, proven impossible, or the
/// budget ends. A storage failure is retried: it is not evidence either way.
pub async fn await_room_acceptance(
    uow: &IngressUnitOfWork,
    pending: &PendingRoomAcceptance,
    watch: RoomAcceptanceWatch,
    stopped: impl Fn() -> bool,
) -> Result<MailboxOwnerDecision, RoomAcceptanceUnresolved> {
    let deadline = tokio::time::Instant::now() + watch.budget;
    let mut interval = watch.first_interval;
    loop {
        if stopped() {
            return Err(RoomAcceptanceUnresolved::AuthorityStopped);
        }
        match resolve_room_acceptance(uow, pending).await {
            Ok(RoomAcceptancePoll::Decided(decision)) => return Ok(decision),
            Ok(RoomAcceptancePoll::Unresolved(reason)) => return Err(reason),
            Ok(RoomAcceptancePoll::Pending) | Err(_) => {}
        }
        if tokio::time::Instant::now() + interval > deadline {
            return Err(RoomAcceptanceUnresolved::BudgetExhausted);
        }
        tokio::time::sleep(interval).await;
        interval = (interval * 2).min(watch.max_interval);
    }
}

/// The waits for a remote room owner that this node's streams have open,
/// by lane. A lane is one sender session's traffic to one room, so each
/// sample describes one stream: how many of its stanzas await the owner and
/// how long the oldest has waited.
#[cfg(feature = "clustering")]
#[derive(Debug, Default)]
pub struct PendingAcceptances {
    lanes: std::sync::Mutex<
        std::collections::HashMap<crate::ingress_uow::MailboxLaneId, Vec<tokio::time::Instant>>,
    >,
}

#[cfg(feature = "clustering")]
impl PendingAcceptances {
    /// Record that a wait on `lane` began at `started` and sample the lane.
    fn opened(&self, lane: crate::ingress_uow::MailboxLaneId, started: tokio::time::Instant) {
        self.update(lane, |waits| waits.push(started));
    }

    /// Record that the wait on `lane` that began at `started` ended and
    /// sample the lane.
    fn closed(&self, lane: crate::ingress_uow::MailboxLaneId, started: tokio::time::Instant) {
        self.update(lane, |waits| {
            if let Some(index) = waits.iter().position(|wait| *wait == started) {
                waits.swap_remove(index);
            }
        });
    }

    fn update(
        &self,
        lane: crate::ingress_uow::MailboxLaneId,
        change: impl FnOnce(&mut Vec<tokio::time::Instant>),
    ) {
        let (pending, oldest) = {
            let mut lanes = self
                .lanes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let waits = lanes.entry(lane).or_default();
            change(waits);
            let sample = (
                waits.len(),
                waits
                    .iter()
                    .min()
                    .map_or(Duration::ZERO, tokio::time::Instant::elapsed),
            );
            if waits.is_empty() {
                lanes.remove(&lane);
            }
            sample
        };
        waddle_xmpp::telemetry::authority::record_pending_acceptance(pending, oldest);
    }
}

/// Where an origin sends its wake-up after appending a mailbox entry. A hint
/// is best effort and proves nothing; the target's poller is the authority.
pub trait MailboxHintSink: Send + Sync {
    fn hint(&self, target: waddle_xmpp::ownership::NodeIdentity);
}

/// The reserved inbound slot of a stanza that awaits its remote room owner,
/// with the set its connection's tracker consults for withheld slots.
#[cfg(feature = "clustering")]
pub struct InboundAcceptanceSlot {
    pub handoff: crate::server::routes::interpret::OrderedRelayHandoffHandle,
    pub withheld: crate::server::routes::interpret::WithheldInbound,
}

impl IngressAuthority {
    /// Bind where this node's origin commits send their mailbox hints. Bound
    /// once, when clustering wires its relay; without it no hint is sent and
    /// the target's poller still finds the entry.
    pub fn bind_mailbox_hints(&self, hints: Arc<dyn MailboxHintSink>) {
        if self.mailbox_hints.set(hints).is_err() {
            tracing::error!("mailbox hint sink bound more than once; keeping the first");
        }
    }
}

/// Removes one wait from [`PendingAcceptances`] when the wait ends, also
/// when its future is dropped.
#[cfg(feature = "clustering")]
struct OpenAcceptanceWait<'a> {
    waits: &'a PendingAcceptances,
    lane: crate::ingress_uow::MailboxLaneId,
    started: tokio::time::Instant,
}

#[cfg(feature = "clustering")]
impl Drop for OpenAcceptanceWait<'_> {
    fn drop(&mut self) {
        self.waits.closed(self.lane, self.started);
    }
}

#[cfg(feature = "clustering")]
impl IngressAuthority {
    /// Hint the room owner and look for its decision on `pending` until it
    /// is read and consumed, proven impossible, the budget ends, the
    /// authority stops or `abandoned` reports that nobody waits any more.
    async fn read_room_acceptance(
        &self,
        pending: &PendingRoomAcceptance,
        abandoned: impl Fn() -> bool,
    ) -> Result<MailboxOwnerDecision, RoomAcceptanceUnresolved> {
        if let Some(hints) = self.mailbox_hints.get() {
            hints.hint(pending.target_owner.clone());
        }
        let started = tokio::time::Instant::now();
        self.pending_acceptances.opened(pending.lane, started);
        let wait = OpenAcceptanceWait {
            waits: &self.pending_acceptances,
            lane: pending.lane,
            started,
        };
        let outcome =
            await_room_acceptance(&self.uow, pending, RoomAcceptanceWatch::default(), || {
                self.cancellation.is_cancelled() || abandoned()
            })
            .await;
        drop(wait);
        waddle_xmpp::telemetry::reliability::increment_mailbox_room_acceptance(match outcome {
            Ok(MailboxOwnerDecision::Accepted) => {
                waddle_xmpp::telemetry::attributes::MailboxAcceptanceOutcome::Accepted
            }
            Ok(MailboxOwnerDecision::Rejected) => {
                waddle_xmpp::telemetry::attributes::MailboxAcceptanceOutcome::Rejected
            }
            Err(_) => waddle_xmpp::telemetry::attributes::MailboxAcceptanceOutcome::Unresolved,
        });
        waddle_xmpp::telemetry::authority::record_remote_dispatch(match outcome {
            Ok(MailboxOwnerDecision::Accepted) => RemoteDispatchOutcome::Accepted,
            Ok(MailboxOwnerDecision::Rejected) => RemoteDispatchOutcome::Rejected,
            Err(RoomAcceptanceUnresolved::BudgetExhausted) => RemoteDispatchOutcome::TimedOut,
            Err(_) => RemoteDispatchOutcome::Uncertain,
        });
        if let Err(reason) = outcome {
            tracing::warn!(%reason, "remote room acceptance unresolved; stanza stays unacknowledged");
        }
        outcome
    }

    /// Wait, inside the caller's own handler, for the owner's decision on a
    /// stanza whose connection could not reserve a deferred slot for it.
    ///
    /// A connection holds a bounded number of deferred slots. When none is
    /// free, or its completion queue is closed, the committed stanza still
    /// must not be counted before its owner decides, so the handler waits
    /// here and the connection reads no further frame meanwhile. `Ok` means
    /// the decision was read and consumed and the stanza may be counted;
    /// `Err` means it must be left as an unhandled hole. The entry is
    /// durable either way: a dropped wait loses nothing, and the stanza is
    /// then simply never settled on this connection.
    pub async fn await_room_acceptance_inline(
        &self,
        pending: &PendingRoomAcceptance,
    ) -> Result<MailboxOwnerDecision, RoomAcceptanceUnresolved> {
        self.read_room_acceptance(pending, || false).await
    }

    /// Watch a stanza that awaits its remote room owner and settle it.
    ///
    /// The connection calls this once, right after a commit whose decision
    /// carries a [`PendingRoomAcceptance`]. For a stream with stream
    /// management it passes the stanza's reserved inbound slot and leaves
    /// that slot open. The spawned task completes the slot when the owner's
    /// decision has been read and consumed, which lets the handled count
    /// advance past the stanza. If no decision can be proven it withholds
    /// the slot instead: the stanza is never acknowledged on this connection
    /// and the sender keeps responsibility for it. A stream without stream
    /// management has no slot; the task then only consumes the decision.
    pub fn watch_room_acceptance(
        self: &Arc<Self>,
        pending: PendingRoomAcceptance,
        slot: Option<InboundAcceptanceSlot>,
    ) {
        let authority = Arc::clone(self);
        tokio::spawn(async move {
            let outcome = authority
                .read_room_acceptance(&pending, || {
                    slot.as_ref().is_some_and(|slot| slot.handoff.is_closed())
                })
                .await;
            if let Some(slot) = slot {
                match outcome {
                    Ok(_) => slot.handoff.complete(Vec::new()),
                    Err(_) => slot.handoff.withhold(&slot.withheld),
                }
            }
        });
    }
}
