//! Atomic suffix allocation for non-resumable promotion custody.
use crate::db::{DatabaseDriver, Transaction};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    PersistedUnackedStanza, PromotionAppendOutcome, SmAckWindow, SmPersistenceError,
};

fn storage_error(error: impl std::fmt::Display) -> SmPersistenceError {
    SmPersistenceError::Other(error.to_string())
}

/// Retire only an existing canonical source, preserving its unacknowledged
/// suffix. The caller owns the transaction and any required ownership fence.
pub(crate) async fn acknowledge(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
    acknowledged_h: u32,
    driver: DatabaseDriver,
    retire: bool,
) -> Result<(), SmPersistenceError> {
    let sql = match driver {
        DatabaseDriver::Postgres => {
            "SELECT outbound_count, last_acked FROM sm_sessions WHERE stream_id = ? FOR UPDATE"
        }
        DatabaseDriver::Sqlite => {
            "SELECT outbound_count, last_acked FROM sm_sessions WHERE stream_id = ?"
        }
    };
    let mut rows = tx
        .query(sql, crate::db_params![stream_id.as_str()])
        .await
        .map_err(storage_error)?;
    let row = rows
        .next()
        .await
        .map_err(storage_error)?
        .ok_or_else(|| storage_error("Expired SM promotion source is missing"))?;
    let outbound =
        u32::try_from(row.get::<i64>(0).map_err(storage_error)?).map_err(storage_error)?;
    let last_acked =
        u32::try_from(row.get::<i64>(1).map_err(storage_error)?).map_err(storage_error)?;
    let acknowledged = acknowledged_h.wrapping_sub(last_acked);
    if acknowledged >= 0x8000_0000 || acknowledged > outbound.wrapping_sub(last_acked) {
        return Err(storage_error(
            "Expired SM acknowledgement exceeds canonical outbound count",
        ));
    }
    drop(rows);
    super::ingress_append::complete_through(tx, stream_id, last_acked, acknowledged_h).await?;
    tx.execute(
        "DELETE FROM sm_unacked WHERE stream_id = ? \
         AND ((sequence - ? + 4294967296) % 4294967296) > 0 \
         AND ((sequence - ? + 4294967296) % 4294967296) <= ?",
        crate::db_params![
            stream_id.as_str(),
            i64::from(last_acked),
            i64::from(last_acked),
            i64::from(acknowledged),
        ],
    )
    .await
    .map_err(storage_error)?;
    if acknowledged > 0 {
        tx.execute(
            "INSERT INTO sm_acknowledged_windows (stream_id, from_exclusive, to_inclusive) \
             VALUES (?, ?, ?) ON CONFLICT (stream_id, from_exclusive, to_inclusive) DO NOTHING",
            crate::db_params![
                stream_id.as_str(),
                i64::from(last_acked),
                i64::from(acknowledged_h)
            ],
        )
        .await
        .map_err(storage_error)?;
    }
    let sql = if retire {
        "UPDATE sm_sessions SET last_acked = ?, max_resume_secs = 0, max_resume_duration_ms = 0 WHERE stream_id = ?"
    } else {
        "UPDATE sm_sessions SET last_acked = ? WHERE stream_id = ?"
    };
    tx.execute(
        sql,
        crate::db_params![i64::from(acknowledged_h), stream_id.as_str()],
    )
    .await
    .map_err(storage_error)?;
    Ok(())
}

/// Serialize deletion with ACK publication, then fail closed while settlement remains.
pub(crate) async fn require_settled(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
    driver: DatabaseDriver,
) -> Result<(), SmPersistenceError> {
    let sql = match driver {
        DatabaseDriver::Postgres => {
            "SELECT stream_id FROM sm_sessions WHERE stream_id = ? FOR UPDATE"
        }
        DatabaseDriver::Sqlite => "SELECT stream_id FROM sm_sessions WHERE stream_id = ?",
    };
    let mut source = tx
        .query(sql, crate::db_params![stream_id.as_str()])
        .await
        .map_err(storage_error)?;
    let _ = source.next().await.map_err(storage_error)?;
    drop(source);
    if !pending(tx, stream_id).await?.is_empty() {
        return Err(storage_error("SM acknowledgement settlement is pending"));
    }
    Ok(())
}

