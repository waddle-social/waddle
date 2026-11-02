//! Quiescent-writer laboratory only; this is not a production GC authority.
//!
//! A detached stream can carry an already-authorized frame in memory and write
//! its SM ledger entry after the canonical row has gone. The canonical lock
//! cannot fence that writer. These experiments require all custody writers to
//! be quiescent; the type does not attest or enforce that prerequisite.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgConnectOptions;
use sqlx::Row;

use super::{
    prepare_postgres, quote_identifier, read_sqlite_epoch, Backend, DarkIngressAuthority,
    DarkIngressAuthorityError, Database, DatabaseDriver, PhysicalAttestationConfig,
    TransactionBounds,
};
use crate::ingress_substrate::ALIAS_RETENTION;

const MAX_BATCH: u16 = 256;

/// A test-only retention experiment. Fixture history, including any existing
/// frontier, must be controlled, and custody writers must remain stopped from
/// adoption through deletion. This prerequisite is not established by
/// physical attestation and is not enforced by this type.
pub struct RetentionLab {
    authority: DarkIngressAuthority,
}

/// Counts returned only after the whole batch's commit succeeds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RetentionLabBatch {
    pub examined: u64,
    pub adopted: u64,
    pub deleted: u64,
}

impl RetentionLab {
    /// Create the laboratory over the private, physically attested PG pool.
    pub fn postgres(
        options: PgConnectOptions,
        expected: PhysicalAttestationConfig,
        pool_size: u32,
        bounds: TransactionBounds,
    ) -> Result<Self, DarkIngressAuthorityError> {
        Ok(Self {
            authority: DarkIngressAuthority::postgres(options, expected, pool_size, bounds)?,
        })
    }

    /// Use the exact SQLite pool of the supplied runtime handle, not a newly
    /// opened private memory database or a separately interpreted URL.
    pub fn sqlite(
        runtime: &Database,
        bounds: TransactionBounds,
    ) -> Result<Self, DarkIngressAuthorityError> {
        if runtime.driver() != DatabaseDriver::Sqlite {
            return Err(DarkIngressAuthorityError::Database);
        }
        Ok(Self {
            authority: DarkIngressAuthority {
                backend: Backend::Sqlite(runtime.clone()),
                bounds,
            },
        })
    }

    /// Examine at most `limit` candidates under one transaction. `now` is
    /// supplied by the laboratory harness; legacy rows are adopted at `now`,
    /// never at their terminal timestamp. An error, including an indeterminate
    /// commit, carries no deletion count.
    pub async fn collect(
        &self,
        now: DateTime<Utc>,
        limit: u16,
    ) -> Result<RetentionLabBatch, DarkIngressAuthorityError> {
        if !(1..=MAX_BATCH).contains(&limit) {
            return Err(DarkIngressAuthorityError::InvalidBounds);
        }
        let cutoff = now
            .checked_sub_signed(ALIAS_RETENTION)
            .ok_or(DarkIngressAuthorityError::InvalidBounds)?;
        match &self.authority.backend {
            Backend::Postgres {
                pool,
                schema,
                schema_oid,
            } => {
                let mut connection = self.authority.acquire_postgres(pool).await?;
                let mut prepared = tokio::time::timeout(
                    self.authority.bounds.acquisition,
                    prepare_postgres(&mut connection, schema, *schema_oid, self.authority.bounds),
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Timeout)??;
                let result =
                    collect_postgres(&mut prepared.transaction, schema, now, cutoff, limit).await?;
                prepared.commit().await?;
                Ok(result)
            }
            Backend::Sqlite(db) => {
                let mut tx = self.authority.begin_sqlite(db).await?;
                read_sqlite_epoch(&mut tx).await?;
                let result = collect_sqlite(&mut tx, now, cutoff, limit).await?;
                tx.commit()
                    .await
                    .map_err(|_| DarkIngressAuthorityError::CommitIndeterminate)?;
                Ok(result)
            }
        }
    }
}

