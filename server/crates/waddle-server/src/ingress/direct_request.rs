//! Direct requests through the durable mailbox: a stanza for a user whose
//! account another node owns.
//!
//! The origin commits the request and its pending mailbox entry in one
//! ingress transaction and waits for the account owner's decision. The
//! account owner takes custody of the stanza through the recipient's
//! session path and records its decision in one transaction that asserts
//! the account's `UserActor` claim, latches the decision and moves the
//! lane's cursor. The origin reads the decision in the transaction that
//! consumes the entry and settles the request.
//!
//! A hint, a relay reply or a timeout proves nothing: the only evidence is
//! the entry's disposition checked against the request's decision. An
//! origin that committed a request and did not read a decision reports the
//! delivery as uncertain and never sends the stanza another way.

use std::time::Duration;

use jid::{BareJid, Jid};
use waddle_xmpp::ingress::mailbox::{
    DirectRequestKey, MailboxAdmissionLimits, MailboxAdmissionRefusal, MailboxEntryDisposition,
    MailboxLaneFence, MailboxLaneIdentity, MailboxLaneOrigin, MailboxLaneRecipient,
    MailboxOperationKey,
};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, NodeIdentity};

use super::IngressAuthority;
use crate::clustering::ordered_relay::OrderedRelayPayload;
use crate::ingress_uow::{
    ClaimRepository, DirectDecision, DirectRequestError, DirectRequestRepository, IngressUowError,
    IngressUowTransaction, MailboxError, MailboxLaneId, MailboxRepository, MailboxTargetFence,
    NewDirectRequest, StoredDirectRequest,
};

/// Pending-entry caps for direct requests. A lane is one origin's stanzas
/// to one recipient, so its backlog is small unless the account owner has
/// stalled.
pub(crate) const DIRECT_REQUEST_LIMITS: MailboxAdmissionLimits = MailboxAdmissionLimits {
    lane_pending: 256,
    node_pending: 16_384,
};

/// A stanza for a user as its origin submits it.
#[derive(Debug, Clone)]
pub struct DirectSubmission {
    pub key: DirectRequestKey,
    /// The recipient, bare or full; also the lane's recipient.
    pub recipient: Jid,
    pub origin: MailboxLaneOrigin,
    /// The origin's claim epoch, as the relay channel carries it.
    pub origin_epoch: ClaimEpoch,
    pub sender_entity: Entity,
    pub payload: OrderedRelayPayload,
    pub message_key: Option<MessageKey>,
}

/// A committed request whose decision the origin waits for.
#[derive(Debug, Clone)]
pub struct PendingDirectRequest {
    pub key: DirectRequestKey,
    pub operation: MailboxOperationKey,
    pub lane: MailboxLaneId,
    pub origin_epoch: ClaimEpoch,
    /// Account owner incarnation the entry was appended for.
    pub target_owner: NodeIdentity,
}

/// Result of the origin's commit.
#[derive(Debug, Clone)]
pub enum DirectCommit {
    /// The mailbox does not carry this stanza: it is not selected, the
    /// protocol epoch is below the mailbox epoch, or the account has no
    /// claim. Nothing was written and the caller keeps the relay.
    NotCarried,
    /// The lane or the account owner's node is at its pending cap. Nothing
    /// was written.
    Refused(MailboxAdmissionRefusal),
    Committed(PendingDirectRequest),
}

/// Why a committed request's decision was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DirectUnresolved {
    #[error("the mailbox entry is gone")]
    EntryMissing,
    #[error("the mailbox entry is dead-lettered and awaits operator repair")]
    DeadLettered,
    #[error("the entry's decision and the request's decision disagree")]
    EvidenceMismatch,
    #[error("the origin's claim was superseded")]
    OriginSuperseded,
    #[error("no decision was recorded within the budget")]
    BudgetExhausted,
    #[error("the ingress authority stopped")]
    AuthorityStopped,
}

enum DirectPoll {
    Pending,
    Decided(DirectDecision),
    Unresolved(DirectUnresolved),
}

/// A head entry that carries a direct request, prepared for the account
/// owner.
#[derive(Debug, Clone)]
pub struct ClaimedDirectRequest {
    pub lane: MailboxLaneId,
    pub operation: MailboxOperationKey,
    pub request: StoredDirectRequest,
}

fn lane_recipient(recipient: &Jid) -> MailboxLaneRecipient {
    match recipient.clone().try_into_full() {
        Ok(full) => MailboxLaneRecipient::FullJid(full),
        Err(bare) => MailboxLaneRecipient::BareJid(bare),
    }
}

