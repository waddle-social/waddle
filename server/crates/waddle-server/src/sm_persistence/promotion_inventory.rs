//! Atomic suffix allocation for non-resumable promotion custody.
use crate::db::{DatabaseDriver, Transaction};
use waddle_xmpp::stream_management::persistence::{PersistedUnackedStanza, SmPersistenceError};

fn storage_error(error: impl std::fmt::Display) -> SmPersistenceError {
    SmPersistenceError::Other(error.to_string())
}

/// The fenced caller asserts the exact SM claim before entering this helper.
/// Lock the source row so payload allocation and its wrapping sequence watermark
/// commit together, without replacing any rows a promotion or scrub retired.
pub(crate) async fn append(
    tx: &mut Transaction<'_>,
    stanza: PersistedUnackedStanza,
    driver: DatabaseDriver,
) -> Result<(), SmPersistenceError> {
    let sql = match driver {
        DatabaseDriver::Postgres => {
            "SELECT outbound_count, max_resume_secs, max_resume_duration_ms \
            FROM sm_sessions WHERE stream_id = ? FOR UPDATE"
        }
        DatabaseDriver::Sqlite => {
            "SELECT outbound_count, max_resume_secs, max_resume_duration_ms \
            FROM sm_sessions WHERE stream_id = ?"
        }
    };
    let mut rows = tx
        .query(sql, crate::db_params![stanza.stream_id.as_str()])
        .await
        .map_err(storage_error)?;
    let row = rows
        .next()
        .await
        .map_err(storage_error)?
        .ok_or_else(|| storage_error("Promotion inventory is missing"))?;
    let outbound =
        u32::try_from(row.get::<i64>(0).map_err(storage_error)?).map_err(storage_error)?;
    let resume: Option<i64> = row.get(1).map_err(storage_error)?;
    let duration: i64 = row.get(2).map_err(storage_error)?;
    if resume != Some(0) || duration != 0 {
        return Err(storage_error("Promotion inventory must be non-resumable"));
    }
    drop(rows);
    let distance = stanza.sequence.wrapping_sub(outbound);
    if distance == 0 || distance >= (1_u32 << 31) {
        return Ok(());
    }
    let xml = super::codec::serialize_stanza(&stanza.stanza)?;
    tx.execute(
        "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT (stream_id, sequence) DO NOTHING",
        crate::db_params![stanza.stream_id.as_str(), i64::from(stanza.sequence), xml,
            stanza.original_receipt_at.timestamp_millis(),
            super::codec::encode_ingress_receipts(&stanza.ingress_receipts)],
    ).await.map_err(storage_error)?;
    tx.execute(
        "UPDATE sm_sessions SET outbound_count = ? WHERE stream_id = ?",
        crate::db_params![i64::from(stanza.sequence), stanza.stream_id.as_str()],
    )
    .await
    .map_err(storage_error)?;
    Ok(())
}
