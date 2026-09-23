//! SQL boundary for immutable ingress allocations and their durable payload custody.

use chrono::{DateTime, Utc};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, PersistedIngressAppend, SmPersistenceError,
};
use waddle_xmpp::stream_management::{SmIngressAppendKey, SmIngressReceiptKind};

use crate::db::{Database, DatabaseError, Row, Transaction};

/// Allocate once; a duplicate primary key is handled by the snapshot caller.
pub(crate) async fn insert(
    tx: &mut Transaction<'_>,
    append: &PersistedIngressAppend,
) -> Result<(), DatabaseError> {
    write(tx, append, false).await.map(drop)
}

async fn write(
    tx: &mut Transaction<'_>,
    append: &PersistedIngressAppend,
    withhold_conflict: bool,
) -> Result<u64, DatabaseError> {
    if append.disposition != IngressCustodyDisposition::Pending {
        return Err(DatabaseError::QueryFailed(
            "new ingress allocations must be pending".into(),
        ));
    }
    let payload = super::codec::serialize_stanza(&append.payload)
        .map_err(|error| DatabaseError::QueryFailed(error.to_string()))?;
    let sql = if withhold_conflict {
        "INSERT INTO sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (message_key, receipt_kind, semantic_identity_hash, resource) DO NOTHING"
    } else {
        "INSERT INTO sm_ingress_appends (message_key, receipt_kind, semantic_identity_hash, resource, accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    };
    tx.execute(
        sql,
        crate::db_params![
            append.key.message_key.to_storage().to_string(),
            i64::from(append.key.kind.to_storage()),
            append.key.semantic_identity_hash.to_vec(),
            append.key.resource.to_string(),
            append.accepting_stream.as_str().to_string(),
            i64::from(append.sequence),
            append.appended_at.timestamp_millis(),
            payload,
            append.original_receipt_at.timestamp_millis(),
            encode_disposition(append.disposition),
        ],
    )
    .await
}

/// A drained sequence is already counted: withhold only the conflicting proof.
pub(crate) async fn insert_or_withhold(
    tx: &mut Transaction<'_>,
    appends: &[PersistedIngressAppend],
) -> Result<Vec<SmIngressAppendKey>, DatabaseError> {
    let mut withheld = Vec::new();
    for append in appends {
        if write(tx, append, true).await? == 0 {
            withheld.push(append.key.clone());
        }
    }
    Ok(withheld)
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
    let mut rows = conn.query(
        "SELECT accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition FROM sm_ingress_appends WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ?",
        crate::db_params![key.message_key.to_storage().to_string(), i64::from(key.kind.to_storage()), key.semantic_identity_hash.to_vec(), key.resource.to_string()],
    ).await.map_err(storage_error)?;
    rows.next()
        .await
        .map_err(storage_error)?
        .map(|row| decode(&row, key))
        .transpose()
}

/// Find all allocations for a replay entry, including terminal proofs. The
/// caller compares receipt time and payload to distinguish counter reuse.
pub(crate) async fn get_for_sequence(
    db: &Database,
    stream: &SmSessionId,
    sequence: u32,
) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError> {
    let conn = db.guard().await.map_err(storage_error)?;
    let mut rows = conn.query("SELECT accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition, message_key, receipt_kind, semantic_identity_hash, resource FROM sm_ingress_appends WHERE accepting_stream_id = ? AND sequence = ?", crate::db_params![stream.as_str().to_string(), i64::from(sequence)]).await.map_err(storage_error)?;
    let mut appends = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        appends.push(decode(&row, &decode_key(&row)?)?);
    }
    Ok(appends)
}

pub(crate) async fn list_pending(
    db: &Database,
    limit: usize,
) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError> {
    let limit = i64::try_from(limit)
        .map_err(|_| SmPersistenceError::Other("ingress custody limit exceeds i64".into()))?;
    let conn = db.guard().await.map_err(storage_error)?;
    let mut rows = conn.query(
        "SELECT accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition, message_key, receipt_kind, semantic_identity_hash, resource FROM sm_ingress_appends WHERE disposition = 0 ORDER BY appended_at_ms, message_key, receipt_kind, resource LIMIT ?",
        crate::db_params![limit],
    ).await.map_err(storage_error)?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        let key = decode_key(&row)?;
        pending.push(decode(&row, &key)?);
    }
    Ok(pending)
}

