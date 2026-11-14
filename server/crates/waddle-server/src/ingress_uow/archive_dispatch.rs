//! Durable predecessors for archive-bearing delivery copies.
//!
//! Registration shares the archive insert transaction and its counter lock.
//! Readiness never acquires canonical locks or executes predecessor work;
//! callers bound any re-probes and defer persistent barriers to recovery.
use crate::db::Database;
use crate::db::Row;
use crate::db::Rows;
use crate::ingress_substrate::DefinerApiCandidateContext;
use crate::ingress_substrate::IngressSubstrateError;
use jid::{BareJid, FullJid};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::mam::ArchiveOrdinal;
use waddle_xmpp::pending_delivery::PendingRowId;

use super::{EffectReceiptRepository, IngressUowError, IngressUowTransaction};
use crate::db::DatabaseDriver;
use crate::ingress::decision::EffectReceiptKey;

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
    /// Supplemental offline custody beneath the original direct-route effect.
    /// The caller holds canonical and physical pending locks. Existing resource
    /// audiences stay frozen; no new archive position or effect is minted.
    pub(crate) async fn pin_pending_handoff(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        intent: &waddle_xmpp::ingress::IngressEffectIntent,
        row: &waddle_xmpp::pending_delivery::PendingRow,
        physical_id: &PendingRowId,
    ) -> Result<bool, IngressUowError> {
        use waddle_xmpp::ingress::IngressEffectIntent;
        use waddle_xmpp::pending_delivery::PendingPayload;
        let (IngressEffectIntent::RouteDirect { recipient, .. }, PendingPayload::Archived(stamp)) =
            (intent, &row.payload)
        else {
            return Err(IngressUowError::EffectIntentConflict);
        };
        if recipient != &row.recipient {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let recorded = super::EffectIntentRepository::load(tx, key).await?;
        if !recorded.contains(intent) {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let Some(ordinal) = recorded.iter().find_map(|recorded| match recorded {
            IngressEffectIntent::ArchiveAuthoritative {
                archive,
                by,
                stanza_id,
                ordinal,
                ..
            } if archive == recipient && by == &stamp.by.to_bare() && stanza_id == stamp => {
                Some(*ordinal)
            }
            _ => None,
        }) else {
            return Ok(false);
        };
        let mut rows = tx.transaction_mut().query(
            "SELECT 1 FROM pending_delivery WHERE row_id = ? AND recipient_jid = ? AND payload_kind = 'archived' AND archive_stanza_by = ? AND archive_stanza_id = ?",
            crate::db_params![physical_id.as_str(), recipient.to_string(), stamp.by.to_bare().to_string(), stamp.id.clone()],
        ).await?;
        if rows.next().await?.is_none() {
            return Err(IngressUowError::EffectIntentConflict);
        }
        drop(rows);
        let ordinal = match ordinal {
            Some(ordinal) => ordinal,
            None => {
                let mut rows = tx.transaction_mut().query(
                    "SELECT archive_seq FROM mam_messages WHERE room_jid = ? AND (id = ? OR stanza_id = ?)",
                    crate::db_params![recipient.to_string(), stamp.id.clone(), stamp.id.clone()],
                ).await?;
                let Some(stored) = rows.next().await? else {
                    return Ok(false);
                };
                let ordinal = ArchiveOrdinal::from_storage(stored.get::<i64>(0)?)
                    .map_err(|_| IngressUowError::EffectIntentConflict)?;
                if rows.next().await?.is_some() {
                    return Err(IngressUowError::EffectIntentConflict);
                }
                ordinal
            }
        };
        let receipt = crate::ingress::receipt_key(intent)?;
        for position in Self::positions(tx, key, &receipt).await? {
            if position.archive == *recipient && position.ordinal != ordinal {
                return Err(IngressUowError::EffectIntentConflict);
            }
        }
        let insert = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "INSERT INTO ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES (?, ?, ?::uuid, ?, ?, '', ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES (?, ?, ?, ?, ?, '', ?) ON CONFLICT DO NOTHING"
        };
        let (transaction, prepared) = tx.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_insert_archive_dispatch");
            super::candidate_boolean(
                transaction,
                &format!(
                    "SELECT {function}(?::text, ?::bigint, ?::uuid, ?::integer, ?::bytea, ?::text, ?::text)"
                ),
                crate::db_params![
                    recipient.to_string(),
                    ordinal.to_storage(),
                    key.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec(),
                    "",
                    physical_id.as_str(),
                ],
            )
            .await?;
        } else {
            transaction
                .execute(
                    insert,
                    crate::db_params![
                        recipient.to_string(),
                        ordinal.to_storage(),
                        key.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        physical_id.as_str(),
                    ],
                )
                .await?;
        }
        Ok(true)
    }

    pub(crate) async fn positions(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        receipt: &EffectReceiptKey,
    ) -> Result<Vec<waddle_xmpp::stream_management::ArchiveDispatchPosition>, IngressUowError> {
        let (transaction, prepared) = tx.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let function = api.qualified_function("waddle_ingress_list_archive_positions");
            let mut positions = Vec::new();
            let (mut after_archive, mut after_sequence) = (None::<String>, None::<i64>);
            loop {
                let mut rows = transaction
                    .query(
                        &format!(
                            "SELECT archive_jid, archive_seq FROM {function}(?::uuid, ?::integer, ?::bytea, ?::text, ?::bigint, 256)"
                        ),
                        crate::db_params![
                            key.to_storage().to_string(),
                            receipt.kind.to_storage(),
                            receipt.semantic_identity_hash.to_vec(),
                            after_archive.clone(),
                            after_sequence,
                        ],
                    )
                    .await?;
                let mut count = 0usize;
                while let Some(row) = rows.next().await? {
                    let archive: String = row.get(0)?;
                    let sequence: i64 = row.get(1)?;
                    let archive: BareJid = archive
                        .parse()
                        .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?;
                    let ordinal = ArchiveOrdinal::from_storage(sequence)
                        .map_err(|_| IngressUowError::EffectIntentConflict)?;
                    after_archive = Some(archive.to_string());
                    after_sequence = Some(ordinal.to_storage());
                    positions.push(waddle_xmpp::stream_management::ArchiveDispatchPosition {
                        archive,
                        ordinal,
                    });
                    count += 1;
                }
                drop(rows);
                if count < 256 {
                    break;
                }
            }
            return Ok(positions);
        }
        let query = crate::ingress_substrate::prepared_ingress_sql(
            transaction.driver(),
            prepared,
            crate::ingress_substrate::PreparedIngressRelation::ArchiveDispatch,
            (
                "SELECT DISTINCT archive_jid, archive_seq FROM ",
                " WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY archive_jid, archive_seq",
            ),
            positions_query(DatabaseDriver::Sqlite),
        );
        let rows = transaction
            .query(
                &query,
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
        db: &Database,
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
        validate_registration(tx, key, archive, ordinal, obligations).await?;
        let (transaction, prepared) = tx.prepared_parts();
        if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
            let insert_function = api.qualified_function("waddle_ingress_insert_archive_dispatch");
            let position_function =
                api.qualified_function("waddle_ingress_archive_dispatch_position");
            for obligation in obligations {
                let resource = match &obligation.target {
                    DispatchTarget::Resource(resource) => resource.to_string(),
                    DispatchTarget::ArchiveWide | DispatchTarget::Pending(_) => String::new(),
                };
                let pending = match &obligation.target {
                    DispatchTarget::Pending(id) => id.as_str(),
                    _ => "",
                };
                let receipt = &obligation.receipt;
                super::candidate_boolean(
                    transaction,
                    &format!(
                        "SELECT {insert_function}(?::text, ?::bigint, ?::uuid, ?::integer, ?::bytea, ?::text, ?::text)"
                    ),
                    crate::db_params![
                        archive.to_string(),
                        ordinal.to_storage(),
                        key.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        resource.clone(),
                        pending,
                    ],
                )
                .await?;
                let mut rows = transaction
                    .query(
                        &format!(
                            "SELECT {position_function}(?::text, ?::uuid, ?::integer, ?::bytea, ?::text, ?::text)"
                        ),
                        crate::db_params![
                            archive.to_string(),
                            key.to_storage().to_string(),
                            receipt.kind.to_storage(),
                            receipt.semantic_identity_hash.to_vec(),
                            resource,
                            pending,
                        ],
                    )
                    .await?;
                let row = rows
                    .next()
                    .await?
                    .ok_or(IngressSubstrateError::InvalidDefinerApiResult)?;
                let stored: Option<i64> = row.get(0)?;
                if rows.next().await?.is_some() || stored != Some(ordinal.to_storage()) {
                    return Err(IngressUowError::EffectIntentConflict);
                }
            }
            return Ok(());
        }
        let insert = crate::ingress_substrate::prepared_ingress_sql(
            transaction.driver(),
            prepared,
            crate::ingress_substrate::PreparedIngressRelation::ArchiveDispatch,
            (
                "INSERT INTO ",
                " (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES (?, ?, ?::uuid, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
            ),
            "INSERT INTO ingress_archive_dispatch (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
        );
        let select = crate::ingress_substrate::prepared_ingress_sql(
            transaction.driver(),
            prepared,
            crate::ingress_substrate::PreparedIngressRelation::ArchiveDispatch,
            (
                "SELECT archive_seq FROM ",
                " WHERE archive_jid = ? AND message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND resource = ? AND pending_row_id = ?",
            ),
            "SELECT archive_seq FROM ingress_archive_dispatch WHERE archive_jid = ? AND message_key = ? AND kind = ? AND semantic_identity_hash = ? AND resource = ? AND pending_row_id = ?",
        );
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
            transaction
                .execute(
                    &insert,
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
            let mut rows = transaction
                .query(
                    &select,
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
        resource: &FullJid,
        row_id: &PendingRowId,
        stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
    ) -> Result<DispatchReadiness, IngressUowError> {
        let recipient = resource.to_bare();
        let (transaction, prepared) = tx.prepared_parts();
        let mut positions = if let Some(api) =
            prepared.and_then(DefinerApiCandidateContext::from_prepared)
        {
            let function = api.qualified_function("waddle_ingress_list_pending_archive_positions");
            let mut positions = Vec::new();
            let (mut after_sequence, mut after_resource) = (None::<i64>, None::<String>);
            loop {
                let mut rows = transaction
                    .query(
                        &format!(
                            "SELECT archive_jid, archive_seq, resource FROM {function}(?::text, ?::text, ?::bigint, ?::text, 256)"
                        ),
                        crate::db_params![
                            recipient.to_string(),
                            row_id.as_str(),
                            after_sequence,
                            after_resource.clone(),
                        ],
                    )
                    .await?;
                let mut count = 0usize;
                while let Some(row) = rows.next().await? {
                    let position = decode_position(&row)?;
                    after_sequence = Some(position.ordinal.to_storage());
                    after_resource = Some(
                        position
                            .resource
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    );
                    positions.push(position);
                    count += 1;
                }
                drop(rows);
                if count < 256 {
                    break;
                }
            }
            positions
        } else {
            let mut rows = transaction.query(
                "SELECT DISTINCT archive_jid, archive_seq, resource FROM ingress_archive_dispatch WHERE archive_jid = ? AND pending_row_id = ?",
                crate::db_params![recipient.to_string(), row_id.as_str()],
            ).await?;
            let mut positions = Vec::new();
            while let Some(row) = rows.next().await? {
                positions.push(decode_position(&row)?);
            }
            drop(rows);
            positions
        };
        if positions.is_empty() && pending_store_exists(tx).await? {
            // SM detach promotion is an independent pending producer; recover
            // its position directly from its typed archive pointer.
            let sql = format!(
                "SELECT p.archive_stanza_by, {PENDING_ORDINAL}, '' FROM pending_delivery p WHERE p.row_id = ? AND p.recipient_jid = ? AND p.payload_kind = 'archived' AND {PENDING_ORDINAL} IS NOT NULL"
            );
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
        // The flush has selected its actual destination. Copies owed only to
        // another resource are outside this ordering chain; earlier pending
        // rows retain their wildcard registrations and still block this copy.
        for position in &mut positions {
            position.resource = Some(resource.clone());
        }
        positions_ready(tx, positions, stream).await
    }
}

fn positions_query(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => {
            "SELECT DISTINCT archive_jid, archive_seq FROM ingress_archive_dispatch WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ORDER BY archive_jid, archive_seq"
        }
        DatabaseDriver::Sqlite => {
            "SELECT DISTINCT archive_jid, archive_seq FROM ingress_archive_dispatch WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ORDER BY archive_jid, archive_seq"
        }
    }
}

