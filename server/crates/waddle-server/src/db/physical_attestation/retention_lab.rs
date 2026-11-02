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

// Every referenced table is mandatory and schema-qualified: search_path must
// not substitute a same-named table in a different schema.
fn custody_predicate(schema: &str, alias: &str, message_as_text: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM {schema}.ingress_sm_refs r WHERE r.message_key = {alias}.message_key) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_effect_descendants d WHERE d.message_key = {alias}.message_key AND d.settled_at IS NULL) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_effect_intents i WHERE i.message_key = {alias}.message_key AND NOT EXISTS \
             (SELECT 1 FROM {schema}.ingress_effect_receipts r WHERE r.message_key = i.message_key AND r.kind = i.kind AND r.semantic_identity_hash = i.semantic_identity_hash)) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_archive_dispatch d JOIN {schema}.pending_delivery p ON p.row_id = d.pending_row_id \
             WHERE d.message_key = {alias}.message_key AND d.pending_row_id <> '') \
         AND NOT EXISTS (SELECT 1 FROM {schema}.sm_ingress_appends a WHERE a.message_key = {message_as_text} AND a.disposition = 0) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_owner_rejections rejection WHERE rejection.source_message_key = {alias}.message_key \
             AND ({alias}.owner_decision IS NULL OR {alias}.owner_decision <> 2)) \
         AND ({alias}.owner_decision IS NULL OR {alias}.owner_decision <> 2 OR EXISTS \
             (SELECT 1 FROM {schema}.ingress_owner_rejections rejection WHERE rejection.source_message_key = {alias}.message_key)) \
         AND NOT ({alias}.source_binding_version IS NOT NULL AND ({alias}.owner_decision IS NULL OR {alias}.owner_decision = 0) AND EXISTS \
             (SELECT 1 FROM {schema}.ingress_effect_intents owner_intent WHERE owner_intent.message_key = {alias}.message_key AND owner_intent.kind = 12)) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_admission_pins pin WHERE pin.message_key = {alias}.message_key) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.direct_admission_lab admission WHERE admission.message_key = {alias}.message_key)"
    )
}

