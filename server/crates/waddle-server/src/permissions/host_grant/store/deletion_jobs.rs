use super::{corrupt, decode_operation, read_space_deletion_header, HostGrantStore};
use crate::db::Transaction;
use crate::ingress_substrate::set_local_transaction_timeouts;
use crate::permissions::host_grant::{
    GrantSubject, HostGrantError, ScopeKey, SpaceDeletionAttempt, SpaceDeletionAttemptOutcome,
    SpaceDeletionDeliveryKind, HOST_GRANT_POLICY_VERSION, SPACE_BOOKMARK_RESTORE_LOCK,
};

mod bindings;

const MAX_ATTEMPTS: i32 = 16;
const LEASE_MS: i64 = 30_000;
const MAX_AGE_MS: i64 = 24 * 60 * 60 * 1_000;

impl HostGrantStore {
    /// Claim due notices across replicas. Expired staging leases are ambiguous.
    pub async fn claim_space_deletion_jobs(
        &self,
        kind: SpaceDeletionDeliveryKind,
        limit: usize,
    ) -> Result<Vec<SpaceDeletionAttempt>, HostGrantError> {
        let limit = i64::try_from(limit.min(32)).expect("bounded job claim");
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut tx = self.begin_write().await?;
        sweep_unavailable(&mut tx, kind, limit).await?;
        let mut rows = tx.query(
            "WITH clock AS (
                SELECT (EXTRACT(EPOCH FROM pg_catalog.statement_timestamp()) * 1000)::bigint AS now_ms
             ), selected AS (
                SELECT j.lifecycle_id, j.subject_jid, j.delivery_kind
                  FROM host_grant_space_deletion_jobs j
                  JOIN host_grant_space_deletions d ON d.lifecycle_id = j.lifecycle_id
                  JOIN host_grant_operations o ON o.operation_id = d.operation_id
                  JOIN host_grant_scope_lifecycles l ON l.lifecycle_id = j.lifecycle_id
                  JOIN host_grant_scopes s ON s.lifecycle_id = j.lifecycle_id
                  JOIN host_grant_space_bindings b ON b.lifecycle_id = j.lifecycle_id
                  JOIN host_grant_authority a ON a.singleton AND a.incarnation = s.authority_incarnation
                  CROSS JOIN clock
                 WHERE j.delivery_kind = ? AND j.attempts < ?
                   AND ((j.state = 'pending' AND j.due_at_ms <= clock.now_ms)
                     OR (j.state = 'leased' AND j.lease_until_ms <= clock.now_ms))
                   AND o.kind = 'delete_space' AND o.lifecycle_id = j.lifecycle_id
                   AND o.recorded_at_ms >= clock.now_ms - ?
                   AND a.incarnation = b.authority_incarnation
                   AND a.incarnation = o.authority_incarnation AND l.retired_at_ms IS NULL
                   AND s.admission_state = 'active' AND s.policy_version = ?
                   AND s.projected_generation >= o.generation
                 ORDER BY j.due_at_ms, j.lifecycle_id, j.subject_jid
                 LIMIT ? FOR UPDATE OF j SKIP LOCKED
             ), claimed AS (
                UPDATE host_grant_space_deletion_jobs j
                   SET state = 'leased', attempts = j.attempts + 1,
                       attempt_token = pg_catalog.gen_random_uuid(), lease_until_ms = clock.now_ms + ?
                  FROM selected, clock
                 WHERE (j.lifecycle_id, j.subject_jid, j.delivery_kind) =
                       (selected.lifecycle_id, selected.subject_jid, selected.delivery_kind)
                 RETURNING j.*
             )
             SELECT o.operation_id, o.lifecycle_id, o.generation, o.fingerprint, o.kind,
                    o.authority_incarnation, o.projection_digest, b.service_jid, b.space_node,
                    j.subject_jid, j.attempt_token::text, j.attempts, j.lease_until_ms
               FROM claimed j
               JOIN host_grant_space_deletions d ON d.lifecycle_id = j.lifecycle_id
               JOIN host_grant_operations o ON o.operation_id = d.operation_id
               JOIN host_grant_space_bindings b ON b.lifecycle_id = j.lifecycle_id",
            crate::db_params![kind.as_storage(), MAX_ATTEMPTS, MAX_AGE_MS, HOST_GRANT_POLICY_VERSION, limit, LEASE_MS],
        ).await?;
        let mut attempts = Vec::new();
        while let Some(row) = rows.next().await? {
            let service = row
                .get::<String>(7)?
                .parse()
                .map_err(|_| corrupt("deletion service"))?;
            attempts.push(SpaceDeletionAttempt {
                key: ScopeKey::new(service, row.get::<String>(8)?)?,
                record: decode_operation(&row)?.record,
                subject: GrantSubject::from_storage(&row.get::<String>(9)?)
                    .ok_or(corrupt("deletion recipient"))?,
                kind,
                token: row
                    .get::<String>(10)?
                    .parse()
                    .map_err(|_| corrupt("deletion token"))?,
                attempt: row.get(11)?,
                lease_until_ms: row.get(12)?,
            });
        }
        drop(rows);
        tx.commit().await?;
        Ok(attempts)
    }

    /// Record that this attempt may reach transport before any staging starts.
    pub async fn begin_space_deletion_staging(
        &self,
        attempt: &SpaceDeletionAttempt,
    ) -> Result<bool, HostGrantError> {
        if attempt.kind != SpaceDeletionDeliveryKind::Remote {
            return Ok(false);
        }
        let Some(mut tx) = self.lock_space_deletion_attempt(attempt).await? else {
            return Ok(false);
        };
        let updated = tx.execute(
            "UPDATE host_grant_space_deletion_jobs SET state = 'staging'
              WHERE lifecycle_id = ? AND subject_jid = ? AND delivery_kind = ?
                AND attempt_token = ?::uuid AND state = 'leased'
                AND lease_until_ms > (EXTRACT(EPOCH FROM pg_catalog.statement_timestamp()) * 1000)::bigint",
            crate::db_params![
                attempt.record.lifecycle.to_storage(), attempt.subject.as_str(),
                attempt.kind.as_storage(), attempt.token.to_string()
            ],
        ).await?;
        tx.commit().await?;
        Ok(updated == 1)
    }

    /// Check the winning attempt under shared locks until synchronous staging.
    /// Release the returned guard before any peer wait or flush.
    pub async fn hold_space_deletion_staging(
        &self,
        attempt: &SpaceDeletionAttempt,
    ) -> Result<Option<Transaction<'_>>, HostGrantError> {
        if attempt.kind != SpaceDeletionDeliveryKind::Remote {
            return Ok(None);
        }
        let Some(mut tx) = self.lock_space_deletion_attempt(attempt).await? else {
            return Ok(None);
        };
        let mut rows = tx
            .query(
                "SELECT lease_until_ms FROM host_grant_space_deletion_jobs
              WHERE lifecycle_id = ? AND subject_jid = ? AND delivery_kind = ?
                AND attempt_token = ?::uuid AND state = 'staging'
              FOR SHARE",
                crate::db_params![
                    attempt.record.lifecycle.to_storage(),
                    attempt.subject.as_str(),
                    attempt.kind.as_storage(),
                    attempt.token.to_string()
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let lease_until_ms = row.get::<i64>(0)?;
        drop(rows);
        // Compare the deadline after locking the row. A competing expiry
        // or settlement may have delayed that lock.
        let mut clock = tx
            .query(
                "SELECT (EXTRACT(EPOCH FROM pg_catalog.clock_timestamp()) * 1000)::bigint",
                (),
            )
            .await?;
        let live = clock
            .next()
            .await?
            .ok_or(corrupt("deletion lease clock"))?
            .get::<i64>(0)?
            < lease_until_ms;
        drop(clock);
        Ok(live.then_some(tx))
    }

    /// Settle only this unexpired attempt. Retry requires proof that no
    /// transfer occurred, even when the staging state was entered.
    pub async fn finish_space_deletion_attempt(
        &self,
        attempt: &SpaceDeletionAttempt,
        outcome: SpaceDeletionAttemptOutcome,
    ) -> Result<bool, HostGrantError> {
        let (state, can_be_leased) = match outcome {
            SpaceDeletionAttemptOutcome::Retry => ("pending", true),
            SpaceDeletionAttemptOutcome::Custody => ("custody", false),
            SpaceDeletionAttemptOutcome::Ambiguous => ("ambiguous", false),
            SpaceDeletionAttemptOutcome::Refused => ("refused", true),
        };
        let mut tx = self.begin_write().await?;
        let updated = tx.execute(
            "WITH decision AS (
                 SELECT ?::text AS next, (EXTRACT(EPOCH FROM pg_catalog.statement_timestamp()) * 1000)::bigint AS now_ms
             )
             UPDATE host_grant_space_deletion_jobs j SET
                 state = CASE WHEN decision.next = 'pending' AND j.attempts >= ?
                              THEN 'refused' ELSE decision.next END,
                 due_at_ms = CASE WHEN decision.next = 'pending'
                     THEN decision.now_ms + LEAST(300000, 1000 * (1 << LEAST(j.attempts - 1, 8)))
                     ELSE j.due_at_ms END,
                 attempt_token = NULL, lease_until_ms = NULL
              FROM decision
             WHERE lifecycle_id = ? AND subject_jid = ? AND delivery_kind = ?
               AND attempt_token = ?::uuid AND lease_until_ms > decision.now_ms
               AND (state = 'staging' OR (state = 'leased' AND ?::bigint <> 0))
               AND (delivery_kind = 'remote' OR (NOT local_snapshot_frozen AND decision.next IN ('pending', 'refused')))",
            crate::db_params![
                state, MAX_ATTEMPTS, attempt.record.lifecycle.to_storage(), attempt.subject.as_str(),
                attempt.kind.as_storage(), attempt.token.to_string(), can_be_leased
            ],
        ).await?;
        tx.commit().await?;
        Ok(updated == 1)
    }

    async fn lock_space_deletion_attempt(
        &self,
        attempt: &SpaceDeletionAttempt,
    ) -> Result<Option<Transaction<'_>>, HostGrantError> {
        let mut tx = self.database().begin().await?;
        if !set_local_transaction_timeouts(&mut tx, super::LOCK_TIMEOUT, super::STATEMENT_TIMEOUT)
            .await?
        {
            return Err(HostGrantError::BoundsUnproven);
        }
        tx.execute(
            "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)",
            crate::db_params![SPACE_BOOKMARK_RESTORE_LOCK],
        )
        .await?;
        let Some((record, _, _)) =
            read_space_deletion_header(&mut tx, &attempt.key, attempt.record.operation, true)
                .await?
        else {
            return Ok(None);
        };
        Ok((record == attempt.record).then_some(tx))
    }
}