async fn decode_positions(
    mut rows: Rows,
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
    let (transaction, prepared) = tx.prepared_parts();
    if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
        let function = api.qualified_function("waddle_ingress_list_archive_registration");
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
            let supplemental_pending = receipt.kind.to_storage()
                == waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag()
                && expected
                    .iter()
                    .all(|target| matches!(target, DispatchTarget::Resource(_)));
            let mut stored = Vec::new();
            let (mut after_resource, mut after_pending) = (None::<String>, None::<String>);
            loop {
                let mut rows = transaction
                    .query(
                        &format!(
                            "SELECT archive_seq, resource, pending_row_id FROM {function}(?::text, ?::uuid, ?::integer, ?::bytea, ?::text, ?::text, 256)"
                        ),
                        crate::db_params![
                            archive.to_string(),
                            key.to_storage().to_string(),
                            receipt.kind.to_storage(),
                            receipt.semantic_identity_hash.to_vec(),
                            after_resource.clone(),
                            after_pending.clone(),
                        ],
                    )
                    .await?;
                let mut count = 0usize;
                while let Some(row) = rows.next().await? {
                    let position: i64 = row.get(0)?;
                    let resource: String = row.get(1)?;
                    let pending: String = row.get(2)?;
                    if position != ordinal.to_storage() {
                        return Err(IngressUowError::EffectIntentConflict);
                    }
                    after_resource = Some(resource.clone());
                    after_pending = Some(pending.clone());
                    if !pending.is_empty() {
                        if supplemental_pending {
                            count += 1;
                            continue;
                        }
                        stored.push(DispatchTarget::Pending(PendingRowId::new(pending)));
                    } else if resource.is_empty() {
                        stored.push(DispatchTarget::ArchiveWide);
                    } else {
                        stored.push(DispatchTarget::Resource(
                            resource
                                .parse()
                                .map_err(|_| IngressUowError::InvalidStoredDeliveryResource)?,
                        ));
                    }
                    count += 1;
                }
                drop(rows);
                if count < 256 {
                    break;
                }
            }
            if !stored.is_empty()
                && (stored.iter().any(|target| !expected.contains(&target))
                    || expected.iter().any(|target| !stored.contains(target)))
            {
                return Err(IngressUowError::EffectIntentConflict);
            }
        }
        return Ok(());
    }
    let query = crate::ingress_substrate::prepared_ingress_sql(
        transaction.driver(),
        prepared,
        crate::ingress_substrate::PreparedIngressRelation::ArchiveDispatch,
        (
            "SELECT archive_seq, resource, pending_row_id FROM ",
            " WHERE archive_jid = ? AND message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ?",
        ),
        "SELECT archive_seq, resource, pending_row_id FROM ingress_archive_dispatch WHERE archive_jid = ? AND message_key = ? AND kind = ? AND semantic_identity_hash = ?",
    );
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
        let supplemental_pending = receipt.kind.to_storage()
            == waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag()
            && expected
                .iter()
                .all(|target| matches!(target, DispatchTarget::Resource(_)));
        let mut rows = transaction
            .query(
                &query,
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
                if supplemental_pending {
                    // A typed handoff pin adds physical custody without
                    // rewriting the original direct-route resource audience.
                    continue;
                }
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
macro_rules! predecessors_sql {
    ($holder_lost:literal) => {
        concat!(
            "
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
",
            $holder_lost,
            "ORDER BY p.archive_seq, p.message_key"
        )
    };
}

const PREDECESSORS: &str = predecessors_sql!("");

// A clustered owner commits a copy's completed send attempt right after the
// socket accepted it, and the resource progress in a later transaction. While
// the owner lives, the progress follows and the copy keeps holding later ones.
// An owner lost between the two commits leaves the completed attempt as the
// only delivery evidence (`ingress::live_delivery`), and the copy is already in
// the resource's stream. A completed attempt whose holder incarnation is
// committed as gone (its lease is expired or names a later epoch) therefore
// releases later copies for that resource; recovery records the progress after
// its grace. An attempt that only started has an unknown outcome and still
// holds them, and a holder without a lease row has unknown liveness.
#[cfg(feature = "clustering")]
const PREDECESSORS_CLUSTERED: &str = predecessors_sql!(
    "    AND NOT EXISTS (SELECT 1 FROM ingress_send_attempts a
        JOIN clustering_nodes n ON n.node_id = a.node_id
        WHERE p.resource <> '' AND a.message_key = p.message_key AND a.kind = p.kind
            AND a.semantic_identity_hash = p.semantic_identity_hash AND a.recipient = p.resource
            AND a.state = 2 AND (n.expired OR n.node_epoch <> a.node_incarnation))
"
);

