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
    MailboxRoomLane,
};
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
    /// The entry was decided or the lane changed hands meanwhile.
    Settled,
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
        let key = entry.operation.message_key();
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

    /// Count a failed attempt at `operation` and defer the next one, or park
    /// the entry for an operator once [`MAILBOX_ATTEMPT_LIMIT`] is reached.
    /// The lane's cursor never moves here.
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
