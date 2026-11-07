use super::{info, DatabaseDriver, InboxStorageError};
use crate::db::{CatalogFenceLock, Database, IntoParams, Rows, Transaction};
use sqlx::PgConnection;
use std::time::Duration;

const RECOVERY_MESSAGE_KEY_GATE: &str = "groupchat_notification_recovery.message_key";
const POSTGRES_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(35);
const POSTGRES_COMMIT_TIMEOUT: Duration = Duration::from_secs(5);
const POSTGRES_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(5);

/// SQLite schema access retains the inbox store's existing per-call pool
/// acquisition without constructing a live inbox storage object.
struct OwnerSchemaDb<'a> {
    db: &'a Database,
}

impl OwnerSchemaDb<'_> {
    async fn query(&self, sql: &str, params: impl IntoParams) -> Result<Rows, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }

    async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))
    }

    async fn execute_batch(&self, sql: &str) -> Result<(), InboxStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        conn.execute_batch(sql)
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        Ok(())
    }
}

pub async fn bootstrap_schema_owner(db: &Database) -> Result<(), InboxStorageError> {
    match db.driver() {
        DatabaseDriver::Postgres => initialize_postgres(db).await,
        DatabaseDriver::Sqlite => initialize_sqlite(&OwnerSchemaDb { db }).await,
    }
}

async fn initialize_postgres(db: &Database) -> Result<(), InboxStorageError> {
    if db.uses_attested_postgres_pool() {
        return Err(InboxStorageError::Other(
            "inbox owner initialization requires an owner database handle".to_owned(),
        ));
    }
    let mut tx = db
        .begin_catalog_fenced_with_idle_bound(&[CatalogFenceLock::ExclusiveHashtext(
            RECOVERY_MESSAGE_KEY_GATE,
        )])
        .await
        .map_err(inbox_database_error)?;
    let result =
        match tokio::time::timeout(POSTGRES_INITIALIZE_TIMEOUT, initialize_postgres_tx(&mut tx))
            .await
        {
            Ok(result) => result,
            Err(_) => Err(InboxStorageError::Other(
                "inbox PostgreSQL initialization timed out".into(),
            )),
        };
    if let Err(error) = result {
        if !tx.rollback_bounded_discard(POSTGRES_ROLLBACK_TIMEOUT).await {
            return Err(InboxStorageError::Other(
                "inbox PostgreSQL initialization rollback was not acknowledged; reconcile the original backend".into(),
            ));
        }
        return Err(error);
    }
    // The checkout is already marked for disposal on any unacknowledged end.
    // Neither an existing table nor a retry proves what this commit did.
    if !matches!(
        tokio::time::timeout(POSTGRES_COMMIT_TIMEOUT, tx.commit()).await,
        Ok(Ok(()))
    ) {
        return Err(InboxStorageError::CommitIndeterminate);
    }
    Ok(())
}

async fn initialize_postgres_tx(tx: &mut Transaction<'_>) -> Result<(), InboxStorageError> {
    let connection = tx.postgres_connection().ok_or_else(|| {
        InboxStorageError::Other("inbox owner initialization requires PostgreSQL".to_owned())
    })?;
    // Capture only through qualified builtins, before evaluating any operator
    // from an untrusted search path. DDL below always names this schema.
    let schema: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.current_schema()::pg_catalog.text")
            .fetch_one(&mut *connection)
            .await
            .map_err(inbox_database_error)?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await
        .map_err(inbox_database_error)?;
    for (setting, cap) in [("lock_timeout", "5s"), ("statement_timeout", "30s")] {
        sqlx::query(
            "SELECT pg_catalog.set_config($1, \
             CASE WHEN pg_catalog.current_setting($1)::pg_catalog.interval = '0'::pg_catalog.interval \
               OR pg_catalog.current_setting($1)::pg_catalog.interval > $2::pg_catalog.interval \
             THEN $2 ELSE pg_catalog.current_setting($1) END, true)",
        )
        .bind(setting)
        .bind(cap)
        .execute(&mut *connection)
        .await
        .map_err(inbox_database_error)?;
    }
    let schema = schema.ok_or_else(|| {
        InboxStorageError::Other("inbox active PostgreSQL schema is missing".to_owned())
    })?;
    if schema.starts_with("pg_") || schema == "information_schema" {
        return Err(InboxStorageError::Other(
            "inbox owner requires a persistent application schema".to_owned(),
        ));
    }
    let shadowed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n \
           WHERE n.oid = pg_catalog.pg_my_temp_schema() AND n.nspname = $1) \
         OR EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
           WHERE c.relnamespace = pg_catalog.pg_my_temp_schema() \
             AND c.relname IN ('inbox_entries', 'groupchat_notification_recovery'))",
    )
    .bind(&schema)
    .fetch_one(&mut *connection)
    .await
    .map_err(inbox_database_error)?;
    if shadowed {
        return Err(InboxStorageError::Other(
            "inbox owner refused a temporary schema shadow".to_owned(),
        ));
    }
    // `CREATE TABLE IF NOT EXISTS` also accepts a relation of another kind.
    // In particular, never let the legacy DELETE run through an updatable
    // view or foreign table. Inspect both names before making any changes.
    for table in ["inbox_entries", "groupchat_notification_recovery"] {
        lock_and_verify_postgres_relation(connection, &schema, table, false).await?;
    }
    prepare_postgres_schema(connection, &schema).await
}

