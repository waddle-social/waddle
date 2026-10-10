//! Upgrade scheduler ancestry without treating an old transport ACK as delivery.
use super::*;
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_uow::{run_with_retry, EffectDescendantRepository, IngressUowError};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, NotificationActivityMutation};

const PAGE: i64 = 64;

#[path = "legacy_ancestry/quarantine.rs"]
mod quarantine;

#[derive(Clone, Copy)]
enum AdoptionPhase {
    QuarantinePreflight,
    Ancestry,
}

#[cfg(test)]
#[path = "legacy_ancestry_tests.rs"]
mod tests;

struct Recorded {
    message: MessageKey,
    kind: i32,
    hash: Vec<u8>,
    intent: IngressEffectIntent,
}

enum CandidateBridge {
    Unmatched,
    Pending,
    Established,
}

struct LegacyCandidate {
    candidate: NotificationCandidate,
    outboxed: bool,
    suppressed: bool,
}

impl NotificationOutboxStore {
    /// Run after schema/identity adoption and before starting ingress GC. Each
    /// page and mutation is bounded; interrupted pending handoffs resume on the
    /// next startup. Settled bridges are never associated with newer jobs.
    pub(crate) async fn adopt_legacy_ancestry(&self) -> Result<(), NotificationOutboxError> {
        self.adopt_legacy_ancestry_phase(AdoptionPhase::Ancestry)
            .await
    }

    /// Preserve malformed audit values before a CHECK rebuild can reject them.
    pub(super) async fn quarantine_legacy_candidates_before_checks(
        &self,
    ) -> Result<(), NotificationOutboxError> {
        self.adopt_legacy_ancestry_phase(AdoptionPhase::QuarantinePreflight)
            .await
    }

    async fn adopt_legacy_ancestry_phase(
        &self,
        phase: AdoptionPhase,
    ) -> Result<(), NotificationOutboxError> {
        let mut tx = begin_bounded(&self.db).await?;
        if !has_table(&mut tx, "ingress_effect_descendants").await? {
            tx.commit().await?;
            return Ok(());
        }
        tx.commit().await?;
        let mut cursor: Option<(MessageKey, i32, Vec<u8>)> = None;
        loop {
            let recorded = load_page(&self.db, cursor.as_ref()).await?;
            if recorded.is_empty() {
                return Ok(());
            }
            for row in &recorded {
                self.adopt_recorded(row, phase).await?;
            }
            let last = recorded
                .last()
                .ok_or(IngressUowError::EffectIntentConflict)?;
            cursor = Some((last.message, last.kind, last.hash.clone()));
        }
    }