/// Keyset pagination lets the recovery sweep pass active sessions without starvation.
pub(crate) async fn list_pending_after(
    db: &Database,
    after: Option<&SmIngressAppendKey>,
    limit: usize,
) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError> {
    let limit = i64::try_from(limit)
        .map_err(|_| SmPersistenceError::Other("ingress custody limit exceeds i64".into()))?;
    let conn = db.guard().await.map_err(storage_error)?;
    let mut rows = if let Some(after) = after {
        conn.query("SELECT accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition, message_key, receipt_kind, semantic_identity_hash, resource FROM sm_ingress_appends WHERE disposition = 0 AND (message_key, receipt_kind, semantic_identity_hash, resource) > (?, ?, ?, ?) ORDER BY message_key, receipt_kind, semantic_identity_hash, resource LIMIT ?", crate::db_params![after.message_key.to_storage().to_string(), i64::from(after.kind.to_storage()), after.semantic_identity_hash.to_vec(), after.resource.to_string(), limit]).await.map_err(storage_error)?
    } else {
        conn.query("SELECT accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition, message_key, receipt_kind, semantic_identity_hash, resource FROM sm_ingress_appends WHERE disposition = 0 ORDER BY message_key, receipt_kind, semantic_identity_hash, resource LIMIT ?", crate::db_params![limit]).await.map_err(storage_error)?
    };
    let mut pending = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        pending.push(decode(&row, &decode_key(&row)?)?);
    }
    Ok(pending)
}

pub(crate) async fn complete(
    db: &Database,
    key: &SmIngressAppendKey,
    stream: &SmSessionId,
    sequence: u32,
    disposition: IngressCustodyDisposition,
) -> Result<bool, SmPersistenceError> {
    if disposition == IngressCustodyDisposition::Pending {
        return Err(SmPersistenceError::Other(
            "pending is not a custody completion".into(),
        ));
    }
    let conn = db.guard().await.map_err(storage_error)?;
    conn.execute(
        "UPDATE sm_ingress_appends SET disposition = ? WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ? AND accepting_stream_id = ? AND sequence = ? AND disposition = 0",
        crate::db_params![encode_disposition(disposition), key.message_key.to_storage().to_string(), i64::from(key.kind.to_storage()), key.semantic_identity_hash.to_vec(), key.resource.to_string(), stream.as_str().to_string(), i64::from(sequence)],
    ).await.map(|affected| affected > 0).map_err(storage_error)
}

/// The tombstone's matched replay entries and their independent custody must
/// become suppressed together, including allocations committed after its first scan.
pub(crate) async fn delete_tombstoned_unacked(
    tx: &mut Transaction<'_>,
    stream: &SmSessionId,
    sequences: &[u32],
) -> Result<u64, SmPersistenceError> {
    let mut removed = 0;
    for sequence in sequences {
        removed += tx
            .execute(
                "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
                crate::db_params![stream.as_str().to_string(), i64::from(*sequence)],
            )
            .await
            .map_err(storage_error)?;
        tx.execute("UPDATE sm_ingress_appends SET disposition = 3 WHERE accepting_stream_id = ? AND sequence = ? AND disposition = 0", crate::db_params![stream.as_str().to_string(), i64::from(*sequence)]).await.map_err(storage_error)?;
    }
    Ok(removed)
}

/// Preserve the immutable allocation while durably suppressing retracted content.
pub(crate) async fn scrub_custody(
    db: &Database,
    target: &waddle_xmpp::tombstone::TombstoneTarget,
    through: DateTime<Utc>,
) -> Result<(), SmPersistenceError> {
    let mut tx = db.begin_immediate().await.map_err(storage_error)?;
    let mut rows = tx.query("SELECT accepting_stream_id, sequence, appended_at_ms, custody_payload, original_receipt_at_ms, disposition, message_key, receipt_kind, semantic_identity_hash, resource FROM sm_ingress_appends WHERE disposition = 0 AND original_receipt_at_ms <= ?", crate::db_params![through.timestamp_millis()]).await.map_err(storage_error)?;
    let mut matches = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        let append = decode(&row, &decode_key(&row)?)?;
        if target.matches_message_element(&append.payload.to_element()) {
            matches.push(append);
        }
    }
    drop(rows);
    for append in matches {
        tx.execute("UPDATE sm_ingress_appends SET disposition = 3 WHERE message_key = ? AND receipt_kind = ? AND semantic_identity_hash = ? AND resource = ? AND accepting_stream_id = ? AND sequence = ? AND disposition = 0", crate::db_params![append.key.message_key.to_storage().to_string(), i64::from(append.key.kind.to_storage()), append.key.semantic_identity_hash.to_vec(), append.key.resource.to_string(), append.accepting_stream.as_str().to_string(), i64::from(append.sequence)]).await.map_err(storage_error)?;
    }
    tx.commit().await.map_err(storage_error)
}

