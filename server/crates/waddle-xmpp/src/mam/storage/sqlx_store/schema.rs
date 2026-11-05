use std::path::Path;

use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPool;
use sqlx::sqlite::SqlitePool;
use sqlx::{Connection as _, PgConnection, Postgres, Row};
use thiserror::Error;

use super::SqlxMamStorage;
use crate::mam::storage::MamStorageError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MamDatabaseDriver {
    Sqlite,
    Postgres,
}

pub(super) const SELECT_COLUMNS: &str =
    "id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, archive_seq";

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
    archive_seq INTEGER NOT NULL,
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

// Only incomplete archives are reranked; an interrupted backfill is safe to repeat.
const ARCHIVE_ORDINAL_BACKFILL: &str = r#"
UPDATE mam_messages SET archive_seq = ranked.rn
FROM (
    SELECT id, ROW_NUMBER() OVER (PARTITION BY room_jid ORDER BY timestamp ASC, id ASC) AS rn
    FROM mam_messages
    WHERE room_jid IN (SELECT DISTINCT room_jid FROM mam_messages WHERE archive_seq IS NULL)
) AS ranked
WHERE mam_messages.id = ranked.id
"#;

const ARCHIVE_ORDINAL_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_mam_room_archive_seq ON mam_messages(room_jid, archive_seq)";

const SQLITE_ARCHIVE_SEQUENCES: &str = r#"
CREATE TABLE IF NOT EXISTS mam_archive_sequences (
    archive_jid TEXT PRIMARY KEY,
    next_seq INTEGER NOT NULL
);
"#;

const POSTGRES_ARCHIVE_SEQUENCES: &str = r#"
CREATE TABLE IF NOT EXISTS mam_archive_sequences (
    archive_jid TEXT PRIMARY KEY,
    next_seq BIGINT NOT NULL
);
"#;

const SQLITE_SEED_ARCHIVE_SEQUENCES: &str = r#"
INSERT INTO mam_archive_sequences (archive_jid, next_seq)
SELECT room_jid, MAX(archive_seq) FROM mam_messages GROUP BY room_jid
ON CONFLICT (archive_jid) DO UPDATE
SET next_seq = MAX(mam_archive_sequences.next_seq, excluded.next_seq)
"#;

const POSTGRES_SEED_ARCHIVE_SEQUENCES: &str = r#"
INSERT INTO mam_archive_sequences (archive_jid, next_seq)
SELECT room_jid, MAX(archive_seq) FROM mam_messages GROUP BY room_jid
ON CONFLICT (archive_jid) DO UPDATE
SET next_seq = GREATEST(mam_archive_sequences.next_seq, excluded.next_seq)
"#;

const SQLITE_MAM_ORIGIN_DEDUP_INDEXES: &str = r#"
DROP INDEX IF EXISTS idx_mam_origin_groupchat_unique;
DROP INDEX IF EXISTS idx_mam_origin_direct_unique;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_content_unique;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_candidates;
DROP INDEX IF EXISTS idx_mam_origin_groupchat_sender_content_unique;
DROP INDEX IF EXISTS idx_mam_origin_direct_content_unique;
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
    archive_seq BIGINT NOT NULL,
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
DROP INDEX IF EXISTS idx_mam_origin_groupchat_sender_content_unique;
DROP INDEX IF EXISTS idx_mam_origin_direct_content_unique;
"#;

pub(super) const OBSOLETE_MAM_INDEXES: &[&str] = &[
    "idx_mam_origin_groupchat_unique",
    "idx_mam_origin_direct_unique",
    "idx_mam_origin_groupchat_content_unique",
    "idx_mam_origin_groupchat_candidates",
    "idx_mam_origin_groupchat_sender_content_unique",
    "idx_mam_origin_direct_content_unique",
];

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