fn decode_message(stored: &str) -> Result<MessageKey, IngressUowError> {
    stored
        .parse()
        .map(MessageKey::from_storage)
        .map_err(|_| IngressSubstrateError::InvalidStoredMessageKey.into())
}

struct DispatchPosition {
    archive: BareJid,
    ordinal: ArchiveOrdinal,
    resource: Option<FullJid>,
}

fn decode_position(row: &Row) -> Result<DispatchPosition, IngressUowError> {
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
    let (transaction, prepared) = tx.prepared_parts();
    if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
        let function = api.qualified_function("waddle_ingress_list_receipt_archive_positions");
        let target = resource.map(ToString::to_string).unwrap_or_default();
        let (mut after_archive, mut after_sequence, mut after_resource) =
            (None::<String>, None::<i64>, None::<String>);
        let mut positions = Vec::new();
        loop {
            let mut rows = transaction
                .query(
                    &format!(
                        "SELECT archive_jid, archive_seq, resource FROM {function}(?::uuid, ?::integer, ?::bytea, ?::text, ?::text, ?::bigint, ?::text, 256)"
                    ),
                    crate::db_params![
                        key.to_storage().to_string(),
                        receipt.kind.to_storage(),
                        receipt.semantic_identity_hash.to_vec(),
                        target.clone(),
                        after_archive.clone(),
                        after_sequence,
                        after_resource.clone(),
                    ],
                )
                .await?;
            let mut count = 0usize;
            while let Some(row) = rows.next().await? {
                let position = decode_position(&row)?;
                after_archive = Some(position.archive.to_string());
                after_sequence = Some(position.ordinal.to_storage());
                after_resource = Some(
                    position
                        .resource
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                );
                positions.push(position);
                count += 1;
            }
            drop(rows);
            if count < 256 {
                break;
            }
        }
        return positions_ready(tx, positions, stream).await;
    }
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
    #[cfg(feature = "clustering")]
    let predecessors = if matches!(tx.fencing(), super::IngressFencing::Clustered(_)) {
        PREDECESSORS_CLUSTERED
    } else {
        PREDECESSORS
    };
    #[cfg(not(feature = "clustering"))]
    let predecessors = PREDECESSORS;
    let mut candidates = Vec::new();
    let (transaction, prepared) = tx.prepared_parts();
    if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
        let function = api.qualified_function("waddle_ingress_list_archive_predecessors");
        let (
            mut after_sequence,
            mut after_message,
            mut after_kind,
            mut after_hash,
            mut after_resource,
            mut after_pending,
        ) = (
            None::<i64>,
            None::<String>,
            None::<i32>,
            None::<Vec<u8>>,
            None::<String>,
            None::<String>,
        );
        loop {
            let mut rows = transaction
                .query(
                    &format!(
                        "SELECT message_key, pending_row_id, receipted, archive_seq, kind, semantic_identity_hash, resource FROM {function}(?::text, ?::bigint, ?::text, ?::bigint, ?::uuid, ?::integer, ?::bytea, ?::text, ?::text, 256)"
                    ),
                    crate::db_params![
                        position.archive.to_string(),
                        position.ordinal.to_storage(),
                        target.clone(),
                        after_sequence,
                        after_message.clone(),
                        after_kind,
                        after_hash.clone(),
                        after_resource.clone(),
                        after_pending.clone(),
                    ],
                )
                .await?;
            let mut count = 0usize;
            while let Some(row) = rows.next().await? {
                let stored: String = row.get(0)?;
                let pending: String = row.get(1)?;
                let receipted: bool = row.get(2)?;
                after_sequence = Some(row.get(3)?);
                after_message = Some(stored.clone());
                after_kind = Some(row.get(4)?);
                after_hash = Some(row.get(5)?);
                after_resource = Some(row.get(6)?);
                after_pending = Some(pending.clone());
                candidates.push((
                    decode_message(&stored)?,
                    if pending.is_empty() {
                        None
                    } else {
                        Some(PendingRowId::new(pending))
                    },
                    receipted,
                ));
                count += 1;
            }
            drop(rows);
            if count < 256 {
                break;
            }
        }
    } else {
        let mut rows = transaction
            .query(
                predecessors,
                crate::db_params![
                    position.archive.to_string(),
                    position.ordinal.to_storage(),
                    target.clone(),
                    target
                ],
            )
            .await?;
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
    }
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

