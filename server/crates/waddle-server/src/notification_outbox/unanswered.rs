//! Bounded invitation scans and lineage-free unanswered-call alerts.

use super::{
    decode_candidate, NotificationCandidate, NotificationCandidateInsertOutcome,
    NotificationOutboxError, NotificationOutboxStore,
};

#[derive(Debug)]
pub struct DueUnansweredCall {
    pub candidate: NotificationCandidate,
    pub created_at_ms: i64,
}

impl NotificationOutboxStore {
    /// Scan each existing partial index separately, retaining one shared time bound.
    pub async fn due_unanswered_calls(
        &self,
        after_ms: i64,
        due_ms: i64,
        limit: usize,
    ) -> Result<Vec<DueUnansweredCall>, NotificationOutboxError> {
        let mut rows = self
            .query(
                r#"
            SELECT c.recipient_bare_jid,c.conversation_jid,c.sender_jid,c.thread_id,
                   c.stanza_id_by,c.stanza_id,c.class,c.reason,c.policy_error_count,
                   c.noping,c.no_store,c.no_permanent_store,c.last_message_body,
                   c.reaction,c.delivery_id,c.call_sid,c.call_video,c.created_at_ms
            FROM notification_candidates c
            WHERE c.created_at_ms > ? AND c.created_at_ms <= ?
              AND c.class = 'call' AND c.quarantined_at_ms IS NULL
              AND c.outboxed_at_ms IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM notification_candidates m
                  WHERE m.recipient_bare_jid = c.recipient_bare_jid
                    AND m.conversation_jid = c.conversation_jid
                    AND m.call_sid = c.call_sid AND m.class = 'missed_call'
              )
            UNION ALL
            SELECT c.recipient_bare_jid,c.conversation_jid,c.sender_jid,c.thread_id,
                   c.stanza_id_by,c.stanza_id,c.class,c.reason,c.policy_error_count,
                   c.noping,c.no_store,c.no_permanent_store,c.last_message_body,
                   c.reaction,c.delivery_id,c.call_sid,c.call_video,c.created_at_ms
            FROM notification_candidates c
            WHERE c.created_at_ms > ? AND c.created_at_ms <= ?
              AND c.class = 'call' AND c.quarantined_at_ms IS NULL
              AND c.outboxed_at_ms IS NOT NULL AND c.outboxed_at_ms >= ?
              AND NOT EXISTS (
                  SELECT 1 FROM notification_candidates m
                  WHERE m.recipient_bare_jid = c.recipient_bare_jid
                    AND m.conversation_jid = c.conversation_jid
                    AND m.call_sid = c.call_sid AND m.class = 'missed_call'
              )
            ORDER BY created_at_ms, recipient_bare_jid, conversation_jid, stanza_id
            LIMIT ?
            "#,
                crate::db_params![
                    after_ms,
                    due_ms,
                    after_ms,
                    due_ms,
                    after_ms.saturating_sub(30_000),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
            )
            .await?;
        let mut calls = Vec::new();
        while let Some(row) = rows.next().await? {
            calls.push(DueUnansweredCall {
                candidate: decode_candidate(&row)?,
                created_at_ms: row.get(17)?,
            });
        }
        Ok(calls)
    }

    /// Timer work has no ingress parent; the original invitation supplies identity only.
    pub async fn insert_unanswered_call(
        &self,
        call: &NotificationCandidate,
        now_ms: i64,
    ) -> Result<NotificationCandidateInsertOutcome, NotificationOutboxError> {
        let candidate = call.unanswered_call()?;
        let mut tx = crate::ingress_uow::begin_raw_ingress_writer(&self.db).await?;
        let outcome = Self::insert_candidate_in_transaction(&mut tx, &candidate, now_ms).await?;
        tx.commit().await?;
        Ok(outcome)
    }
}
