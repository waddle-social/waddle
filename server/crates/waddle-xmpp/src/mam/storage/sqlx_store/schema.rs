use std::path::Path;

use sqlx::postgres::PgPool;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

use crate::mam::storage::MamStorageError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MamDatabaseDriver {
    Sqlite,
    Postgres,
}

pub(super) const SELECT_COLUMNS: &str =
    "id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id";

// RFC 6121 §5.2.3 / XEP-0313 §3: `body` is nullable here. NULL means
// no `<body>` element on the archived stanza; the empty string means
// an empty `<body></body>` element. Earlier schemas had `body TEXT NOT
// NULL` and collapsed both via `.unwrap_or_default()` in the
// projection, losing the distinction.
//
// RFC 6121 §5.2.2 ("Type Attribute"): "If absent, the message is
// implicitly of type `normal`." The column DEFAULT is `'normal'` to
// match. Pre-#228 commit 8 the default was `'chat'`, mirroring the
// removed `default_message_type() = "chat"` helper — a latent
// conformance bug. Production rows always bind an explicit value
// (the typed `MessageType` field on `ArchivedMessage`); the column
// DEFAULT only fires for direct INSERTs that omit the column, but
// fixing it removes the schema-level mismatch.
const SQLITE_MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT,
    stanza_id TEXT,
    thread_id TEXT,
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'normal',
    stanza_xml TEXT,
    rich_payload TEXT,
    nickname_generation INTEGER,
    parent_thread_id TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);
CREATE INDEX IF NOT EXISTS idx_mam_room_thread
    ON mam_messages(room_jid, thread_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to
    ON mam_messages(room_jid, reply_to_id, timestamp DESC);
"#;

// See `SQLITE_MAM_SCHEMA` for the body-nullability rationale.
const POSTGRES_MAM_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mam_messages (
    id TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL,
    from_jid TEXT NOT NULL,
    to_jid TEXT NOT NULL,
    body TEXT,
    stanza_id TEXT,
    thread_id TEXT,
    reply_to_id TEXT,
    reply_to_jid TEXT,
    origin_id TEXT,
    message_type TEXT NOT NULL DEFAULT 'normal',
    stanza_xml TEXT,
    rich_payload TEXT,
    nickname_generation BIGINT,
    parent_thread_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp
    ON mam_messages(room_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_sender
    ON mam_messages(room_jid, from_jid, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_id
    ON mam_messages(room_jid, id);
CREATE INDEX IF NOT EXISTS idx_mam_room_thread
    ON mam_messages(room_jid, thread_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to
    ON mam_messages(room_jid, reply_to_id, timestamp DESC);
"#;

pub(super) fn infer_driver(database_url: &str) -> Result<MamDatabaseDriver, MamStorageError> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Ok(MamDatabaseDriver::Postgres);
    }
    if lower.starts_with("sqlite:") {
        return Ok(MamDatabaseDriver::Sqlite);
    }

    Err(MamStorageError::Database(format!(
        "unsupported MAM database URL '{}': expected sqlite: or postgres://",
        database_url
    )))
}

pub(super) fn ensure_sqlite_parent_dir(database_url: &str) -> Result<(), MamStorageError> {
    let Some(path) = sqlite_database_path(database_url) else {
        return Ok(());
    };

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            MamStorageError::Database(format!("failed to create sqlite parent directory: {error}"))
        })?;
    }

    Ok(())
}

pub(super) fn is_in_memory_sqlite(database_url: &str) -> bool {
    matches!(
        database_url
            .strip_prefix("sqlite://")
            .or_else(|| database_url.strip_prefix("sqlite:")),
        Some(path) if path.starts_with(":memory:")
    )
}

fn sqlite_database_path(database_url: &str) -> Option<&Path> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?;
    if path.is_empty() || path.starts_with(":memory:") || path.starts_with("file:") {
        return None;
    }
    Some(Path::new(path))
}

async fn execute_sqlite_batch(pool: &SqlitePool, sql: &str) -> Result<(), MamStorageError> {
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn execute_postgres_batch(pool: &PgPool, sql: &str) -> Result<(), MamStorageError> {
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn ensure_sqlite_column(
    pool: &SqlitePool,
    column: &str,
    column_type: &str,
) -> Result<(), MamStorageError> {
    let columns = sqlx::query("PRAGMA table_info(mam_messages)")
        .fetch_all(pool)
        .await?;
    let exists = columns.iter().any(|row| {
        row.try_get::<String, _>("name")
            .is_ok_and(|name| name == column)
    });
    if !exists {
        let sql = format!("ALTER TABLE mam_messages ADD COLUMN {column} {column_type}");
        sqlx::query(&sql).execute(pool).await?;
    }
    Ok(())
}

pub(super) async fn ensure_sqlite_schema(pool: &SqlitePool) -> Result<(), MamStorageError> {
    execute_sqlite_batch(pool, SQLITE_MAM_SCHEMA).await?;
    ensure_sqlite_column(pool, "rich_payload", "TEXT").await?;
    ensure_sqlite_column(pool, "stanza_xml", "TEXT").await?;
    ensure_sqlite_column(pool, "nickname_generation", "INTEGER").await?;
    ensure_sqlite_column(pool, "parent_thread_id", "TEXT").await
}

pub(super) async fn ensure_postgres_schema(pool: &PgPool) -> Result<(), MamStorageError> {
    execute_postgres_batch(pool, POSTGRES_MAM_SCHEMA).await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS rich_payload TEXT")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS stanza_xml TEXT")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS nickname_generation BIGINT")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS parent_thread_id TEXT")
        .execute(pool)
        .await?;
    // RFC 6121 §5.2.3 / XEP-0313 §3: `body` is nullable. Older
    // production deployments were created with `body TEXT NOT NULL`
    // (the schema before the typed `Option<String>` retype in #228);
    // `CREATE TABLE IF NOT EXISTS` is a no-op on those, so the
    // constraint never gets dropped. Body-less archive writes
    // (XEP-0444 reactions, XEP-0424 retractions, sticker- /
    // shared-file-only stanzas) bind `NULL` and are rejected with
    // `23502 not_null_violation`, dropping the row entirely.
    //
    // `DROP NOT NULL` is idempotent on Postgres — a no-op when the
    // column is already nullable.
    sqlx::query("ALTER TABLE mam_messages ALTER COLUMN body DROP NOT NULL")
        .execute(pool)
        .await?;
    Ok(())
}
