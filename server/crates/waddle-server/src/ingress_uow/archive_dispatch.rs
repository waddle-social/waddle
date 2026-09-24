//! Durable predecessors for archive-bearing delivery copies.
//!
//! Registration shares the archive insert transaction and its counter lock.
//! Readiness never acquires canonical locks or waits for another delivery;
//! callers defer blocked work to recovery, outside the connection loop.
use jid::{BareJid, FullJid};
use waddle_xmpp::{ingress::MessageKey, mam::ArchiveOrdinal, pending_delivery::PendingRowId};

use super::{EffectReceiptRepository, IngressUowError, IngressUowTransaction};
use crate::{db::DatabaseDriver, ingress::decision::EffectReceiptKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchTarget {
    Resource(FullJid),
    /// A frozen obligation whose resource audience is not yet known.
    ArchiveWide,
    /// An archived offline copy remains owed while its pending row exists.
    Pending(PendingRowId),
}

#[derive(Clone, Debug)]
pub(crate) struct ArchiveDispatchObligation {
    pub receipt: EffectReceiptKey,
    pub target: DispatchTarget,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DispatchReadiness {
    Ready,
    Completed,
    /// Keys belong to recoverable canonical predecessors. An empty list still
    /// means blocked when an independently promoted pending copy is earlier.
    Blocked(Vec<MessageKey>),
}

pub(crate) struct ArchiveDispatchRepository;

impl ArchiveDispatchRepository {
    pub(crate) async fn positions(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<waddle_xmpp::stream_management::ArchiveDispatchPosition>, IngressUowError> {
        let query = positions_query(tx.transaction_mut().driver());
        let rows = tx
            .transaction_mut()
            .query(
                query,
                crate::db_params![
                    key.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec()
                ],
            )
            .await?;
        decode_positions(rows).await
    }

    pub(crate) async fn positions_pooled(
        db: &crate::db::Database,
        key: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<waddle_xmpp::stream_management::ArchiveDispatchPosition>, IngressUowError> {
        let conn = db.guard().await?;
        let rows = conn
            .query(
                positions_query(db.driver()),
                crate::db_params![
                    key.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec()
                ],
            )
            .await?;
        decode_positions(rows).await
    }
    /// The caller holds canonical authority and the archive counter lock.
    /// Retrying the same registration is harmless; moving it is rejected.
    pub(crate) async fn record(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        archive: &BareJid,
        ordinal: ArchiveOrdinal,
        obligations: &[ArchiveDispatchObligation],
    ) -> Result<(), IngressUowError> {
        let postgres = tx.transaction_mut().driver() == DatabaseDriver::Postgres;
        let insert = if postgres {
            "INSERT INTO ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES (?, ?, ?::uuid, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
        };
        let select = if postgres {
            "SELECT archive_seq FROM ingress_archive_dispatch WHERE archive_jid = ? AND message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND resource = ? AND pending_row_id = ?"
        } else {
            "SELECT archive_seq FROM ingress_archive_dispatch WHERE archive_jid = ? AND message_key = ? AND kind = ? AND semantic_identity_hash = ? AND resource = ? AND pending_row_id = ?"
        };
        validate_registration(tx, key, archive, ordinal, obligations).await?;
        for obligation in obligations {
            // Empty is a storage-only wildcard; protocol boundaries remain typed.
            let resource = match &obligation.target {
                DispatchTarget::Resource(resource) => resource.to_string(),
                DispatchTarget::ArchiveWide | DispatchTarget::Pending(_) => String::new(),
            };
            let pending = match &obligation.target {
                DispatchTarget::Pending(id) => id.as_str(),
                _ => "",
            };
            let receipt = &obligation.receipt;
            tx.transaction_mut()
                .execute(
                    insert,
                    crate::db_params![
                        archive.to_string(),
                        ordinal.to_storage(),
                        key.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        resource.clone(),
                        pending
                    ],
                )
                .await?;
            let mut rows = tx
                .transaction_mut()
                .query(
                    select,
                    crate::db_params![
                        archive.to_string(),
                        key.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        resource,
                        pending
                    ],
                )
                .await?;
            let stored: i64 = rows
                .next()
                .await?
                .ok_or(IngressUowError::EffectIntentMessageMissing)?
                .get(0)?;
            if stored != ordinal.to_storage() {
                return Err(IngressUowError::EffectIntentConflict);
            }
        }
        Ok(())
    }

    /// `None` checks every recorded copy of this receipt. A concrete resource
    /// also conflicts with archive-wide predecessors. Non-archive receipts
    /// have no registrations and are ready. This is an admission check, not an
    /// I/O lease: delivery must use the exact idempotent append identity.
    pub(crate) async fn readiness(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        receipt: &EffectReceiptKey,
        resource: Option<&FullJid>,
        stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
    ) -> Result<DispatchReadiness, IngressUowError> {
        if EffectReceiptRepository::contains(tx, key, receipt.kind, &receipt.semantic_identity_hash)
            .await?
        {
            return Ok(DispatchReadiness::Completed);
        }
        if let Some(resource) = resource {
            if resource_completed(tx, key, receipt, resource).await? {
                return Ok(DispatchReadiness::Completed);
            }
        }
        predecessors(tx, key, receipt, resource, stream).await
    }

    /// A flush has already completed its enqueue receipt, but must still obey
    /// earlier archive positions. Its own row is never its predecessor.
    pub(crate) async fn readiness_pending(
        tx: &mut IngressUowTransaction<'_>,
        recipient: &BareJid,
        row_id: &PendingRowId,
        stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
    ) -> Result<DispatchReadiness, IngressUowError> {
        let mut rows = tx.transaction_mut().query(
            "SELECT DISTINCT archive_jid, archive_seq, resource FROM ingress_archive_dispatch WHERE archive_jid = ? AND pending_row_id = ?",
            crate::db_params![recipient.to_string(), row_id.as_str()],
        ).await?;
        let mut positions = Vec::new();
        while let Some(row) = rows.next().await? {
            positions.push(decode_position(&row)?);
        }
        drop(rows);
        if positions.is_empty() && pending_store_exists(tx).await? {
            // SM detach promotion is an independent pending producer; recover
            // its position directly from its typed archive pointer.
            let sql = format!("SELECT p.archive_stanza_by, {PENDING_ORDINAL}, '' FROM pending_delivery p WHERE p.row_id = ? AND p.recipient_jid = ? AND p.payload_kind = 'archived' AND {PENDING_ORDINAL} IS NOT NULL");
            let mut rows = tx
                .transaction_mut()
                .query(
                    &sql,
                    crate::db_params![row_id.as_str(), recipient.to_string()],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                positions.push(decode_position(&row)?);
            }
        }
        positions_ready(tx, positions, stream).await
    }
}

fn positions_query(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => "SELECT DISTINCT archive_jid, archive_seq FROM ingress_archive_dispatch WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY archive_jid, archive_seq",
        DatabaseDriver::Sqlite => "SELECT DISTINCT archive_jid, archive_seq FROM ingress_archive_dispatch WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ORDER BY archive_jid, archive_seq",
    }
}

async fn decode_positions(
    mut rows: crate::db::Rows,
) -> Result<Vec<waddle_xmpp::stream_management::ArchiveDispatchPosition>, IngressUowError> {
    let mut positions = Vec::new();
    while let Some(row) = rows.next().await? {
        let archive: String = row.get(0)?;
        let ordinal: i64 = row.get(1)?;
        positions.push(waddle_xmpp::stream_management::ArchiveDispatchPosition {
            archive: archive
                .parse()
                .map_err(|_| IngressUowError::EffectIntentConflict)?,
            ordinal: ArchiveOrdinal::from_storage(ordinal)
                .map_err(|_| IngressUowError::EffectIntentConflict)?,
        });
    }
    Ok(positions)
}

/// A receipt's frozen audience is immutable, including the distinction between
/// an opaque wildcard and a particular pending row. Supply its complete target
/// set in one call; a retry may reorder that set but cannot expand or replace it.
async fn validate_registration(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    archive: &BareJid,
    ordinal: ArchiveOrdinal,
    obligations: &[ArchiveDispatchObligation],
) -> Result<(), IngressUowError> {
    let query = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
        "SELECT archive_seq, resource, pending_row_id FROM ingress_archive_dispatch WHERE archive_jid = ? AND message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?"
    } else {
        "SELECT archive_seq, resource, pending_row_id FROM ingress_archive_dispatch WHERE archive_jid = ? AND message_key = ? AND kind = ? AND semantic_identity_hash = ?"
    };
    for (index, obligation) in obligations.iter().enumerate() {
        if obligations[..index]
            .iter()
            .any(|prior| prior.receipt == obligation.receipt)
        {
            continue;
        }
        let expected = obligations
            .iter()
            .filter(|other| other.receipt == obligation.receipt)
            .map(|other| &other.target)
            .collect::<Vec<_>>();
        let receipt = &obligation.receipt;
        let mut rows = tx
            .transaction_mut()
            .query(
                query,
                crate::db_params![
                    archive.to_string(),
                    key.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec()
                ],
            )
            .await?;
        let mut stored = Vec::new();
        while let Some(row) = rows.next().await? {
            let position: i64 = row.get(0)?;
            let resource: String = row.get(1)?;
            let pending: String = row.get(2)?;
            if position != ordinal.to_storage() {
                return Err(IngressUowError::EffectIntentConflict);
            }
            let target = if !pending.is_empty() {
                DispatchTarget::Pending(PendingRowId::new(pending))
            } else if resource.is_empty() {
                DispatchTarget::ArchiveWide
            } else {
                DispatchTarget::Resource(
                    resource
                        .parse()
                        .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?,
                )
            };
            stored.push(target);
        }
        if !stored.is_empty()
            && (stored.iter().any(|target| !expected.contains(&target))
                || expected.iter().any(|target| !stored.contains(target)))
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
    }
    Ok(())
}

// Resource progress proves only that resource; a wildcard must await the
// aggregate receipt. Archive retention cannot remove this independent index.
const PREDECESSORS: &str = "
SELECT CAST(p.message_key AS TEXT), p.archive_seq, p.pending_row_id,
    CASE WHEN EXISTS (SELECT 1 FROM ingress_effect_receipts r WHERE r.message_key = p.message_key
        AND r.kind = p.kind AND r.semantic_identity_hash = p.semantic_identity_hash) THEN 1 ELSE 0 END
FROM ingress_archive_dispatch p
WHERE p.archive_jid = ? AND p.archive_seq < ?
    AND (? = '' OR p.resource = '' OR p.resource = ?)
    AND (p.pending_row_id <> '' OR NOT EXISTS (SELECT 1 FROM ingress_effect_receipts r
        WHERE r.message_key = p.message_key AND r.kind = p.kind AND r.semantic_identity_hash = p.semantic_identity_hash))
    AND NOT EXISTS (SELECT 1 FROM ingress_delivery_receipts r
        WHERE p.resource <> '' AND r.message_key = p.message_key AND r.kind = p.kind
            AND r.semantic_identity_hash = p.semantic_identity_hash AND r.resource = p.resource)
    AND NOT EXISTS (SELECT 1 FROM ingress_carbon_receipts r
        WHERE p.resource <> '' AND r.message_key = p.message_key AND r.kind = p.kind
            AND r.semantic_identity_hash = p.semantic_identity_hash AND r.recipient = p.resource)
ORDER BY p.archive_seq, p.message_key";

fn decode_message(stored: &str) -> Result<MessageKey, IngressUowError> {
    stored.parse().map(MessageKey::from_storage).map_err(|_| {
        crate::ingress_substrate::IngressSubstrateError::InvalidStoredMessageKey.into()
    })
}

struct DispatchPosition {
    archive: BareJid,
    ordinal: ArchiveOrdinal,
    resource: Option<FullJid>,
}

fn decode_position(row: &crate::db::Row) -> Result<DispatchPosition, IngressUowError> {
    let archive: String = row.get(0)?;
    let ordinal: i64 = row.get(1)?;
    let resource: String = row.get(2)?;
    Ok(DispatchPosition {
        archive: archive
            .parse()
            .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?,
        ordinal: ArchiveOrdinal::from_storage(ordinal)
            .map_err(|_| IngressUowError::EffectIntentConflict)?,
        resource: if resource.is_empty() {
            None
        } else {
            Some(
                resource
                    .parse()
                    .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?,
            )
        },
    })
}

async fn predecessors(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    receipt: &EffectReceiptKey,
    resource: Option<&FullJid>,
    stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) -> Result<DispatchReadiness, IngressUowError> {
    let query = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
        "SELECT DISTINCT archive_jid, archive_seq, resource FROM ingress_archive_dispatch WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND (? = '' OR resource = '' OR resource = ?)"
    } else {
        "SELECT DISTINCT archive_jid, archive_seq, resource FROM ingress_archive_dispatch WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? AND (? = '' OR resource = '' OR resource = ?)"
    };
    let target = resource.map(ToString::to_string).unwrap_or_default();
    let mut rows = tx
        .transaction_mut()
        .query(
            query,
            crate::db_params![
                key.to_storage().to_string(),
                receipt.kind.to_storage(),
                receipt.semantic_identity_hash.to_vec(),
                target.clone(),
                target
            ],
        )
        .await?;
    let mut positions = Vec::new();
    while let Some(row) = rows.next().await? {
        positions.push(decode_position(&row)?);
    }
    drop(rows);
    positions_ready(tx, positions, stream).await
}

async fn positions_ready(
    tx: &mut IngressUowTransaction<'_>,
    positions: Vec<DispatchPosition>,
    stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) -> Result<DispatchReadiness, IngressUowError> {
    let has_pending_store = !positions.is_empty() && pending_store_exists(tx).await?;
    let mut blocked = false;
    let mut blockers = Vec::new();
    for position in positions {
        for key in recorded_predecessors(tx, &position, stream).await? {
            blocked = true;
            if !blockers.contains(&key) {
                blockers.push(key);
            }
        }
        if has_pending_store && pending_predecessor(tx, &position, stream).await? {
            blocked = true;
        }
    }
    Ok(if blocked {
        DispatchReadiness::Blocked(blockers)
    } else {
        DispatchReadiness::Ready
    })
}

async fn recorded_predecessors(
    tx: &mut IngressUowTransaction<'_>,
    position: &DispatchPosition,
    stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) -> Result<Vec<MessageKey>, IngressUowError> {
    let target = position
        .resource
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut rows = tx
        .transaction_mut()
        .query(
            PREDECESSORS,
            crate::db_params![
                position.archive.to_string(),
                position.ordinal.to_storage(),
                target.clone(),
                target
            ],
        )
        .await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        let stored: String = row.get(0)?;
        let pending: String = row.get(2)?;
        let receipted: i64 = row.get(3)?;
        candidates.push((
            decode_message(&stored)?,
            if pending.is_empty() {
                None
            } else {
                Some(PendingRowId::new(pending))
            },
            receipted != 0,
        ));
    }
    drop(rows);
    let mut blockers = Vec::new();
    for (key, pending, receipted) in candidates {
        if let Some(pending) = pending.filter(|_| receipted) {
            let mut rows = tx
                .transaction_mut()
                .query(
                    "SELECT row_id FROM pending_delivery WHERE row_id = ? AND NOT (COALESCE(flushed_in_session = NULLIF(?, ''), FALSE) AND outbound_sequence IS NOT NULL)",
                    crate::db_params![pending.as_str(), stream.map(|stream| stream.as_str()).unwrap_or("")],
                )
                .await?;
            if rows.next().await?.is_none() {
                continue;
            }
        }
        if !blockers.contains(&key) {
            blockers.push(key);
        }
    }
    Ok(blockers)
}

