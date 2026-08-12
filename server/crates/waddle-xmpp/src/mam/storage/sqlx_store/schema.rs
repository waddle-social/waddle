use std::path::Path;

use sqlx::postgres::PgPool;
use sqlx::sqlite::SqlitePool;
use sqlx::{Connection as _, Row};

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
    origin_dedup_sender_scope TEXT,
    origin_dedup_fingerprint TEXT,
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
CREATE INDEX IF NOT EXISTS idx_mam_room_origin
    ON mam_messages(room_jid, origin_id);
CREATE INDEX IF NOT EXISTS idx_mam_room_stanza
    ON mam_messages(room_jid, stanza_id);
"#;

// Groupchat retry identity is the real bare JID inside the typed rich payload,
// not nickname_generation. The separately persisted sender scope preserves the
// conflict guard for concurrent exact retries; Rust still performs the final
// typed identity and content match after a conflict.
const SQLITE_MAM_ORIGIN_DEDUP_INDEXES: &str = r#"
DROP INDEX IF EXISTS idx_mam_origin_groupchat_unique;
DROP INDEX IF EXISTS idx_mam_origin_direct_unique;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_content_unique;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_candidates;
CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_origin_groupchat_sender_content_unique
    ON mam_messages(room_jid, origin_id, from_jid, origin_dedup_sender_scope, origin_dedup_fingerprint)
    WHERE origin_id IS NOT NULL AND origin_dedup_sender_scope IS NOT NULL AND origin_dedup_fingerprint IS NOT NULL AND message_type = 'groupchat';
CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_origin_direct_content_unique
    ON mam_messages(
        room_jid,
        origin_id,
        CASE
            WHEN instr(from_jid, '/') = 0 THEN from_jid
            ELSE substr(from_jid, 1, instr(from_jid, '/') - 1)
        END,
        CASE
            WHEN instr(to_jid, '/') = 0 THEN to_jid
            ELSE substr(to_jid, 1, instr(to_jid, '/') - 1)
        END,
        origin_dedup_fingerprint
    )
    WHERE origin_id IS NOT NULL AND origin_dedup_fingerprint IS NOT NULL AND message_type <> 'groupchat';
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
    origin_dedup_sender_scope TEXT,
    origin_dedup_fingerprint TEXT,
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
CREATE INDEX IF NOT EXISTS idx_mam_room_origin
    ON mam_messages(room_jid, origin_id);
CREATE INDEX IF NOT EXISTS idx_mam_room_stanza
    ON mam_messages(room_jid, stanza_id);
"#;

const POSTGRES_MAM_ORIGIN_DEDUP_INDEXES: &str = r#"
DROP INDEX IF EXISTS idx_mam_origin_groupchat_unique;
DROP INDEX IF EXISTS idx_mam_origin_direct_unique;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_content_unique;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_candidates;
CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_origin_groupchat_sender_content_unique
    ON mam_messages(room_jid, origin_id, from_jid, origin_dedup_sender_scope, origin_dedup_fingerprint)
    WHERE origin_id IS NOT NULL AND origin_dedup_sender_scope IS NOT NULL AND origin_dedup_fingerprint IS NOT NULL AND message_type = 'groupchat';
CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_origin_direct_content_unique
    ON mam_messages(
        room_jid,
        origin_id,
        (split_part(from_jid, '/', 1)),
        (split_part(to_jid, '/', 1)),
        origin_dedup_fingerprint
    )
    WHERE origin_id IS NOT NULL AND origin_dedup_fingerprint IS NOT NULL AND message_type <> 'groupchat';
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

async fn execute_postgres_batch(
    conn: &mut sqlx::PgConnection,
    sql: &str,
) -> Result<(), MamStorageError> {
    for statement in sql
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&mut *conn).await?;
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
    ensure_sqlite_column(pool, "parent_thread_id", "TEXT").await?;
    ensure_sqlite_column(pool, "origin_dedup_sender_scope", "TEXT").await?;
    ensure_sqlite_column(pool, "origin_dedup_fingerprint", "TEXT").await?;
    // Same body-NULL constraint risk as Postgres (see
    // `ensure_postgres_schema`): SQLite tables created before #228
    // retained `body TEXT NOT NULL` and `CREATE TABLE IF NOT EXISTS`
    // is a no-op against them. SQLite does not support
    // `ALTER COLUMN ... DROP NOT NULL` — relaxing the constraint
    // requires the 12-step table rebuild. Detect the legacy shape
    // and rebuild only when needed.
    ensure_sqlite_body_nullable(pool).await?;
    execute_sqlite_batch(pool, SQLITE_MAM_ORIGIN_DEDUP_INDEXES).await
}

