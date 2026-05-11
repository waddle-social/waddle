use super::*;
use crate::db::{Database, DatabaseDriver};

#[tokio::test]
async fn test_migration_runner_global() {
    let db = Database::in_memory("test-global").await.unwrap();
    let runner = MigrationRunner::global();

    // Run migrations
    let applied = runner.run(&db).await.unwrap();
    assert!(!applied.is_empty());

    // Running again should apply nothing
    let applied_again = runner.run(&db).await.unwrap();
    assert!(applied_again.is_empty());

    // Check version (global + shared waddle schema)
    let version = runner.current_version(&db).await.unwrap();
    assert_eq!(version, Some(1002));
}

#[tokio::test]
async fn test_migration_runner_waddle() {
    let db = Database::in_memory("test-waddle").await.unwrap();
    let runner = MigrationRunner::waddle();

    // Run migrations
    let applied = runner.run(&db).await.unwrap();
    assert!(!applied.is_empty());

    // Verify tables exist
    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
            (),
        )
        .await
        .unwrap();

    let mut tables = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get(0).unwrap();
        tables.push(name);
    }

    assert!(tables.contains(&"channels".to_string()));
    assert!(tables.contains(&"messages".to_string()));
    assert!(tables.contains(&"reactions".to_string()));
    assert!(tables.contains(&"attachments".to_string()));

    let mut rows = conn
        .query(
            r#"
                SELECT COUNT(*)
                FROM pragma_table_info('channels')
                WHERE name = 'pin_permission'
                "#,
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let has_pin_permission: i64 = row.get(0).unwrap();
    assert_eq!(has_pin_permission, 1);
}

