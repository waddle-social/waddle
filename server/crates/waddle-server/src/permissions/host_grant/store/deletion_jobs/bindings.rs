use std::collections::HashSet;

use jid::FullJid;
use uuid::Uuid;
use waddle_xmpp::ownership::BindingRef;

use super::{corrupt, read_space_deletion_header, HostGrantStore, MAX_ATTEMPTS};
use crate::account_coordinator::AccountCoordinatorConfig;
use crate::db::Transaction;
use crate::permissions::host_grant::{
    GrantOperationId, GrantSubject, HostGrantError, ScopeKey, SpaceDeletionAttempt,
    SpaceDeletionAttemptOutcome, SpaceDeletionBinding, SpaceDeletionBindingAttempt,
    SpaceDeletionDeliveryKind,
};

const MAX_BINDING_BYTES: usize = 4096;

impl HostGrantStore {
    /// Determine whether the initial exact socket selection was saved.
    pub async fn local_space_deletion_bindings_frozen(
        &self,
        parent: &SpaceDeletionAttempt,
    ) -> Result<bool, HostGrantError> {
        let Some((tx, frozen)) = self.lock_local_deletion_parent(parent, false).await? else {
            return Ok(false);
        };
        tx.rollback().await?;
        Ok(frozen)
    }

    /// Freeze the first authoritative selection under the live parent lease.
    /// A later attempt may advance these targets but cannot add new sessions.
    pub async fn freeze_local_space_deletion_bindings(
        &self,
        parent: &SpaceDeletionAttempt,
        targets: &[SpaceDeletionBinding],
    ) -> Result<bool, HostGrantError> {
        let cap = AccountCoordinatorConfig::default().max_resources.get();
        let mut resources = HashSet::new();
        if targets.len() > cap
            || targets.iter().any(|target| {
                &target.resource.to_bare() != parent.subject.jid()
                    || !resources.insert(&target.resource)
            })
        {
            return Err(corrupt("local deletion binding snapshot"));
        }
        let Some((mut tx, frozen)) = self.lock_local_deletion_parent(parent, true).await? else {
            return Ok(false);
        };
        if !frozen {
            let lifecycle = parent.record.lifecycle.to_storage();
            for target in targets {
                tx.execute(
                    "INSERT INTO host_grant_space_deletion_bindings
                         (lifecycle_id, subject_jid, resource_jid, binding) VALUES (?, ?, ?, ?)",
                    crate::db_params![
                        lifecycle.as_str(),
                        parent.subject.as_str(),
                        target.resource.as_str(),
                        encode_binding(&target.binding)?
                    ],
                )
                .await?;
            }
            tx.execute(
                "UPDATE host_grant_space_deletion_jobs SET local_snapshot_frozen = true
                  WHERE lifecycle_id = ? AND subject_jid = ? AND delivery_kind = 'local'",
                crate::db_params![lifecycle.as_str(), parent.subject.as_str()],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Lease unresolved destinations, leaving previous custody terminal.
    pub async fn claim_local_space_deletion_bindings(
        &self,
        parent: &SpaceDeletionAttempt,
        limit: usize,
    ) -> Result<Vec<SpaceDeletionBindingAttempt>, HostGrantError> {
        let limit =
            i64::try_from(limit.min(AccountCoordinatorConfig::default().max_resources.get()))
                .expect("bounded local deletion binding claim");
        let Some((mut tx, true)) = self.lock_local_deletion_parent(parent, true).await? else {
            return Ok(Vec::new());
        };
        recover_bindings(&mut tx, parent).await?;
        let lifecycle = parent.record.lifecycle.to_storage();
        let mut rows = tx.query(
            "WITH due AS (
                 SELECT resource_jid FROM host_grant_space_deletion_bindings
                  WHERE lifecycle_id = ? AND subject_jid = ? AND state = 'pending'
                  ORDER BY resource_jid LIMIT ?
             )
             UPDATE host_grant_space_deletion_bindings b
                SET state = 'leased', parent_token = ?::uuid, attempt_token = pg_catalog.gen_random_uuid()
               FROM due
              WHERE b.lifecycle_id = ? AND b.subject_jid = ? AND b.resource_jid = due.resource_jid
              RETURNING b.resource_jid, b.binding, b.attempt_token::text",
            crate::db_params![
                lifecycle.as_str(), parent.subject.as_str(), limit, parent.token.to_string(),
                lifecycle.as_str(), parent.subject.as_str()
            ],
        ).await?;
        let mut attempts = Vec::new();
        while let Some(row) = rows.next().await? {
            let raw = row.get::<String>(0)?;
            let resource: FullJid = raw.parse().map_err(|_| corrupt("deletion resource"))?;
            if raw != resource.as_str() || &resource.to_bare() != parent.subject.jid() {
                return Err(corrupt("deletion resource account"));
            }
            attempts.push(SpaceDeletionBindingAttempt {
                parent: parent.clone(),
                target: SpaceDeletionBinding {
                    resource,
                    binding: decode_binding(&row.get::<String>(1)?)?,
                },
                token: row
                    .get::<String>(2)?
                    .parse()
                    .map_err(|_| corrupt("deletion binding token"))?,
            });
        }
        drop(rows);
        tx.commit().await?;
        Ok(attempts)
    }

    /// Recover the child lease from its exact socket node before staging.
    pub async fn load_local_space_deletion_binding_attempt(
        &self,
        key: &ScopeKey,
        operation: GrantOperationId,
        resource: &FullJid,
        binding: &BindingRef,
        token: Uuid,
    ) -> Result<Option<SpaceDeletionBindingAttempt>, HostGrantError> {
        let subject =
            GrantSubject::new(resource.to_bare()).map_err(|_| corrupt("deletion recipient"))?;
        let mut tx = self.begin_interactive_read().await?;
        let Some((record, _, _)) =
            read_space_deletion_header(&mut tx, key, operation, false).await?
        else {
            return Ok(None);
        };
        let mut rows = tx.query(
            "SELECT j.attempt_token::text, j.attempts, j.lease_until_ms
               FROM host_grant_space_deletion_bindings b
               JOIN host_grant_space_deletion_jobs j USING (lifecycle_id, subject_jid, delivery_kind)
              WHERE b.lifecycle_id = ? AND b.subject_jid = ? AND b.resource_jid = ? AND b.binding = ?
                AND b.attempt_token = ?::uuid AND b.state = 'leased'
                AND j.state = 'leased' AND j.attempt_token = b.parent_token
                AND j.lease_until_ms > (EXTRACT(EPOCH FROM pg_catalog.clock_timestamp()) * 1000)::bigint",
            crate::db_params![
                record.lifecycle.to_storage(), subject.as_str(), resource.as_str(), encode_binding(binding)?,
                token.to_string()
            ],
        ).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let result = SpaceDeletionBindingAttempt {
            parent: SpaceDeletionAttempt {
                key: key.clone(),
                record,
                subject,
                kind: SpaceDeletionDeliveryKind::Local,
                token: row
                    .get::<String>(0)?
                    .parse()
                    .map_err(|_| corrupt("deletion token"))?,
                attempt: row.get(1)?,
                lease_until_ms: row.get(2)?,
            },
            target: SpaceDeletionBinding {
                resource: resource.clone(),
                binding: binding.clone(),
            },
            token,
        };
        drop(rows);
        tx.rollback().await?;
        Ok(Some(result))
    }

    /// Transition one resource to staging before it can take transport custody.
    pub async fn begin_local_space_deletion_binding_staging(
        &self,
        child: &SpaceDeletionBindingAttempt,
    ) -> Result<bool, HostGrantError> {
        self.advance_local_deletion_binding(child, "staging", true)
            .await
    }

    /// Hold the exact absolute parent lease and the staged child through transfer.
    pub async fn hold_local_space_deletion_binding_staging(
        &self,
        child: &SpaceDeletionBindingAttempt,
    ) -> Result<Option<Transaction<'_>>, HostGrantError> {
        let Some((mut tx, true)) = self
            .lock_local_deletion_parent(&child.parent, false)
            .await?
        else {
            return Ok(None);
        };
        let mut rows = tx
            .query(
                "SELECT 1 FROM host_grant_space_deletion_bindings
              WHERE lifecycle_id = ? AND subject_jid = ? AND resource_jid = ? AND binding = ?
                AND parent_token = ?::uuid AND attempt_token = ?::uuid AND state = 'staging'
              FOR SHARE",
                binding_params(child)?,
            )
            .await?;
        let found = rows.next().await?.is_some();
        drop(rows);
        Ok(found.then_some(tx))
    }

    /// Settle the exact child. Retry requires proof that no transfer occurred.
    pub async fn finish_local_space_deletion_binding_attempt(
        &self,
        child: &SpaceDeletionBindingAttempt,
        outcome: SpaceDeletionAttemptOutcome,
    ) -> Result<bool, HostGrantError> {
        let (state, can_be_leased) = match outcome {
            SpaceDeletionAttemptOutcome::Retry => ("pending", true),
            SpaceDeletionAttemptOutcome::Custody => ("custody", false),
            SpaceDeletionAttemptOutcome::Ambiguous => ("ambiguous", false),
            SpaceDeletionAttemptOutcome::Refused => ("refused", true),
        };
        self.advance_local_deletion_binding(child, state, can_be_leased)
            .await
    }

    async fn advance_local_deletion_binding(
        &self,
        child: &SpaceDeletionBindingAttempt,
        state: &str,
        can_be_leased: bool,
    ) -> Result<bool, HostGrantError> {
        let Some((mut tx, true)) = self
            .lock_local_deletion_parent(&child.parent, false)
            .await?
        else {
            return Ok(false);
        };
        let mut params = crate::db_params![state, state, state];
        params.extend(binding_params(child)?);
        params.extend(crate::db_params![state, i64::from(can_be_leased)]);
        let count = tx
            .execute(
                "UPDATE host_grant_space_deletion_bindings SET state = ?::text,
                 parent_token = CASE WHEN ?::text = 'staging' THEN parent_token ELSE NULL END,
                 attempt_token = CASE WHEN ?::text = 'staging' THEN attempt_token ELSE NULL END
              WHERE lifecycle_id = ? AND subject_jid = ? AND resource_jid = ? AND binding = ?
                AND parent_token = ?::uuid AND attempt_token = ?::uuid
                AND ((state = 'staging' AND ?::text <> 'staging')
                    OR (state = 'leased' AND ?::bigint <> 0))",
                params,
            )
            .await?;
        tx.commit().await?;
        Ok(count == 1)
    }

    /// Settle the subscriber from its children. An unobserved live child
    /// keeps the lease; proved-unsent pending children get a backed-off retry.
    pub async fn settle_local_space_deletion_parent(
        &self,
        parent: &SpaceDeletionAttempt,
    ) -> Result<bool, HostGrantError> {
        let Some((mut tx, true)) = self.lock_local_deletion_parent(parent, true).await? else {
            return Ok(false);
        };
        recover_bindings(&mut tx, parent).await?;
        let lifecycle = parent.record.lifecycle.to_storage();
        let changed = tx
            .execute(
                "WITH summary AS (
                SELECT count(*) FILTER (WHERE state IN ('leased', 'staging')) AS in_flight,
                       count(*) FILTER (WHERE state = 'pending') AS pending,
                       count(*) FILTER (WHERE state = 'custody') AS accepted,
                       count(*) FILTER (WHERE state = 'ambiguous') AS unknown
                  FROM host_grant_space_deletion_bindings WHERE lifecycle_id = ? AND subject_jid = ?
             )
             UPDATE host_grant_space_deletion_jobs j
                SET state = CASE
                    WHEN summary.pending > 0 AND j.attempts < ? THEN 'pending'
                    WHEN summary.accepted > 0 THEN 'custody'
                    WHEN summary.unknown > 0 THEN 'ambiguous'
                    ELSE 'refused' END,
                    due_at_ms = (EXTRACT(EPOCH FROM pg_catalog.clock_timestamp()) * 1000)::bigint
                       + 1000 * (1 << LEAST(j.attempts - 1, 8)),
                    attempt_token = NULL, lease_until_ms = NULL
               FROM summary
              WHERE j.lifecycle_id = ? AND j.subject_jid = ? AND j.delivery_kind = 'local'
                AND summary.in_flight = 0",
                crate::db_params![
                    lifecycle.as_str(),
                    parent.subject.as_str(),
                    MAX_ATTEMPTS,
                    lifecycle.as_str(),
                    parent.subject.as_str()
                ],
            )
            .await?;
        if parent.attempt >= MAX_ATTEMPTS {
            tx.execute(
                "UPDATE host_grant_space_deletion_bindings SET state = 'refused'
                  WHERE lifecycle_id = ? AND subject_jid = ? AND state = 'pending'
                    AND EXISTS (SELECT 1 FROM host_grant_space_deletion_jobs j
                         WHERE j.lifecycle_id = ? AND j.subject_jid = ? AND j.delivery_kind = 'local'
                           AND j.state IN ('custody', 'ambiguous', 'refused'))",
                crate::db_params![lifecycle.as_str(), parent.subject.as_str(),
                    lifecycle.as_str(), parent.subject.as_str()],
            ).await?;
        }
        tx.commit().await?;
        Ok(changed == 1)
    }

