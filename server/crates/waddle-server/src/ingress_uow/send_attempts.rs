//! Durable exclusion at the resource queue boundary. A started attempt is
//! retried only after a bounded ambiguity window; lost acknowledgements can
//! then duplicate delivery, but cannot suppress offline delivery forever.
use std::time::Duration;

use jid::FullJid;
use uuid::Uuid;
use waddle_xmpp::{ingress::MessageKey, ownership::NodeIdentity};

use super::{CanonicalMessageRepository, IngressUowError, IngressUowTransaction};
use crate::{db::DatabaseDriver, ingress::decision::EffectReceiptKey};

/// One resource within a recorded effect. The caller must authorize the resource
/// against the frozen intent before claiming; the foreign key validates only the
/// message and receipt identity, not the audience or effect kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendObligation {
    pub message: MessageKey,
    pub receipt: EffectReceiptKey,
    pub recipient: FullJid,
}

/// Capability returned only after winning the durable claim. Commit its
/// transaction before using it to start a queue attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendLease {
    obligation: SendObligation,
    owner: NodeIdentity,
    token: Uuid,
    recovered: bool,
    previously_started: bool,
}

/// Whether this obligation can begin a new queue attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendClaim {
    /// Commit the claim transaction before starting this reservation.
    Acquired(SendLease),
    /// Another unexpired reservation owns this obligation.
    Busy,
    /// An attempt started, but its outcome is unknown; this is not delivery proof.
    Ambiguous,
    /// A previous holder durably recorded enqueue success.
    Completed,
}

/// Durable state observed without minting a claim capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendAttemptStatus {
    Leased,
    Started,
    ExpiredStarted,
    ExpiredLease,
    Completed,
}

/// Bound uncertainty independently of the short pre-invocation reservation.
const STARTED_GRACE_MS: i64 = 60_000;

/// SQL shared by custody and fanout boundaries. Completion never expires;
/// either in-flight phase excludes competing sinks only until its deadline.
pub(crate) fn send_attempt_blocks_delivery(driver: DatabaseDriver) -> String {
    let clock = database_clock(driver);
    format!("(state = 2 OR expires_at_ms > {clock})")
}

pub(crate) fn database_clock(driver: DatabaseDriver) -> &'static str {
    if driver == DatabaseDriver::Postgres {
        "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint"
    } else {
        "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)"
    }
}

/// Transaction-scoped queue exclusion, independent of delivery authorization.
///
/// Like the other ingress repositories, mutations take a caller-owned unit of
/// work transaction. The caller must commit a successful `start` before touching
/// the sink and retain its socket-owner witness across database awaits.
/// The supplied node identity records the lease holder; it is not a live node
/// authority guard. Callers must separately fence node rotation or shutdown at
/// the actual sink boundary.
pub struct SendAttemptRepository;

impl SendAttemptRepository {
    /// Serialize claims on the canonical message, including the absent-row
    /// case. Expired starts may also be reclaimed: availability wins over
    /// suppressing every duplicate after an unknown outcome.
    pub async fn claim(
        tx: &mut IngressUowTransaction<'_>,
        obligation: &SendObligation,
        owner: &NodeIdentity,
        duration: Duration,
    ) -> Result<SendClaim, IngressUowError> {
        let ttl = i64::try_from(duration.as_millis())
            .ok()
            .filter(|ttl| *ttl > 0 && *ttl <= i64::MAX / 2)
            .ok_or(IngressUowError::InvalidSendLeaseDuration)?;
        if !owner.is_active() {
            return Err(IngressUowError::AuthorityStopped);
        }
        let (recovered, previously_started) = match Self::status(tx, obligation).await? {
            Some(SendAttemptStatus::Leased) => return Ok(SendClaim::Busy),
            Some(SendAttemptStatus::Started) => return Ok(SendClaim::Ambiguous),
            Some(SendAttemptStatus::Completed) => return Ok(SendClaim::Completed),
            Some(SendAttemptStatus::ExpiredStarted) => (true, true),
            Some(SendAttemptStatus::ExpiredLease) => (true, false),
            None => (false, false),
        };
        let (key, clock) = dialect(tx);
        let lease = SendLease {
            obligation: obligation.clone(),
            owner: owner.clone(),
            token: Uuid::new_v4(),
            recovered,
            previously_started,
        };
        let sql = format!("INSERT INTO ingress_send_attempts (message_key, kind, semantic_identity_hash, recipient, node_id, node_incarnation, lease_token, expires_at_ms, state, recovered) VALUES ({key}, ?, ?, ?, ?, ?, ?, {clock} + ?, 0, ?) ON CONFLICT (message_key, kind, semantic_identity_hash, recipient) DO UPDATE SET node_id = excluded.node_id, node_incarnation = excluded.node_incarnation, lease_token = excluded.lease_token, expires_at_ms = excluded.expires_at_ms, state = 0, recovered = excluded.recovered");
        tx.transaction_mut()
            .execute(
                &sql,
                crate::db_params![
                    obligation.message.to_storage().to_string(),
                    obligation.receipt.kind.to_storage(),
                    obligation.receipt.semantic_identity_hash.to_vec(),
                    obligation.recipient.to_string(),
                    owner.node_id.clone(),
                    owner.node_epoch.clone(),
                    lease.token.to_string(),
                    ttl,
                    i64::from(previously_started)
                ],
            )
            .await?;
        Ok(SendClaim::Acquired(lease))
    }