async fn lock_and_verify_postgres_relation(
    connection: &mut PgConnection,
    schema: &str,
    table: &str,
    required: bool,
) -> Result<(), InboxStorageError> {
    async fn relation(
        connection: &mut PgConnection,
        schema: &str,
        table: &str,
    ) -> Result<Option<(i64, bool)>, InboxStorageError> {
        sqlx::query_as(
            "SELECT c.oid::pg_catalog.int8, c.relkind = 'r' AND c.relpersistence = 'p' \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i \
                 WHERE i.inhparent = c.oid OR i.inhrelid = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(schema)
        .bind(table)
        .fetch_optional(connection)
        .await
        .map_err(inbox_database_error)
    }
    let oid = match relation(connection, schema, table).await? {
        Some((oid, true)) => oid,
        None if !required => return Ok(()),
        _ => {
            return Err(InboxStorageError::Other(format!(
                "inbox owner refused unexpected PostgreSQL relation kind or inheritance for {table}"
            )));
        }
    };
    let namespace = crate::db::physical_attestation::quote_identifier(schema);
    let name = crate::db::physical_attestation::quote_identifier(table);
    postgres_execute(
        connection,
        &format!("LOCK TABLE ONLY {namespace}.{name} IN ACCESS EXCLUSIVE MODE"),
    )
    .await?;
    if relation(connection, schema, table).await? != Some((oid, true)) {
        return Err(InboxStorageError::Other(format!(
            "inbox owner relation changed while locking {table}"
        )));
    }
    if table == "groupchat_notification_recovery" {
        // The legacy migration deletes rows. An executable rule, trigger,
        // policy, or incoming FK could make that DELETE mutate another table.
        let direct_delete: bool = sqlx::query_scalar(
            "SELECT NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class = c.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint f \
                 WHERE f.contype = 'f' AND f.confrelid = c.oid) \
             FROM pg_catalog.pg_class c WHERE c.oid = $1::pg_catalog.oid",
        )
        .bind(oid)
        .fetch_one(&mut *connection)
        .await
        .map_err(inbox_database_error)?;
        if !direct_delete {
            return Err(InboxStorageError::Other(
                "inbox owner refused executable PostgreSQL recovery DELETE side effects".into(),
            ));
        }
    }
    Ok(())
}

async fn prepare_postgres_schema(
    connection: &mut PgConnection,
    schema: &str,
) -> Result<(), InboxStorageError> {
    let namespace = crate::db::physical_attestation::quote_identifier(schema);
    let entries = format!("{namespace}.\"inbox_entries\"");
    let recovery = format!("{namespace}.\"groupchat_notification_recovery\"");
    postgres_execute(
        connection,
        &format!(
            "CREATE TABLE IF NOT EXISTS {entries} (\
           user_jid TEXT NOT NULL, partner_jid TEXT NOT NULL, thread_id TEXT NOT NULL DEFAULT '', \
           kind TEXT NOT NULL, last_stanza_id TEXT NOT NULL, last_updated BIGINT NOT NULL, \
           unread INTEGER NOT NULL DEFAULT 0, preview TEXT, thread_title TEXT, \
           reply_count INTEGER NOT NULL DEFAULT 0, author TEXT, call_thread_kind TEXT, \
           call_thread_media TEXT, call_ended_at BIGINT, call_duration TEXT, \
           PRIMARY KEY (user_jid, partner_jid, thread_id))"
        ),
    )
    .await?;
    lock_and_verify_postgres_relation(connection, schema, "inbox_entries", true).await?;

    let current_type: Option<(bool, bool)> = sqlx::query_as(
        "SELECT a.atttypid = 'pg_catalog.int4'::pg_catalog.regtype, \
                a.atttypid = 'pg_catalog.int8'::pg_catalog.regtype \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = 'inbox_entries' \
           AND a.attname = 'last_updated' AND a.attnum > 0 AND NOT a.attisdropped",
    )
    .bind(schema)
    .fetch_optional(&mut *connection)
    .await
    .map_err(inbox_database_error)?;
    let (is_int4, is_int8) = current_type.ok_or_else(|| {
        InboxStorageError::Other("inbox owner requires last_updated column".into())
    })?;
    if is_int4 {
        postgres_execute(
            connection,
            &format!("ALTER TABLE {entries} ALTER COLUMN last_updated TYPE BIGINT"),
        )
        .await?;
    } else if !is_int8 {
        return Err(InboxStorageError::Other(
            "inbox owner refused non-integer last_updated conversion".into(),
        ));
    }
    postgres_execute(
        connection,
        &format!(
            "CREATE INDEX IF NOT EXISTS idx_inbox_entries_user_updated \
         ON {entries} (user_jid, last_updated DESC)"
        ),
    )
    .await?;
    postgres_execute(
        connection,
        &format!(
            "CREATE INDEX IF NOT EXISTS idx_inbox_entries_user_room_threads \
         ON {entries} (user_jid, partner_jid, thread_id) WHERE thread_id != ''"
        ),
    )
    .await?;
    for (column, sql_type) in [
        ("call_thread_kind", "TEXT"),
        ("call_thread_media", "TEXT"),
        ("call_ended_at", "BIGINT"),
        ("call_duration", "TEXT"),
    ] {
        postgres_execute(
            connection,
            &format!("ALTER TABLE {entries} ADD COLUMN IF NOT EXISTS {column} {sql_type}"),
        )
        .await?;
    }

    postgres_execute(connection, &format!(
        "CREATE TABLE IF NOT EXISTS {recovery} (\
           message_key TEXT NOT NULL, recipient_bare_jid TEXT NOT NULL, room_jid TEXT NOT NULL, \
           thread_id TEXT NOT NULL DEFAULT '', stanza_id_by TEXT NOT NULL, stanza_id TEXT NOT NULL, \
           sender_jid TEXT NOT NULL, is_live_occupant INTEGER NOT NULL, room_members_only INTEGER NOT NULL, \
           sender_can_broadcast_channel_mention INTEGER NOT NULL, created_at_ms BIGINT NOT NULL, \
           completed_at_ms BIGINT, \
           PRIMARY KEY (recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id))"
    )).await?;
    lock_and_verify_postgres_relation(connection, schema, "groupchat_notification_recovery", true)
        .await?;
    postgres_execute(
        connection,
        &format!(
            "ALTER TABLE {recovery} ADD COLUMN IF NOT EXISTS \
         sender_can_broadcast_channel_mention INTEGER NOT NULL DEFAULT 0"
        ),
    )
    .await?;
    let message_key_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = 'groupchat_notification_recovery' \
           AND column_name = 'message_key')",
    )
    .bind(schema)
    .fetch_one(&mut *connection)
    .await
    .map_err(inbox_database_error)?;
    if !message_key_exists {
        info!("Discarding legacy groupchat notification recoveries before adding canonical message keys");
        postgres_execute(connection, &format!("DELETE FROM {recovery}")).await?;
        postgres_execute(
            connection,
            &format!("ALTER TABLE {recovery} ADD COLUMN message_key TEXT NOT NULL"),
        )
        .await?;
    }
    postgres_execute(
        connection,
        &format!(
            "CREATE INDEX IF NOT EXISTS idx_groupchat_notification_recovery_pending \
         ON {recovery} (created_at_ms, recipient_bare_jid, room_jid, thread_id, stanza_id) \
         WHERE completed_at_ms IS NULL"
        ),
    )
    .await?;
    postgres_execute(connection, &format!(
        "CREATE INDEX IF NOT EXISTS idx_groupchat_notification_recovery_completed_prune \
         ON {recovery} (completed_at_ms, recipient_bare_jid, room_jid, thread_id, stanza_id_by, stanza_id) \
         WHERE completed_at_ms IS NOT NULL"
    )).await?;
    Ok(())
}

