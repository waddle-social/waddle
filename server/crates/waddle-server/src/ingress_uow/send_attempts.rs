//! Durable exclusion at the resource queue boundary. A started attempt is
//! deliberately never stolen: expiry alone cannot prove whether enqueue ran.
use std::time::Duration;

use jid::FullJid;
use uuid::Uuid;
use waddle_xmpp::{ingress::MessageKey, ownership::NodeIdentity};

use super::{CanonicalMessageRepository, IngressUowError, IngressUowTransaction};
use crate::{db::DatabaseDriver, ingress::decision::EffectReceiptKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendObligation {
    pub message: MessageKey,
    pub receipt: EffectReceiptKey,
    pub recipient: FullJid,
}

/// Capability returned only after winning the durable claim. Commit its
/// transaction before using it to start a queue attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendLease {
    obligation: SendObligation,
    owner: NodeIdentity,
    token: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendClaim {
    Acquired(SendLease),
    Busy,
    Ambiguous,
    Completed,
}

pub(crate) struct SendAttemptRepository;

impl SendAttemptRepository {
    /// Serialize claims on the canonical message, including the absent-row
    /// case. Only an expired, not-yet-started lease can be reclaimed.
    pub(crate) async fn claim(
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
        lock(tx, obligation).await?;
        let (key, clock) = dialect(tx);
        let sql = format!("SELECT state, CASE WHEN expires_at_ms <= {clock} THEN 1 ELSE 0 END FROM ingress_send_attempts WHERE message_key = {key} AND kind = ? AND semantic_identity_hash = ? AND recipient = ?");
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
        if let Some(row) = rows.next().await? {
            let state: i64 = row.get(0)?;
            let expired: i64 = row.get(1)?;
            match state {
                0 if expired == 1 => {}
                0 => return Ok(SendClaim::Busy),
                1 => return Ok(SendClaim::Ambiguous),
                2 => return Ok(SendClaim::Completed),
                _ => return Err(IngressUowError::InvalidStoredSendAttempt),
            }
        }
        drop(rows);
        let lease = SendLease {
            obligation: obligation.clone(),
            owner: owner.clone(),
            token: Uuid::new_v4(),
        };
        let sql = format!("INSERT INTO ingress_send_attempts (message_key, kind, semantic_identity_hash, recipient, node_id, node_incarnation, lease_token, expires_at_ms, state) VALUES ({key}, ?, ?, ?, ?, ?, ?, {clock} + ?, 0) ON CONFLICT (message_key, kind, semantic_identity_hash, recipient) DO UPDATE SET node_id = excluded.node_id, node_incarnation = excluded.node_incarnation, lease_token = excluded.lease_token, expires_at_ms = excluded.expires_at_ms, state = 0");
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
                    ttl
                ],
            )
            .await?;
        Ok(SendClaim::Acquired(lease))
    }

    /// Commit this transition before queue invocation. A false result revokes
    /// permission to invoke, even if this process originally won the claim.
    pub(crate) async fn start(
        tx: &mut IngressUowTransaction<'_>,
        lease: &SendLease,
    ) -> Result<bool, IngressUowError> {
        let (_, clock) = dialect(tx);
        transition(
            tx,
            lease,
            "UPDATE ingress_send_attempts SET state = 1",
            &format!("state = 0 AND expires_at_ms > {clock}"),
        )
        .await
    }

    /// Record observed enqueue success. Expiry after start does not revoke
    /// the token, since a started attempt can never be stolen.
    pub(crate) async fn complete(
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
    pub(crate) async fn release_proven_not_enqueued(
        tx: &mut IngressUowTransaction<'_>,
        lease: &SendLease,
    ) -> Result<bool, IngressUowError> {
        transition(
            tx,
            lease,
            "DELETE FROM ingress_send_attempts",
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