    async fn adopt_recorded(
        &self,
        recorded: &Recorded,
        phase: AdoptionPhase,
    ) -> Result<(), NotificationOutboxError> {
        let Some((owner, conversation, archive)) = target(&recorded.intent) else {
            return Ok(());
        };
        let mut tx = begin_bounded(&self.db).await?;
        let mut rows = tx.query(
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,outboxed_at_ms,suppressed_reason,quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND stanza_id_by = ? AND stanza_id = ? ORDER BY class LIMIT 16",
            crate::db_params![owner.to_string(),conversation.to_string(),archive.by.to_string(),archive.id.clone()],
        ).await?;
        let mut candidates = Vec::new();
        let mut malformed = Vec::new();
        while let Some(row) = rows.next().await? {
            let suppressed: Option<String> = row.get(16)?;
            let invalid_audit = suppressed
                .as_deref()
                .is_some_and(|reason| SuppressedReason::from_db_value(reason).is_err());
            let quarantined = row.get::<Option<i64>>(17)?.is_some();
            let candidate = match decode_candidate(&row) {
                Ok(candidate) if !invalid_audit && !quarantined => candidate,
                Ok(_) => {
                    malformed.push(row);
                    continue;
                }
                Err(error) if quarantine::is_candidate_data_error(&error) => {
                    malformed.push(row);
                    continue;
                }
                Err(error) => return Err(error),
            };
            candidates.push(LegacyCandidate {
                candidate,
                outboxed: row.get::<Option<i64>>(15)?.is_some(),
                suppressed: suppressed.is_some(),
            });
        }
        drop(rows);
        tx.commit().await?;
        for row in malformed {
            run_with_retry(5, || self.quarantine_recorded_candidate(recorded, &row))
                .await
                .map_err(|error| error.last_error)?;
        }
        if matches!(phase, AdoptionPhase::QuarantinePreflight) {
            return Ok(());
        }
        for candidate in candidates {
            let active = run_with_retry(5, || self.attach_legacy_candidate(recorded, &candidate))
                .await
                .map_err(|error| error.last_error)?;
            match active {
                CandidateBridge::Pending if candidate.outboxed && !candidate.suppressed => {
                    self.adopt_jobs(recorded, &candidate.candidate).await?
                }
                CandidateBridge::Established => {
                    self.reconcile_existing_jobs(recorded, &candidate.candidate)
                        .await?
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn attach_legacy_candidate(
        &self,
        recorded: &Recorded,
        legacy: &LegacyCandidate,
    ) -> Result<CandidateBridge, IngressUowError> {
        let id = legacy
            .candidate
            .delivery_id
            .ok_or(IngressUowError::EffectIntentConflict)?;
        let mut tx = begin_bounded(&self.db).await?;
        if !quarantine::lock_scanned_parent(&mut tx, recorded).await? {
            tx.commit().await?;
            return Ok(CandidateBridge::Unmatched);
        }
        if !Self::lock_unquarantined_candidate(&mut tx, &legacy.candidate).await? {
            tx.commit().await?;
            return Ok(CandidateBridge::Unmatched);
        }
        if !source_matches(&mut tx, recorded, &legacy.candidate).await? {
            tx.commit().await?;
            return Ok(CandidateBridge::Unmatched);
        }
        let query = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT settled_at IS NOT NULL FROM ingress_effect_descendants WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? AND descendant_key = ?"
        } else {
            "SELECT settled_at IS NOT NULL FROM ingress_effect_descendants WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? AND descendant_key = ?"
        };
        let mut rows = tx
            .query(
                query,
                crate::db_params![
                    recorded.message.to_storage().to_string(),
                    recorded.kind,
                    recorded.hash.clone(),
                    id.to_string()
                ],
            )
            .await?;
        let settled = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        drop(rows);
        if settled {
            tx.commit().await?;
            return Ok(CandidateBridge::Established);
        }
        EffectDescendantRepository::attach_raw(
            &mut tx,
            recorded.message,
            &recorded.intent.semantic_key(),
            id,
        )
        .await?;
        if legacy.suppressed {
            EffectDescendantRepository::settle_all_raw(&mut tx, id, chrono::Utc::now()).await?;
        }
        tx.commit().await?;
        Ok(if legacy.suppressed {
            CandidateBridge::Established
        } else {
            CandidateBridge::Pending
        })
    }

    async fn reconcile_existing_jobs(
        &self,
        recorded: &Recorded,
        candidate: &NotificationCandidate,
    ) -> Result<(), IngressUowError> {
        let id = candidate
            .delivery_id
            .ok_or(IngressUowError::EffectIntentConflict)?;
        let mut cursor = String::new();
        loop {
            let mut tx = begin_bounded(&self.db).await?;
            let mut rows = tx.query("SELECT job_id FROM notification_outbox_lineage WHERE candidate_delivery_id = ? AND settled_at_ms IS NULL AND job_id > ? ORDER BY job_id LIMIT ?", crate::db_params![id.to_string(), &cursor, PAGE]).await?;
            let mut jobs = Vec::new();
            while let Some(row) = rows.next().await? {
                let raw: String = row.get(0)?;
                jobs.push(
                    Uuid::parse_str(&raw).map_err(|_| IngressUowError::EffectIntentConflict)?,
                );
            }
            drop(rows);
            tx.commit().await?;
            if jobs.is_empty() {
                return Ok(());
            }
            for job in &jobs {
                run_with_retry(5, || self.adopt_job(recorded, candidate, *job, false))
                    .await
                    .map_err(|error| error.last_error)?;
            }
            cursor = jobs
                .last()
                .ok_or(IngressUowError::EffectIntentConflict)?
                .to_string();
        }
    }

    async fn adopt_jobs(
        &self,
        recorded: &Recorded,
        candidate: &NotificationCandidate,
    ) -> Result<(), IngressUowError> {
        let candidate_id = candidate
            .delivery_id
            .ok_or(IngressUowError::EffectIntentConflict)?;
        let mut cursor = String::new();
        let mut found = false;
        let mut ambiguous = false;
        loop {
            let mut tx = begin_bounded(&self.db).await?;
            if !quarantine::lock_scanned_parent(&mut tx, recorded).await?
                || !Self::lock_unquarantined_candidate(&mut tx, candidate).await?
            {
                tx.commit().await?;
                return Ok(());
            }
            let mut rows = tx.query(
                "SELECT job_id,context_xml,sender_jid,EXISTS(SELECT 1 FROM notification_outbox_lineage l WHERE l.candidate_delivery_id = ? AND l.job_id = notification_outbox.job_id) FROM notification_outbox WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND class = ? AND job_id > ? ORDER BY job_id LIMIT ?",
                crate::db_params![candidate_id.to_string(),candidate.recipient_bare_jid.to_string(),candidate.conversation_jid.to_string(),candidate.thread_id.as_str(),candidate.class.as_db_value(),&cursor,PAGE],
            ).await?;
            let mut jobs = Vec::new();
            let mut scanned = false;
            while let Some(row) = rows.next().await? {
                let raw: String = row.get(0)?;
                cursor.clone_from(&raw);
                scanned = true;
                let recorded_lineage: bool = row.get(3)?;
                let context: String = row.get(1)?;
                let sender: Option<String> = row.get(2)?;
                if recorded_lineage
                    || legacy_context_matches(candidate, &context, sender.as_deref())
                {
                    match Uuid::parse_str(&raw) {
                        Ok(job) => jobs.push((job, !recorded_lineage)),
                        Err(_) => ambiguous = true,
                    }
                } else {
                    // A different latest context can also hide this candidate in
                    // older coalesced fanout. It proves neither inclusion nor absence.
                    ambiguous = true;
                }
            }
            drop(rows);
            if ambiguous {
                retain_legacy_fanout_gap(&mut tx, recorded).await?;
            }
            tx.commit().await?;
            if !scanned {
                break;
            }
            for (job, require_context_match) in jobs {
                let adopted = run_with_retry(5, || {
                    self.adopt_job(recorded, candidate, job, require_context_match)
                })
                .await
                .map_err(|error| error.last_error)?;
                found |= adopted;
                ambiguous |= !adopted;
            }
        }
        // Unknown fanout remains on the candidate even after known children settle.
        // A partial reconstruction cannot discharge missing/coalesced history.
        if found || ambiguous {
            let id = candidate
                .delivery_id
                .ok_or(IngressUowError::EffectIntentConflict)?;
            run_with_retry(5, || async {
                let mut tx = begin_bounded(&self.db).await?;
                if !quarantine::lock_scanned_parent(&mut tx, recorded).await?
                    || !Self::lock_unquarantined_candidate(&mut tx, candidate).await?
                {
                    tx.commit().await?;
                    return Ok(());
                }
                let gap = legacy_fanout_gap_key(recorded);
                if ambiguous {
                    // Persist the observation: later pruning cannot turn an
                    // unknown target into evidence that the fanout was complete.
                    EffectDescendantRepository::attach_raw(
                        &mut tx, recorded.message, &recorded.intent.semantic_key(), gap,
                    ).await?;
                }
                let query = if tx.driver() == DatabaseDriver::Postgres {
                    "SELECT 1 FROM ingress_effect_descendants WHERE message_key = ?::uuid AND descendant_key = ? AND settled_at IS NULL"
                } else {
                    "SELECT 1 FROM ingress_effect_descendants WHERE message_key = ? AND descendant_key = ? AND settled_at IS NULL"
                };
                let mut rows = tx.query(query, crate::db_params![recorded.message.to_storage().to_string(), gap.to_string()]).await?;
                let gap_pending = rows.next().await?.is_some();
                drop(rows);
                if found && !ambiguous && !gap_pending {
                    EffectDescendantRepository::settle_all_raw(&mut tx, id, chrono::Utc::now()).await?;
                }
                tx.commit().await?;
                Ok(())
            })
            .await
            .map_err(|error| error.last_error)?;
        }
        Ok(())
    }

    async fn adopt_job(
        &self,
        recorded: &Recorded,
        candidate: &NotificationCandidate,
        job: Uuid,
        require_context_match: bool,
    ) -> Result<bool, IngressUowError> {
        let mut tx = begin_bounded(&self.db).await?;
        if !quarantine::lock_scanned_parent(&mut tx, recorded).await? {
            tx.commit().await?;
            return Ok(false);
        }
        if !Self::lock_unquarantined_candidate(&mut tx, candidate).await? {
            tx.commit().await?;
            return Ok(false);
        }
        let query = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT status,push_service_jid,node,context_xml,sender_jid FROM notification_outbox WHERE job_id = ? FOR UPDATE NOWAIT"
        } else {
            "SELECT status,push_service_jid,node,context_xml,sender_jid FROM notification_outbox WHERE job_id = ?"
        };
        let mut rows = tx.query(query, crate::db_params![job.to_string()]).await?;
        let Some(row) = rows.next().await? else {
            drop(rows);
            retain_legacy_fanout_gap(&mut tx, recorded).await?;
            tx.commit().await?;
            return Ok(false);
        };
        if require_context_match {
            let context: String = row.get(3)?;
            let sender: Option<String> = row.get(4)?;
            if !legacy_context_matches(candidate, &context, sender.as_deref()) {
                drop(rows);
                retain_legacy_fanout_gap(&mut tx, recorded).await?;
                tx.commit().await?;
                return Ok(false);
            }
        }
        let status: String = row.get(0)?;
        let service: String = row.get(1)?;
        let node: String = row.get(2)?;
        drop(rows);
        if !matches!(
            status.as_str(),
            STATUS_QUEUED | STATUS_IN_PROGRESS | STATUS_PUBLISHED | STATUS_FAILED
        ) {
            return Err(IngressUowError::EffectIntentConflict);
        }
        // Only a durable provider completion is proof. A Published outbox
        // records PubSub acceptance, and a generic Failed row can be a legacy
        // retry cap after an unknown outcome; neither proves provider success.
        let provider_done = if has_table(&mut tx, "push_publish_jobs").await? {
            let mut rows = tx.query(
                "SELECT 1 FROM push_publish_jobs WHERE item_id = ? AND owner_bare_jid = ? AND node = ? AND push_service_jid = ? AND status = 'published' AND acceptance_scope IN ('legacy', 'canonical') LIMIT 1",
                crate::db_params![job.to_string(),candidate.recipient_bare_jid.to_string(),node,service],
            ).await?;
            rows.next().await?.is_some()
        } else {
            false
        };
        EffectDescendantRepository::attach_raw(
            &mut tx,
            recorded.message,
            &recorded.intent.semantic_key(),
            job,
        )
        .await?;
        let candidate_id = candidate
            .delivery_id
            .ok_or(IngressUowError::EffectIntentConflict)?;
        tx.execute("INSERT INTO notification_outbox_lineage(candidate_delivery_id,job_id) VALUES(?,?) ON CONFLICT DO NOTHING",crate::db_params![candidate_id.to_string(),job.to_string()]).await?;
        if provider_done {
            // An explicit provider completion discharges the scheduling link.
            tx.execute("UPDATE notification_outbox_lineage SET settled_at_ms = ? WHERE candidate_delivery_id = ? AND job_id = ? AND settled_at_ms IS NULL",crate::db_params![crate::time::now_ms(),candidate_id.to_string(),job.to_string()]).await?;
            // Foundation's all-parent helper uses NOWAIT acquisitions and aborts
            // this whole transaction if a concurrent ancestor cannot be held.
            EffectDescendantRepository::lock_all_nowait_raw(&mut tx, job).await?;
            EffectDescendantRepository::settle_all_raw(&mut tx, job, chrono::Utc::now()).await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

fn legacy_fanout_gap_key(recorded: &Recorded) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"waddle.legacy-notification-fanout-gap.v1\0");
    hash.update(recorded.message.to_storage().as_bytes());
    hash.update(recorded.kind.to_be_bytes());
    hash.update(&recorded.hash);
    let mut identity = [0; 16];
    identity.copy_from_slice(&hash.finalize()[..16]);
    Uuid::from_bytes(identity)
}

async fn retain_legacy_fanout_gap(
    tx: &mut Transaction<'_>,
    recorded: &Recorded,
) -> Result<(), IngressUowError> {
    EffectDescendantRepository::attach_raw(
        tx,
        recorded.message,
        &recorded.intent.semantic_key(),
        legacy_fanout_gap_key(recorded),
    )
    .await
}

/// Only the latest exact message can be reconstructed from an old context.
/// Established lineage remains authoritative after later coalescing.
fn legacy_context_matches(
    candidate: &NotificationCandidate,
    context: &str,
    sender: Option<&str>,
) -> bool {
    let Some(sender) = sender.and_then(|raw| raw.parse::<Jid>().ok()) else {
        return false;
    };
    let Ok(context) = context.parse::<Element>() else {
        return false;
    };
    sender == candidate.sender_jid && context == build_waddle_context(candidate)
}

fn target(intent: &IngressEffectIntent) -> Option<(&BareJid, &BareJid, &StanzaId)> {
    match intent {
        IngressEffectIntent::NotificationActivityPreview {
            owner,
            mutation:
                NotificationActivityMutation::NotificationCandidate {
                    conversation,
                    archive_stanza_id,
                    ..
                }
                | NotificationActivityMutation::OfflineDelivery {
                    conversation,
                    archive_stanza_id,
                },
        } => Some((owner, conversation, archive_stanza_id)),
        IngressEffectIntent::GroupchatNotificationRecovery { mutation } => Some((
            &mutation.recipient,
            &mutation.room,
            &mutation.archive_stanza_id,
        )),
        _ => None,
    }
}

async fn source_matches(
    tx: &mut Transaction<'_>,
    recorded: &Recorded,
    candidate: &NotificationCandidate,
) -> Result<bool, IngressUowError> {
    if let IngressEffectIntent::GroupchatNotificationRecovery { mutation } = &recorded.intent {
        return Ok(mutation.sender == candidate.sender_jid
            && mutation
                .thread_id
                .as_ref()
                .map_or("", |thread| thread.as_str())
                == candidate.thread_id.as_str());
    }
    let envelope = crate::ingress_substrate::load_envelope(tx, recorded.message)
        .await?
        .ok_or(IngressUowError::EffectIntentConflict)?;
    let message = envelope.message();
    if message.from.as_ref() != Some(&candidate.sender_jid) {
        return Ok(false);
    }
    if matches!(
        candidate.class,
        NotificationClass::DirectMessage | NotificationClass::DirectMessageMention
    ) {
        let reconstructed = direct_candidate_from_envelope(
            message,
            &candidate.recipient_bare_jid,
            &candidate.sender_jid,
            &candidate.archive_stanza_id,
        )
        .map_err(|_| IngressUowError::EffectIntentConflict)?;
        return Ok(reconstructed.class == candidate.class
            && reconstructed.thread_id == candidate.thread_id);
    }
    Ok(true)
}

async fn load_page(
    db: &Database,
    cursor: Option<&(MessageKey, i32, Vec<u8>)>,
) -> Result<Vec<Recorded>, IngressUowError> {
    let mut tx = begin_bounded(db).await?;
    let mut rows = match (tx.driver(),cursor) {
        (DatabaseDriver::Postgres,Some((key,kind,hash))) => tx.query("SELECT message_key::text,kind::int,semantic_identity_hash,payload_version::int,payload FROM ingress_effect_intents WHERE kind IN (7,21) AND (message_key,kind,semantic_identity_hash) > (?::uuid,?,?) ORDER BY message_key,kind,semantic_identity_hash LIMIT ?",crate::db_params![key.to_storage().to_string(),*kind,hash.clone(),PAGE]).await?,
        (DatabaseDriver::Sqlite,Some((key,kind,hash))) => tx.query("SELECT message_key,kind,semantic_identity_hash,payload_version,payload FROM ingress_effect_intents WHERE kind IN (7,21) AND (message_key,kind,semantic_identity_hash) > (?,?,?) ORDER BY message_key,kind,semantic_identity_hash LIMIT ?",crate::db_params![key.to_storage().to_string(),*kind,hash.clone(),PAGE]).await?,
        (DatabaseDriver::Postgres,None) => tx.query("SELECT message_key::text,kind::int,semantic_identity_hash,payload_version::int,payload FROM ingress_effect_intents WHERE kind IN (7,21) ORDER BY message_key,kind,semantic_identity_hash LIMIT ?",crate::db_params![PAGE]).await?,
        (DatabaseDriver::Sqlite,None) => tx.query("SELECT message_key,kind,semantic_identity_hash,payload_version,payload FROM ingress_effect_intents WHERE kind IN (7,21) ORDER BY message_key,kind,semantic_identity_hash LIMIT ?",crate::db_params![PAGE]).await?,
    };
    let mut recorded = Vec::new();
    while let Some(row) = rows.next().await? {
        let key: String = row.get(0)?;
        let kind =
            i32::try_from(row.get::<i64>(1)?).map_err(|_| IngressUowError::EffectIntentConflict)?;
        let hash: Vec<u8> = row.get(2)?;
        if row.get::<i64>(3)? != 1 {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let payload: Vec<u8> = row.get(4)?;
        let intent = IngressEffectIntent::decode_v1(kind, &payload)?;
        if hash != Sha256::digest(intent.semantic_key().storage_identity().as_bytes()).as_slice() {
            return Err(IngressUowError::EffectIntentConflict);
        }
        recorded.push(Recorded {
            message: MessageKey::from_storage(
                key.parse()
                    .map_err(|_| IngressUowError::EffectIntentConflict)?,
            ),
            kind,
            hash,
            intent,
        });
    }
    drop(rows);
    tx.commit().await?;
    Ok(recorded)
}

async fn begin_bounded(db: &Database) -> Result<Transaction<'_>, IngressUowError> {
    let timeout = std::time::Duration::from_millis(100);
    let mut tx = tokio::time::timeout(timeout, db.begin_immediate())
        .await
        .map_err(|_| IngressUowError::Timeout)??;
    if !crate::ingress_substrate::set_local_transaction_timeouts(
        &mut tx,
        timeout,
        std::time::Duration::from_millis(250),
    )
    .await?
    {
        return Err(IngressUowError::TransactionBoundsUnproven);
    }
    Ok(tx)
}

async fn has_table(tx: &mut Transaction<'_>, name: &str) -> Result<bool, IngressUowError> {
    let query = if tx.driver() == DatabaseDriver::Postgres {
        "SELECT 1 WHERE to_regclass(?) IS NOT NULL"
    } else {
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?"
    };
    let mut rows = tx.query(query, crate::db_params![name]).await?;
    Ok(rows.next().await?.is_some())
}
