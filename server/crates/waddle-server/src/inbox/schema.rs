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
                call_thread_kind TEXT,
                call_thread_media TEXT,
                call_ended_at INTEGER,
                call_duration TEXT,
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
                call_thread_kind TEXT,
                call_thread_media TEXT,
                call_ended_at {i64_type},
                call_duration TEXT,
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
    // Existing databases that pre-date the call-thread columns (issue
    // #919) get them added in place; fresh databases already have them
    // from the CREATE TABLE above. Without this targeted ALTER, the
    // runtime SELECT against the new columns errors with "no such
    // column" / "column does not exist" on every list query.
    migrate_inbox_call_thread_columns(storage).await?;
    // Fresh databases get the full schema via CREATE TABLE IF NOT
    // EXISTS. Existing databases that pre-date the
    // `sender_can_broadcast_channel_mention` column are migrated
    // below via the targeted `migrate_*_recovery_channel_broadcast`
    // helpers — without that path, the runtime SELECT against the
    // new column errors with "no such column" / "column does not
    // exist" on every recovery replay (reviewer on PR #738).
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
    migrate_recovery_channel_broadcast_column(storage).await?;
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

/// Adds the `sender_can_broadcast_channel_mention` column to existing
/// `groupchat_notification_recovery` tables that pre-date PR #738.
/// Idempotent on both SQLite and Postgres — re-runs at every startup
/// are no-ops once the column exists.
///
/// XEP-0513 §"Multi-User Chats Permissions" §304 default value `0`
/// (deny) on backfill: a recovery row created before the gate
/// landed has no frozen permission to consult; on replay the
/// candidate must NOT be granted channel-broadcast authority
/// retroactively. The recovery path then downgrades the class to
/// `NotifyAll`, which composes with the existing XEP-0492 policy.
async fn migrate_recovery_channel_broadcast_column(
    storage: &DatabaseInboxStorage,
) -> Result<(), InboxStorageError> {
    const COLUMN: &str = "sender_can_broadcast_channel_mention";
    match storage.db.driver() {
        DatabaseDriver::Sqlite => {
            if !recovery_column_present_sqlite(storage, COLUMN).await? {
                info!(
                    column = COLUMN,
                    "Adding column to groupchat_notification_recovery (SQLite)"
                );
                storage
                    .execute(
                        &format!(
                            "ALTER TABLE groupchat_notification_recovery \
                             ADD COLUMN {COLUMN} INTEGER NOT NULL DEFAULT 0"
                        ),
                        (),
                    )
                    .await?;
            }
        }
        DatabaseDriver::Postgres => {
            // `ADD COLUMN IF NOT EXISTS` makes this idempotent at the
            // SQL layer; no PRAGMA-equivalent probe needed.
            storage
                .execute(
                    &format!(
                        "ALTER TABLE groupchat_notification_recovery \
                         ADD COLUMN IF NOT EXISTS {COLUMN} INTEGER NOT NULL DEFAULT 0"
                    ),
                    (),
                )
                .await?;
        }
    }
    Ok(())
}

/// SQLite-only: returns `true` if the column exists on
/// `groupchat_notification_recovery`. Mirrors `needs_thread_migration`
/// but for a specific column rather than table-existence.
async fn recovery_column_present_sqlite(
    storage: &DatabaseInboxStorage,
    column: &str,
) -> Result<bool, InboxStorageError> {
    column_present_sqlite(storage, "groupchat_notification_recovery", column).await
}

/// Adds the four call-thread columns (issue #919) to existing
/// `inbox_entries` tables that pre-date them. Idempotent on both SQLite
/// and Postgres — re-runs at every startup are no-ops once the columns
/// exist. All four are nullable: a row that pre-dates a call thread has
/// no anchor metadata, and a row whose call has not ended has no ended
/// summary.
async fn migrate_inbox_call_thread_columns(
    storage: &DatabaseInboxStorage,
) -> Result<(), InboxStorageError> {
    let i64_type = crate::db::i64_sql_type(storage.db.driver());
    // (column, SQL type) for the four call-thread columns.
    let columns: [(&str, &str); 4] = [
        ("call_thread_kind", "TEXT"),
        ("call_thread_media", "TEXT"),
        ("call_ended_at", i64_type),
        ("call_duration", "TEXT"),
    ];
    match storage.db.driver() {
        DatabaseDriver::Sqlite => {
            for (column, sql_type) in columns {
                if !column_present_sqlite(storage, "inbox_entries", column).await? {
                    info!(column, "Adding column to inbox_entries (SQLite)");
                    storage
                        .execute(
                            &format!("ALTER TABLE inbox_entries ADD COLUMN {column} {sql_type}"),
                            (),
                        )
                        .await?;
                }
            }
        }
        DatabaseDriver::Postgres => {
            // `ADD COLUMN IF NOT EXISTS` makes this idempotent at the
            // SQL layer; no PRAGMA-equivalent probe needed.
            for (column, sql_type) in columns {
                storage
                    .execute(
                        &format!(
                            "ALTER TABLE inbox_entries ADD COLUMN IF NOT EXISTS {column} {sql_type}"
                        ),
                        (),
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

/// SQLite-only: returns `true` if `column` exists on `table`. Probes
/// `PRAGMA table_info(<table>)`. When the table itself does not yet
/// exist, returns `true` so callers treat the missing-column ALTER as
/// moot (the CREATE TABLE IF NOT EXISTS path owns that case).
async fn column_present_sqlite(
    storage: &DatabaseInboxStorage,
    table: &str,
    column: &str,
) -> Result<bool, InboxStorageError> {
    let mut rows = storage
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
            crate::db_params![table],
        )
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let table_exists = rows
        .next()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?
        .is_some();
    if !table_exists {
        // The CREATE TABLE IF NOT EXISTS path ran first; if the table
        // still doesn't exist, this run is unusual but the
        // missing-column path is moot.
        return Ok(true);
    }
    // SAFETY: `table` is interpolated directly into the PRAGMA string.
    // SQLite's `PRAGMA table_info(...)` cannot take a bound parameter for
    // the table name, so interpolation is unavoidable here. `table` MUST
    // therefore be a compile-time string literal — all current callers
    // pass `&'static str` constants ("inbox_entries",
    // "groupchat_notification_recovery"). NEVER pass user input or any
    // runtime-derived value to this helper.
    let mut cols = storage
        .query(&format!("PRAGMA table_info({table})"), ())
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
        if col_name == column {
            return Ok(true);
        }
    }
    Ok(false)
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
