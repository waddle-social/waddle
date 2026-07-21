//! T0 candidate persistence: idempotent inserts and counts.

use super::*;

impl NotificationOutboxStore {
    pub async fn insert_candidate(
        &self,
        candidate: &NotificationCandidate,
    ) -> Result<NotificationCandidateInsertOutcome, NotificationOutboxError> {
        let now_ms = crate::time::now_ms();
        let inserted = self
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
                    reaction
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, ?, ?)
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
                    now_ms,
                    0_i64,
                    i64::from(candidate.noping),
                    i64::from(candidate.no_store),
                    i64::from(candidate.no_permanent_store),
                    candidate.last_message_body.clone(),
                    i64::from(candidate.reaction),
                ],
            )
            .await?;
        if inserted == 0 {
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
                recipient = %candidate.recipient_bare_jid(),
                conversation = %candidate.conversation_jid(),
                notification_class = candidate.class().as_db_value(),
                push_stage = "coalesced",
                "push pipeline transition"
            );
            return Ok(NotificationCandidateInsertOutcome::Duplicate);
        }
        waddle_xmpp::telemetry::reliability::increment_push_candidate_created();
        tracing::info!(
            recipient = %candidate.recipient_bare_jid(),
            conversation = %candidate.conversation_jid(),
            notification_class = candidate.class().as_db_value(),
            push_stage = "candidate_created",
            "push pipeline transition"
        );
        Ok(NotificationCandidateInsertOutcome::Inserted)
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