async fn ensure_sqlite_body_nullable(pool: &SqlitePool) -> Result<(), MamStorageError> {
    let columns = sqlx::query("PRAGMA table_info(mam_messages)")
        .fetch_all(pool)
        .await?;
    let body_is_not_null = columns.iter().any(|row| {
        let name: String = match row.try_get("name") {
            Ok(value) => value,
            Err(_) => return false,
        };
        if name != "body" {
            return false;
        }
        // `PRAGMA table_info` reports `notnull` as an integer (1 = NOT NULL).
        let notnull: i64 = row.try_get("notnull").unwrap_or(0);
        notnull != 0
    });
    if !body_is_not_null {
        return Ok(());
    }
    // SQLite table rebuild: copy → drop → rename, all inside a
    // single transaction on a single pool-acquired connection. The
    // pool-per-statement path (`execute_sqlite_batch`) lets WAL
    // visibility race so a later `RENAME` connection can still see
    // the soon-to-be-dropped `mam_messages` table, producing
    // `SQLITE_ERROR: there is already another table or index with
    // this name`. The transaction also makes the rebuild atomic —
    // a crash mid-rebuild leaves the legacy table intact.
    //
    // No `PRAGMA foreign_keys=OFF` needed — `mam_messages` has no
    // foreign keys defined in this codebase.
    let mut tx = pool.begin().await?;
    for statement in [
        r#"CREATE TABLE mam_messages__new (
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
            origin_dedup_sender_scope TEXT,
            origin_dedup_fingerprint TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"#,
        r#"INSERT INTO mam_messages__new
            SELECT id, room_jid, timestamp, from_jid, to_jid, body,
                   stanza_id, thread_id, reply_to_id, reply_to_jid,
                   origin_id, message_type, stanza_xml, rich_payload,
                   nickname_generation, parent_thread_id, origin_dedup_sender_scope,
                   origin_dedup_fingerprint, created_at
            FROM mam_messages"#,
        "DROP TABLE mam_messages",
        "ALTER TABLE mam_messages__new RENAME TO mam_messages",
        // Indexes were dropped with the old table; recreate them.
        "CREATE INDEX IF NOT EXISTS idx_mam_room_timestamp ON mam_messages(room_jid, timestamp DESC)",
        "CREATE INDEX IF NOT EXISTS idx_mam_room_sender ON mam_messages(room_jid, from_jid, timestamp DESC)",
        "CREATE INDEX IF NOT EXISTS idx_mam_room_id ON mam_messages(room_jid, id)",
        "CREATE INDEX IF NOT EXISTS idx_mam_room_thread ON mam_messages(room_jid, thread_id, timestamp DESC)",
        "CREATE INDEX IF NOT EXISTS idx_mam_room_reply_to ON mam_messages(room_jid, reply_to_id, timestamp DESC)",
        "CREATE INDEX IF NOT EXISTS idx_mam_room_origin ON mam_messages(room_jid, origin_id)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_origin_groupchat_sender_content_unique ON mam_messages(room_jid, origin_id, from_jid, origin_dedup_sender_scope, origin_dedup_fingerprint) WHERE origin_id IS NOT NULL AND origin_dedup_sender_scope IS NOT NULL AND origin_dedup_fingerprint IS NOT NULL AND message_type = 'groupchat'",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_origin_direct_content_unique ON mam_messages(room_jid, origin_id, CASE WHEN instr(from_jid, '/') = 0 THEN from_jid ELSE substr(from_jid, 1, instr(from_jid, '/') - 1) END, CASE WHEN instr(to_jid, '/') = 0 THEN to_jid ELSE substr(to_jid, 1, instr(to_jid, '/') - 1) END, origin_dedup_fingerprint) WHERE origin_id IS NOT NULL AND origin_dedup_fingerprint IS NOT NULL AND message_type <> 'groupchat'",
    ] {
        sqlx::query(statement).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Serializes concurrent PostgreSQL schema initialization. Two callers
/// racing the `IF NOT EXISTS` DDL below (server replicas booting
/// together, or test processes sharing one database) collide on the
/// catalog's own unique indexes (`pg_type_typname_nsp_index`) even
/// though every statement is individually idempotent.
///
/// A session-scoped `pg_advisory_lock` guards the whole batch while every
/// statement keeps its original autocommit granularity: bundling the DDL
/// into one transaction instead would accumulate `ACCESS EXCLUSIVE` locks
/// across statements (`ADD COLUMN IF NOT EXISTS` takes it even when the
/// column exists) and deadlock against concurrent archive readers and
/// writers — observed as `deadlock detected` under parallel test suites.
const POSTGRES_SCHEMA_INIT_LOCK: i64 = 0x7761_6464_6d61_6d31; // "waddmam1"

pub(super) async fn ensure_postgres_schema(pool: &PgPool) -> Result<(), MamStorageError> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(POSTGRES_SCHEMA_INIT_LOCK)
        .execute(&mut *conn)
        .await?;
    let ensured = ensure_postgres_schema_locked(&mut conn).await;
    let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(POSTGRES_SCHEMA_INIT_LOCK)
        .execute(&mut *conn)
        .await;
    if ensured.is_err() || unlocked.is_err() {
        // The session advisory lock must never return to the pool still
        // held — a later checkout would wedge every other initializer.
        // Detaching closes the connection, which releases session locks.
        let connection = conn.detach();
        drop(connection.close().await);
    }
    ensured?;
    unlocked?;
    Ok(())
}

async fn ensure_postgres_schema_locked(
    conn: &mut sqlx::PgConnection,
) -> Result<(), MamStorageError> {
    execute_postgres_batch(&mut *conn, POSTGRES_MAM_SCHEMA).await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS rich_payload TEXT")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS stanza_xml TEXT")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS nickname_generation BIGINT")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS parent_thread_id TEXT")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS origin_dedup_sender_scope TEXT")
        .execute(&mut *conn)
        .await?;
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN IF NOT EXISTS origin_dedup_fingerprint TEXT")
        .execute(&mut *conn)
        .await?;
    // RFC 6121 §5.2.3 / XEP-0313 §3: `body` is nullable. Older
    // production deployments were created with `body TEXT NOT NULL`
    // (the schema before the typed `Option<String>` retype in #228);
    // `CREATE TABLE IF NOT EXISTS` is a no-op on those, so the
    // constraint never gets dropped. Body-less archive writes
    // (XEP-0444 reactions, XEP-0424 retractions, sticker- /
    // shared-file-only stanzas) bind `NULL` and are rejected with
    // `23502 not_null_violation`, dropping the row entirely. This
    // also unblocks the tombstone UPDATE site in `write.rs` that
    // sets `body = NULL` for XEP-0424 retractions.
    //
    // Gate on `information_schema.columns.is_nullable` so once the
    // constraint is dropped, subsequent restarts skip the ALTER.
    // `ALTER COLUMN ... DROP NOT NULL` is not a documented no-op on
    // Postgres — issuing it unconditionally would acquire
    // `ACCESS EXCLUSIVE` on this hot write table on every replica
    // boot, serializing rolling-deploy startups against the live
    // archive INSERT path. Mirrors the gating pattern used in
    // `pending_delivery/database/schema.rs` (#455).
    let is_nullable: Option<String> = sqlx::query_scalar(
        "SELECT is_nullable \
         FROM information_schema.columns \
         WHERE table_schema = current_schema() \
           AND table_name = 'mam_messages' \
           AND column_name = 'body'",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let needs_drop = is_nullable
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("NO"));
    if needs_drop {
        sqlx::query("ALTER TABLE mam_messages ALTER COLUMN body DROP NOT NULL")
            .execute(&mut *conn)
            .await?;
    }
    execute_postgres_batch(&mut *conn, POSTGRES_MAM_ORIGIN_DEDUP_INDEXES).await
}
