//! SQL boundary for the stream-independent ingress allocation ledger.

use chrono::{DateTime, Utc};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{PersistedIngressAppend, SmPersistenceError};
use waddle_xmpp::stream_management::SmIngressAppendKey;

use crate::db::{Database, DatabaseError, Row, Transaction};

/// Write the allocation. `Ok(false)` means a replacement found no matching prior
/// row, so another writer already superseded it and this obligation stays theirs.
pub(crate) async fn insert(
    tx: &mut Transaction<'_>,
    append: &PersistedIngressAppend,
) -> Result<bool, DatabaseError> {
    let Some(prior) = append.supersedes.as_ref() else {
        tx.execute(
            "INSERT INTO sm_ingress_appends \
             (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            crate::db_params![
                append.key.message_key.to_storage().to_string(),
                i64::from(append.key.kind.to_storage()),
                append.key.semantic_identity_hash.to_vec(),
                append.key.resource.to_string(),
                append.accepting_stream.as_str().to_string(),
                i64::from(append.sequence),
                append.appended_at.timestamp_millis(),
            ],
        )
        .await?;
        return Ok(true);
    };
    // Replace exactly the evicted allocation. The guarded `DO UPDATE` keeps the
    // database the arbiter: a racing writer that already replaced the row leaves
    // the predicate false, so no second allocation is issued.
    let affected = tx
        .execute(
            "INSERT INTO sm_ingress_appends \
             (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (message_key, receipt_kind, semantic_identity_hash, resource) DO UPDATE SET \
             accepting_stream_id = excluded.accepting_stream_id, \
             sequence = excluded.sequence, \
             appended_at_ms = excluded.appended_at_ms \
             WHERE sm_ingress_appends.accepting_stream_id = ? AND sm_ingress_appends.sequence = ?",
            crate::db_params![
                append.key.message_key.to_storage().to_string(),
                i64::from(append.key.kind.to_storage()),
                append.key.semantic_identity_hash.to_vec(),
                append.key.resource.to_string(),
                append.accepting_stream.as_str().to_string(),
                i64::from(append.sequence),
                append.appended_at.timestamp_millis(),
                prior.accepting_stream.as_str().to_string(),
                i64::from(prior.sequence),
            ],
        )
        .await?;
    Ok(affected > 0)
}

/// Match the ledger's primary key, never an unrelated uniqueness/check failure.
/// SQLite exposes the extended code and failing columns, whereas Postgres exposes
/// the constraint and table names. Inspect these before flattening the driver error.
pub(crate) fn is_ledger_conflict(error: &DatabaseError) -> bool {
    let DatabaseError::Internal(sqlx::Error::Database(error)) = error else {
        return false;
    };
    if let Some(error) = error.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
        return error.code() == "23505"
            && error.table() == Some("sm_ingress_appends")
            && error.constraint() == Some("sm_ingress_appends_pkey");
    }
    if let Some(error) = error.try_downcast_ref::<sqlx::sqlite::SqliteError>() {
        use sqlx::error::DatabaseError as _;
        // SQLITE_CONSTRAINT_PRIMARYKEY / SQLITE_CONSTRAINT_UNIQUE, narrowed to this table.
        // Matching the table prefix rather than the exact column list keeps a real
        // conflict from being misread as a hard error if SQLite ever reorders or
        // reformats the failing-column list — that misreading would leave the
        // obligation permanently unresolvable instead of reporting it as allocated.
        return matches!(error.code().as_deref(), Some("1555" | "2067"))
            && error
                .message()
                .starts_with("UNIQUE constraint failed: sm_ingress_appends.");
    }
    false
}

pub(crate) async fn get(
    db: &Database,
    key: &SmIngressAppendKey,
) -> Result<Option<PersistedIngressAppend>, SmPersistenceError> {
    let conn = db.guard().await.map_err(storage_error)?;
    let mut rows = conn
        .query(
            "SELECT accepting_stream_id, sequence, appended_at_ms FROM sm_ingress_appends \
             WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?",
            crate::db_params![
                key.message_key.to_storage().to_string(),
                i64::from(key.kind.to_storage()),
                key.semantic_identity_hash.to_vec(),
                key.resource.to_string(),
            ],
        )
        .await
        .map_err(storage_error)?;
    rows.next()
        .await
        .map_err(storage_error)?
        .map(|row| decode(&row, key))
        .transpose()
}