#[tokio::test]
async fn test_waddle_v1002_adds_pin_permission_to_existing_v1001_schema() {
    let db = Database::in_memory("test-waddle-v1002-pin-permission")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(
        r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO _migrations (version, description) VALUES (1001, 'Hard-cut per-waddle schema with user_id principals')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        r#"
            CREATE TABLE channels (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                channel_type TEXT NOT NULL DEFAULT 'text',
                position INTEGER NOT NULL DEFAULT 0,
                is_default INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        (),
    )
    .await
    .unwrap();
    conn.execute(
        r#"
            INSERT INTO channels (id, name, description, channel_type, position, is_default)
            VALUES ('chat', 'Chat', 'General member chat', 'text', 0, 1)
            "#,
        (),
    )
    .await
    .unwrap();
    drop(conn);

    let runner = MigrationRunner::waddle();
    let applied = runner.run(&db).await.unwrap();
    assert_eq!(applied, vec![1002]);

    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query("SELECT pin_permission FROM channels WHERE id = 'chat'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let pin_permission: String = row.get(0).unwrap();
    assert_eq!(pin_permission, "admins-only");

    let version = runner.current_version(&db).await.unwrap();
    assert_eq!(version, Some(1002));
}

#[tokio::test]
async fn test_global_v0004_adds_policy_digest_to_existing_v0003_schema() {
    // Mirror of `test_waddle_v1002_adds_pin_permission_to_existing_v1001_schema`
    // for V0004: seed a database that already has the V0003-shaped
    // `user_avatar_fetch_state` (the migration history is at v3),
    // run the global migration runner, and assert that V0004 added
    // `last_fetch_policy_digest` and that the column accepts both
    // NULL and a non-NULL string value (both code paths used by
    // `backfill::persist_attempt`).
    let db = Database::in_memory("test-global-v0004-policy-digest")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(
        r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        (),
    )
    .await
    .unwrap();
    for (version, description) in [
        (1, "Hard-cut auth broker schema with roster pre-approval"),
        (
            2,
            "Add user_avatar_source provenance table for OIDC user-managed avatar guard",
        ),
        (
            3,
            "Add user_avatar_fetch_state for startup-backfill throttle",
        ),
    ] {
        conn.execute(
            "INSERT INTO _migrations (version, description) VALUES (?, ?)",
            (version, description),
        )
        .await
        .unwrap();
    }
    // Materialise the V0003 shape so V0004's ALTER has a target.
    conn.execute(
        r#"
            CREATE TABLE user_avatar_fetch_state (
                xmpp_localpart TEXT PRIMARY KEY,
                last_attempt_at TEXT NOT NULL,
                last_error TEXT,
                updated_at TEXT NOT NULL
            )
            "#,
        (),
    )
    .await
    .unwrap();
    // Seed a row mimicking the prod scenario: a `mime_rejected`
    // throttle persisted before V0004 existed (so the digest column
    // is NULL after the migration).
    conn.execute(
        r#"
            INSERT INTO user_avatar_fetch_state
              (xmpp_localpart, last_attempt_at, last_error, updated_at)
            VALUES ('alice', '2026-05-10T12:08:46.886293143+00:00', 'mime_rejected', '2026-05-10T12:08:46.886293143+00:00')
            "#,
        (),
    )
    .await
    .unwrap();
    drop(conn);

    // `MigrationRunner::global()` composes global + waddle migrations,
    // so the runner also reports applying 1001 and 1002 (the waddle
    // schema tables) on top of V0004. The test's invariant is V0004
    // specifically, asserted via the `pragma_table_info` probe below;
    // the version list is included in the assertion so a future PR
    // that reorders or renumbers can't silently shift it.
    let runner = MigrationRunner::global();
    let applied = runner.run(&db).await.unwrap();
    assert_eq!(applied, vec![4, 5, 1001, 1002]);

    // Column exists.
    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            r#"
                SELECT COUNT(*)
                FROM pragma_table_info('user_avatar_fetch_state')
                WHERE name = 'last_fetch_policy_digest'
                "#,
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let has_digest_column: i64 = row.get(0).unwrap();
    assert_eq!(
        has_digest_column, 1,
        "V0004 must add last_fetch_policy_digest column"
    );

    // Pre-V0004 row's digest column is NULL — this is the path
    // `should_throttle` uses to mark policy-dependent kinds as
    // not-yet-attempted on the first post-migration backfill.
    let mut rows = conn
        .query(
            "SELECT last_fetch_policy_digest FROM user_avatar_fetch_state WHERE xmpp_localpart = 'alice'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let digest: Option<String> = row.get(0).unwrap();
    assert_eq!(
        digest, None,
        "rows that predate V0004 must have NULL digest after the migration"
    );

    // Round-trip: write a non-NULL digest into the new column and
    // read it back. Confirms the column is plain TEXT-compatible
    // and `persist_attempt`'s 5-column UPSERT will land cleanly.
    conn.execute(
        "UPDATE user_avatar_fetch_state SET last_fetch_policy_digest = ? WHERE xmpp_localpart = 'alice'",
        ["test_digest_v1"],
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            "SELECT last_fetch_policy_digest FROM user_avatar_fetch_state WHERE xmpp_localpart = 'alice'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let digest: Option<String> = row.get(0).unwrap();
    assert_eq!(digest.as_deref(), Some("test_digest_v1"));

    let version = runner.current_version(&db).await.unwrap();
    assert_eq!(
        version,
        Some(1002),
        "current version reflects the highest applied across global+waddle"
    );
}

#[tokio::test]
async fn test_has_pending_migrations() {
    let db = Database::in_memory("test-pending").await.unwrap();
    let runner = MigrationRunner::global();

    // Should have pending migrations on fresh DB
    assert!(runner.has_pending(&db).await.unwrap());

    // Run migrations
    runner.run(&db).await.unwrap();

    // Should not have pending migrations
    assert!(!runner.has_pending(&db).await.unwrap());
}

#[tokio::test]
async fn test_incompatible_history_forces_hard_cut_reapply() {
    let db = Database::in_memory("test-incompatible-history")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(
        r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO _migrations (version, description) VALUES (1, 'legacy initial schema')",
        (),
    )
    .await
    .unwrap();
    drop(conn);

    let runner = MigrationRunner::global();
    let applied = runner.run(&db).await.unwrap();
    assert_eq!(applied, vec![1, 2, 3, 4, 5, 1001, 1002]);

    let applied_again = runner.run(&db).await.unwrap();
    assert!(applied_again.is_empty());

    let version = runner.current_version(&db).await.unwrap();
    assert_eq!(version, Some(1002));
}

#[tokio::test]
async fn test_incompatible_history_recreates_existing_owned_tables() {
    let db = Database::in_memory("test-incompatible-existing-tables")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(
        r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO _migrations (version, description) VALUES (1, 'legacy initial schema')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        r#"
            CREATE TABLE roster_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_jid TEXT NOT NULL,
                contact_jid TEXT NOT NULL,
                subscription TEXT NOT NULL DEFAULT 'none'
            )
            "#,
        (),
    )
    .await
    .unwrap();
    drop(conn);

    let runner = MigrationRunner::global();
    let applied = runner.run(&db).await.unwrap();
    assert_eq!(applied, vec![1, 2, 3, 4, 5, 1001, 1002]);

    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            r#"
                SELECT COUNT(*)
                FROM pragma_table_info('roster_items')
                WHERE name = 'approved'
                "#,
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let has_approved: i64 = row.get(0).unwrap();
    assert_eq!(has_approved, 1);
}

// --- Postgres dialect validation (no live DB required) ---
//
// These tests verify that every Postgres-dialect migration SQL:
//   - is non-empty
//   - contains no SQLite-only syntax (PRAGMA, AUTOINCREMENT, datetime('now'), bare BLOB type)
//   - uses DROP ... CASCADE instead of bare DROP TABLE
// and that SQLite SQL:
//   - contains no Postgres-only syntax (BIGSERIAL, BYTEA, CASCADE drops)

fn sqlite_only_patterns() -> Vec<&'static str> {
    vec!["PRAGMA ", "AUTOINCREMENT", "datetime('now')", " BLOB "]
}

fn postgres_only_patterns() -> Vec<&'static str> {
    vec!["BIGSERIAL", "BYTEA", "::TEXT"]
}

#[test]
fn postgres_global_v0001_has_no_sqlite_syntax() {
    let sql = global::V0001_AUTH_BROKER_SCHEMA_POSTGRES;
    assert!(
        !sql.is_empty(),
        "Postgres global V0001 SQL must not be empty"
    );
    for pat in sqlite_only_patterns() {
        assert!(
            !sql.contains(pat),
            "Postgres global V0001 SQL must not contain SQLite-only pattern: {pat}"
        );
    }
    assert!(
        sql.contains("CASCADE"),
        "Postgres global V0001 DROP TABLE statements must use CASCADE"
    );
}

#[test]
fn postgres_waddle_v0001_has_no_sqlite_syntax() {
    let sql = waddle::V0001_SCHEMA_POSTGRES;
    assert!(
        !sql.is_empty(),
        "Postgres waddle V0001 SQL must not be empty"
    );
    for pat in sqlite_only_patterns() {
        assert!(
            !sql.contains(pat),
            "Postgres waddle V0001 SQL must not contain SQLite-only pattern: {pat}"
        );
    }
    assert!(
        sql.contains("CASCADE"),
        "Postgres waddle V0001 DROP TABLE statements must use CASCADE"
    );
}

#[test]
fn sqlite_global_v0001_has_no_postgres_syntax() {
    let sql = global::V0001_AUTH_BROKER_SCHEMA;
    for pat in postgres_only_patterns() {
        assert!(
            !sql.contains(pat),
            "SQLite global V0001 SQL must not contain Postgres-only pattern: {pat}"
        );
    }
}

#[test]
fn sqlite_waddle_v0001_has_no_postgres_syntax() {
    let sql = waddle::V0001_SCHEMA;
    for pat in postgres_only_patterns() {
        assert!(
            !sql.contains(pat),
            "SQLite waddle V0001 SQL must not contain Postgres-only pattern: {pat}"
        );
    }
}

#[test]
fn migration_sql_for_returns_correct_dialect() {
    let m = Migration {
        version: 1,
        description: "test".to_string(),
        sql_sqlite: "SELECT 1",
        sql_postgres: "SELECT 2",
    };
    assert_eq!(m.sql_for(DatabaseDriver::Sqlite), "SELECT 1");
    assert_eq!(m.sql_for(DatabaseDriver::Postgres), "SELECT 2");
}

#[test]
fn all_migrations_have_non_empty_postgres_sql() {
    for m in MigrationRunner::single().migrations {
        assert!(
            !m.sql_postgres.is_empty(),
            "Migration v{} has empty Postgres SQL",
            m.version
        );
        assert!(
            !m.sql_sqlite.is_empty(),
            "Migration v{} has empty SQLite SQL",
            m.version
        );
    }
}

#[test]
fn postgres_channel_pin_permission_migration_is_hot_patch_safe() {
    assert!(
        waddle::V1002_ADD_CHANNEL_PIN_PERMISSION_POSTGRES.contains("ADD COLUMN IF NOT EXISTS"),
        "Postgres v1002 must tolerate prod databases where pin_permission was hot-patched before the migration was recorded"
    );
}