/// Exclude mapped children and sources with a blocked child before applying the
/// batch limit. Each admitted source consumes one candidate even if it deletes
/// two messages. `cutoff` is a backend-specific reusable SQL parameter.
fn pair_candidate_predicate(schema: &str, child_custody: &str, child_expired: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM {schema}.ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key) \
         AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR EXISTS \
           (SELECT 1 FROM {schema}.ingress_owner_rejections rejection WHERE rejection.source_message_key = m.message_key)) \
         AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_owner_rejections pair WHERE pair.source_message_key = m.message_key \
           AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR NOT EXISTS \
             (SELECT 1 FROM {schema}.ingress_messages child WHERE child.message_key = pair.child_message_key \
               AND child.terminal_at IS NOT NULL AND (child.retention_eligible_at IS NULL OR {child_expired}) \
               AND (child.owner_decision IS NULL OR child.owner_decision <> 2) \
               AND NOT EXISTS (SELECT 1 FROM {schema}.ingress_owner_rejections nested WHERE nested.source_message_key = child.message_key) \
               AND {child_custody})))"
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
    let custody = custody_predicate(&schema, "m", "m.message_key::text");
    let child_custody = custody_predicate(&schema, "child", "child.message_key::text");
    let pair_predicate =
        pair_candidate_predicate(&schema, &child_custody, "child.retention_eligible_at <= $1");
    let candidates = format!(
        "SELECT m.message_key, NOT ({custody}) FROM {schema}.ingress_messages m \
         WHERE m.terminal_at IS NOT NULL AND {pair_predicate} AND \
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
        let pair: (Option<uuid::Uuid>, Option<i32>, bool) = sqlx::query_as(&format!(
            "SELECT pair.child_message_key, m.owner_decision::int, EXISTS \
             (SELECT 1 FROM {schema}.ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key) \
             FROM {schema}.ingress_messages m LEFT JOIN {schema}.ingress_owner_rejections pair ON pair.source_message_key = m.message_key WHERE m.message_key = $1"
        )).bind(key).fetch_one(&mut **tx).await.map_err(super::postgres_error)?;
        let child = match pair {
            (_, _, true) | (None, Some(2), _) => continue,
            (Some(child), Some(2), false) => Some(child),
            (Some(_), _, _) => continue,
            (None, _, false) => None,
        };
        if let Some(child) = child {
            let locked: Option<uuid::Uuid> = sqlx::query_scalar(&format!(
                "SELECT message_key FROM {schema}.ingress_messages WHERE message_key = $1 FOR UPDATE SKIP LOCKED"
            )).bind(child).fetch_optional(&mut **tx).await.map_err(super::postgres_error)?;
            if locked.is_none() {
                continue;
            }
        }
        let mut collect = true;
        for member in [Some(key), child].into_iter().flatten() {
            // Child is never allowed to be a source of another rejection.
            let child_guard = if Some(member) == child {
                format!("AND (m.owner_decision IS NULL OR m.owner_decision <> 2) AND NOT EXISTS \
                    (SELECT 1 FROM {schema}.ingress_owner_rejections nested WHERE nested.source_message_key = m.message_key)")
            } else {
                String::new()
            };
            let row = sqlx::query(&format!(
                "SELECT m.retention_eligible_at, (m.terminal_at IS NOT NULL AND {custody} {child_guard}) FROM {schema}.ingress_messages m WHERE m.message_key = $1"
            )).bind(member).fetch_one(&mut **tx).await.map_err(super::postgres_error)?;
            let eligible: Option<DateTime<Utc>> = row
                .try_get(0)
                .map_err(|_| DarkIngressAuthorityError::Database)?;
            let ready: bool = row
                .try_get(1)
                .map_err(|_| DarkIngressAuthorityError::Database)?;
            if !ready {
                sqlx::query(&format!("UPDATE {schema}.ingress_messages SET retention_eligible_at = NULL WHERE message_key = $1"))
                    .bind(member).execute(&mut **tx).await.map_err(super::postgres_error)?;
                collect = false;
            } else if eligible.is_none() || (member == key && observed_custody) {
                sqlx::query(&format!("UPDATE {schema}.ingress_messages SET retention_eligible_at = $1 WHERE message_key = $2"))
                    .bind(now).bind(member).execute(&mut **tx).await.map_err(super::postgres_error)?;
                result.adopted += 1;
                collect = false;
            } else if eligible.is_some_and(|at| at > cutoff) {
                collect = false;
            }
        }
        if !collect {
            continue;
        }
        for member in [child, Some(key)].into_iter().flatten() {
            for table in ["ingress_origin_aliases", "ingress_deliveries"] {
                sqlx::query(&format!(
                    "DELETE FROM {schema}.{table} WHERE message_key = $1"
                ))
                .bind(member)
                .execute(&mut **tx)
                .await
                .map_err(super::postgres_error)?;
            }
            let deleted = sqlx::query(&format!(
                "DELETE FROM {schema}.ingress_messages WHERE message_key = $1"
            ))
            .bind(member)
            .execute(&mut **tx)
            .await
            .map_err(super::postgres_error)?
            .rows_affected();
            if deleted != 1 {
                return Err(DarkIngressAuthorityError::Database);
            }
            sqlx::query(&format!("DELETE FROM {schema}.sm_ingress_appends WHERE message_key = $1 AND disposition <> 0"))
                .bind(member.to_string()).execute(&mut **tx).await.map_err(super::postgres_error)?;
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
    let custody = custody_predicate(schema, "m", "m.message_key");
    let child_custody = custody_predicate(schema, "child", "child.message_key");
    let pair_predicate = pair_candidate_predicate(
        schema,
        &child_custody,
        "julianday(child.retention_eligible_at) <= julianday(?1)",
    );
    let candidates = format!(
        "SELECT m.message_key, NOT ({custody}) FROM main.ingress_messages m \
         WHERE m.terminal_at IS NOT NULL AND {pair_predicate} AND \
         (({custody} AND (m.retention_eligible_at IS NULL OR julianday(m.retention_eligible_at) <= julianday(?1))) \
          OR (NOT ({custody}) AND m.retention_eligible_at IS NOT NULL)) \
         ORDER BY m.retention_eligible_at NULLS FIRST, m.message_key LIMIT ?2"
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
        keys.push((
            row.get::<String>(0)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.get::<bool>(1)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
        ));
    }
    drop(rows);
    let mut result = RetentionLabBatch::default();
    for (key, observed_custody) in keys {
        result.examined += 1;
        let mut rows = tx.query(
            "SELECT pair.child_message_key, m.owner_decision, EXISTS (SELECT 1 FROM main.ingress_owner_rejections parent WHERE parent.child_message_key = m.message_key) FROM main.ingress_messages m LEFT JOIN main.ingress_owner_rejections pair ON pair.source_message_key = m.message_key WHERE m.message_key = ?",
            crate::db_params![key.clone()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
        let row = rows
            .next()
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?
            .ok_or(DarkIngressAuthorityError::Database)?;
        let pair = (
            row.get::<Option<String>>(0)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.get::<Option<i32>>(1)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
            row.get::<bool>(2)
                .map_err(|_| DarkIngressAuthorityError::Database)?,
        );
        drop(rows);
        let child = match pair {
            (_, _, true) | (None, Some(2), _) => continue,
            (Some(child), Some(2), false) => Some(child),
            (Some(_), _, _) => continue,
            (None, _, false) => None,
        };
        let mut collect = true;
        for member in [Some(key.as_str()), child.as_deref()].into_iter().flatten() {
            let child_guard = if Some(member) == child.as_deref() {
                "AND (m.owner_decision IS NULL OR m.owner_decision <> 2) AND NOT EXISTS (SELECT 1 FROM main.ingress_owner_rejections nested WHERE nested.source_message_key = m.message_key)"
            } else {
                ""
            };
            let recheck = format!("SELECT m.retention_eligible_at, (m.terminal_at IS NOT NULL AND {custody} {child_guard}) FROM main.ingress_messages m WHERE m.message_key = ?");
            let mut rows = tx
                .query(&recheck, crate::db_params![member.to_owned()])
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
                tx.execute("UPDATE main.ingress_messages SET retention_eligible_at = NULL WHERE message_key = ?", crate::db_params![member.to_owned()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                collect = false;
            } else if eligible.is_none() || (member == key && observed_custody) {
                tx.execute("UPDATE main.ingress_messages SET retention_eligible_at = ? WHERE message_key = ?", crate::db_params![now.to_rfc3339(), member.to_owned()]).await.map_err(|_| DarkIngressAuthorityError::Database)?;
                result.adopted += 1;
                collect = false;
            } else if let Some(eligible) = eligible {
                let at = DateTime::parse_from_rfc3339(&eligible)
                    .map_err(|_| DarkIngressAuthorityError::Database)?
                    .with_timezone(&Utc);
                if at > cutoff {
                    collect = false;
                }
            }
        }
        if !collect {
            continue;
        }
        for member in [child.as_deref(), Some(key.as_str())].into_iter().flatten() {
            for table in ["ingress_origin_aliases", "ingress_deliveries"] {
                tx.execute(
                    &format!("DELETE FROM main.{table} WHERE message_key = ?"),
                    crate::db_params![member.to_owned()],
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Database)?;
            }
            let deleted = tx
                .execute(
                    "DELETE FROM main.ingress_messages WHERE message_key = ?",
                    crate::db_params![member.to_owned()],
                )
                .await
                .map_err(|_| DarkIngressAuthorityError::Database)?;
            if deleted != 1 {
                return Err(DarkIngressAuthorityError::Database);
            }
            tx.execute(
                "DELETE FROM main.sm_ingress_appends WHERE message_key = ? AND disposition <> 0",
                crate::db_params![member.to_owned()],
            )
            .await
            .map_err(|_| DarkIngressAuthorityError::Database)?;
            result.deleted += 1;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use waddle_xmpp::auth::{
        AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
    };
    use waddle_xmpp::ingress::{MessageKey, SemanticDigest};

    use super::*;
    use crate::db::MigrationRunner;
    use crate::ingress_substrate::{
        record_message, record_message_with_source, terminalize_message, CanonicalSourceBinding,
    };

    #[tokio::test]
    async fn sqlite_owner_rejection_pair_waits_for_source_and_rolls_back_together() {
        let database = Database::in_memory("owner-rejection-retention-lab")
            .await
            .expect("open SQLite");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrate SQLite");
        let connection = database.guard().await.expect("fixture DDL connection");
        connection
            .execute(
                "CREATE TABLE direct_admission_lab (message_key TEXT NOT NULL)",
                (),
            )
            .await
            .expect("install direct admission retention fixture");
        connection
            .execute(
                "CREATE TABLE ingress_admission_pins (message_key TEXT NOT NULL)",
                (),
            )
            .await
            .expect("install admission pin retention fixture");
        connection
            .execute(
                "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY)",
                (),
            )
            .await
            .expect("install pending delivery retention fixture");
        drop(connection);

        let source = MessageKey::new();
        let child = MessageKey::new();
        let digest = SemanticDigest::from_storage(1, [9; 32]).expect("digest");
        let principal = AuthenticatedPrincipalRef::new(
            "romeo@example.com".parse().expect("source JID"),
            AuthContextId::new(uuid::Uuid::new_v4()),
            AuthContextVersion::new(3),
            PrincipalAuthEpoch::new(5),
        );
        let binding = CanonicalSourceBinding::new(principal, None);
        let now = Utc::now();
        let terminal = now - chrono::Duration::days(9);
        let mut tx = database
            .begin_immediate()
            .await
            .expect("seed retention rows");
        record_message_with_source(&mut tx, source, &digest, None, &binding)
            .await
            .expect("source with immutable principal");
        record_message(&mut tx, child, &digest, None)
            .await
            .expect("rejection child");
        terminalize_message(&mut tx, source, terminal)
            .await
            .expect("terminalize source");
        terminalize_message(&mut tx, child, terminal)
            .await
            .expect("terminalize child");
        tx.execute(
            "UPDATE ingress_messages SET owner_decision = 0 WHERE message_key = ?",
            crate::db_params![source.to_storage().to_string()],
        )
        .await
        .expect("initialize source decision");
        tx.execute(
            "INSERT INTO ingress_owner_rejections (source_message_key, child_message_key) VALUES (?, ?)",
            crate::db_params![source.to_storage().to_string(), child.to_storage().to_string()],
        )
        .await
        .expect("retain child binding");
        tx.execute(
            "UPDATE ingress_messages SET owner_decision = 2 WHERE message_key = ?",
            crate::db_params![source.to_storage().to_string()],
        )
        .await
        .expect("latch rejected owner result");
        crate::ingress_substrate::invalidate_retention(&mut tx, source)
            .await
            .expect("restart source tail");
        crate::ingress_substrate::refresh_retention(&mut tx, source, now)
            .await
            .expect("start source tail");
        tx.commit().await.expect("commit retention seed");

        let bounds = TransactionBounds::new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("transaction bounds");
        let lab = RetentionLab::sqlite(&database, bounds).expect("dark SQLite lab");
        let first = lab
            .collect(now, 16)
            .await
            .expect("retain the pair during source tail");
        assert_eq!(first.deleted, 0);
        let connection = database.guard().await.expect("read retention state");
        let mut rows = connection.query(
            "SELECT (SELECT COUNT(*) FROM ingress_messages WHERE message_key = ?), (SELECT COUNT(*) FROM ingress_messages WHERE message_key = ?), (SELECT COUNT(*) FROM ingress_owner_rejections)",
            crate::db_params![source.to_storage().to_string(), child.to_storage().to_string()],
        ).await.expect("read pair state");
        let row = rows.next().await.expect("state").expect("row");
        assert_eq!(row.get::<i64>(0).expect("source"), 1);
        assert_eq!(row.get::<i64>(1).expect("child"), 1);
        assert_eq!(row.get::<i64>(2).expect("mapping"), 1);
        drop(rows);
        connection.execute(&format!("CREATE TRIGGER fail_source_delete BEFORE DELETE ON ingress_messages WHEN OLD.message_key = '{}' BEGIN SELECT RAISE(ABORT, 'after child delete'); END", source.to_storage()), ()).await.expect("fault trigger");
        drop(connection);
        let later = now + ALIAS_RETENTION + chrono::Duration::seconds(1);
        assert!(lab.collect(later, 16).await.is_err());
        let connection = database.guard().await.expect("verify rollback");
        let mut rows = connection.query("SELECT (SELECT COUNT(*) FROM ingress_messages), (SELECT COUNT(*) FROM ingress_owner_rejections)", ()).await.expect("rollback state");
        let row = rows.next().await.expect("state").expect("row");
        assert_eq!(row.get::<i64>(0).expect("messages"), 2);
        assert_eq!(row.get::<i64>(1).expect("mapping"), 1);
        drop(rows);
        connection
            .execute("DROP TRIGGER fail_source_delete", ())
            .await
            .expect("remove trigger");
        drop(connection);
        assert_eq!(
            lab.collect(later, 16).await.expect("collect pair").deleted,
            2
        );
    }
}