// Both custody tables are deliberately mandatory. A missing table is an
// error even for an empty batch; a same-named table on search_path is never
// substituted for the expected schema (or SQLite's main database).
fn custody_predicate(schema: &str, message_as_text: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM {schema}.ingress_sm_refs r WHERE r.message_key = m.message_key) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.settled_at IS NULL) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_effect_intents i WHERE i.message_key = m.message_key AND NOT EXISTS \
             (SELECT 1 FROM {schema}.ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash)) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_archive_dispatch d JOIN {schema}.pending_delivery p ON p.row_id = d.pending_row_id \
             WHERE d.message_key = m.message_key AND d.pending_row_id <> '') \
         AND NOT EXISTS (SELECT 1 FROM {schema}.sm_ingress_appends a WHERE a.message_key = {message_as_text} AND a.disposition = 0) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_admission_pins pin WHERE pin.message_key = m.message_key)"
    )
}

async fn collect_postgres(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    schema: &str,
    now: DateTime<Utc>,
    cutoff: DateTime<Utc>,
    limit: u16,
) -> Result<RetentionLabBatch, DarkIngressAuthorityError> {
    let schema = quote_identifier(schema);
    let custody = custody_predicate(&schema, "m.message_key::text");
    let candidates = format!(
        "SELECT m.message_key, NOT ({custody}) FROM {schema}.ingress_messages m \
         WHERE m.terminal_at IS NOT NULL AND \
         (({custody} AND (m.retention_eligible_at IS NULL OR m.retention_eligible_at <= $1)) \
          OR (NOT ({custody}) AND m.retention_eligible_at IS NOT NULL)) \
         ORDER BY m.retention_eligible_at NULLS FIRST, m.message_key LIMIT $2 FOR UPDATE OF m SKIP LOCKED"
    );
    let keys: Vec<(uuid::Uuid, bool)> = sqlx::query_as(&candidates)
        .bind(cutoff)
        .bind(i64::from(limit))
        .fetch_all(&mut **tx)
        .await
        .map_err(super::postgres_error)?;
    let mut result = RetentionLabBatch::default();
    for (key, observed_custody) in keys {
        result.examined += 1;
        // READ COMMITTED gives this statement a new snapshot after the row lock.
        let recheck = format!(
            "SELECT m.retention_eligible_at, (m.terminal_at IS NOT NULL AND {custody}) AS ready \
             FROM {schema}.ingress_messages m WHERE m.message_key = $1"
        );
        let row = sqlx::query(&recheck)
            .bind(key)
            .fetch_one(&mut **tx)
            .await
            .map_err(super::postgres_error)?;
        let eligible: Option<DateTime<Utc>> = row
            .try_get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        let ready: bool = row
            .try_get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        if !ready {
            sqlx::query(&format!("UPDATE {schema}.ingress_messages SET retention_eligible_at = NULL WHERE message_key = $1"))
                .bind(key).execute(&mut **tx).await.map_err(super::postgres_error)?;
        } else if eligible.is_none() || observed_custody {
            // A blocker disappearing after selection invalidates the old tail.
            sqlx::query(&format!("UPDATE {schema}.ingress_messages SET retention_eligible_at = $1 WHERE message_key = $2"))
                .bind(now).bind(key).execute(&mut **tx).await.map_err(super::postgres_error)?;
            result.adopted += 1;
        } else if eligible.is_some_and(|at| at <= cutoff) {
            for table in ["ingress_origin_aliases", "ingress_deliveries"] {
                sqlx::query(&format!(
                    "DELETE FROM {schema}.{table} WHERE message_key = $1"
                ))
                .bind(key)
                .execute(&mut **tx)
                .await
                .map_err(super::postgres_error)?;
            }
            let deleted = sqlx::query(&format!(
                "DELETE FROM {schema}.ingress_messages WHERE message_key = $1"
            ))
            .bind(key)
            .execute(&mut **tx)
            .await
            .map_err(super::postgres_error)?
            .rows_affected();
            if deleted != 1 {
                return Err(DarkIngressAuthorityError::Database);
            }
            sqlx::query(&format!("DELETE FROM {schema}.sm_ingress_appends WHERE message_key = $1 AND disposition <> 0"))
                .bind(key.to_string()).execute(&mut **tx).await.map_err(super::postgres_error)?;
            result.deleted += 1;
        }
    }
    Ok(result)
}