/// One authoritative read of a pending request. A decided entry is checked
/// against the request's decision under the lane and request locks, then
/// consumed and settled in this transaction.
async fn resolve(
    authority: &IngressAuthority,
    pending: &PendingDirectRequest,
) -> Result<DirectPoll, IngressUowError> {
    let mut tx = authority
        .uow
        .begin_with_timeouts(Duration::from_millis(250), Duration::from_millis(500))
        .await?;
    let Some(entry) = MailboxRepository::lookup(&mut tx, pending.operation).await? else {
        return Ok(DirectPoll::Unresolved(DirectUnresolved::EntryMissing));
    };
    let consumed = match entry.disposition {
        MailboxEntryDisposition::Pending if entry.dead_letter.is_some() => {
            return Ok(DirectPoll::Unresolved(DirectUnresolved::DeadLettered));
        }
        MailboxEntryDisposition::Pending => return Ok(DirectPoll::Pending),
        MailboxEntryDisposition::OwnerAccepted | MailboxEntryDisposition::OwnerRejected => false,
        MailboxEntryDisposition::Consumed(_) => true,
    };
    if consumed {
        // An earlier read of this request already consumed the entry; the
        // request still records what was decided.
        let recorded = DirectRequestRepository::read(&mut tx, pending.key)
            .await?
            .and_then(|request| request.decision);
        return Ok(match recorded {
            Some(decision) => DirectPoll::Decided(decision),
            None => DirectPoll::Unresolved(DirectUnresolved::EvidenceMismatch),
        });
    }
    let lane =
        match MailboxRepository::lock_lane_for_origin(&mut tx, entry.lane, pending.origin_epoch)
            .await
        {
            Ok(lane) => lane,
            Err(IngressUowError::Mailbox(MailboxError::StaleFence(_))) => {
                return Ok(DirectPoll::Unresolved(DirectUnresolved::OriginSuperseded));
            }
            Err(error) => return Err(error),
        };
    let Some(request) = DirectRequestRepository::lock(&mut tx, pending.key).await? else {
        return Ok(DirectPoll::Unresolved(DirectUnresolved::EntryMissing));
    };
    let Some(decision) = request.decision else {
        return Ok(DirectPoll::Unresolved(DirectUnresolved::EvidenceMismatch));
    };
    let agrees = matches!(
        (entry.disposition, decision.mailbox()),
        (
            MailboxEntryDisposition::OwnerAccepted,
            waddle_xmpp::ingress::mailbox::MailboxOwnerDecision::Accepted
        ) | (
            MailboxEntryDisposition::OwnerRejected,
            waddle_xmpp::ingress::mailbox::MailboxOwnerDecision::Rejected
        )
    );
    if !agrees {
        return Ok(DirectPoll::Unresolved(DirectUnresolved::EvidenceMismatch));
    }
    MailboxRepository::consume(&mut tx, &lane, pending.operation).await?;
    DirectRequestRepository::settle(&mut tx, pending.key).await?;
    tx.commit().await?;
    Ok(DirectPoll::Decided(decision))
}

impl IngressAuthority {
    /// Commit a direct request and its pending mailbox entry as the origin.
    ///
    /// One ingress transaction reads the recipient account's `UserActor`
    /// claim, locks the lane, checks admission, inserts the request and
    /// appends the entry. A retry with the same key finds the stored
    /// request and its entry and writes nothing. After the commit the
    /// account owner is hinted; its poller finds the entry without the
    /// hint.
    pub async fn commit_direct_request(
        &self,
        submission: DirectSubmission,
    ) -> Result<DirectCommit, IngressUowError> {
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        if !self.uow.mailbox_acceptance_active(&tx) {
            return Ok(DirectCommit::NotCarried);
        }
        let account = submission.recipient.to_bare();
        let Some((target_owner, target_epoch)) =
            ClaimRepository::user_claim(&mut tx, &account).await?
        else {
            return Ok(DirectCommit::NotCarried);
        };
        let identity = MailboxLaneIdentity {
            origin: submission.origin,
            recipient: lane_recipient(&submission.recipient),
        };
        let fence = MailboxLaneFence {
            origin_epoch: submission.origin_epoch,
            target_epoch,
            target_owner: target_owner.clone(),
        };
        let operation = MailboxOperationKey::Direct(submission.key);
        let mut lane = MailboxRepository::lock_lane(&mut tx, &identity, &fence).await?;
        // A retry of an attempt that already has its entry is never refused
        // for a backlog it is itself part of.
        if MailboxRepository::lookup(&mut tx, operation)
            .await?
            .is_none()
        {
            let load = MailboxRepository::pending_load(&mut tx, &lane).await?;
            if let Err(refusal) = DIRECT_REQUEST_LIMITS.admit(load) {
                return Ok(DirectCommit::Refused(refusal));
            }
        }
        DirectRequestRepository::insert(
            &mut tx,
            &NewDirectRequest {
                key: submission.key,
                recipient: submission.recipient,
                sender_entity: submission.sender_entity,
                payload: submission.payload,
                message_key: submission.message_key,
            },
        )
        .await?;
        let appended =
            MailboxRepository::append(&mut tx, &mut lane, operation, &DIRECT_REQUEST_LIMITS)
                .await?;
        let lane = appended.entry().lane;
        tx.commit().await?;
        if let Some(hints) = self.mailbox_hints.get() {
            hints.hint(target_owner.clone());
        }
        Ok(DirectCommit::Committed(PendingDirectRequest {
            key: submission.key,
            operation,
            lane,
            origin_epoch: submission.origin_epoch,
            target_owner,
        }))
    }

