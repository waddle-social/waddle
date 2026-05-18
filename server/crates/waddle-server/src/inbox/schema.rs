use super::*;

const GROUPCHAT_NOTIFICATION_RECOVERY_REQUIRED_COLUMNS: &[&str] = &[
    "recipient_bare_jid",
    "room_jid",
    "thread_id",
    "stanza_id_by",
    "stanza_id",
    "sender_jid",
    "is_live_occupant",
    "room_members_only",
    "sender_role",
    "mentions_count",
    "mentions_individual",
    "mentions_channel",
    "occupant_id_bare_jids",
    "created_at_ms",
    "completed_at_ms",
];

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
                sender_role TEXT NOT NULL DEFAULT 'none',
                mentions_count INTEGER NOT NULL DEFAULT 0,
                mentions_individual TEXT NOT NULL DEFAULT 'none',
                mentions_channel TEXT NOT NULL DEFAULT 'none',
                occupant_id_bare_jids TEXT NOT NULL DEFAULT '[]',
                created_at_ms {i64_type} NOT NULL,
                completed_at_ms {i64_type},
                PRIMARY KEY (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id)
            )
            "#
            ),
            (),
        )
        .await?;
    validate_groupchat_notification_recovery_schema(storage).await?;
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

async fn validate_groupchat_notification_recovery_schema(
    storage: &DatabaseInboxStorage,
) -> Result<(), InboxStorageError> {
    let columns = table_columns(storage, "groupchat_notification_recovery").await?;
    let missing = GROUPCHAT_NOTIFICATION_RECOVERY_REQUIRED_COLUMNS
        .iter()
        .copied()
        .filter(|required| !columns.iter().any(|column| column == required))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(
        InboxStorageError::InvalidGroupchatNotificationRecoverySchema {
            missing_columns: missing.into_iter().map(str::to_string).collect(),
        },
    )
}

async fn table_columns(
    storage: &DatabaseInboxStorage,
    table: &str,
) -> Result<Vec<String>, InboxStorageError> {
    let sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            format!(
                "SELECT column_name FROM information_schema.columns WHERE table_name = '{table}'"
            )
        }
        DatabaseDriver::Sqlite => format!("PRAGMA table_info({table})"),
    };
    let mut rows = storage
        .query(&sql, ())
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;

    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?
    {
        let index = match storage.db.driver() {
            DatabaseDriver::Postgres => 0,
            DatabaseDriver::Sqlite => 1,
        };
        let column: String = row
            .get(index)
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        columns.push(column);
    }
    Ok(columns)
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
