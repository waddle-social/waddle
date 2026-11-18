//! T0 candidate persistence: idempotent inserts and counts.

use super::{
    decode_candidate, BareJid, DatabaseError, NotificationCandidate,
    NotificationCandidateInsertOutcome, NotificationClass, NotificationOutboxError,
    NotificationOutboxStore, SuppressedReason, MISSED_CALL_WINDOW_SECONDS,
};
use crate::db::DatabaseDriver;
use crate::db::Transaction;
use crate::ingress_substrate::DefinerApiCandidateContext;
use crate::ingress_substrate::EnteredTransaction;
use crate::ingress_uow::EffectDescendantRepository;
use crate::ingress_uow::IngressUowError;
use chrono::{DateTime, Utc};
use xmpp_parsers::jingle::SessionId;

impl NotificationOutboxStore {
    /// Resolve invitation evidence against the original retract receipt, including on recovery.
    /// Only the lower bound is checked: the invitation's time comes from the
    /// database clock and the receipt from this process, so an upper bound
    /// would drop a quick cancel under clock skew.
    pub(crate) async fn has_recent_call_candidate(
        &self,
        recipient: &BareJid,
        caller: &BareJid,
        session_id: &SessionId,
        receipt_at: DateTime<Utc>,
    ) -> Result<bool, NotificationOutboxError> {
        let receipt_ms = receipt_at.timestamp_millis();
        let mut rows = self
            .query(
                "SELECT 1 FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND class = ? AND call_sid = ? AND quarantined_at_ms IS NULL AND created_at_ms >= ? LIMIT 1",
                crate::db_params![
                    recipient.to_string(),
                    caller.to_string(),
                    NotificationClass::Call.as_db_value(),
                    session_id.0.clone(),
                    receipt_ms.saturating_sub(MISSED_CALL_WINDOW_SECONDS * 1_000),
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

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
        tx: &mut Transaction<'_>,
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
                    delivery_id,
                    call_sid,
                    call_video
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?)
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
                    candidate.call_session_id().map(|sid| sid.0.clone()),
                    candidate.call_video().map(i64::from),
                ],
            )
            .await?;
        if inserted == 0 {
            // Adopt missing scheduling identity without changing approval.
            Self::candidate_delivery_id_in_transaction(tx, candidate).await?;
            // UNIQUE-constraint collision. `notification_candidates`
            // carries stanza-identity constraints and a missed-call SID
            // constraint, all suppressed by untargeted `ON CONFLICT DO NOTHING`:
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
            // 3. The missed-call index on `(recipient_bare_jid,
            //    conversation_jid, call_sid)` — repeated retracts may
            //    have distinct stanza identities but share one alert.
            //
            // All are intended Duplicate triggers. Canonical callers resolve
            // the stored SID identity before attaching scheduling custody.
            // Any future constraint with different dedup semantics needs explicit
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

    /// A new retract owns its own receipt while sharing the first alert's custody.
    /// Keep the incoming archive authority outside this scheduling-only resolution.
    pub(crate) async fn candidate_for_lineage_in_transaction(
        tx: &mut Transaction<'_>,
        candidate: &NotificationCandidate,
    ) -> Result<NotificationCandidate, IngressUowError> {
        if candidate.class != NotificationClass::MissedCall {
            return Ok(candidate.clone());
        }
        let session_id = candidate
            .call_session_id()
            .ok_or(IngressUowError::EffectIntentConflict)?;
        let query = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,call_sid,call_video,quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND class = ? AND call_sid = ? FOR UPDATE NOWAIT"
        } else {
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,call_sid,call_video,quarantined_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND class = ? AND call_sid = ?"
        };
        let mut rows = tx
            .query(
                query,
                crate::db_params![
                    candidate.recipient_bare_jid.to_string(),
                    candidate.conversation_jid.to_string(),
                    candidate.class.as_db_value(),
                    session_id.0.clone(),
                ],
            )
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?;
        let row = rows
            .next()
            .await
            .map_err(crate::ingress_uow::canonical_nowait_error)?
            .ok_or(IngressUowError::EffectIntentConflict)?;
        if row.get::<Option<i64>>(17)?.is_some() {
            return Err(IngressUowError::EffectIntentConflict);
        }
        decode_candidate(&row).map_err(|_| IngressUowError::EffectIntentConflict)
    }

