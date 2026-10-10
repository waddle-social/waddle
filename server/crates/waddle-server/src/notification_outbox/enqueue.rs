//! T0 candidate persistence: idempotent inserts and counts.

use super::*;

impl NotificationOutboxStore {
    pub async fn insert_candidate(
        &self,
        candidate: &NotificationCandidate,
    ) -> Result<NotificationCandidateInsertOutcome, NotificationOutboxError> {
        let mut tx = self.db.begin().await?;
        let outcome =
            Self::insert_candidate_in_transaction(&mut tx, candidate, crate::time::now_ms())
                .await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// Use the caller's transaction so candidate and ingress receipts commit together.
    /// Preserve the original receipt timestamp so recovery retains outbox ordering.
    pub(crate) async fn insert_candidate_in_transaction(
        tx: &mut crate::db::Transaction<'_>,
        candidate: &NotificationCandidate,
        created_at_ms: i64,
    ) -> Result<NotificationCandidateInsertOutcome, DatabaseError> {
        let inserted = tx
            .execute(
                r#"
                INSERT INTO notification_candidates (
                    recipient_bare_jid,
                    conversation_jid,
                    sender_jid,
                    thread_id,
                    stanza_id_by,
                    stanza_id,
                    class,
                    reason,
                    created_at_ms,
                    policy_error_count,
                    next_attempt_at_ms,
                    outboxed_at_ms,
                    suppressed_reason,
                    noping,
                    no_store,
                    no_permanent_store,
                    last_message_body,
                    reaction,
                    delivery_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, ?, ?, ?)
                ON CONFLICT DO NOTHING
                "#,
                crate::db_params![
                    candidate.recipient_bare_jid.to_string(),
                    candidate.conversation_jid.to_string(),
                    candidate.sender_jid.to_string(),
                    candidate.thread_id.as_str(),
                    candidate.archive_stanza_id.by.to_string(),
                    candidate.archive_stanza_id.id.clone(),
                    candidate.class.as_db_value(),
                    candidate.reason.as_db_value(),
                    created_at_ms,
                    0_i64,
                    i64::from(candidate.noping),
                    i64::from(candidate.no_store),
                    i64::from(candidate.no_permanent_store),
                    candidate.last_message_body.clone(),
                    i64::from(candidate.reaction),
                    uuid::Uuid::new_v4().to_string(),
                ],
            )
            .await?;
        if inserted == 0 {
            // Adopt missing scheduling identity without changing approval.
            Self::candidate_delivery_id_in_transaction(tx, candidate).await?;
            // UNIQUE-constraint collision. `notification_candidates`
            // carries TWO intentional unique constraints, both of
            // which the `ON CONFLICT DO NOTHING` (no target)
            // suppresses:
            //
            // 1. The PRIMARY KEY on `(recipient_bare_jid,
            //    conversation_jid, thread_id, stanza_id_by,
            //    stanza_id, class)` — exact-identity dedup.
            // 2. The `idx_notification_candidates_identity` UNIQUE
            //    index on `(recipient_bare_jid, conversation_jid,
            //    thread_id, stanza_id, class)` — cross-archive
            //    dedup for the same logical stanza minted under
            //    different `by=` JIDs (XEP-0359).
            //
            // Both are intended Duplicate triggers, so the
            // counter increments on either path. If a third
            // unique constraint is ever added with different
            // dedup semantics, the SQL needs an explicit chained
            // `ON CONFLICT (cols) DO NOTHING` for each path
            // (Greptile review on PR #758).
            waddle_xmpp::telemetry::reliability::increment_push_candidate_coalesced();
            tracing::info!(
                notification_class = candidate.class().as_db_value(),
                push_stage = "coalesced",
                "push pipeline transition"
            );
            return Ok(NotificationCandidateInsertOutcome::Duplicate);
        }
        waddle_xmpp::telemetry::reliability::increment_push_candidate_created();
        tracing::info!(
            notification_class = candidate.class().as_db_value(),
            push_stage = "candidate_created",
            "push pipeline transition"
        );
        Ok(NotificationCandidateInsertOutcome::Inserted)
    }

    /// Read the stable host identity after inserting a canonical candidate.
    /// Cross-archive aliases resolve to the same stored identity.
    pub(crate) async fn candidate_delivery_id_in_transaction(
        tx: &mut crate::db::Transaction<'_>,
        candidate: &NotificationCandidate,
    ) -> Result<Option<uuid::Uuid>, DatabaseError> {
        tx.execute(
            "UPDATE notification_candidates SET delivery_id = ? WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? AND delivery_id IS NULL AND quarantined_at_ms IS NULL",
            crate::db_params![uuid::Uuid::new_v4().to_string(), candidate.recipient_bare_jid.to_string(),
                candidate.conversation_jid.to_string(), candidate.thread_id.as_str(),
                candidate.archive_stanza_id.id.clone(), candidate.class.as_db_value()],
        ).await?;
        let mut rows = tx.query(
            "SELECT delivery_id FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? AND quarantined_at_ms IS NULL",
            crate::db_params![candidate.recipient_bare_jid.to_string(), candidate.conversation_jid.to_string(),
                candidate.thread_id.as_str(), candidate.archive_stanza_id.id.clone(), candidate.class.as_db_value()],
        ).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        row.get::<Option<String>>(0)?
            .map(|raw| {
                uuid::Uuid::parse_str(&raw).map_err(|_| {
                    DatabaseError::QueryFailed("invalid notification delivery identity".to_string())
                })
            })
            .transpose()
    }

    /// Reused, already marked pending custody must retain the exact existing
    /// approval and its live jobs, without inserting a new candidate or receipt.
    pub(crate) async fn attach_existing_candidate_lineage_in_transaction(
        tx: &mut crate::db::Transaction<'_>,
        message_key: waddle_xmpp::ingress::MessageKey,
        effect: &waddle_xmpp::ingress::IngressEffectKey,
        expected: &NotificationCandidate,
    ) -> Result<bool, crate::ingress_uow::IngressUowError> {
        let query = if tx.driver() == crate::db::DatabaseDriver::Postgres {
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,suppressed_reason,outboxed_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? AND quarantined_at_ms IS NULL FOR UPDATE NOWAIT"
        } else {
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,suppressed_reason,outboxed_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? AND quarantined_at_ms IS NULL"
        };
        let mut rows = tx
            .query(
                query,
                crate::db_params![
                    expected.recipient_bare_jid.to_string(),
                    expected.conversation_jid.to_string(),
                    expected.thread_id.as_str(),
                    expected.archive_stanza_id.id.clone(),
                    expected.class.as_db_value(),
                ],
            )
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?;
        let Some(row) = rows
            .next()
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?
        else {
            return Ok(false);
        };
        let mut stored = decode_candidate(&row)
            .map_err(|_| crate::ingress_uow::IngressUowError::EffectIntentConflict)?;
        let suppression = row.get::<Option<String>>(15)?;
        if let Some(reason) = &suppression {
            SuppressedReason::from_db_value(reason)
                .map_err(|_| crate::ingress_uow::IngressUowError::EffectIntentConflict)?;
        }
        let delivery = stored.delivery_id;
        let outboxed = row.get::<Option<i64>>(16)?.is_some();
        // Scheduling counters/identity may evolve; admitted intrinsic approval
        // (including exact archive pointer) must still agree completely.
        stored.delivery_id = expected.delivery_id;
        stored.policy_error_count = expected.policy_error_count;
        if &stored != expected {
            return Ok(false);
        }
        drop(rows);
        if outboxed && suppression.is_none() {
            let Some(id) = delivery else { return Ok(false) };
            // Outboxed alone never discharges legacy unknown fanout. A live
            // candidate bridge, or absent lineage, retains the original route.
            let mut rows = tx.query(
                "SELECT EXISTS(SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL), EXISTS(SELECT 1 FROM notification_outbox_lineage WHERE candidate_delivery_id = ?)",
                crate::db_params![id.to_string(), id.to_string()],
            ).await?;
            let proof = rows
                .next()
                .await?
                .ok_or(crate::ingress_uow::IngressUowError::EffectIntentConflict)?;
            if proof.get::<bool>(0)? || !proof.get::<bool>(1)? {
                return Ok(false);
            }
        }
        Self::attach_candidate_lineage_in_transaction(tx, message_key, effect, expected).await?;
        Ok(true)
    }

    /// Canonical candidate custody follows all live jobs even when another
    /// parent arrives after T1 coalesced/fanned out the original candidate.
    /// A settled scheduling relation remains an explicit terminal disposition.
    pub(crate) async fn attach_candidate_lineage_in_transaction(
        tx: &mut crate::db::Transaction<'_>,
        message_key: waddle_xmpp::ingress::MessageKey,
        effect: &waddle_xmpp::ingress::IngressEffectKey,
        candidate: &NotificationCandidate,
    ) -> Result<(), crate::ingress_uow::IngressUowError> {
        crate::ingress_uow::EffectDescendantRepository::lock_raw(tx, message_key).await?;
        if !Self::lock_unquarantined_candidate(tx, candidate).await? {
            return Err(crate::ingress_uow::IngressUowError::EffectIntentConflict);
        }
        let Some(id) = Self::candidate_delivery_id_in_transaction(tx, candidate).await? else {
            return Err(crate::ingress_uow::IngressUowError::EffectIntentConflict);
        };
        let mut rows = tx
            .query(
                "SELECT outboxed_at_ms, EXISTS(SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL), suppressed_reason, EXISTS(SELECT 1 FROM notification_outbox_lineage WHERE candidate_delivery_id = ?) FROM notification_candidates WHERE delivery_id = ?",
                crate::db_params![id.to_string(), id.to_string(), id.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(crate::ingress_uow::IngressUowError::EffectIntentConflict);
        };
        let outboxed = row.get::<Option<i64>>(0)?.is_some();
        let pending_bridge = row.get::<bool>(1)?;
        let suppression = row.get::<Option<String>>(2)?;
        if let Some(reason) = &suppression {
            SuppressedReason::from_db_value(reason)
                .map_err(|_| crate::ingress_uow::IngressUowError::EffectIntentConflict)?;
        }
        let established_history = suppression.is_some() || row.get::<bool>(3)?;
        drop(rows);
        crate::ingress_uow::EffectDescendantRepository::attach_raw(tx, message_key, effect, id)
            .await?;
        if !outboxed {
            return Ok(());
        }
        let lineage_sql = if tx.driver() == crate::db::DatabaseDriver::Postgres {
            "SELECT job_id FROM notification_outbox_lineage WHERE candidate_delivery_id = ? AND settled_at_ms IS NULL ORDER BY job_id FOR UPDATE"
        } else {
            "SELECT job_id FROM notification_outbox_lineage WHERE candidate_delivery_id = ? AND settled_at_ms IS NULL ORDER BY job_id"
        };
        let mut rows = tx
            .query(lineage_sql, crate::db_params![id.to_string()])
            .await?;
        let mut jobs = Vec::new();
        while let Some(row) = rows.next().await? {
            let job: String = row.get(0)?;
            jobs.push(
                uuid::Uuid::parse_str(&job)
                    .map_err(|_| crate::ingress_uow::IngressUowError::EffectIntentConflict)?,
            );
        }
        for job in jobs {
            crate::ingress_uow::EffectDescendantRepository::copy_raw(tx, id, job).await?;
        }
        // Existing unresolved bridges are durable unknown fanout, including
        // legacy history. Copying known children cannot discharge that proof.
        if !pending_bridge && established_history {
            crate::ingress_uow::EffectDescendantRepository::settle_all_raw(
                tx,
                id,
                chrono::Utc::now(),
            )
            .await?;
        }
        Ok(())
    }

    /// Caller holds its canonical parent first; retain the candidate lock through
    /// ancestry mutation so another parent cannot quarantine between check/write.
    pub(super) async fn lock_unquarantined_candidate(
        tx: &mut crate::db::Transaction<'_>,
        candidate: &NotificationCandidate,
    ) -> Result<bool, crate::ingress_uow::IngressUowError> {
        let query = if tx.driver() == crate::db::DatabaseDriver::Postgres {
            "SELECT quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? FOR UPDATE NOWAIT"
        } else {
            "SELECT quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ?"
        };
        let mut rows = tx
            .query(
                query,
                crate::db_params![
                    candidate.recipient_bare_jid.to_string(),
                    candidate.conversation_jid.to_string(),
                    candidate.thread_id.as_str(),
                    candidate.archive_stanza_id.id.clone(),
                    candidate.class.as_db_value()
                ],
            )
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?;
        Ok(
            match rows
                .next()
                .await
                .map_err(crate::ingress_uow::canonical_nowait_error)?
            {
                Some(row) => row.get::<Option<i64>>(0)?.is_none(),
                None => false,
            },
        )
    }

    /// Test/diagnostic helper: total count of `notification_candidates`
    /// rows, including ones already marked outboxed.
    ///
    /// Compliance regression tests use this to assert that a
    /// T0-suppressed XEP-0492 outcome persists *no* row at all
    /// (`count_all_candidates == 0`), distinct from the older
    /// "row exists, marked outboxed without a job" shape.
    pub async fn count_all_candidates(&self) -> Result<i64, NotificationOutboxError> {
        let mut rows = self
            .query("SELECT COUNT(*) FROM notification_candidates", ())
            .await?;
        // `COUNT(*)` is guaranteed to return exactly one row on every
        // SQL backend; an empty result here would mean a corrupted
        // driver. Default to 0 fail-loud-via-row-decode instead of
        // panicking.
        let Some(row) = rows.next().await? else {
            return Ok(0);
        };
        Ok(row.get::<i64>(0)?)
    }
}