pub(crate) async fn pending(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
) -> Result<Vec<SmAckWindow>, SmPersistenceError> {
    let mut rows = tx.query(
        "SELECT from_exclusive, to_inclusive FROM sm_acknowledged_windows WHERE stream_id = ? ORDER BY from_exclusive, to_inclusive",
        crate::db_params![stream_id.as_str()],
    ).await.map_err(storage_error)?;
    let mut windows = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        windows.push(SmAckWindow {
            from_exclusive: u32::try_from(row.get::<i64>(0).map_err(storage_error)?)
                .map_err(storage_error)?,
            to_inclusive: u32::try_from(row.get::<i64>(1).map_err(storage_error)?)
                .map_err(storage_error)?,
        });
    }
    Ok(windows)
}

pub(crate) async fn claim_protected_streams(
    tx: &mut Transaction<'_>,
) -> Result<Vec<SmSessionId>, SmPersistenceError> {
    let mut rows = tx
        .query(
            "SELECT stream_id FROM sm_sessions UNION SELECT stream_id FROM sm_acknowledged_windows ORDER BY stream_id",
            (),
        )
        .await
        .map_err(storage_error)?;
    let mut streams = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        streams.push(SmSessionId::new(
            row.get::<String>(0).map_err(storage_error)?,
        ));
    }
    Ok(streams)
}

pub(crate) async fn confirm(
    tx: &mut Transaction<'_>,
    stream_id: &SmSessionId,
    window: SmAckWindow,
) -> Result<(), SmPersistenceError> {
    tx.execute(
        "DELETE FROM sm_acknowledged_windows WHERE stream_id = ? AND from_exclusive = ? AND to_inclusive = ?",
        crate::db_params![stream_id.as_str(), i64::from(window.from_exclusive), i64::from(window.to_inclusive)],
    ).await.map_err(storage_error)?;
    Ok(())
}

/// The fenced caller asserts the exact SM claim before entering this helper.
/// Lock the source row so payload allocation and its wrapping sequence watermark
/// commit together, without replacing any rows a promotion or scrub retired.
pub(crate) async fn append(
    tx: &mut Transaction<'_>,
    stanza: PersistedUnackedStanza,
    driver: DatabaseDriver,
) -> Result<PromotionAppendOutcome, SmPersistenceError> {
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
        let mut rows = tx
            .query(
                "SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts \
             FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
                crate::db_params![stanza.stream_id.as_str(), i64::from(stanza.sequence)],
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(PromotionAppendOutcome::Retired);
        };
        let existing = super::codec::decode_unacked(&row)?;
        if existing.stanza.to_element() != stanza.stanza.to_element()
            || existing.original_receipt_at.timestamp_millis()
                != stanza.original_receipt_at.timestamp_millis()
            || existing.ingress_receipts != stanza.ingress_receipts
        {
            return Err(SmPersistenceError::PromotionSequenceConflict {
                stream_id: stanza.stream_id,
                sequence: stanza.sequence,
            });
        }
        return Ok(PromotionAppendOutcome::AlreadyRecorded);
    }
    let xml = super::codec::serialize_stanza(&stanza.stanza)?;
    let inserted = tx.execute(
        "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, ingress_receipts) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT (stream_id, sequence) DO NOTHING",
        crate::db_params![stanza.stream_id.as_str(), i64::from(stanza.sequence), xml,
            stanza.original_receipt_at.timestamp_millis(),
            super::codec::encode_ingress_receipts(&stanza.ingress_receipts)],
    ).await.map_err(storage_error)?;
    if inserted != 1 {
        return Err(SmPersistenceError::PromotionSequenceConflict {
            stream_id: stanza.stream_id,
            sequence: stanza.sequence,
        });
    }
    tx.execute(
        "UPDATE sm_sessions SET outbound_count = ? WHERE stream_id = ?",
        crate::db_params![i64::from(stanza.sequence), stanza.stream_id.as_str()],
    )
    .await
    .map_err(storage_error)?;
    Ok(PromotionAppendOutcome::Appended)
}
