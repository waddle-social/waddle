use super::*;

pub(super) async fn initialize(storage: &DatabaseInboxStorage) -> Result<(), InboxStorageError> {
    let i64_type = crate::db::i64_sql_type(storage.db.driver());

    // Check if the table already exists with the old schema (missing thread_id column).
    let needs_migration = needs_thread_migration(storage).await?;

    if needs_migration {
        info!("Migrating inbox_entries to thread-aware schema");
        storage.execute_batch(
            r#"
            CREATE TABLE inbox_entries_new (
                user_jid TEXT NOT NULL,
                partner_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL,
                last_stanza_id TEXT NOT NULL,
                last_updated INTEGER NOT NULL,
                unread INTEGER NOT NULL DEFAULT 0,
                preview TEXT,
                thread_title TEXT,
                reply_count INTEGER NOT NULL DEFAULT 0,
                author TEXT,
                PRIMARY KEY (user_jid, partner_jid, thread_id)
            );
            INSERT INTO inbox_entries_new (user_jid, partner_jid, thread_id, kind, last_stanza_id, last_updated, unread, preview)
                SELECT user_jid, partner_jid, '', kind, last_stanza_id, last_updated, unread, preview
                FROM inbox_entries;
            DROP TABLE inbox_entries;
            ALTER TABLE inbox_entries_new RENAME TO inbox_entries;
            "#,
        )
        .await?;
        info!("Inbox migration complete");
    } else {
        storage
            .execute(
                &format!(
                    r#"
            CREATE TABLE IF NOT EXISTS inbox_entries (
                user_jid TEXT NOT NULL,
                partner_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL,
                last_stanza_id TEXT NOT NULL,
                last_updated {i64_type} NOT NULL,
                unread INTEGER NOT NULL DEFAULT 0,
                preview TEXT,
                thread_title TEXT,
                reply_count INTEGER NOT NULL DEFAULT 0,
                author TEXT,
                PRIMARY KEY (user_jid, partner_jid, thread_id)
            );
            "#,
                ),
                (),
            )
            .await?;
    }

    crate::db::widen_postgres_i64_column_to_bigint(&storage.db, "inbox_entries", "last_updated")
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;

    storage.execute(
        "CREATE INDEX IF NOT EXISTS idx_inbox_entries_user_updated ON inbox_entries (user_jid, last_updated DESC)",
        (),
    )
    .await?;
    storage.execute(
        "CREATE INDEX IF NOT EXISTS idx_inbox_entries_user_room_threads ON inbox_entries (user_jid, partner_jid, thread_id) WHERE thread_id != ''",
        (),
    )
    .await?;
    // Per CLAUDE.md "Assume no production servers/users/data for this
    // project; prioritize clean design over compatibility" — schema
    // changes modify `CREATE TABLE IF NOT EXISTS` in place. Local
    // SQLite files built against an earlier branch must be discarded
    // (or the table dropped) before pulling a schema change; CI runs
    // against ephemeral databases so this is invisible there.
    storage
        .execute(
            &format!(
                r#"
            CREATE TABLE IF NOT EXISTS groupchat_notification_recovery (
                recipient_bare_jid TEXT NOT NULL,
                room_jid TEXT NOT NULL,
                thread_id TEXT NOT NULL DEFAULT '',
                stanza_id_by TEXT NOT NULL,
                stanza_id TEXT NOT NULL,
                sender_jid TEXT NOT NULL,
                is_live_occupant INTEGER NOT NULL,
                room_members_only INTEGER NOT NULL,
                -- XEP-0513 §"Multi-User Chats Permissions" §304: persist
                -- the sender's frozen channel-mention permission so a
                -- recovery replay re-creates the same notification class
                -- the original T0 emission did. Without this column the
                -- recovery path would silently downgrade every channel
                -- mention to `NotifyAll`, which is then suppressed at T1
                -- by the public-group `OnMention` XEP-0492 default — a
                -- silent moderator-push outage after every server
                -- restart.
                sender_can_broadcast_channel_mention INTEGER NOT NULL,
                created_at_ms {i64_type} NOT NULL,
                completed_at_ms {i64_type},
                PRIMARY KEY (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id)
            )
            "#
            ),
            (),
        )
        .await?;
    storage.execute(
        "CREATE INDEX IF NOT EXISTS idx_groupchat_notification_recovery_pending \
         ON groupchat_notification_recovery (created_at_ms, recipient_bare_jid, room_jid, thread_id, stanza_id) \
         WHERE completed_at_ms IS NULL",
        (),
    )
    .await?;
    storage.execute(
        "CREATE INDEX IF NOT EXISTS idx_groupchat_notification_recovery_completed_prune \
         ON groupchat_notification_recovery (completed_at_ms, recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id) \
         WHERE completed_at_ms IS NOT NULL",
        (),
    )
    .await?;
    Ok(())
}

/// Returns true if inbox_entries exists but lacks the thread_id column.
async fn needs_thread_migration(storage: &DatabaseInboxStorage) -> Result<bool, InboxStorageError> {
    if storage.db.driver() != DatabaseDriver::Sqlite {
        return Ok(false);
    }

    let mut rows = storage
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='inbox_entries'",
            (),
        )
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;

    let table_exists = rows
        .next()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?
        .is_some();

    if !table_exists {
        return Ok(false);
    }

    let mut cols = storage
        .query("PRAGMA table_info(inbox_entries)", ())
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;

    while let Some(row) = cols
        .next()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?
    {
        let col_name: String = row
            .get(1)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        if col_name == "thread_id" {
            return Ok(false);
        }
    }

    Ok(true)
}