async fn postgres_execute(
    connection: &mut PgConnection,
    sql: &str,
) -> Result<(), InboxStorageError> {
    sqlx::query(sql)
        .execute(connection)
        .await
        .map_err(inbox_database_error)?;
    Ok(())
}

fn inbox_database_error(error: impl std::fmt::Display) -> InboxStorageError {
    InboxStorageError::Other(error.to_string())
}

async fn initialize_sqlite(storage: &OwnerSchemaDb<'_>) -> Result<(), InboxStorageError> {
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
                message_key TEXT NOT NULL,
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
    migrate_recovery_message_key_column(storage).await?;
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

/// Adds the canonical ingress message key to SQLite recovery rows. Legacy rows
/// are discarded before the required column is added. This migration alone
/// does not establish a cutoff for old writers or deletion safety.
async fn migrate_recovery_message_key_column(
    storage: &OwnerSchemaDb<'_>,
) -> Result<(), InboxStorageError> {
    const COLUMN: &str = "message_key";
    let mut tx = storage
        .db
        .begin_immediate()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let mut rows = tx
        .query(
            "SELECT 1 FROM pragma_table_info('groupchat_notification_recovery') WHERE name = ?",
            crate::db_params![COLUMN],
        )
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let column_present = rows
        .next()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?
        .is_some();
    drop(rows);
    if !column_present {
        info!(
            column = COLUMN,
            "Discarding legacy groupchat notification recoveries before adding canonical message keys"
        );
        tx.execute("DELETE FROM groupchat_notification_recovery", ())
            .await
            .map_err(|error| InboxStorageError::Other(error.to_string()))?;
        tx.execute(
            "ALTER TABLE groupchat_notification_recovery \
             ADD COLUMN message_key TEXT NOT NULL",
            (),
        )
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    }
    tx.commit()
        .await
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    Ok(())
}

/// Adds the `sender_can_broadcast_channel_mention` column to existing
/// `groupchat_notification_recovery` tables that pre-date PR #738.
/// Idempotent on SQLite — re-runs at every startup
/// are no-ops once the column exists.
///
/// XEP-0513 §"Multi-User Chats Permissions" §304 default value `0`
/// (deny) on backfill: a recovery row created before the gate
/// landed has no frozen permission to consult; on replay the
/// candidate must NOT be granted channel-broadcast authority
/// retroactively. The recovery path then downgrades the class to
/// `NotifyAll`, which composes with the existing XEP-0492 policy.
async fn migrate_recovery_channel_broadcast_column(
    storage: &OwnerSchemaDb<'_>,
) -> Result<(), InboxStorageError> {
    const COLUMN: &str = "sender_can_broadcast_channel_mention";
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
    Ok(())
}

/// SQLite-only: returns `true` if the column exists on
/// `groupchat_notification_recovery`. Mirrors `needs_thread_migration`
/// but for a specific column rather than table-existence.
async fn recovery_column_present_sqlite(
    storage: &OwnerSchemaDb<'_>,
    column: &str,
) -> Result<bool, InboxStorageError> {
    column_present_sqlite(storage, "groupchat_notification_recovery", column).await
}

/// Adds the four call-thread columns (issue #919) to existing
/// `inbox_entries` tables that pre-date them. Idempotent on SQLite —
/// re-runs at every startup are no-ops once the columns
/// exist. All four are nullable: a row that pre-dates a call thread has
/// no anchor metadata, and a row whose call has not ended has no ended
/// summary.
async fn migrate_inbox_call_thread_columns(
    storage: &OwnerSchemaDb<'_>,
) -> Result<(), InboxStorageError> {
    let i64_type = crate::db::i64_sql_type(storage.db.driver());
    // (column, SQL type) for the four call-thread columns.
    let columns: [(&str, &str); 4] = [
        ("call_thread_kind", "TEXT"),
        ("call_thread_media", "TEXT"),
        ("call_ended_at", i64_type),
        ("call_duration", "TEXT"),
    ];
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
    Ok(())
}

/// SQLite-only: returns `true` if `column` exists on `table`. Probes
/// `PRAGMA table_info(<table>)`. When the table itself does not yet
/// exist, returns `true` so callers treat the missing-column ALTER as
/// moot (the CREATE TABLE IF NOT EXISTS path owns that case).
async fn column_present_sqlite(
    storage: &OwnerSchemaDb<'_>,
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
async fn needs_thread_migration(storage: &OwnerSchemaDb<'_>) -> Result<bool, InboxStorageError> {
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