    /// Read the stable host identity after inserting a canonical candidate.
    /// Cross-archive aliases resolve to the same stored identity.
    pub(crate) async fn candidate_delivery_id_in_transaction(
        tx: &mut Transaction<'_>,
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
    pub(crate) async fn attach_existing_candidate_lineage_with_api(
        tx: &mut EnteredTransaction<'_>,
        api: Option<&DefinerApiCandidateContext<'_>>,
        message_key: waddle_xmpp::ingress::MessageKey,
        effect: &waddle_xmpp::ingress::IngressEffectKey,
        expected: &NotificationCandidate,
    ) -> Result<bool, IngressUowError> {
        let query = if tx.driver() == DatabaseDriver::Postgres {
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,call_sid,call_video,suppressed_reason,outboxed_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? AND quarantined_at_ms IS NULL FOR UPDATE NOWAIT"
        } else {
            "SELECT recipient_bare_jid,conversation_jid,sender_jid,thread_id,stanza_id_by,stanza_id,class,reason,policy_error_count,noping,no_store,no_permanent_store,last_message_body,reaction,delivery_id,call_sid,call_video,suppressed_reason,outboxed_at_ms FROM notification_candidates WHERE recipient_bare_jid = ? AND conversation_jid = ? AND thread_id = ? AND stanza_id = ? AND class = ? AND quarantined_at_ms IS NULL"
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
        let mut stored =
            decode_candidate(&row).map_err(|_| IngressUowError::EffectIntentConflict)?;
        let suppression = row.get::<Option<String>>(17)?;
        if let Some(reason) = &suppression {
            SuppressedReason::from_db_value(reason)
                .map_err(|_| IngressUowError::EffectIntentConflict)?;
        }
        let delivery = stored.delivery_id;
        let outboxed = row.get::<Option<i64>>(18)?.is_some();
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
            let pending = pending_descendant_sql(api);
            let mut rows = tx.query(
                &format!("SELECT {pending}, EXISTS(SELECT 1 FROM notification_outbox_lineage WHERE candidate_delivery_id = ?)"),
                crate::db_params![id.to_string(), id.to_string()],
            ).await?;
            let proof = rows
                .next()
                .await?
                .ok_or(IngressUowError::EffectIntentConflict)?;
            if proof.get::<bool>(0)? || !proof.get::<bool>(1)? {
                return Ok(false);
            }
        }
        Self::attach_candidate_lineage_with_api(tx, api, message_key, effect, expected).await?;
        Ok(true)
    }

    /// Canonical candidate custody follows all live jobs even when another
    /// parent arrives after T1 coalesced/fanned out the original candidate.
    /// A settled scheduling relation remains an explicit terminal disposition.
    #[cfg(test)]
    pub(crate) async fn attach_candidate_lineage_in_transaction(
        tx: &mut EnteredTransaction<'_>,
        message_key: waddle_xmpp::ingress::MessageKey,
        effect: &waddle_xmpp::ingress::IngressEffectKey,
        candidate: &NotificationCandidate,
    ) -> Result<(), IngressUowError> {
        Self::attach_candidate_lineage_with_api(tx, None, message_key, effect, candidate).await
    }

    pub(crate) async fn attach_candidate_lineage_with_api(
        tx: &mut EnteredTransaction<'_>,
        api: Option<&DefinerApiCandidateContext<'_>>,
        message_key: waddle_xmpp::ingress::MessageKey,
        effect: &waddle_xmpp::ingress::IngressEffectKey,
        candidate: &NotificationCandidate,
    ) -> Result<(), IngressUowError> {
        if let Some(api) = api {
            EffectDescendantRepository::lock_message_with_candidate_api(tx, api, message_key)
                .await?;
        } else {
            EffectDescendantRepository::lock_raw(tx, message_key).await?;
        }
        if !Self::lock_unquarantined_candidate(tx, candidate).await? {
            return Err(IngressUowError::EffectIntentConflict);
        }
        let Some(id) = Self::candidate_delivery_id_in_transaction(tx, candidate).await? else {
            return Err(IngressUowError::EffectIntentConflict);
        };
        let pending = pending_descendant_sql(api);
        let mut rows = tx
            .query(
                &format!("SELECT outboxed_at_ms, {pending}, suppressed_reason, EXISTS(SELECT 1 FROM notification_outbox_lineage WHERE candidate_delivery_id = ?) FROM notification_candidates WHERE delivery_id = ?"),
                crate::db_params![id.to_string(), id.to_string(), id.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(IngressUowError::EffectIntentConflict);
        };
        let outboxed = row.get::<Option<i64>>(0)?.is_some();
        let pending_bridge = row.get::<bool>(1)?;
        let suppression = row.get::<Option<String>>(2)?;
        if let Some(reason) = &suppression {
            SuppressedReason::from_db_value(reason)
                .map_err(|_| IngressUowError::EffectIntentConflict)?;
        }
        let established_history = suppression.is_some() || row.get::<bool>(3)?;
        drop(rows);
        if let Some(api) = api {
            EffectDescendantRepository::attach_effect_with_candidate_api(
                tx,
                api,
                message_key,
                effect,
                id,
            )
            .await?;
        } else {
            EffectDescendantRepository::attach_raw(tx, message_key, effect, id).await?;
        }
        if !outboxed {
            return Ok(());
        }
        let lineage_sql = if tx.driver() == DatabaseDriver::Postgres {
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
                uuid::Uuid::parse_str(&job).map_err(|_| IngressUowError::EffectIntentConflict)?,
            );
        }
        for job in jobs {
            if let Some(api) = api {
                EffectDescendantRepository::copy_with_candidate_api(tx, api, id, job).await?;
            } else {
                EffectDescendantRepository::copy_raw(tx, id, job).await?;
            }
        }
        // Existing unresolved bridges are durable unknown fanout, including
        // legacy history. Copying known children cannot discharge that proof.
        if !pending_bridge && established_history {
            if let Some(api) = api {
                EffectDescendantRepository::settle_with_candidate_api(
                    tx,
                    api,
                    id,
                    chrono::Utc::now(),
                )
                .await?;
            } else {
                EffectDescendantRepository::settle_all_raw(tx, id, chrono::Utc::now()).await?;
            }
        }
        Ok(())
    }

    /// Caller holds its canonical parent first; retain the candidate lock through
    /// ancestry mutation so another parent cannot quarantine between check/write.
    pub(super) async fn lock_unquarantined_candidate(
        tx: &mut Transaction<'_>,
        candidate: &NotificationCandidate,
    ) -> Result<bool, IngressUowError> {
        let query = if tx.driver() == DatabaseDriver::Postgres {
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

fn pending_descendant_sql(api: Option<&DefinerApiCandidateContext<'_>>) -> String {
    if let Some(api) = api {
        format!(
            "{}(?::text, TRUE)",
            api.qualified_function("waddle_ingress_descendant_exists")
        )
    } else {
        "EXISTS(SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL)".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_outbox::test_support::{bare, store};
    use crate::notification_outbox::{NotificationCall, NotificationMessageHints, StanzaId};

    #[tokio::test]
    async fn recent_call_evidence_uses_receipt_window_and_original_scope() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let caller = bare("bob@example.com");
        // Long before the current wall clock: recovery must use this frozen receipt.
        let receipt = DateTime::from_timestamp_millis(1_000_000).expect("receipt");
        for (sid, created_at, outboxed, suppressed, quarantined, expected) in [
            ("lower-bound", 940_000, false, false, false, true),
            ("upper-bound", 1_000_000, false, false, false, true),
            ("expired", 939_999, false, false, false, false),
            // The database clock may run ahead of this process.
            ("clock-ahead", 1_000_001, false, false, false, true),
            ("outboxed", 980_000, true, false, false, true),
            ("suppressed", 980_000, true, true, false, true),
            ("quarantined", 980_000, false, false, true, false),
        ] {
            let session_id = SessionId(sid.to_owned());
            let candidate = NotificationCandidate::call_with_hints(
                recipient.clone(),
                "bob@example.com/phone".parse().expect("sender"),
                StanzaId::new(sid, recipient.clone().into()),
                NotificationCall {
                    session_id: session_id.clone(),
                    video: true,
                },
                NotificationMessageHints::none(),
            )
            .expect("call");
            let mut tx = store.db.begin().await.expect("insert transaction");
            NotificationOutboxStore::insert_candidate_in_transaction(
                &mut tx, &candidate, created_at,
            )
            .await
            .expect("insert evidence");
            tx.execute(
                "UPDATE notification_candidates SET outboxed_at_ms = ?, suppressed_reason = ?, quarantined_at_ms = ? WHERE stanza_id = ?",
                crate::db_params![
                    outboxed.then_some(created_at),
                    suppressed.then_some("waddle_dnd"),
                    quarantined.then_some(created_at),
                    sid,
                ],
            )
            .await
            .expect("evidence state");
            tx.commit().await.expect("evidence commit");
            assert_eq!(
                store
                    .has_recent_call_candidate(&recipient, &caller, &session_id, receipt)
                    .await
                    .expect("evidence lookup"),
                expected,
                "{sid}",
            );
        }
        for (recipient, caller, sid) in [
            (bare("other@example.com"), caller.clone(), "upper-bound"),
            (recipient.clone(), bare("other@example.com"), "upper-bound"),
            (recipient.clone(), caller.clone(), "unknown"),
        ] {
            assert!(!store
                .has_recent_call_candidate(&recipient, &caller, &SessionId(sid.into()), receipt)
                .await
                .expect("scope lookup"));
        }
    }

    #[tokio::test]
    async fn missed_call_sid_resolution_preserves_first_candidate_and_rejects_quarantine() {
        let store = store().await;
        let recipient = bare("alice@example.com");
        let candidate = |stanza: &str| {
            NotificationCandidate::missed_call_with_hints(
                recipient.clone(),
                "bob@example.com/phone".parse().expect("sender"),
                StanzaId::new(stanza, recipient.clone().into()),
                SessionId("shared-call".into()),
                NotificationMessageHints::none(),
            )
            .expect("missed call")
        };
        let first = candidate("first-retract");
        let duplicate = candidate("second-retract");
        let mut tx = store.db.begin().await.expect("transaction");
        assert_eq!(
            NotificationOutboxStore::insert_candidate_in_transaction(&mut tx, &first, 1)
                .await
                .expect("first"),
            NotificationCandidateInsertOutcome::Inserted
        );
        assert_eq!(
            NotificationOutboxStore::insert_candidate_in_transaction(&mut tx, &duplicate, 2)
                .await
                .expect("duplicate"),
            NotificationCandidateInsertOutcome::Duplicate
        );
        let stored =
            NotificationOutboxStore::candidate_for_lineage_in_transaction(&mut tx, &duplicate)
                .await
                .expect("resolve duplicate custody");
        assert_eq!(stored.archive_stanza_id(), first.archive_stanza_id());
        assert!(stored.call().is_none());
        assert_eq!(stored.call_session_id(), duplicate.call_session_id());
        assert!(stored.delivery_id.is_some());
        tx.execute(
            "UPDATE notification_candidates SET quarantined_at_ms = 3",
            (),
        )
        .await
        .expect("quarantine");
        assert!(matches!(
            NotificationOutboxStore::candidate_for_lineage_in_transaction(&mut tx, &duplicate)
                .await,
            Err(IngressUowError::EffectIntentConflict)
        ));
        tx.commit().await.expect("commit");
    }
}
