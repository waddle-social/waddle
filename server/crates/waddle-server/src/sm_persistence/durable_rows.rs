//! Transaction-scoped access to `sm_unacked` rows.
//!
//! The durable session authority stores a session's retained frames in this
//! relation, inside the transaction that holds its session row lock. Every function here runs on
//! the caller's transaction and uses the portable store's row encoding, so a
//! row written by either writer decodes the same way.

use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{PersistedUnackedStanza, SmPersistenceError};

use super::codec::{bounded_provenance_projection, decode_unacked};
use super::implementation::append_unacked_in_transaction;
use crate::db::{DatabaseDriver, Transaction};

fn storage(error: crate::db::DatabaseError) -> SmPersistenceError {
    SmPersistenceError::Other(error.to_string())
}

/// Insert one retained frame under its stored sequence.
pub(crate) async fn append_unacked(
    tx: &mut Transaction<'_>,
    frame: &PersistedUnackedStanza,
) -> Result<(), SmPersistenceError> {
    append_unacked_in_transaction(tx, frame).await
}

/// Every retained frame of the stream, in stored sequence order. The caller
/// orders them by ordinal, since stored sequences wrap.
pub(crate) async fn load_unacked(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
    let provenance = bounded_provenance_projection(tx.driver(), "provenance");
    let mut rows = tx
        .query(
            &format!(
                "SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms, \
                        ingress_receipts, {provenance} \
                 FROM sm_unacked WHERE stream_id = ? ORDER BY sequence ASC"
            ),
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(storage)?;
    let mut frames = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        frames.push(decode_unacked(&row)?);
    }
    Ok(frames)
}

/// Delete the frames whose stored sequence lies in the wrapping interval
/// `(after, through]`.
pub(crate) async fn delete_unacked_through(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
    after: u32,
    through: u32,
) -> Result<(), SmPersistenceError> {
    let stream = stream_id.as_str().to_string();
    let (sql, params) = if after <= through {
        (
            "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence > ? AND sequence <= ?",
            crate::db_params![stream, i64::from(after), i64::from(through)],
        )
    } else {
        // The interval crosses the counter wrap.
        (
            "DELETE FROM sm_unacked WHERE stream_id = ? AND (sequence > ? OR sequence <= ?)",
            crate::db_params![stream, i64::from(after), i64::from(through)],
        )
    };
    tx.execute(sql, params).await.map_err(storage)?;
    Ok(())
}

/// Payload bytes of the stream's retained frames.
pub(crate) async fn retained_payload_bytes(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
) -> Result<u64, SmPersistenceError> {
    let length = match tx.driver() {
        DatabaseDriver::Postgres => "octet_length(stanza_xml)",
        DatabaseDriver::Sqlite => "length(CAST(stanza_xml AS BLOB))",
    };
    let bigint = crate::db::i64_sql_type(tx.driver());
    let mut rows = tx
        .query(
            &format!(
                "SELECT CAST(COALESCE(SUM({length}), 0) AS {bigint}) \
                 FROM sm_unacked WHERE stream_id = ?"
            ),
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await
        .map_err(storage)?;
    let row = rows
        .next()
        .await
        .map_err(storage)?
        .ok_or_else(|| SmPersistenceError::Other("retained byte sum returned no row".into()))?;
    let bytes: i64 = row.get(0).map_err(storage)?;
    u64::try_from(bytes)
        .map_err(|_| SmPersistenceError::Other("retained byte sum is negative".into()))
}