async fn sqlite_column_is_nullable(
    pool: &SqlitePool,
    column: &str,
) -> Result<bool, MamStorageError> {
    let columns = sqlx::query("PRAGMA table_info(mam_messages)")
        .fetch_all(pool)
        .await?;
    Ok(columns.iter().any(|row| {
        row.try_get::<String, _>("name")
            .is_ok_and(|name| name == column)
            && row.try_get::<i64, _>("notnull").unwrap_or(0) == 0
    }))
}

pub(super) async fn ensure_sqlite_schema(pool: &SqlitePool) -> Result<(), MamStorageError> {
    execute_sqlite_batch(pool, SQLITE_MAM_SCHEMA).await?;
    ensure_sqlite_column(pool, "rich_payload", "TEXT").await?;
    ensure_sqlite_column(pool, "stanza_xml", "TEXT").await?;
    ensure_sqlite_column(pool, "nickname_generation", "INTEGER").await?;
    ensure_sqlite_column(pool, "parent_thread_id", "TEXT").await?;
    execute_sqlite_batch(pool, SQLITE_MAM_ORIGIN_DEDUP_INDEXES).await?;
    // SQLite has no DROP COLUMN IF EXISTS; inspect the schema first.
    for column in ["origin_dedup_sender_scope", "origin_dedup_fingerprint"] {
        let columns = sqlx::query("PRAGMA table_info(mam_messages)")
            .fetch_all(pool)
            .await?;
        if columns
            .iter()
            .any(|row| row.get::<String, _>("name") == column)
        {
            sqlx::query(&format!("ALTER TABLE mam_messages DROP COLUMN {column}"))
                .execute(pool)
                .await?;
        }
    }
    // Add the nullable ordinal first so historical rows can be ranked. SQLite
    // cannot alter column nullability: after backfill, the atomic rebuild
    // enforces archive_seq NOT NULL and also relaxes legacy body NOT NULL.
    ensure_sqlite_column(pool, "archive_seq", "INTEGER").await?;
    execute_sqlite_batch(pool, SQLITE_ARCHIVE_SEQUENCES).await?;
    // Fresh tables declare the column NOT NULL; only a legacy table whose
    // ordinal column is still nullable needs the one-time rank backfill.
    if sqlite_column_is_nullable(pool, "archive_seq").await? {
        sqlx::query(ARCHIVE_ORDINAL_BACKFILL).execute(pool).await?;
        sqlx::query(SQLITE_SEED_ARCHIVE_SEQUENCES)
            .execute(pool)
            .await?;
    }
    ensure_sqlite_table_shape(pool).await?;
    sqlx::query(ARCHIVE_ORDINAL_INDEX).execute(pool).await?;
    Ok(())
}