// Match the archive resolver: canonical UID first, legacy stanza-id fallback
// second. A pointer whose archive row was removed is discarded by the existing
// materializer and has no remaining archive position to order.
const PENDING_ORDINAL: &str = "COALESCE((SELECT archive_seq FROM mam_messages m WHERE m.room_jid = p.archive_stanza_by AND m.id = p.archive_stanza_id), (SELECT MIN(archive_seq) FROM mam_messages m WHERE m.room_jid = p.archive_stanza_by AND m.stanza_id = p.archive_stanza_id))";

async fn pending_store_exists(tx: &mut IngressUowTransaction<'_>) -> Result<bool, IngressUowError> {
    let query = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
        "SELECT 1 WHERE to_regclass('pending_delivery') IS NOT NULL"
    } else {
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'pending_delivery'"
    };
    let mut rows = tx.transaction_mut().query(query, ()).await?;
    Ok(rows.next().await?.is_some())
}

async fn pending_predecessor(
    tx: &mut IngressUowTransaction<'_>,
    position: &DispatchPosition,
    stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
) -> Result<bool, IngressUowError> {
    let recipient = position
        .resource
        .as_ref()
        .map(|target| target.to_bare().to_string())
        .unwrap_or_default();
    let query = format!("SELECT p.row_id FROM pending_delivery p WHERE p.payload_kind = 'archived' AND p.archive_stanza_by = ? AND (? = '' OR p.recipient_jid = ?) AND {PENDING_ORDINAL} < ? AND NOT (COALESCE(p.flushed_in_session = NULLIF(?, ''), FALSE) AND p.outbound_sequence IS NOT NULL) LIMIT 1");
    let mut rows = tx
        .transaction_mut()
        .query(
            &query,
            crate::db_params![
                position.archive.to_string(),
                recipient.clone(),
                recipient,
                position.ordinal.to_storage(),
                stream.map(|stream| stream.as_str()).unwrap_or("")
            ],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn resource_completed(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    receipt: &EffectReceiptKey,
    resource: &FullJid,
) -> Result<bool, IngressUowError> {
    let query = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
        "SELECT resource FROM ingress_delivery_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND resource = ? UNION ALL SELECT recipient FROM ingress_carbon_receipts WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND recipient = ? LIMIT 1"
    } else {
        "SELECT resource FROM ingress_delivery_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? AND resource = ? UNION ALL SELECT recipient FROM ingress_carbon_receipts WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? AND recipient = ? LIMIT 1"
    };
    let mut rows = tx
        .transaction_mut()
        .query(
            query,
            crate::db_params![
                key.to_storage().to_string(),
                receipt.kind.to_storage(),
                receipt.semantic_identity_hash.to_vec(),
                resource.to_string(),
                key.to_storage().to_string(),
                receipt.kind.to_storage(),
                receipt.semantic_identity_hash.to_vec(),
                resource.to_string()
            ],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

#[cfg(test)]
#[path = "archive_dispatch_tests.rs"]
mod tests;