/// The caller has validated the acknowledgement window. Restrict changes to its
/// forward interval, including the wrap from u32::MAX to zero.
pub(crate) async fn complete_through(
    tx: &mut Transaction<'_>,
    stream: &SmSessionId,
    from_exclusive: u32,
    h: u32,
) -> Result<(), SmPersistenceError> {
    if from_exclusive == h {
        return Ok(());
    }
    if h.wrapping_sub(from_exclusive) >= 0x8000_0000 {
        return Err(SmPersistenceError::Other(
            "invalid ingress custody acknowledgement window".into(),
        ));
    }
    let sql = if h > from_exclusive {
        "UPDATE sm_ingress_appends SET disposition = 1 WHERE accepting_stream_id = ? AND disposition = 0 AND sequence > ? AND sequence <= ?"
    } else {
        "UPDATE sm_ingress_appends SET disposition = 1 WHERE accepting_stream_id = ? AND disposition = 0 AND (sequence > ? OR sequence <= ?)"
    };
    tx.execute(
        sql,
        crate::db_params![
            stream.as_str().to_string(),
            i64::from(from_exclusive),
            i64::from(h)
        ],
    )
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn encode_disposition(disposition: IngressCustodyDisposition) -> i64 {
    match disposition {
        IngressCustodyDisposition::Pending => 0,
        IngressCustodyDisposition::Acknowledged => 1,
        IngressCustodyDisposition::Promoted => 2,
        IngressCustodyDisposition::Tombstoned => 3,
    }
}

fn decode(
    row: &Row,
    key: &SmIngressAppendKey,
) -> Result<PersistedIngressAppend, SmPersistenceError> {
    let accepting_stream = SmSessionId::try_from_wire(row.get::<String>(0).map_err(storage_error)?)
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let corrupt = |detail: &str| SmPersistenceError::Corrupt {
        stream_id: accepting_stream.clone(),
        detail: detail.into(),
    };
    let sequence = u32::try_from(row.get::<i64>(1).map_err(storage_error)?)
        .map_err(|_| corrupt("ingress append sequence outside u32"))?;
    let appended_at =
        DateTime::<Utc>::from_timestamp_millis(row.get::<i64>(2).map_err(storage_error)?)
            .ok_or_else(|| corrupt("invalid ingress append timestamp"))?;
    let payload_xml: String = row.get(3).map_err(storage_error)?;
    let payload = super::codec::parse_stanza(
        payload_xml
            .parse()
            .map_err(|_| corrupt("invalid ingress custody payload XML"))?,
    )?;
    let original_receipt_at =
        DateTime::<Utc>::from_timestamp_millis(row.get::<i64>(4).map_err(storage_error)?)
            .ok_or_else(|| corrupt("invalid ingress custody receipt timestamp"))?;
    let disposition = match row.get::<i64>(5).map_err(storage_error)? {
        0 => IngressCustodyDisposition::Pending,
        1 => IngressCustodyDisposition::Acknowledged,
        2 => IngressCustodyDisposition::Promoted,
        3 => IngressCustodyDisposition::Tombstoned,
        _ => return Err(corrupt("invalid ingress custody disposition")),
    };
    Ok(PersistedIngressAppend {
        key: key.clone(),
        accepting_stream,
        sequence,
        appended_at,
        payload,
        original_receipt_at,
        disposition,
    })
}

fn storage_error(error: DatabaseError) -> SmPersistenceError {
    SmPersistenceError::Other(error.to_string())
}

fn decode_key(row: &Row) -> Result<SmIngressAppendKey, SmPersistenceError> {
    let message_key: String = row.get(6).map_err(storage_error)?;
    let kind: i64 = row.get(7).map_err(storage_error)?;
    let hash: Vec<u8> = row.get(8).map_err(storage_error)?;
    let resource: String = row.get(9).map_err(storage_error)?;
    Ok(SmIngressAppendKey {
        message_key: waddle_xmpp::ingress::MessageKey::from_storage(
            message_key
                .parse()
                .map_err(|error: uuid::Error| SmPersistenceError::Other(error.to_string()))?,
        ),
        kind: SmIngressReceiptKind::from_storage(
            i32::try_from(kind).map_err(|error| SmPersistenceError::Other(error.to_string()))?,
        ),
        semantic_identity_hash: hash.try_into().map_err(|_| {
            SmPersistenceError::Other("invalid ingress custody semantic hash".into())
        })?,
        resource: resource
            .parse()
            .map_err(|error: jid::Error| SmPersistenceError::Other(error.to_string()))?,
    })
}
