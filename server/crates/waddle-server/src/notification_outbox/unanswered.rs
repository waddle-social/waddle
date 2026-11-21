//! Bounded invitation scans and lineage-free unanswered-call alerts.

use super::{
    decode_candidate, NotificationCandidate, NotificationCandidateInsertOutcome,
    NotificationOutboxError, NotificationOutboxStore,
};

#[derive(Debug)]
pub struct DueUnansweredCall {
    pub candidate: NotificationCandidate,
    pub created_at_ms: i64,
    /// The stored ordering key after `created_at_ms`, for the next page.
    key: DueCallKey,
}

/// Recipient, conversation and stanza id as stored: one invitation row.
pub(super) type DueCallKey = (String, String, String);

impl DueUnansweredCall {
    pub(super) fn key(&self) -> &DueCallKey {
        &self.key
    }
}

const DUE_CALL_COLUMNS: &str = "c.recipient_bare_jid,c.conversation_jid,c.sender_jid,c.thread_id,\
     c.stanza_id_by,c.stanza_id,c.class,c.reason,c.policy_error_count,\
     c.noping,c.no_store,c.no_permanent_store,c.last_message_body,\
     c.reaction,c.delivery_id,c.call_sid,c.call_video,c.created_at_ms";

const NO_MISSED_CALL_YET: &str = "NOT EXISTS (\
     SELECT 1 FROM notification_candidates m \
     WHERE m.recipient_bare_jid = c.recipient_bare_jid \
       AND m.conversation_jid = c.conversation_jid \
       AND m.call_sid = c.call_sid AND m.class = 'missed_call')";

impl NotificationOutboxStore {
    /// Invitations created in `(after_ms, due_ms]` that have no missed-call row.
    pub async fn due_unanswered_calls(
        &self,
        after_ms: i64,
        due_ms: i64,
        limit: usize,
    ) -> Result<Vec<DueUnansweredCall>, NotificationOutboxError> {
        self.due_unanswered_calls_page(after_ms, None, due_ms, limit)
            .await
    }

    /// The page after `previous` in `(created_at_ms, recipient, conversation,
    /// stanza id)` order. Rows that needed no alert stay eligible, so paging
    /// on the timestamp alone could return the same rows forever.
    pub async fn due_unanswered_calls_after(
        &self,
        after_ms: i64,
        previous: &DueUnansweredCall,
        due_ms: i64,
        limit: usize,
    ) -> Result<Vec<DueUnansweredCall>, NotificationOutboxError> {
        self.due_unanswered_calls_page(after_ms, Some(previous), due_ms, limit)
            .await
    }

    /// Scan each existing partial index separately, retaining one shared time bound.
    async fn due_unanswered_calls_page(
        &self,
        after_ms: i64,
        previous: Option<&DueUnansweredCall>,
        due_ms: i64,
        limit: usize,
    ) -> Result<Vec<DueUnansweredCall>, NotificationOutboxError> {
        let (start, start_params) = match previous {
            None => ("c.created_at_ms > ?", crate::db_params![after_ms]),
            Some(previous) => (
                "c.created_at_ms >= ? AND (c.created_at_ms > ? OR \
                 (c.recipient_bare_jid, c.conversation_jid, c.stanza_id) > (?, ?, ?))",
                crate::db_params![
                    previous.created_at_ms,
                    previous.created_at_ms,
                    previous.key.0.clone(),
                    previous.key.1.clone(),
                    previous.key.2.clone(),
                ],
            ),
        };
        let branch = |outboxed: &str| {
            format!(
                "SELECT {DUE_CALL_COLUMNS} FROM notification_candidates c \
                 WHERE {start} AND c.created_at_ms <= ? \
                   AND c.class = 'call' AND c.quarantined_at_ms IS NULL \
                   AND {outboxed} AND {NO_MISSED_CALL_YET}"
            )
        };
        let sql = format!(
            "{} UNION ALL {} \
             ORDER BY created_at_ms, recipient_bare_jid, conversation_jid, stanza_id LIMIT ?",
            branch("c.outboxed_at_ms IS NULL"),
            branch("c.outboxed_at_ms IS NOT NULL AND c.outboxed_at_ms >= ?"),
        );
        let mut params = start_params.clone();
        params.extend(crate::db_params![due_ms]);
        params.extend(start_params);
        params.extend(crate::db_params![
            due_ms,
            after_ms.saturating_sub(30_000),
            i64::try_from(limit).unwrap_or(i64::MAX),
        ]);
        let mut rows = self.query(&sql, params).await?;
        let mut calls = Vec::new();
        while let Some(row) = rows.next().await? {
            calls.push(DueUnansweredCall {
                candidate: decode_candidate(&row)?,
                created_at_ms: row.get(17)?,
                key: (row.get(0)?, row.get(1)?, row.get(5)?),
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