async fn collect_sqlite(
    tx: &mut crate::db::Transaction<'_>,
    now: DateTime<Utc>,
    cutoff: DateTime<Utc>,
    limit: u16,
) -> Result<RetentionLabBatch, DarkIngressAuthorityError> {
    let schema = "main";
    let custody = custody_predicate(schema, "m.message_key");
    let candidates = format!(
        "SELECT m.message_key, NOT ({custody}) FROM {schema}.ingress_messages m \
         WHERE m.terminal_at IS NOT NULL AND \
         (({custody} AND (m.retention_eligible_at IS NULL OR julianday(m.retention_eligible_at) <= julianday(?))) \
          OR (NOT ({custody}) AND m.retention_eligible_at IS NOT NULL)) \
         ORDER BY m.retention_eligible_at NULLS FIRST, m.message_key LIMIT ?"
    );
    let mut rows = tx
        .query(
            &candidates,
            crate::db_params![cutoff.to_rfc3339(), i64::from(limit)],
        )
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?;
    let mut keys = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| DarkIngressAuthorityError::Database)?
    {
        let key = row
            .get::<String>(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        let observed_custody = row
            .get::<bool>(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        keys.push((key, observed_custody));
    }
    drop(rows);
    let mut result = RetentionLabBatch::default();
    for (key, observed_custody) in keys {
        result.examined += 1;
        let recheck = format!(
            "SELECT m.retention_eligible_at, (m.terminal_at IS NOT NULL AND {custody}) \
             FROM {schema}.ingress_messages m WHERE m.message_key = ?"
        );
        let mut rows = tx
            .query(&recheck, crate::db_params![key.clone()])
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        let row = rows
            .next()
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?
            .ok_or(DarkIngressAuthorityError::Database)?;
        let eligible: Option<String> = row
            .get(0)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        let ready: bool = row
            .get(1)
            .map_err(|_| DarkIngressAuthorityError::Database)?;
        drop(rows);
        if !ready {
            tx.execute("UPDATE main.ingress_messages SET retention_eligible_at = NULL WHERE message_key = ?", crate::db_params![key]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
        } else if observed_custody || eligible.is_none() {
            tx.execute(
                "UPDATE main.ingress_messages SET retention_eligible_at = ? WHERE message_key = ?",
                crate::db_params![now.to_rfc3339(), key],
            )
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?;
            result.adopted += 1;
        } else if let Some(eligible) = eligible {
            let at = DateTime::parse_from_rfc3339(&eligible)
                .map_err(|_| DarkIngressAuthorityError::Database)?
                .with_timezone(&Utc);
            if at <= cutoff {
                for table in ["ingress_origin_aliases", "ingress_deliveries"] {
                    tx.execute(
                        &format!("DELETE FROM main.{table} WHERE message_key = ?"),
                        crate::db_params![key.clone()],
                    )
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?;
                }
                let deleted = tx
                    .execute(
                        "DELETE FROM main.ingress_messages WHERE message_key = ?",
                        crate::db_params![key.clone()],
                    )
                    .await
                    .map_err(|_| DarkIngressAuthorityError::Database)?;
                if deleted != 1 {
                    return Err(DarkIngressAuthorityError::Database);
                }
                tx.execute("DELETE FROM main.sm_ingress_appends WHERE message_key = ? AND disposition <> 0", crate::db_params![key]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                result.deleted += 1;
            }
        }
    }
    Ok(result)
}