async fn sweep_unavailable(
    tx: &mut Transaction<'_>,
    kind: SpaceDeletionDeliveryKind,
    limit: i64,
) -> Result<(), HostGrantError> {
    tx.execute(
        "WITH clock AS (
             SELECT (EXTRACT(EPOCH FROM pg_catalog.statement_timestamp()) * 1000)::bigint AS now_ms
         ), selected AS (
             SELECT j.lifecycle_id, j.subject_jid, j.delivery_kind
               FROM host_grant_space_deletion_jobs j
               JOIN host_grant_space_deletions d ON d.lifecycle_id = j.lifecycle_id
               JOIN host_grant_operations o ON o.operation_id = d.operation_id
               JOIN host_grant_scope_lifecycles l ON l.lifecycle_id = j.lifecycle_id
               JOIN host_grant_authority a ON a.singleton
               CROSS JOIN clock
              WHERE j.delivery_kind = ?
                AND ((j.state = 'staging' AND j.lease_until_ms <= clock.now_ms)
                  OR ((j.state = 'pending'
                      OR (j.state = 'leased' AND j.lease_until_ms <= clock.now_ms))
                     AND (j.attempts >= ? OR o.recorded_at_ms < clock.now_ms - ?
                         OR o.authority_incarnation <> a.incarnation OR l.retired_at_ms IS NOT NULL)))
              ORDER BY j.due_at_ms, j.lifecycle_id, j.subject_jid
              LIMIT ? FOR UPDATE OF j SKIP LOCKED
         ), terminal_children AS (
             UPDATE host_grant_space_deletion_bindings b SET
                    state = CASE WHEN b.state = 'staging' THEN 'ambiguous' ELSE 'refused' END,
                    parent_token = NULL, attempt_token = NULL
               FROM selected
              WHERE b.lifecycle_id = selected.lifecycle_id AND b.subject_jid = selected.subject_jid
                AND selected.delivery_kind = 'local' AND b.state IN ('pending', 'leased', 'staging')
         )
         UPDATE host_grant_space_deletion_jobs j
            SET state = CASE
                  WHEN j.state = 'staging' THEN 'ambiguous'
                  WHEN j.delivery_kind = 'local' AND EXISTS (
                       SELECT 1 FROM host_grant_space_deletion_bindings b
                        WHERE b.lifecycle_id = j.lifecycle_id AND b.subject_jid = j.subject_jid
                          AND b.state = 'custody') THEN 'custody'
                  WHEN j.delivery_kind = 'local' AND EXISTS (
                       SELECT 1 FROM host_grant_space_deletion_bindings b
                        WHERE b.lifecycle_id = j.lifecycle_id AND b.subject_jid = j.subject_jid
                          AND b.state IN ('ambiguous', 'staging')) THEN 'ambiguous'
                  ELSE 'refused' END,
                attempt_token = NULL, lease_until_ms = NULL
           FROM selected
          WHERE (j.lifecycle_id, j.subject_jid, j.delivery_kind) =
                (selected.lifecycle_id, selected.subject_jid, selected.delivery_kind)",
        crate::db_params![kind.as_storage(), MAX_ATTEMPTS, MAX_AGE_MS, limit],
    ).await?;
    Ok(())
}