// Match the row the archive resolver actually replays: newest UID or stanza-id
// match. Removed pointers have no remaining archive position and are discarded
// by the existing materializer.
const PENDING_ORDINAL: &str = "(SELECT MAX(archive_seq) FROM mam_messages m WHERE m.room_jid = p.archive_stanza_by AND (m.id = p.archive_stanza_id OR m.stanza_id = p.archive_stanza_id))";

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
    let query = format!(
        "SELECT p.row_id FROM pending_delivery p WHERE p.payload_kind = 'archived' AND p.archive_stanza_by = ? AND (? = '' OR p.recipient_jid = ?) AND {PENDING_ORDINAL} < ? AND NOT (COALESCE(p.flushed_in_session = NULLIF(?, ''), FALSE) AND p.outbound_sequence IS NOT NULL) LIMIT 1"
    );
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
    let (transaction, prepared) = tx.prepared_parts();
    if let Some(api) = prepared.and_then(DefinerApiCandidateContext::from_prepared) {
        let function = api.qualified_function("waddle_ingress_resource_receipt_exists");
        return super::candidate_boolean(
            transaction,
            &format!("SELECT {function}(?::uuid, ?::integer, ?::bytea, ?::text)"),
            crate::db_params![
                key.to_storage().to_string(),
                receipt.kind.to_storage(),
                receipt.semantic_identity_hash.to_vec(),
                resource.to_string(),
            ],
        )
        .await;
    }
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