pub(crate) fn decode(
    row: &Row,
    key: &SmIngressAppendKey,
) -> Result<PersistedIngressAppend, SmPersistenceError> {
    let accepting_stream = SmSessionId::try_from_wire(row.get::<String>(0).map_err(storage_error)?)
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let sequence = u32::try_from(row.get::<i64>(1).map_err(storage_error)?).map_err(|_| {
        SmPersistenceError::Corrupt {
            stream_id: accepting_stream.clone(),
            detail: "ingress append sequence outside u32".into(),
        }
    })?;
    let millis = row.get::<i64>(2).map_err(storage_error)?;
    let appended_at = DateTime::<Utc>::from_timestamp_millis(millis).ok_or_else(|| {
        SmPersistenceError::Corrupt {
            stream_id: accepting_stream.clone(),
            detail: "invalid ingress append timestamp".into(),
        }
    })?;
    Ok(PersistedIngressAppend {
        key: key.clone(),
        accepting_stream,
        sequence,
        appended_at,
        // Storage never reports a pending supersede: the row read back is whatever
        // allocation currently stands.
        supersedes: None,
    })
}

fn storage_error(error: DatabaseError) -> SmPersistenceError {
    SmPersistenceError::Other(error.to_string())
}

/// Proofs whose payload the session's replay gap covers, i.e. entries the
/// bounded queue evicted rather than delivered.
fn gap_covered(
    rows: &[(PersistedIngressAppendRow, u32)],
    gap: u32,
) -> Vec<&(PersistedIngressAppendRow, u32)> {
    rows.iter()
        .filter(|(_, sequence)| {
            waddle_xmpp::stream_management::sequence::sequence_lte(*sequence, gap)
        })
        .collect()
}

/// The primary key of one ledger row, as stored.
pub(crate) struct PersistedIngressAppendRow {
    message_key: String,
    receipt_kind: i64,
    semantic_identity_hash: Vec<u8>,
    resource: String,
}

/// Retire proofs for allocations this session lost before its row is deleted.
///
/// Deleting the durable session destroys `replay_gap_through`, which is the only
/// evidence distinguishing an evicted allocation from a delivered one. A later
/// retry would then read "no durable session" as a discharged obligation and
/// commit resource progress for a stanza nothing ever delivered. Retiring the
/// gap-covered proofs first lets that retry allocate a replacement instead.
///
/// Promotion never delivers a gap-covered entry either: it hands out only the
/// retained queue, which by construction sits strictly above the gap.
pub(crate) async fn void_gap_covered(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
) -> Result<(), DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT replay_gap_through FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await?;
    let gap = match rows.next().await? {
        Some(row) => row.get::<Option<i64>>(0)?,
        None => return Ok(()),
    };
    drop(rows);
    let Some(gap) = gap.and_then(|gap| u32::try_from(gap).ok()) else {
        return Ok(());
    };
    let mut rows = tx
        .query(
            "SELECT message_key, receipt_kind, semantic_identity_hash, resource, sequence \
             FROM sm_ingress_appends WHERE accepting_stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await?;
    let mut proofs = Vec::new();
    while let Some(row) = rows.next().await? {
        let sequence = row.get::<i64>(4)?;
        let Ok(sequence) = u32::try_from(sequence) else {
            continue;
        };
        proofs.push((
            PersistedIngressAppendRow {
                message_key: row.get::<String>(0)?,
                receipt_kind: row.get::<i64>(1)?,
                semantic_identity_hash: row.get::<Vec<u8>>(2)?,
                resource: row.get::<String>(3)?,
            },
            sequence,
        ));
    }
    drop(rows);
    // Wrap-aware in Rust rather than SQL: the comparison is modulo 2^32 and no
    // portable dialect expression states that clearly.
    for (row, sequence) in gap_covered(&proofs, gap) {
        // Retire only the exact allocation that was read. A retry can supersede
        // this proof onto a newer stream between the select and this delete; a
        // primary-key-only predicate would then delete the replacement, and the
        // next retry would see no proof and append a duplicate.
        tx.execute(
            "DELETE FROM sm_ingress_appends \
             WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ? \
             AND accepting_stream_id = ? AND sequence = ?",
            crate::db_params![
                row.message_key.clone(),
                row.receipt_kind,
                row.semantic_identity_hash.clone(),
                row.resource.clone(),
                stream_id.as_str().to_string(),
                i64::from(*sequence),
            ],
        )
        .await?;
    }
    Ok(())
}
