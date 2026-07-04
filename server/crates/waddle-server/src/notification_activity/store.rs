//! Activity writers (XEP-0085 chat states, XEP-0490 read markers,
//! XEP-0045 presence, outbound messages) and the reader implementation.

use super::*;

impl NotificationActivityStore {
    /// Record a XEP-0085 chat-state change as activity for the user
    /// on the named conversation.
    ///
    /// Idempotent: re-applying the same `(owner, conversation, state)`
    /// at a later time advances both `last_active_at_ms` and
    /// `updated_at_ms`. Concurrent writers race to `INSERT … ON
    /// CONFLICT DO UPDATE`; the row reflects the most recent commit.
    pub async fn record_chat_state(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        chat_state: NotificationChatState,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, ?, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                last_chat_state = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.last_chat_state
                    ELSE notification_activity.last_chat_state
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms >= notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                chat_state.as_db_value(),
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Mark `(owner, conversation)` as no longer active. Used for the
    /// XEP-0085 `<gone/>` signal: the user has ended participation in
    /// the conversation, so any prior activity window must be
    /// invalidated regardless of how recent it was. Bypasses the
    /// monotonic clamp on `last_active_at_ms` — `<gone/>` is the only
    /// path that legitimately regresses activity, because semantically
    /// it tells us the recipient is *not* currently engaged. The T1
    /// XEP-0513 `<active/>` filter then sees `now_ms - 0` which is
    /// huge, so the `ActiveChannelMention` is suppressed with
    /// `Xep0513ActiveMiss`. The audit trail preserves the chat-state
    /// token as `gone` for diagnostics (Codex review on PR #731).
    pub async fn record_chat_state_gone(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, 0, ?, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = 0,
                last_chat_state = excluded.last_chat_state,
                updated_at_ms = excluded.updated_at_ms
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                NotificationChatState::Gone.as_db_value(),
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record a XEP-0490 read-marker advance as activity for the user
    /// on the named conversation. Updates both `last_read_at_ms` and
    /// `last_active_at_ms` — a read-marker advance is by definition a
    /// currently-engaged signal. Both timestamp columns advance
    /// monotonically: a late-arriving stale write CANNOT regress
    /// either column (XEP-0490 read-marker invariant + general
    /// projection monotonicity under concurrent writers).
    pub async fn record_read_marker(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, ?, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                last_read_at_ms = CASE
                    WHEN notification_activity.last_read_at_ms IS NULL THEN excluded.last_read_at_ms
                    WHEN excluded.last_read_at_ms >= notification_activity.last_read_at_ms
                    THEN excluded.last_read_at_ms
                    ELSE notification_activity.last_read_at_ms
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms >= notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                now_ms,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record an outbound message commit as activity for the sender
    /// on the named conversation. Sending a message is the strongest
    /// "currently active" signal we have.
    pub async fn record_outbound_message(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms >= notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record a XEP-0045 presence event (join or available `<show/>`
    /// change) for the user against the given MUC room.
    ///
    /// A `None` `show` is the canonical default-`available` token (no
    /// `<show/>` child); the column accepts it and the read path
    /// preserves the distinction. The typed
    /// [`NotificationPresenceShow`] enum guarantees the persisted
    /// value is one of the four RFC 6121 §4.7.2.1 tokens — no
    /// truncation or sanitisation needed at the writer.
    pub async fn record_presence_available(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        show: Option<NotificationPresenceShow>,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        let show_db_value: Option<&'static str> = show.map(NotificationPresenceShow::as_db_value);
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, NULL, ?, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                presence_show = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.presence_show
                    ELSE notification_activity.presence_show
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms >= notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                show_db_value,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    /// Record a XEP-0045 `<presence type='unavailable'/>` event. Per
    /// the brief, an explicit leave still counts as recent activity
    /// (so we bump `last_active_at_ms`) but clears the `<show/>` value
    /// — there is no longer an available presence to report.
    pub async fn record_presence_unavailable(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
        now_ms: i64,
    ) -> Result<(), NotificationActivityError> {
        self.execute(
            r#"
            INSERT INTO notification_activity (
                owner_bare_jid,
                conversation_jid,
                last_active_at_ms,
                last_chat_state,
                last_read_at_ms,
                presence_show,
                created_at_ms,
                updated_at_ms
            ) VALUES (?, ?, ?, NULL, NULL, NULL, ?, ?)
            ON CONFLICT (owner_bare_jid, conversation_jid) DO UPDATE SET
                last_active_at_ms = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN excluded.last_active_at_ms
                    ELSE notification_activity.last_active_at_ms
                END,
                presence_show = CASE
                    WHEN excluded.last_active_at_ms >= notification_activity.last_active_at_ms
                    THEN NULL
                    ELSE notification_activity.presence_show
                END,
                updated_at_ms = CASE
                    WHEN excluded.updated_at_ms >= notification_activity.updated_at_ms
                    THEN excluded.updated_at_ms
                    ELSE notification_activity.updated_at_ms
                END
            "#,
            crate::db_params![
                owner.to_string(),
                conversation.to_string(),
                now_ms,
                now_ms,
                now_ms,
            ],
        )
        .await?;
        Ok(())
    }

    async fn read(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
        let mut rows = self
            .query(
                r#"
                SELECT last_active_at_ms,
                       last_chat_state,
                       last_read_at_ms,
                       presence_show
                FROM notification_activity
                WHERE owner_bare_jid = ?
                  AND conversation_jid = ?
                "#,
                crate::db_params![owner.to_string(), conversation.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let last_active_at_ms: i64 = row.get(0)?;
        let last_chat_state_raw: Option<String> = row.get(1)?;
        let last_read_at_ms: Option<i64> = row.get(2)?;
        let presence_show_raw: Option<String> = row.get(3)?;
        let last_chat_state = last_chat_state_raw
            .map(|raw| NotificationChatState::from_db_value(&raw))
            .transpose()?;
        let presence_show = presence_show_raw
            .map(|raw| NotificationPresenceShow::from_db_value(&raw))
            .transpose()?;
        Ok(Some(NotificationActivity {
            last_active_at_ms,
            last_chat_state,
            last_read_at_ms,
            presence_show,
        }))
    }
}

#[async_trait]
impl NotificationActivityReader for NotificationActivityStore {
    async fn read_activity(
        &self,
        owner: &BareJid,
        conversation: &BareJid,
    ) -> Result<Option<NotificationActivity>, NotificationActivityError> {
        self.read(owner, conversation).await
    }
}