    /// Look for the account owner's decision on `pending` until it is read
    /// and consumed, proven impossible, `budget` ends or the authority
    /// stops. A storage failure is retried: it is not evidence either way.
    pub async fn await_direct_request(
        &self,
        pending: &PendingDirectRequest,
        budget: Duration,
    ) -> Result<DirectDecision, DirectUnresolved> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut interval = Duration::from_millis(25);
        loop {
            if self.cancellation.is_cancelled() {
                return Err(DirectUnresolved::AuthorityStopped);
            }
            match resolve(self, pending).await {
                Ok(DirectPoll::Decided(decision)) => return Ok(decision),
                Ok(DirectPoll::Unresolved(reason)) => return Err(reason),
                Ok(DirectPoll::Pending) | Err(_) => {}
            }
            if tokio::time::Instant::now() + interval > deadline {
                return Err(DirectUnresolved::BudgetExhausted);
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(Duration::from_millis(500));
        }
    }

    /// Load the request a lane head carries, for the account owner's
    /// worker. `Ok(None)` when the stored request cannot be used, which the
    /// worker parks for an operator.
    pub(super) async fn load_claimed_direct(
        tx: &mut IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        operation: MailboxOperationKey,
    ) -> Result<Option<ClaimedDirectRequest>, IngressUowError> {
        let Some(key) = operation.direct() else {
            return Ok(None);
        };
        let request = match DirectRequestRepository::read(tx, key).await {
            Ok(Some(request)) => request,
            Ok(None) | Err(IngressUowError::DirectRequest(DirectRequestError::InvalidStored)) => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        Ok(Some(ClaimedDirectRequest {
            lane,
            operation,
            request,
        }))
    }

    /// Decide a direct request as the owner of the recipient's account, in
    /// one transaction.
    ///
    /// The transaction reads the account's `UserActor` claim, requires it
    /// to be held by `owner` and asserts it, locks the lane as its target,
    /// locks the request, latches the decision and moves the lane's cursor
    /// past the entry. A request that already records a decision keeps it:
    /// the replay only moves the entry. The recorded decision is returned.
    pub async fn decide_direct_request(
        &self,
        claimed: &ClaimedDirectRequest,
        owner: &NodeIdentity,
        decision: DirectDecision,
    ) -> Result<DirectDecision, IngressUowError> {
        let account: BareJid = claimed.request.recipient.to_bare();
        let mut tx = self
            .uow
            .begin_with_timeouts(Duration::from_millis(250), Duration::from_secs(1))
            .await?;
        let Some((claim_owner, claim_epoch)) =
            ClaimRepository::user_claim(&mut tx, &account).await?
        else {
            return Err(IngressUowError::ClaimFenceMissing);
        };
        if claim_owner != *owner {
            return Err(IngressUowError::ClaimFenceMissing);
        }
        ClaimRepository::assert_user_claim(&mut tx, &account, owner, claim_epoch).await?;
        let mut lane = MailboxRepository::lock_lane_for_target(
            &mut tx,
            claimed.lane,
            &MailboxTargetFence {
                target_epoch: claim_epoch,
                target_owner: claim_owner,
            },
        )
        .await?;
        let stored = DirectRequestRepository::lock(&mut tx, claimed.request.key)
            .await?
            .ok_or(DirectRequestError::RequestMissing)?;
        let recorded = match stored.decision {
            Some(recorded) => recorded,
            None => {
                DirectRequestRepository::latch_decision(&mut tx, claimed.request.key, decision)
                    .await?
            }
        };
        MailboxRepository::decide(&mut tx, &mut lane, claimed.operation, recorded.mailbox())
            .await?;
        tx.commit().await?;
        Ok(recorded)
    }
}