async fn ensure_sqlite_table_shape(pool: &SqlitePool) -> Result<(), MamStorageError> {
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
    let ordinal_is_not_null = columns.iter().any(|row| {
        row.get::<String, _>("name") == "archive_seq" && row.get::<i64, _>("notnull") != 0
    });
    if !body_is_not_null && ordinal_is_not_null {
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
            archive_seq INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )"#,
        r#"INSERT INTO mam_messages__new
            SELECT id, room_jid, timestamp, from_jid, to_jid, body,
                   stanza_id, thread_id, reply_to_id, reply_to_jid,
                   origin_id, message_type, stanza_xml, rich_payload,
                   nickname_generation, parent_thread_id, archive_seq, created_at
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
        "CREATE INDEX IF NOT EXISTS idx_mam_room_stanza ON mam_messages(room_jid, stanza_id)",
        ARCHIVE_ORDINAL_INDEX,
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

/// Outcome of an exclusively owned, autocommit MAM schema bootstrap.
#[derive(Debug, Error)]
pub enum MamSchemaBootstrapError {
    #[error("MAM schema bootstrap requires an autocommit connection")]
    TransactionOpen,
    #[error("failed to acquire the MAM schema advisory lock")]
    Lock(#[source] sqlx::Error),
    #[error("the MAM schema advisory lock is already held by this session")]
    AlreadyHeld,
    #[error("failed to inspect preexisting MAM objects before bootstrap")]
    Preflight(#[source] sqlx::Error),
    #[error(
        "the current role or a preexisting MAM object conflicts with the attested owner schema"
    )]
    PreexistingObjectConflict,
    #[error("MAM schema bootstrap may have committed a prefix or an unknown statement")]
    PartialOrUnknown(#[source] MamStorageError),
    #[error("MAM schema advisory unlock outcome is unknown")]
    Unlock(#[source] sqlx::Error),
    #[error("MAM schema advisory lock was not held at unlock")]
    LockNotHeld,
}

impl SqlxMamStorage {
    /// Run the same owner schema preparation as legacy `open` on a supplied
    /// autocommit session. This has no physical-attestation or role-handoff
    /// claim. The connection is consumed so an error or cancelled future
    /// discards a session that may still hold the MAM advisory lock.
    pub async fn bootstrap_legacy_postgres_schema_on_owned_connection(
        mut conn: PgConnection,
    ) -> Result<PgConnection, MamSchemaBootstrapError> {
        bootstrap_postgres_connection(&mut conn, None).await?;
        Ok(conn)
    }

    /// Bootstrap the internally selected MAM schema on an exclusively owned,
    /// direct PostgreSQL connection. The caller must already have established
    /// the physical identity, owner role, lineage, ledger, and their lock order.
    ///
    /// The connection must be in autocommit mode, including no raw `BEGIN`
    /// outside SQLx's transaction tracking. The MAM session lock is acquired
    /// after any caller-held lineage and ledger locks and is released before a
    /// successful return. The caller retains responsibility for its other locks.
    ///
    /// Every statement autocommits. A failure after DDL begins, or a cancelled
    /// future, may leave a committed prefix or unknown outcome. On every error
    /// or cancellation the owned connection is dropped rather than returned;
    /// a retry requires independently fencing the previous backend and primary
    /// and inspecting authoritative state afresh.
    pub async fn bootstrap_postgres_schema_on_owned_connection(
        mut conn: PgConnection,
        schema: &str,
        expected_owner: &str,
    ) -> Result<PgConnection, MamSchemaBootstrapError> {
        bootstrap_postgres_connection(&mut conn, Some((schema, expected_owner))).await?;
        Ok(conn)
    }
}

async fn bootstrap_postgres_connection(
    conn: &mut PgConnection,
    target: Option<(&str, &str)>,
) -> Result<(), MamSchemaBootstrapError> {
    if conn.is_in_transaction() {
        return Err(MamSchemaBootstrapError::TransactionOpen);
    }
    let already_held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND pid = pg_catalog.pg_backend_pid() AND granted AND objsubid = 1 AND ((classid::pg_catalog.int8 << 32) | objid::pg_catalog.int8) = $1)",
    )
    .bind(POSTGRES_SCHEMA_INIT_LOCK)
    .fetch_one(&mut *conn)
    .await
    .map_err(MamSchemaBootstrapError::Lock)?;
    if already_held {
        return Err(MamSchemaBootstrapError::AlreadyHeld);
    }
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(POSTGRES_SCHEMA_INIT_LOCK)
        .execute(&mut *conn)
        .await
        .map_err(MamSchemaBootstrapError::Lock)?;
    let migrate_chat_default = match target {
        Some((schema, owner)) => preflight_mam_owner_schema(&mut *conn, schema, owner).await?,
        None => false,
    };
    ensure_postgres_schema_locked(
        &mut *conn,
        target.map(|(schema, _)| schema),
        migrate_chat_default,
    )
    .await
    .map_err(MamSchemaBootstrapError::PartialOrUnknown)?;
    let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(POSTGRES_SCHEMA_INIT_LOCK)
        .fetch_one(&mut *conn)
        .await
        .map_err(MamSchemaBootstrapError::Unlock)?;
    if !unlocked {
        return Err(MamSchemaBootstrapError::LockNotHeld);
    }
    Ok(())
}

// Check existing names and their authority before the first autocommit DDL.
// This is a point-in-time preflight: callers must exclude non-cooperating DDL.
async fn preflight_mam_owner_schema(
    conn: &mut PgConnection,
    schema: &str,
    expected_owner: &str,
) -> Result<bool, MamSchemaBootstrapError> {
    let (current_user, schema_exists): (String, bool) = sqlx::query_as(
        "SELECT CURRENT_USER::pg_catalog.text, EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
    )
    .bind(schema)
    .fetch_one(&mut *conn)
    .await
    .map_err(MamSchemaBootstrapError::Preflight)?;
    if current_user != expected_owner || !schema_exists {
        return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
    }
    let expected_indexes = super::schema_verify::EXPECTED_MAM_INDEXES;
    let mut names = vec!["mam_messages", "mam_archive_sequences"];
    names.extend(expected_indexes.iter().map(|index| index.name));
    names.extend_from_slice(OBSOLETE_MAM_INDEXES);
    let rows = sqlx::query(
        r#"SELECT c.relname::pg_catalog.text AS name, c.relkind::pg_catalog.text AS kind,
                  c.relpersistence::pg_catalog.text AS persistence,
                  owner.rolname::pg_catalog.text AS owner,
                  target.relname::pg_catalog.text AS target
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles owner ON owner.oid = c.relowner
           LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid
           LEFT JOIN pg_catalog.pg_class target ON target.oid = i.indrelid
           WHERE n.nspname = $1 AND (c.relname::pg_catalog.text = ANY($2)
              OR target.relname IN ('mam_messages', 'mam_archive_sequences'))"#,
    )
    .bind(schema)
    .bind(&names)
    .fetch_all(&mut *conn)
    .await
    .map_err(MamSchemaBootstrapError::Preflight)?;
    let mut messages_exist = false;
    for row in rows {
        let name: String = row
            .try_get("name")
            .map_err(MamSchemaBootstrapError::Preflight)?;
        let kind: String = row
            .try_get("kind")
            .map_err(MamSchemaBootstrapError::Preflight)?;
        let persistence: String = row
            .try_get("persistence")
            .map_err(MamSchemaBootstrapError::Preflight)?;
        let owner: String = row
            .try_get("owner")
            .map_err(MamSchemaBootstrapError::Preflight)?;
        if owner != expected_owner || persistence != "p" {
            return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
        }
        if name == "mam_messages" || name == "mam_archive_sequences" {
            if kind != "r" {
                return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
            }
            messages_exist |= name == "mam_messages";
            continue;
        }
        let target: Option<String> = row
            .try_get("target")
            .map_err(MamSchemaBootstrapError::Preflight)?;
        let expected_target =
            if let Some(index) = expected_indexes.iter().find(|index| index.name == name) {
                index.table
            } else if OBSOLETE_MAM_INDEXES.contains(&name.as_str()) {
                "mam_messages"
            } else {
                return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
            };
        if kind != "i" || target.as_deref() != Some(expected_target) {
            return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
        }
    }
    if !messages_exist {
        return Ok(false);
    }
    let row = sqlx::query(
        r#"SELECT pg_catalog.format_type(a.atttypid, a.atttypmod) AS kind,
                  pg_catalog.pg_get_expr(d.adbin, d.adrelid) AS default_value
           FROM pg_catalog.pg_attribute a
           JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
           WHERE n.nspname = $1 AND c.relname = 'mam_messages' AND a.attname = 'message_type'
             AND a.attnum > 0 AND NOT a.attisdropped"#,
    )
    .bind(schema)
    .fetch_optional(conn)
    .await
    .map_err(MamSchemaBootstrapError::Preflight)?;
    let Some(row) = row else {
        return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
    };
    let kind: String = row
        .try_get("kind")
        .map_err(MamSchemaBootstrapError::Preflight)?;
    let default: Option<String> = row
        .try_get("default_value")
        .map_err(MamSchemaBootstrapError::Preflight)?;
    if kind != "text" {
        return Err(MamSchemaBootstrapError::PreexistingObjectConflict);
    }
    match default.as_deref() {
        Some("'normal'::text") => Ok(false),
        Some("'chat'::text") => Ok(true),
        _ => Err(MamSchemaBootstrapError::PreexistingObjectConflict),
    }
}

struct PooledSchemaGuard(Option<PoolConnection<Postgres>>);

impl PooledSchemaGuard {
    fn connection(&mut self) -> Result<&mut PgConnection, MamStorageError> {
        self.0.as_deref_mut().ok_or_else(|| {
            MamStorageError::Database("MAM schema connection guard is empty".to_owned())
        })
    }

    fn return_to_pool(mut self) {
        drop(self.0.take());
    }
}

impl Drop for PooledSchemaGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            drop(conn.detach());
        }
    }
}

