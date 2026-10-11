//! Atomic notification candidate and recovery writes beneath the canonical lock.
use waddle_xmpp::{
    inbox::storage::GroupchatNotificationRecoveryKey,
    ingress::{IngressEffectIntent, MessageKey, NotificationActivityMutation},
};

use super::{IngressUowError, IngressUowTransaction};
use crate::notification_outbox::{
    NotificationCandidate, NotificationCandidateInsertOutcome, NotificationOutboxStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryCompletion {
    Completed,
    AlreadyCompleted,
    Missing,
}

async fn pending_notification_sources(
    tx: &mut IngressUowTransaction<'_>,
    row: &waddle_xmpp::pending_delivery::PendingRow,
) -> Result<Vec<PendingNotificationSource>, IngressUowError> {
    let waddle_xmpp::pending_delivery::PendingPayload::Archived(stamp) = &row.payload else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    let (routes, route_seed) = pending_route_notification_sources(tx, row).await?;
    let mut rows = tx.transaction_mut().query(
        "SELECT CAST(d.message_key AS TEXT), d.kind, d.semantic_identity_hash, i.payload_version, i.payload, d.pending_row_id FROM ingress_archive_dispatch d JOIN ingress_effect_intents i ON i.message_key = d.message_key AND i.kind = d.kind AND i.semantic_identity_hash = d.semantic_identity_hash WHERE d.archive_jid = ? AND d.pending_row_id <> '' AND d.archive_seq IN (SELECT archive_seq FROM ingress_archive_dispatch WHERE archive_jid = ? AND pending_row_id = ?) ORDER BY d.message_key, d.kind, d.semantic_identity_hash, d.pending_row_id",
        crate::db_params![row.recipient.to_string(), row.recipient.to_string(), row.id.as_str()],
    ).await?;
    let mut sources = Vec::new();
    while let Some(source) = rows.next().await? {
        let key: String = source.get(0)?;
        let message = MessageKey::from_storage(
            key.parse()
                .map_err(|_| IngressUowError::EffectIntentConflict)?,
        );
        let kind = i32::try_from(source.get::<i64>(1)?)
            .map_err(|_| IngressUowError::EffectIntentConflict)?;
        let hash: Vec<u8> = source.get(2)?;
        if source.get::<i64>(3)? != 1 {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let payload: Vec<u8> = source.get(4)?;
        let intent = IngressEffectIntent::decode_v1(kind, &payload)?;
        let pending_id: String = source.get(5)?;
        match &intent {
            IngressEffectIntent::PendingDelivery {
                mutation:
                    waddle_xmpp::ingress::PendingDeliveryMutation::Archived {
                        recipient,
                        row_id,
                        archive_stanza_id,
                    },
            } if recipient == &row.recipient
                && archive_stanza_id == stamp
                && row_id.as_str() == pending_id => {}
            IngressEffectIntent::RouteDirect { .. }
                if routes.contains(&PendingNotificationSource {
                    message,
                    intent: intent.clone(),
                }) => {}
            _ => return Err(IngressUowError::EffectIntentConflict),
        }
        let receipt = crate::ingress::receipt_key(&intent)?;
        if receipt.kind.to_storage() != kind || receipt.semantic_identity_hash.as_slice() != hash {
            return Err(IngressUowError::EffectIntentConflict);
        }
        sources.push(PendingNotificationSource { message, intent });
    }
    drop(rows);
    if !sources.is_empty() || route_seed {
        sources.extend(routes);
    }
    sources.sort_by_key(|source| source.message.to_storage());
    sources.dedup();
    Ok(sources)
}

async fn pending_route_notification_sources(
    tx: &mut IngressUowTransaction<'_>,
    row: &waddle_xmpp::pending_delivery::PendingRow,
) -> Result<(Vec<PendingNotificationSource>, bool), IngressUowError> {
    use waddle_xmpp::ingress::IngressEffectKind;
    let waddle_xmpp::pending_delivery::PendingPayload::Archived(stamp) = &row.payload else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    // This constructs a lookup identity, never a new authority/receipt. Every
    // result must carry an independently validated stored archive obligation.
    let archive_lookup = IngressEffectIntent::ArchiveAuthoritative {
        ordinal: None,
        archive: row.recipient.clone(),
        by: stamp.by.to_bare(),
        stanza_id: stamp.clone(),
        archived_at: row.original_receipt_at,
    };
    let archive_receipt = crate::ingress::receipt_key(&archive_lookup)?;
    let mut rows = tx.transaction_mut().query(
        "SELECT CAST(r.message_key AS TEXT), r.kind, r.semantic_identity_hash, r.payload_version, r.payload, a.payload_version, a.payload FROM ingress_effect_intents r JOIN ingress_effect_intents a ON a.message_key = r.message_key WHERE r.kind = ? AND a.kind = ? AND a.semantic_identity_hash = ? ORDER BY r.message_key, r.semantic_identity_hash",
        crate::db_params![IngressEffectKind::RouteDirect.storage_tag(), IngressEffectKind::ArchiveAuthoritative.storage_tag(), archive_receipt.semantic_identity_hash.to_vec()],
    ).await?;
    let mut sources = Vec::new();
    let mut seeded = false;
    while let Some(source) = rows.next().await? {
        let key: String = source.get(0)?;
        let message = MessageKey::from_storage(
            key.parse()
                .map_err(|_| IngressUowError::EffectIntentConflict)?,
        );
        if source.get::<i64>(3)? != 1 || source.get::<i64>(5)? != 1 {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let archive_payload: Vec<u8> = source.get(6)?;
        let archive = IngressEffectIntent::decode_v1(
            IngressEffectKind::ArchiveAuthoritative.storage_tag(),
            &archive_payload,
        )?;
        if !matches!(&archive, IngressEffectIntent::ArchiveAuthoritative { archive, by, stanza_id, .. }
            if archive == &row.recipient && by == &stamp.by.to_bare() && stanza_id == stamp)
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let kind = i32::try_from(source.get::<i64>(1)?)
            .map_err(|_| IngressUowError::EffectIntentConflict)?;
        let payload: Vec<u8> = source.get(4)?;
        let intent = IngressEffectIntent::decode_v1(kind, &payload)?;
        if !matches!(&intent, IngressEffectIntent::RouteDirect { recipient, .. } if recipient == &row.recipient)
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let receipt = crate::ingress::receipt_key(&intent)?;
        let hash: Vec<u8> = source.get(2)?;
        if receipt.semantic_identity_hash.as_slice() != hash {
            return Err(IngressUowError::EffectIntentConflict);
        }
        seeded |= crate::ingress::ambiguous_offline_pending_id(message, &receipt) == row.id;
        sources.push(PendingNotificationSource { message, intent });
    }
    Ok((sources, seeded))
}

async fn lock_pending_notification_row(
    tx: &mut IngressUowTransaction<'_>,
    expected: &waddle_xmpp::pending_delivery::PendingRow,
) -> Result<bool, IngressUowError> {
    let query = if tx.transaction_mut().driver() == crate::db::DatabaseDriver::Postgres {
        "SELECT recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id, flushed_in_session, notification_outboxed_at_ms FROM pending_delivery WHERE row_id = ? FOR UPDATE NOWAIT"
    } else {
        "SELECT recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id, flushed_in_session, notification_outboxed_at_ms FROM pending_delivery WHERE row_id = ?"
    };
    let mut rows = tx
        .transaction_mut()
        .query(query, crate::db_params![expected.id.as_str()])
        .await
        .map_err(super::canonical_nowait_error)?;
    let Some(row) = rows.next().await.map_err(super::canonical_nowait_error)? else {
        return Ok(false);
    };
    if row.get::<Option<String>>(5)?.is_some() || row.get::<Option<i64>>(6)?.is_some() {
        return Ok(false);
    }
    let waddle_xmpp::pending_delivery::PendingPayload::Archived(stamp) = &expected.payload else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    let recipient: String = row.get(0)?;
    let by: Option<String> = row.get(3)?;
    let id: Option<String> = row.get(4)?;
    if recipient != expected.recipient.to_string()
        || row.get::<i64>(1)? != expected.original_receipt_at.timestamp_millis()
        || row.get::<String>(2)? != "archived"
        || by.as_deref() != Some(stamp.by.to_bare().as_str())
        || id.as_deref() != Some(stamp.as_str())
    {
        return Err(IngressUowError::EffectIntentConflict);
    }
    Ok(true)
}

fn pending_candidate_from_envelope(
    envelope: &crate::ingress_substrate::MessageEnvelope,
    row: &waddle_xmpp::pending_delivery::PendingRow,
) -> Result<Option<NotificationCandidate>, IngressUowError> {
    let waddle_xmpp::pending_delivery::PendingPayload::Archived(stamp) = &row.payload else {
        return Err(IngressUowError::EffectIntentConflict);
    };
    let message = envelope.message();
    let sender = message
        .from
        .as_ref()
        .ok_or(IngressUowError::EffectIntentConflict)?;
    match crate::notification_outbox::direct_candidate_from_envelope(
        message,
        &row.recipient,
        sender,
        stamp,
    ) {
        Ok(candidate) => Ok(Some(candidate)),
        Err(
            crate::notification_outbox::NotificationOutboxError::SelfDirectedNotificationCandidate(
                _,
            ),
        ) => Ok(None),
        Err(_) => Err(IngressUowError::EffectIntentConflict),
    }
}

pub(crate) struct RecoveryReceiptRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingNotificationSource {
    message: MessageKey,
    intent: IngressEffectIntent,
}

/// Frozen canonical preparation. Construction stays inside the repository;
/// completion revalidates the physical row and every source beneath locks.
#[derive(Debug)]
pub(crate) struct PendingNotificationRecovery {
    row: waddle_xmpp::pending_delivery::PendingRow,
    envelope: Box<crate::ingress_substrate::MessageEnvelope>,
    candidate: Option<NotificationCandidate>,
    sources: Vec<PendingNotificationSource>,
}

impl PendingNotificationRecovery {
    pub(crate) fn envelope(&self) -> &crate::ingress_substrate::MessageEnvelope {
        &self.envelope
    }
}

#[derive(Debug)]
pub(crate) enum PendingNotificationPreparation {
    SourceGone,
    Ready(Box<PendingNotificationRecovery>),
}

impl RecoveryReceiptRepository {
    pub(crate) async fn attach_existing_pending_notification(
        tx: &mut IngressUowTransaction<'_>,
        message: MessageKey,
        intent: &IngressEffectIntent,
        envelope: &crate::ingress_substrate::MessageEnvelope,
        row: &waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<bool, IngressUowError> {
        let Some(candidate) = pending_candidate_from_envelope(envelope, row)? else {
            return Ok(false);
        };
        NotificationOutboxStore::attach_existing_candidate_lineage_in_transaction(
            tx.transaction_mut(),
            message,
            &intent.semantic_key(),
            &candidate,
        )
        .await
    }

    /// Resolve host-owned pending provenance before taking any pending or
    /// candidate lock. Same-archive-copy parents must independently agree on
    /// the exact archived pointer and frozen notification, never just ordinal.
    pub(crate) async fn prepare_pending_notification(
        tx: &mut IngressUowTransaction<'_>,
        row: &waddle_xmpp::pending_delivery::PendingRow,
    ) -> Result<PendingNotificationPreparation, IngressUowError> {
        let sources = pending_notification_sources(tx, row).await?;
        let mut locked = std::collections::BTreeSet::new();
        for source in &sources {
            if locked.insert(source.message.to_storage()) {
                if let Err(error) =
                    super::EffectDescendantRepository::lock_nowait(tx, source.message).await
                {
                    return Err(
                        if matches!(error, IngressUowError::EffectIntentMessageMissing) {
                            IngressUowError::Database {
                                retry_class: super::DbRetryClass::CanonicalLockContention,
                            }
                        } else {
                            error
                        },
                    );
                }
            }
        }
        if !lock_pending_notification_row(tx, row).await? {
            return Ok(PendingNotificationPreparation::SourceGone);
        }
        if pending_notification_sources(tx, row).await? != sources {
            // A new parent must never be acquired after the pending child.
            return Err(IngressUowError::Database {
                retry_class: super::DbRetryClass::CanonicalLockContention,
            });
        }
        let mut frozen: Option<(
            Box<crate::ingress_substrate::MessageEnvelope>,
            Option<NotificationCandidate>,
        )> = None;
        for source in &sources {
            let envelope = super::CanonicalMessageRepository::load_envelope(tx, source.message)
                .await?
                .ok_or(IngressUowError::EffectIntentMessageMissing)?;
            let candidate = pending_candidate_from_envelope(&envelope, row)?;
            if let Some((_, expected)) = &frozen {
                if expected != &candidate {
                    return Err(IngressUowError::EffectIntentConflict);
                }
            } else {
                frozen = Some((Box::new(envelope), candidate));
            }
        }
        let (envelope, candidate) = frozen.ok_or(IngressUowError::EffectIntentMessageMissing)?;
        Ok(PendingNotificationPreparation::Ready(Box::new(
            PendingNotificationRecovery {
                row: row.clone(),
                envelope,
                candidate,
                sources,
            },
        )))
    }

    /// Add scheduling custody and the pending marker in the same UoW. Existing
    /// pending receipts are untouched; this is not notification/provider proof.
    pub(crate) async fn complete_pending_notification(
        tx: &mut IngressUowTransaction<'_>,
        prepared: &PendingNotificationRecovery,
        candidate: Option<&NotificationCandidate>,
    ) -> Result<RecoveryCompletion, IngressUowError> {
        let fresh = Self::prepare_pending_notification(tx, &prepared.row).await?;
        let PendingNotificationPreparation::Ready(fresh) = fresh else {
            return Ok(RecoveryCompletion::Missing);
        };
        if fresh.candidate != prepared.candidate
            || candidate.is_some_and(|candidate| fresh.candidate.as_ref() != Some(candidate))
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        if let Some(candidate) = candidate {
            NotificationOutboxStore::insert_candidate_in_transaction(
                tx.transaction_mut(),
                candidate,
                fresh.row.original_receipt_at.timestamp_millis(),
            )
            .await?;
            for source in &fresh.sources {
                NotificationOutboxStore::attach_candidate_lineage_in_transaction(
                    tx.transaction_mut(),
                    source.message,
                    &source.intent.semantic_key(),
                    candidate,
                )
                .await?;
            }
        }
        if tx.transaction_mut().execute(
            "UPDATE pending_delivery SET notification_outboxed_at_ms = ? WHERE row_id = ? AND flushed_in_session IS NULL AND notification_outboxed_at_ms IS NULL",
            crate::db_params![crate::time::now_ms(), fresh.row.id.as_str()],
        ).await? != 1
        {
            return Err(IngressUowError::EffectIntentConflict);
        }
        Ok(RecoveryCompletion::Completed)
    }

    pub(crate) async fn insert_candidate(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        candidate: &NotificationCandidate,
        created_at_ms: i64,
    ) -> Result<NotificationCandidateInsertOutcome, IngressUowError> {
        if !super::CanonicalMessageRepository::lock(tx, message_key).await? {
            return Err(IngressUowError::EffectIntentMessageMissing);
        }
        let intents = super::EffectIntentRepository::load(tx, message_key).await?;
        let authority = intents
            .iter()
            .find(|intent| match intent {
                IngressEffectIntent::NotificationActivityPreview { owner, mutation } => {
                    owner == candidate.recipient_bare_jid()
                        && match mutation {
                            NotificationActivityMutation::NotificationCandidate {
                                archive_stanza_id,
                                ..
                            }
                            | NotificationActivityMutation::OfflineDelivery {
                                archive_stanza_id,
                                ..
                            } => archive_stanza_id == candidate.archive_stanza_id(),
                            _ => false,
                        }
                }
                IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                    &mutation.recipient == candidate.recipient_bare_jid()
                        && &mutation.room == candidate.conversation_jid()
                        && &mutation.archive_stanza_id == candidate.archive_stanza_id()
                }
                _ => false,
            })
            .ok_or(IngressUowError::EffectIntentConflict)?;
        let outcome = NotificationOutboxStore::insert_candidate_in_transaction(
            tx.transaction_mut(),
            candidate,
            created_at_ms,
        )
        .await?;
        NotificationOutboxStore::attach_candidate_lineage_in_transaction(
            tx.transaction_mut(),
            message_key,
            &authority.semantic_key(),
            candidate,
        )
        .await?;
        Ok(outcome)
    }

    pub(crate) async fn complete(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<RecoveryCompletion, IngressUowError> {
        let changed = tx.transaction_mut().execute(
            "UPDATE groupchat_notification_recovery SET completed_at_ms = ? WHERE message_key = ? AND recipient_bare_jid = ? AND room_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND completed_at_ms IS NULL",
            crate::db_params![crate::time::now_ms(), message_key.to_storage().to_string(), key.recipient.to_string(), key.room.to_string(), key.thread_id.clone().unwrap_or_default(), key.archive_stanza_id.by.to_string(), key.archive_stanza_id.id.clone()],
        ).await?;
        if changed > 0 {
            return Ok(RecoveryCompletion::Completed);
        }
        Ok(if Self::is_completed(tx, message_key, key).await? {
            RecoveryCompletion::AlreadyCompleted
        } else {
            RecoveryCompletion::Missing
        })
    }

    pub(crate) async fn is_completed(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<bool, IngressUowError> {
        let mut rows = tx.transaction_mut().query(
            "SELECT 1 FROM groupchat_notification_recovery WHERE message_key = ? AND recipient_bare_jid = ? AND room_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ? AND completed_at_ms IS NOT NULL",
            crate::db_params![message_key.to_storage().to_string(), key.recipient.to_string(), key.room.to_string(), key.thread_id.clone().unwrap_or_default(), key.archive_stanza_id.by.to_string(), key.archive_stanza_id.id.clone()],
        ).await?;
        Ok(rows.next().await?.is_some())
    }

    pub(crate) async fn delete(
        tx: &mut IngressUowTransaction<'_>,
        message_key: MessageKey,
        key: &GroupchatNotificationRecoveryKey,
    ) -> Result<(), IngressUowError> {
        tx.transaction_mut().execute(
            "DELETE FROM groupchat_notification_recovery WHERE message_key = ? AND recipient_bare_jid = ? AND room_jid = ? AND thread_id = ? AND stanza_id_by = ? AND stanza_id = ?",
            crate::db_params![message_key.to_storage().to_string(), key.recipient.to_string(), key.room.to_string(), key.thread_id.clone().unwrap_or_default(), key.archive_stanza_id.by.to_string(), key.archive_stanza_id.id.clone()],
        ).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn postgres_pending_recovery_uses_read_committed_before_epoch_reads() {
        let Some(fixture) =
            crate::ingress::test_support::IngressFixture::postgres("pending_recovery_isolation")
                .await
        else {
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query(
                        "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
                    )
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect(fixture.db.database_url())
            .await
            .expect("fixture-local repeatable-read pool");
        let db = crate::db::Database::from_postgres_pool("recovery-isolation", pool.clone());
        let uow = super::super::IngressUnitOfWork::open(db, fixture.uow.lineage.clone())
            .expect("same attested fixture lineage");
        let mut ordinary = uow.begin().await.expect("ordinary epoch-proven UoW");
        assert_eq!(isolation(&mut ordinary).await, "repeatable read");
        ordinary.commit().await.expect("ordinary commit");

        // BEGIN must override the session default before timeout and epoch
        // SELECTs: PostgreSQL rejects a later isolation change after any read.
        let mut recovery = uow
            .begin_read_committed_with_timeouts(
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(250),
            )
            .await
            .expect("recovery begins before its first snapshot is pinned");
        assert_eq!(isolation(&mut recovery).await, "read committed");
        recovery.commit().await.expect("recovery commit");
        let mut ordinary = uow.begin().await.expect("unchanged session default");
        assert_eq!(isolation(&mut ordinary).await, "repeatable read");
        ordinary.commit().await.expect("final ordinary commit");
        drop(uow);
        pool.close().await;
        fixture.close().await;
    }

    async fn isolation(tx: &mut IngressUowTransaction<'_>) -> String {
        let mut rows = tx
            .transaction_mut()
            .query("SHOW transaction_isolation", ())
            .await
            .expect("transaction isolation");
        rows.next()
            .await
            .expect("isolation row")
            .expect("isolation exists")
            .get(0)
            .expect("isolation text")
    }
}