    async fn lock_local_deletion_parent(
        &self,
        parent: &SpaceDeletionAttempt,
        exclusive: bool,
    ) -> Result<Option<(Transaction<'_>, bool)>, HostGrantError> {
        if parent.kind != SpaceDeletionDeliveryKind::Local {
            return Ok(None);
        }
        let Some(mut tx) = self.lock_space_deletion_attempt(parent).await? else {
            return Ok(None);
        };
        let lock = if exclusive { "FOR UPDATE" } else { "FOR SHARE" };
        let mut rows = tx.query(
            &format!("SELECT local_snapshot_frozen, lease_until_ms FROM host_grant_space_deletion_jobs
                 WHERE lifecycle_id = ? AND subject_jid = ? AND delivery_kind = 'local'
                   AND attempt_token = ?::uuid AND state = 'leased' {lock}"),
            crate::db_params![
                parent.record.lifecycle.to_storage(), parent.subject.as_str(), parent.token.to_string()
            ],
        ).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let frozen = row.get(0)?;
        let lease_until_ms: i64 = row.get(1)?;
        drop(rows);
        let mut clock = tx
            .query(
                "SELECT (EXTRACT(EPOCH FROM pg_catalog.clock_timestamp()) * 1000)::bigint",
                (),
            )
            .await?;
        let now = clock
            .next()
            .await?
            .ok_or(corrupt("deletion lease clock"))?
            .get::<i64>(0)?;
        drop(clock);
        Ok((now < lease_until_ms).then_some((tx, frozen)))
    }
}

/// Active claims all inherit the parent expiry. A parent replacement
/// establishes that the previous children can no longer begin transfer.
async fn recover_bindings(
    tx: &mut Transaction<'_>,
    parent: &SpaceDeletionAttempt,
) -> Result<(), HostGrantError> {
    tx.execute(
        "UPDATE host_grant_space_deletion_bindings
            SET state = CASE WHEN state = 'staging' THEN 'ambiguous' ELSE 'pending' END,
                parent_token = NULL, attempt_token = NULL
          WHERE lifecycle_id = ? AND subject_jid = ? AND parent_token <> ?::uuid
            AND state IN ('leased', 'staging')",
        crate::db_params![
            parent.record.lifecycle.to_storage(),
            parent.subject.as_str(),
            parent.token.to_string()
        ],
    )
    .await?;
    Ok(())
}

fn binding_params(
    child: &SpaceDeletionBindingAttempt,
) -> Result<Vec<crate::db::Value>, HostGrantError> {
    Ok(crate::db_params![
        child.parent.record.lifecycle.to_storage(),
        child.parent.subject.as_str(),
        child.target.resource.as_str(),
        encode_binding(&child.target.binding)?,
        child.parent.token.to_string(),
        child.token.to_string()
    ])
}

fn encode_binding(binding: &BindingRef) -> Result<String, HostGrantError> {
    let encoded =
        serde_json::to_string(binding).map_err(|_| corrupt("deletion binding identity"))?;
    if encoded.len() > MAX_BINDING_BYTES {
        return Err(corrupt("deletion binding size"));
    }
    Ok(encoded)
}

fn decode_binding(encoded: &str) -> Result<BindingRef, HostGrantError> {
    let binding: BindingRef =
        serde_json::from_str(encoded).map_err(|_| corrupt("deletion binding identity"))?;
    if encode_binding(&binding)? != encoded {
        return Err(corrupt("noncanonical deletion binding identity"));
    }
    Ok(binding)
}