pub(super) async fn ensure_postgres_schema(pool: &PgPool) -> Result<(), MamStorageError> {
    // Arm the guard before dispatching the advisory-lock request; even a lost
    // lock response may mean that the physical session acquired the lock.
    let mut guard = PooledSchemaGuard(Some(pool.acquire().await?));
    bootstrap_postgres_connection(guard.connection()?, None)
        .await
        .map_err(|error| match error {
            MamSchemaBootstrapError::PartialOrUnknown(source) => source,
            MamSchemaBootstrapError::Lock(source)
            | MamSchemaBootstrapError::Preflight(source)
            | MamSchemaBootstrapError::Unlock(source) => source.into(),
            error => MamStorageError::Database(error.to_string()),
        })?;
    guard.return_to_pool();
    Ok(())
}

async fn ensure_postgres_schema_locked(
    conn: &mut PgConnection,
    schema: Option<&str>,
    migrate_chat_default: bool,
) -> Result<(), MamStorageError> {
    let messages = super::postgres_relation(schema, "mam_messages");
    let sequences = super::postgres_relation(schema, "mam_archive_sequences");
    if schema.is_some() {
        // Split the fixed template before inserting an identifier: a valid
        // quoted schema name may itself contain a semicolon.
        for statement in POSTGRES_MAM_SCHEMA
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let statement = statement
                .replace("TEXT", "pg_catalog.text")
                .replace("TIMESTAMPTZ", "pg_catalog.timestamptz")
                .replace("BIGINT", "pg_catalog.int8")
                .replace(
                    "CREATE TABLE IF NOT EXISTS mam_messages",
                    "CREATE TABLE IF NOT EXISTS __MAM_TABLE__",
                )
                .replace("ON mam_messages(", "ON __MAM_TABLE__(")
                .replace("__MAM_TABLE__", &messages);
            sqlx::query(&statement).execute(&mut *conn).await?;
        }
    } else {
        execute_postgres_batch(&mut *conn, POSTGRES_MAM_SCHEMA).await?;
    }
    let text_type = if schema.is_some() {
        "pg_catalog.text"
    } else {
        "TEXT"
    };
    let bigint_type = if schema.is_some() {
        "pg_catalog.int8"
    } else {
        "BIGINT"
    };
    sqlx::query(&format!(
        "ALTER TABLE {messages} ADD COLUMN IF NOT EXISTS rich_payload {text_type}"
    ))
    .execute(&mut *conn)
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {messages} ADD COLUMN IF NOT EXISTS stanza_xml {text_type}"
    ))
    .execute(&mut *conn)
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {messages} ADD COLUMN IF NOT EXISTS nickname_generation {bigint_type}"
    ))
    .execute(&mut *conn)
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {messages} ADD COLUMN IF NOT EXISTS parent_thread_id {text_type}"
    ))
    .execute(&mut *conn)
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {messages} ADD COLUMN IF NOT EXISTS archive_seq {bigint_type}"
    ))
    .execute(&mut *conn)
    .await?;
    if schema.is_some() {
        sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {sequences} (archive_jid pg_catalog.text PRIMARY KEY, next_seq pg_catalog.int8 NOT NULL)"))
            .execute(&mut *conn)
            .await?;
    } else {
        execute_postgres_batch(&mut *conn, POSTGRES_ARCHIVE_SEQUENCES).await?;
    }
    // Fresh tables declare the column NOT NULL; only a legacy table whose
    // ordinal column is still nullable needs the one-time rank backfill.
    // Each statement autocommits under the advisory lock, so an interrupted
    // backfill resumes here on the next start.
    let ordinal_nullable: String = sqlx::query_scalar(
        "SELECT is_nullable FROM information_schema.columns WHERE table_schema = COALESCE($1, pg_catalog.current_schema()) AND table_name = 'mam_messages' AND column_name = 'archive_seq'",
    ).bind(schema).fetch_one(&mut *conn).await?;
    if ordinal_nullable == "YES" {
        let backfill = if schema.is_some() {
            format!("UPDATE {messages} AS target SET archive_seq = ranked.rn FROM (SELECT id, pg_catalog.row_number() OVER (PARTITION BY room_jid ORDER BY timestamp ASC, id ASC) AS rn FROM {messages} WHERE room_jid IN (SELECT DISTINCT room_jid FROM {messages} WHERE archive_seq IS NULL)) AS ranked WHERE target.id = ranked.id")
        } else {
            ARCHIVE_ORDINAL_BACKFILL.to_owned()
        };
        sqlx::query(&backfill).execute(&mut *conn).await?;
        let seed = if schema.is_some() {
            format!("INSERT INTO {sequences} AS target (archive_jid, next_seq) SELECT room_jid, pg_catalog.max(archive_seq) FROM {messages} GROUP BY room_jid ON CONFLICT (archive_jid) DO UPDATE SET next_seq = GREATEST(target.next_seq, EXCLUDED.next_seq)")
        } else {
            POSTGRES_SEED_ARCHIVE_SEQUENCES.to_owned()
        };
        sqlx::query(&seed).execute(&mut *conn).await?;
        sqlx::query(&format!(
            "ALTER TABLE {messages} ALTER COLUMN archive_seq SET NOT NULL"
        ))
        .execute(&mut *conn)
        .await?;
    }
    let ordinal_index =
        ARCHIVE_ORDINAL_INDEX.replace("ON mam_messages(", &format!("ON {messages}("));
    sqlx::query(&ordinal_index).execute(&mut *conn).await?;
    if schema.is_some() {
        for index in OBSOLETE_MAM_INDEXES {
            let index = super::postgres_relation(schema, index);
            sqlx::query(&format!("DROP INDEX IF EXISTS {index}"))
                .execute(&mut *conn)
                .await?;
        }
    } else {
        execute_postgres_batch(&mut *conn, POSTGRES_MAM_ORIGIN_DEDUP_INDEXES).await?;
    }
    sqlx::query(&format!(
        "ALTER TABLE {messages} DROP COLUMN IF EXISTS origin_dedup_sender_scope"
    ))
    .execute(&mut *conn)
    .await?;
    sqlx::query(&format!(
        "ALTER TABLE {messages} DROP COLUMN IF EXISTS origin_dedup_fingerprint"
    ))
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
         WHERE table_schema = COALESCE($1, pg_catalog.current_schema()) \
           AND table_name = 'mam_messages' \
           AND column_name = 'body'",
    )
    .bind(schema)
    .fetch_optional(&mut *conn)
    .await?;
    let needs_drop = is_nullable
        .as_deref()
        .is_some_and(|v| v.eq_ignore_ascii_case("NO"));
    if needs_drop {
        sqlx::query(&format!(
            "ALTER TABLE {messages} ALTER COLUMN body DROP NOT NULL"
        ))
        .execute(&mut *conn)
        .await?;
    }
    if migrate_chat_default {
        sqlx::query(&format!("ALTER TABLE {messages} ALTER COLUMN message_type SET DEFAULT 'normal'::pg_catalog.text"))
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod cutover_tests {
    use super::*;

    const LEGACY_COLUMNS_AND_INDEXES: &[&str] = &[
        "ALTER TABLE mam_messages ADD COLUMN origin_dedup_sender_scope TEXT",
        "ALTER TABLE mam_messages ADD COLUMN origin_dedup_fingerprint TEXT",
        "CREATE UNIQUE INDEX idx_mam_origin_groupchat_sender_content_unique ON mam_messages(room_jid, origin_id, origin_dedup_sender_scope, origin_dedup_fingerprint) WHERE origin_id IS NOT NULL AND message_type = 'groupchat'",
        "CREATE UNIQUE INDEX idx_mam_origin_direct_content_unique ON mam_messages(room_jid, origin_id, origin_dedup_fingerprint) WHERE origin_id IS NOT NULL AND message_type <> 'groupchat'",
    ];

    #[tokio::test]
    async fn sqlite_schema_cutover_drops_origin_dedupe_and_preserves_origin_lookup() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("SQLite pool");
        ensure_sqlite_schema(&pool).await.expect("initial schema");
        for statement in LEGACY_COLUMNS_AND_INDEXES {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("legacy schema");
        }
        ensure_sqlite_schema(&pool).await.expect("cutover");
        ensure_sqlite_schema(&pool)
            .await
            .expect("idempotent cutover");
        let columns = sqlx::query("PRAGMA table_info(mam_messages)")
            .fetch_all(&pool)
            .await
            .expect("columns");
        let names: Vec<String> = columns.iter().map(|row| row.get("name")).collect();
        assert!(names.contains(&"origin_id".to_owned()));
        assert!(!names.iter().any(|name| name.starts_with("origin_dedup_")));
        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'mam_messages'",
        )
        .fetch_all(&pool)
        .await
        .expect("indexes");
        assert!(indexes.contains(&"idx_mam_room_origin".to_owned()));
        assert!(!indexes.iter().any(|name| name.ends_with("content_unique")));
    }

    #[tokio::test]
    async fn postgres_schema_cutover_drops_origin_dedupe_and_preserves_origin_lookup() {
        let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping Postgres MAM schema test: WADDLE_TEST_POSTGRES_URL is unset");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("Postgres pool");
        let schema = format!("mam_cutover_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&pool)
            .await
            .expect("isolated schema");
        sqlx::query(&format!("SET search_path TO {schema}"))
            .execute(&pool)
            .await
            .expect("set schema");
        ensure_postgres_schema(&pool).await.expect("initial schema");
        for statement in LEGACY_COLUMNS_AND_INDEXES {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .expect("legacy schema");
        }
        ensure_postgres_schema(&pool).await.expect("cutover");
        ensure_postgres_schema(&pool)
            .await
            .expect("idempotent cutover");
        let names: Vec<String> = sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'mam_messages'").fetch_all(&pool).await.expect("columns");
        assert!(names.contains(&"origin_id".to_owned()));
        assert!(!names.iter().any(|name| name.starts_with("origin_dedup_")));
        let indexes: Vec<String> = sqlx::query_scalar("SELECT indexname FROM pg_indexes WHERE schemaname = current_schema() AND tablename = 'mam_messages'").fetch_all(&pool).await.expect("indexes");
        assert!(indexes.contains(&"idx_mam_room_origin".to_owned()));
        assert!(!indexes.iter().any(|name| name.ends_with("content_unique")));
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&pool)
            .await
            .expect("drop schema");
    }
}

#[cfg(test)]
#[path = "schema_ordinal_tests.rs"]
mod ordinal_tests;

#[cfg(test)]
#[path = "schema_owner_tests.rs"]
mod owner_tests;