    /// Inspect exclusion without claiming. The canonical lock also interlocks
    /// this read with new detached custody allocations.
    pub(crate) async fn status(
        tx: &mut IngressUowTransaction<'_>,
        obligation: &SendObligation,
    ) -> Result<Option<SendAttemptStatus>, IngressUowError> {
        lock(tx, obligation).await?;
        let (key, clock) = dialect(tx);
        let sql = format!("SELECT state, CASE WHEN expires_at_ms <= {clock} THEN 1 ELSE 0 END, recovered FROM ingress_send_attempts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND recipient = ?");
        let mut rows = tx
            .transaction_mut()
            .query(
                &sql,
                crate::db_params![
                    obligation.message.to_storage().to_string(),
                    obligation.receipt.kind.to_storage(),
                    obligation.receipt.semantic_identity_hash.to_vec(),
                    obligation.recipient.to_string()
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let state: i64 = row.get(0)?;
        let expired: i64 = row.get(1)?;
        let recovered: i64 = row.get(2)?;
        match state {
            0 if expired == 1 && recovered == 1 => Ok(Some(SendAttemptStatus::ExpiredStarted)),
            0 if expired == 1 => Ok(Some(SendAttemptStatus::ExpiredLease)),
            0 => Ok(Some(SendAttemptStatus::Leased)),
            1 if expired == 1 => Ok(Some(SendAttemptStatus::ExpiredStarted)),
            1 => Ok(Some(SendAttemptStatus::Started)),
            2 => Ok(Some(SendAttemptStatus::Completed)),
            _ => Err(IngressUowError::InvalidStoredSendAttempt),
        }
    }

    /// Earliest active delivery deadline, measured using the database clock.
    /// Maintenance can sleep until this bound without treating the row as
    /// permanently unsupported. Expired attempts remain immediately runnable.
    pub(crate) async fn next_retry_delay(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
    ) -> Result<Option<Duration>, IngressUowError> {
        let (key, clock) = dialect(tx);
        let sql = format!("SELECT MIN(expires_at_ms - {clock}) FROM ingress_send_attempts WHERE message_key = {key} AND state IN (0, 1)");
        let mut rows = tx
            .transaction_mut()
            .query(&sql, crate::db_params![message.to_storage().to_string()])
            .await?;
        let delay: Option<i64> = rows
            .next()
            .await?
            .ok_or(IngressUowError::InvalidStoredSendAttempt)?
            .get(0)?;
        Ok(delay.map(|delay| Duration::from_millis(u64::try_from(delay).unwrap_or(0))))
    }

    /// An expired start or an expired never-started reservation. Both leave the
    /// resource without a sink owner and qualify for offline handoff.
    pub(crate) async fn has_expired_attempt(
        tx: &mut IngressUowTransaction<'_>,
        obligation: &SendObligation,
    ) -> Result<bool, IngressUowError> {
        Ok(matches!(
            Self::status(tx, obligation).await?,
            Some(SendAttemptStatus::ExpiredStarted | SendAttemptStatus::ExpiredLease)
        ))
    }

    /// Revoke an expired reservation or started token only while committing its replacement
    /// custody/settlement. This is not evidence that the old socket never sent.
    pub(crate) async fn retire_expired_attempt(
        tx: &mut IngressUowTransaction<'_>,
        obligation: &SendObligation,
    ) -> Result<(), IngressUowError> {
        lock(tx, obligation).await?;
        let (key, clock) = dialect(tx);
        let sql = format!("DELETE FROM ingress_send_attempts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND recipient = ? AND state IN (0, 1) AND expires_at_ms <= {clock}");
        tx.transaction_mut()
            .execute(
                &sql,
                crate::db_params![
                    obligation.message.to_storage().to_string(),
                    obligation.receipt.kind.to_storage(),
                    obligation.receipt.semantic_identity_hash.to_vec(),
                    obligation.recipient.to_string()
                ],
            )
            .await?;
        Ok(())
    }

    /// An SM append keeps custody even after its accepting socket disappears.
    /// Read it under the SAME canonical lock as a live claim; do not consult the
    /// in-memory SM registry from inside this transaction.
    pub(crate) async fn has_custody(
        tx: &mut IngressUowTransaction<'_>,
        obligation: &SendObligation,
    ) -> Result<bool, IngressUowError> {
        lock(tx, obligation).await?;
        let mut rows = tx.transaction_mut().query(
            "SELECT 1 FROM sm_ingress_appends WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?",
            crate::db_params![obligation.message.to_storage().to_string(), obligation.receipt.kind.to_storage(), obligation.receipt.semantic_identity_hash.to_vec(), obligation.recipient.to_string()]
        ).await?;
        Ok(rows.next().await?.is_some())
    }

    pub(crate) async fn has_resource_receipt(
        tx: &mut IngressUowTransaction<'_>,
        obligation: &SendObligation,
    ) -> Result<bool, IngressUowError> {
        let (key, _) = dialect(tx);
        let sql = format!("SELECT 1 FROM ingress_carbon_receipts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND recipient = ?");
        let mut rows = tx
            .transaction_mut()
            .query(
                &sql,
                crate::db_params![
                    obligation.message.to_storage().to_string(),
                    obligation.receipt.kind.to_storage(),
                    obligation.receipt.semantic_identity_hash.to_vec(),
                    obligation.recipient.to_string()
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    /// Commit this transition before queue invocation. A false result revokes
    /// permission to invoke, even if this process originally won the claim.
    pub async fn start(
        tx: &mut IngressUowTransaction<'_>,
        lease: &SendLease,
    ) -> Result<bool, IngressUowError> {
        let (_, clock) = dialect(tx);
        transition(
            tx,
            lease,
            &format!("UPDATE ingress_send_attempts SET state = 1, expires_at_ms = {clock} + {STARTED_GRACE_MS}"),
            &format!("state = 0 AND expires_at_ms > {clock}"),
        )
        .await
    }

    /// Record observed enqueue success while this token still owns the row.
    /// A takeover or offline handoff revokes it, including late completions.
    pub async fn complete(
        tx: &mut IngressUowTransaction<'_>,
        lease: &SendLease,
    ) -> Result<bool, IngressUowError> {
        transition(
            tx,
            lease,
            "UPDATE ingress_send_attempts SET state = 2",
            "state = 1",
        )
        .await
    }

    /// Caller must have positive evidence that this attempt never enqueued.
    /// Timeout, cancellation, and a lost reply are not such evidence.
    pub async fn release_proven_not_enqueued(
        tx: &mut IngressUowTransaction<'_>,
        lease: &SendLease,
    ) -> Result<bool, IngressUowError> {
        transition(
            tx,
            lease,
            if lease.recovered && lease.previously_started {
                // A definite failure of THIS retry does not prove the older
                // unknown attempt failed. Retain the earlier start evidence.
                "UPDATE ingress_send_attempts SET state = 1, expires_at_ms = 0"
            } else if lease.recovered {
                // Even a known uninvoked reservation must remain eligible for
                // offline handoff if its socket disappeared before the retry.
                "UPDATE ingress_send_attempts SET state = 0, expires_at_ms = 0"
            } else {
                "DELETE FROM ingress_send_attempts"
            },
            "state IN (0, 1)",
        )
        .await
    }
}

fn dialect(tx: &mut IngressUowTransaction<'_>) -> (&'static str, &'static str) {
    if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
        (
            "?::uuid",
            "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint",
        )
    } else {
        (
            "?",
            "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)",
        )
    }
}

async fn lock(
    tx: &mut IngressUowTransaction<'_>,
    obligation: &SendObligation,
) -> Result<(), IngressUowError> {
    if !CanonicalMessageRepository::lock(tx, obligation.message).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    Ok(())
}

async fn transition(
    tx: &mut IngressUowTransaction<'_>,
    lease: &SendLease,
    action: &str,
    condition: &str,
) -> Result<bool, IngressUowError> {
    lock(tx, &lease.obligation).await?;
    let (key, _) = dialect(tx);
    let sql = format!("{action} WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND recipient = ? AND node_id = ? AND node_incarnation = ? AND lease_token = ? AND {condition}");
    Ok(tx
        .transaction_mut()
        .execute(
            &sql,
            crate::db_params![
                lease.obligation.message.to_storage().to_string(),
                lease.obligation.receipt.kind.to_storage(),
                lease.obligation.receipt.semantic_identity_hash.to_vec(),
                lease.obligation.recipient.to_string(),
                lease.owner.node_id.clone(),
                lease.owner.node_epoch.clone(),
                lease.token.to_string()
            ],
        )
        .await?
        == 1)
}

#[cfg(test)]
#[path = "send_attempts_tests.rs"]
mod tests;
