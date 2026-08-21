use super::*;
use crate::db::{
    migration_checksum, Database, DatabaseConfig, DatabaseDriver, DatabaseError,
    MigrationLedgerError, MigrationNamespace, WADDLE_NAMESPACE_START,
};
use sqlx::{Column, Row};
use std::{collections::HashSet, fs, path::PathBuf};

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

    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name IN (?, ?, ?)",
            crate::db_params![
                "auth_context_id",
                "auth_context_version",
                "principal_auth_epoch"
            ],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let auth_context_columns: i64 = row.get(0).unwrap();
    assert_eq!(auth_context_columns, 3);

    // Check version (global + shared waddle schema)
    let version = runner.current_version(&db).await.unwrap();
    assert_eq!(version, Some(1011));
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
    assert!(tables.contains(&"group_dm_archive_boundaries".to_string()));

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

    let mut rows = conn
        .query(
            r#"
                SELECT COUNT(*)
                FROM pragma_table_info('channels')
                WHERE name IN ('members_only', 'public_room')
                "#,
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let has_policy_columns: i64 = row.get(0).unwrap();
    assert_eq!(has_policy_columns, 2);
}

#[tokio::test]
async fn test_waddle_v1002_adds_pin_permission_to_existing_v1001_schema() {
    let db = Database::in_memory("test-waddle-v1002-pin-permission")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        waddle::all()
            .into_iter()
            .filter(|migration| migration.version == 1001),
        DatabaseDriver::Sqlite,
    )
    .await;
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
    assert_eq!(
        applied,
        vec![1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );

    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT pin_permission, members_only, public_room FROM channels WHERE id = 'chat'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let pin_permission: String = row.get(0).unwrap();
    assert_eq!(pin_permission, "admins-only");
    let members_only: i64 = row.get(1).unwrap();
    assert_eq!(members_only, 1);
    let public_room: i64 = row.get(2).unwrap();
    assert_eq!(public_room, 1);

    let version = runner.current_version(&db).await.unwrap();
    assert_eq!(version, Some(1011));
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

    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        global::all()
            .into_iter()
            .filter(|migration| migration.version < 4),
        DatabaseDriver::Sqlite,
    )
    .await;
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
    create_legacy_session_schema(&conn).await;
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
    // so the runner also reports applying 1001 through 1011 (the waddle
    // schema tables) on top of V0004. The test's invariant is V0004
    // specifically, asserted via the `pragma_table_info` probe below;
    // the version list is included in the assertion so a future PR
    // that reorders or renumbers can't silently shift it.
    let runner = MigrationRunner::global();
    let applied = runner.run(&db).await.unwrap();
    assert_eq!(
        applied,
        vec![
            4, 5, 6, 7, 8, 9, 10, 11, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010,
            1011
        ]
    );

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
        Some(1011),
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
async fn incompatible_history_fails_closed_without_changing_the_ledger() {
    let db = Database::in_memory("test-incompatible-history")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO _migrations (version, description) VALUES (1, 'legacy initial schema')",
        (),
    )
    .await
    .unwrap();
    drop(conn);

    let error = MigrationRunner::global().run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::DescriptionMismatch {
            version: 1,
            ..
        })
    ));

    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT version, description FROM _migrations ORDER BY version",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<String>(1).unwrap(), "legacy initial schema");
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn incompatible_history_leaves_existing_owned_tables_untouched() {
    let db = Database::in_memory("test-incompatible-existing-tables")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();

    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
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

    let error = MigrationRunner::global().run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::DescriptionMismatch {
            version: 1,
            ..
        })
    ));

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
    assert_eq!(has_approved, 0);

    let mut rows = conn
        .query("SELECT description FROM _migrations WHERE version = 1", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "legacy initial schema");
}

#[tokio::test]
async fn sqlite_pre_ledger_history_is_adopted_once_before_pending_migrations() {
    let db = Database::in_memory("sqlite-migration-ledger-adoption")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();
    let first = migration_by_version(1001);
    conn.execute_batch(first.sql_for(DatabaseDriver::Sqlite))
        .await
        .unwrap();
    conn.execute(
        r#"
            CREATE TABLE _migrations (
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
        "INSERT INTO _migrations (version, description) VALUES (?, ?)",
        crate::db_params![first.version, first.description.as_str()],
    )
    .await
    .unwrap();
    drop(conn);

    let runner = MigrationRunner::waddle();
    assert_eq!(
        runner.run(&db).await.unwrap(),
        vec![1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );
    let expected_checksum = migration_checksum(&first, DatabaseDriver::Sqlite);
    assert_eq!(
        migration_ledger_checksum(&db, 1001).await.as_deref(),
        Some(expected_checksum.as_str())
    );
    assert!(runner.run(&db).await.unwrap().is_empty());
}

#[tokio::test]
async fn sqlite_single_runner_refuses_to_reapply_global_initial_migration_when_schema_exists_but_ledger_is_empty(
) {
    let db = Database::in_memory("sqlite-schema-without-ledger-global")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    runner.run(&db).await.unwrap();

    let schema_before = sqlite_schema_object_count(&db).await;
    let conn = db.guard().await.unwrap();
    conn.execute("DELETE FROM _migrations", ()).await.unwrap();
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::SchemaWithoutLedger {
            namespace: MigrationNamespace::Global,
            ref table,
        }) if table == "users"
    ));
    assert_eq!(migration_ledger_row_count(&db).await, 0);
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
    assert!(sqlite_table_exists(&db, "users").await);
    assert!(sqlite_table_exists(&db, "channels").await);
}

#[tokio::test]
async fn sqlite_single_runner_refuses_to_reapply_waddle_initial_migration_when_only_waddle_ledger_rows_are_missing(
) {
    let db = Database::in_memory("sqlite-schema-without-ledger-waddle")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    runner.run(&db).await.unwrap();

    let schema_before = sqlite_schema_object_count(&db).await;
    let global_before = migration_ledger_namespace_row_count(&db, MigrationNamespace::Global).await;
    let conn = db.guard().await.unwrap();
    conn.execute(
        "DELETE FROM _migrations WHERE version >= ?",
        crate::db_params![WADDLE_NAMESPACE_START],
    )
    .await
    .unwrap();
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::SchemaWithoutLedger {
            namespace: MigrationNamespace::Waddle,
            ref table,
        }) if table == "channels"
    ));
    assert_eq!(
        migration_ledger_namespace_row_count(&db, MigrationNamespace::Global).await,
        global_before
    );
    assert_eq!(
        migration_ledger_namespace_row_count(&db, MigrationNamespace::Waddle).await,
        0
    );
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
    assert!(sqlite_table_exists(&db, "users").await);
    assert!(sqlite_table_exists(&db, "channels").await);
}

#[tokio::test]
async fn sqlite_single_runner_backfills_checksums_when_legacy_ledger_has_no_pending_migrations() {
    let db = Database::in_memory("sqlite-ledger-pure-adoption-single")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    assert_eq!(runner.migrations.len(), 22);
    runner.run(&db).await.unwrap();

    let conn = db.guard().await.unwrap();
    conn.execute("ALTER TABLE _migrations DROP COLUMN checksum", ())
        .await
        .unwrap();
    drop(conn);

    assert!(runner.run(&db).await.unwrap().is_empty());
    assert_eq!(migration_ledger_row_count(&db).await, 22);
    assert_all_migration_checksums(&db, DatabaseDriver::Sqlite).await;
    assert!(runner.run(&db).await.unwrap().is_empty());
}

#[tokio::test]
async fn unknown_owned_ledger_version_fails_closed_without_changes() {
    let db = Database::in_memory("unknown-migration-ledger-version")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    runner.run(&db).await.unwrap();
    let before = migration_ledger_row_count(&db).await;
    let schema_before = sqlite_schema_object_count(&db).await;
    let conn = db.guard().await.unwrap();
    conn.execute(
        "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
        crate::db_params![12_i64, "future migration", "future-checksum"],
    )
    .await
    .unwrap();
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::UnknownVersion { version: 12, .. })
    ));
    assert_eq!(migration_ledger_row_count(&db).await, before + 1);
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
}

#[tokio::test]
async fn ledger_aware_old_binary_fails_closed_against_newer_ledger() {
    let db = Database::in_memory("old-binary-migration-ledger")
        .await
        .unwrap();
    MigrationRunner::single().run(&db).await.unwrap();
    let old_binary = MigrationRunner::new(
        global::all()
            .into_iter()
            .filter(|migration| migration.version < 11)
            .chain(waddle::all())
            .collect(),
    );

    let error = old_binary.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::UnknownVersion { version: 11, .. })
    ));
}

#[tokio::test]
async fn pre_v1010_catalog_refuses_a_v1010_ledger_until_roll_forward() {
    let db = Database::in_memory("pre-v1010-catalog-migration-ledger")
        .await
        .unwrap();
    MigrationRunner::single().run(&db).await.unwrap();
    let pre_v1010_catalog = MigrationRunner::new(
        global::all()
            .into_iter()
            .chain(
                waddle::all()
                    .into_iter()
                    .filter(|migration| migration.version < 1010),
            )
            .collect(),
    );

    let error = pre_v1010_catalog.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::UnknownVersion { version: 1010, .. })
    ));
}

/// An artifact predating the ingress foundation pack cannot safely start
/// after its ledger has recorded V1008/V1009; recovery is roll-forward only.
#[tokio::test]
async fn pre_ingress_catalog_refuses_the_v1008_v1009_ledger() {
    let db = Database::in_memory("pre-ingress-catalog-migration-ledger")
        .await
        .unwrap();
    MigrationRunner::single().run(&db).await.unwrap();
    let pre_ingress_catalog = MigrationRunner::new(
        global::all()
            .into_iter()
            .chain(
                waddle::all()
                    .into_iter()
                    .filter(|migration| migration.version < 1008),
            )
            .collect(),
    );

    let error = pre_ingress_catalog.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::UnknownVersion { version: 1008, .. })
    ));
}

#[tokio::test]
async fn checksum_mismatch_fails_closed_without_applying_migrations() {
    let db = Database::in_memory("checksum-mismatch-migration-ledger")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    runner.run(&db).await.unwrap();
    let before = migration_ledger_row_count(&db).await;
    let schema_before = sqlite_schema_object_count(&db).await;
    let conn = db.guard().await.unwrap();
    conn.execute(
        "UPDATE _migrations SET checksum = ? WHERE version = ?",
        crate::db_params!["wrong-checksum", 1_i64],
    )
    .await
    .unwrap();
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::ChecksumMismatch { version: 1, .. })
    ));
    assert_eq!(migration_ledger_row_count(&db).await, before);
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
}

#[tokio::test]
async fn v1010_checksum_mismatch_fails_closed_without_applying_migrations() {
    let db = Database::in_memory("checksum-mismatch-migration-ledger-v1010")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    runner.run(&db).await.unwrap();
    let before = migration_ledger_row_count(&db).await;
    let schema_before = sqlite_schema_object_count(&db).await;
    let conn = db.guard().await.unwrap();
    conn.execute(
        "UPDATE _migrations SET checksum = ? WHERE version = ?",
        crate::db_params!["wrong-checksum-v1010", 1010_i64],
    )
    .await
    .unwrap();
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::ChecksumMismatch {
            version: 1010,
            ..
        })
    ));
    assert_eq!(migration_ledger_row_count(&db).await, before);
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
}

#[tokio::test]
async fn v1010_rolls_forward_from_a_v1009_ledger() {
    let db = Database::in_memory("roll-forward-v1010").await.unwrap();
    let conn = db.guard().await.unwrap();
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        global::all().into_iter().chain(
            waddle::all()
                .into_iter()
                .filter(|migration| migration.version <= 1009),
        ),
        DatabaseDriver::Sqlite,
    )
    .await;
    drop(conn);

    let applied = MigrationRunner::single().run(&db).await.unwrap();
    assert_eq!(applied, vec![1010]);
    assert_eq!(
        migration_ledger_checksum(&db, 1010).await.as_deref(),
        Some(migration_checksum(&migration_by_version(1010), DatabaseDriver::Sqlite).as_str())
    );
}

#[tokio::test]
async fn missing_checksum_after_adoption_fails_closed() {
    let db = Database::in_memory("missing-checksum-migration-ledger")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    runner.run(&db).await.unwrap();
    let schema_before = sqlite_schema_object_count(&db).await;
    let conn = db.guard().await.unwrap();
    conn.execute(
        "UPDATE _migrations SET checksum = NULL WHERE version = ?",
        crate::db_params![1_i64],
    )
    .await
    .unwrap();
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::MissingChecksum { version: 1 })
    ));
    assert_eq!(migration_ledger_checksum(&db, 1).await, None);
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
}

#[tokio::test]
async fn gap_in_an_owned_namespace_fails_closed() {
    let db = Database::in_memory("gap-migration-ledger").await.unwrap();
    let conn = db.guard().await.unwrap();
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        global::all()
            .into_iter()
            .filter(|migration| matches!(migration.version, 1 | 3)),
        DatabaseDriver::Sqlite,
    )
    .await;
    drop(conn);

    let schema_before = sqlite_schema_object_count(&db).await;

    let error = MigrationRunner::single().run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::VersionGap {
            namespace: MigrationNamespace::Global,
            missing: 2,
            applied_after: 3,
        })
    ));
    assert_eq!(migration_ledger_row_count(&db).await, 2);
    assert_eq!(sqlite_schema_object_count(&db).await, schema_before);
}

#[tokio::test]
async fn waddle_runner_ignores_unowned_global_ledger_rows() {
    let db = Database::in_memory("namespace-owned-migration-ledger")
        .await
        .unwrap();
    MigrationRunner::single().run(&db).await.unwrap();
    let waddle_runner = MigrationRunner::waddle();
    assert!(waddle_runner.run(&db).await.unwrap().is_empty());

    let conn = db.guard().await.unwrap();
    conn.execute(
        "UPDATE _migrations SET checksum = ? WHERE version = ?",
        crate::db_params!["wrong-global-checksum", 1_i64],
    )
    .await
    .unwrap();
    drop(conn);

    assert!(waddle_runner.run(&db).await.unwrap().is_empty());
    assert!(matches!(
        MigrationRunner::single().run(&db).await.unwrap_err(),
        DatabaseError::MigrationLedger(MigrationLedgerError::ChecksumMismatch { version: 1, .. })
    ));
}

#[tokio::test]
async fn single_runner_is_idempotent_with_a_stable_ledger() {
    let db = Database::in_memory("idempotent-migration-ledger")
        .await
        .unwrap();
    let runner = MigrationRunner::single();
    let first = runner.run(&db).await.unwrap();
    let count = migration_ledger_row_count(&db).await;
    assert!(!first.is_empty());
    assert!(runner.run(&db).await.unwrap().is_empty());
    assert_eq!(migration_ledger_row_count(&db).await, count);
}

#[tokio::test]
async fn migration_apply_error_names_the_failing_migration_and_preserves_its_source() {
    let db = Database::in_memory("migration-apply-error-context")
        .await
        .expect("open in-memory database");
    let runner = MigrationRunner::new(vec![Migration {
        version: 42,
        description: "invalid migration for error context".to_string(),
        sql_sqlite: "THIS IS NOT VALID SQL;",
        sql_postgres: "THIS IS NOT VALID SQL;",
    }]);

    let error = runner
        .run(&db)
        .await
        .expect_err("invalid migration must fail");
    assert!(matches!(
        &error,
        DatabaseError::MigrationApply { version: 42, .. }
    ));
    assert!(error.to_string().contains("migration v42"));
    let source = std::error::Error::source(&error)
        .expect("MigrationApply must retain the database error source chain");
    assert!(source.to_string().contains("Internal database error"));
    assert!(
        std::error::Error::source(source).is_some(),
        "MigrationApply must retain the underlying sqlx error beneath DatabaseError::Internal"
    );
}

#[tokio::test]
async fn postgres_pre_ledger_history_is_adopted_once_before_pending_migrations() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (migration ledger adoption)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_adoption");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    let first = migration_by_version(1001);
    conn.execute_batch(first.sql_for(DatabaseDriver::Postgres))
        .await
        .expect("create V1001 schema");
    conn.execute(
        r#"
            CREATE TABLE _migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        (),
    )
    .await
    .expect("create pre-ledger migrations table");
    conn.execute(
        "INSERT INTO _migrations (version, description) VALUES (?, ?)",
        crate::db_params![first.version, first.description.as_str()],
    )
    .await
    .expect("seed pre-ledger migration");
    drop(conn);

    let runner = MigrationRunner::waddle();
    assert_eq!(
        runner.run(&db).await.expect("adopt and run migrations"),
        vec![1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );
    let expected_checksum = migration_checksum(&first, DatabaseDriver::Postgres);
    assert_eq!(
        migration_ledger_checksum(&db, 1001).await.as_deref(),
        Some(expected_checksum.as_str())
    );
    assert!(runner
        .run(&db)
        .await
        .expect("second migration run")
        .is_empty());

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_single_runner_refuses_to_reapply_global_initial_migration_when_schema_exists_but_ledger_is_empty(
) {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set (postgres schema without ledger global)"
        );
        return;
    };
    let schema = unique_postgres_schema_name("ledger_missing_global");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let runner = MigrationRunner::single();
    runner.run(&db).await.expect("initial single migration run");

    let schema_before = postgres_schema_object_count(&db).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute("DELETE FROM _migrations", ())
        .await
        .expect("delete all migration ledger rows");
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::SchemaWithoutLedger {
            namespace: MigrationNamespace::Global,
            ref table,
        }) if table == "users"
    ));
    assert_eq!(migration_ledger_row_count(&db).await, 0);
    assert_eq!(postgres_schema_object_count(&db).await, schema_before);
    assert!(postgres_table_exists(&db, "users").await);
    assert!(postgres_table_exists(&db, "channels").await);

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_single_runner_refuses_to_reapply_waddle_initial_migration_when_only_waddle_ledger_rows_are_missing(
) {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set (postgres schema without ledger waddle)"
        );
        return;
    };
    let schema = unique_postgres_schema_name("ledger_missing_waddle");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let runner = MigrationRunner::single();
    runner.run(&db).await.expect("initial single migration run");

    let schema_before = postgres_schema_object_count(&db).await;
    let global_before = migration_ledger_namespace_row_count(&db, MigrationNamespace::Global).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(
        "DELETE FROM _migrations WHERE version >= ?",
        crate::db_params![WADDLE_NAMESPACE_START],
    )
    .await
    .expect("delete waddle migration ledger rows");
    drop(conn);

    let error = runner.run(&db).await.unwrap_err();
    assert!(matches!(
        error,
        DatabaseError::MigrationLedger(MigrationLedgerError::SchemaWithoutLedger {
            namespace: MigrationNamespace::Waddle,
            ref table,
        }) if table == "channels"
    ));
    assert_eq!(
        migration_ledger_namespace_row_count(&db, MigrationNamespace::Global).await,
        global_before
    );
    assert_eq!(
        migration_ledger_namespace_row_count(&db, MigrationNamespace::Waddle).await,
        0
    );
    assert_eq!(postgres_schema_object_count(&db).await, schema_before);
    assert!(postgres_table_exists(&db, "users").await);
    assert!(postgres_table_exists(&db, "channels").await);

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_single_runner_backfills_checksums_when_legacy_ledger_has_no_pending_migrations() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set (postgres pure migration ledger adoption)"
        );
        return;
    };
    let schema = unique_postgres_schema_name("ledger_pure_adoption");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let runner = MigrationRunner::single();
    assert_eq!(runner.migrations.len(), 21);
    runner.run(&db).await.expect("initial single migration run");

    let conn = db.guard().await.expect("postgres guard");
    conn.execute("ALTER TABLE _migrations DROP COLUMN checksum", ())
        .await
        .expect("drop checksum column");
    drop(conn);

    assert!(runner
        .run(&db)
        .await
        .expect("pure adoption rerun")
        .is_empty());
    assert_eq!(migration_ledger_row_count(&db).await, 21);
    assert_all_migration_checksums(&db, DatabaseDriver::Postgres).await;
    assert!(runner
        .run(&db)
        .await
        .expect("third single migration run")
        .is_empty());

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_migration_runner_blocks_until_the_advisory_lock_is_released() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (migration ledger lock)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_lock");
    let (db_a, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let db_b = open_postgres_database_in_schema(&database_url, &schema, "ledger-lock-b", 10).await;
    let mut lock_tx = db_a.begin().await.expect("start lock transaction");
    lock_tx
        .query(
            "SELECT pg_advisory_xact_lock(?)",
            crate::db_params![super::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY],
        )
        .await
        .expect("take migration advisory lock");

    let local = tokio::task::LocalSet::new();
    let mut run = local.spawn_local(async move {
        let runner = MigrationRunner::single();
        runner.run(&db_b).await
    });
    local
        .run_until(async {
            wait_for_postgres_advisory_waiter(
                &admin,
                super::runner::MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
            )
            .await;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), &mut run)
                    .await
                    .is_err(),
                "migration runner must block while another transaction holds its advisory lock"
            );
        })
        .await;
    lock_tx
        .commit()
        .await
        .expect("release migration advisory lock");
    let applied = local
        .run_until(&mut run)
        .await
        .expect("migration runner task")
        .expect("migration runner result");
    assert!(!applied.is_empty());

    drop(db_a);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_concurrent_migration_runners_record_one_complete_ledger() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (migration ledger race)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_race");
    let (db_a, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let db_b = open_postgres_database_in_schema(&database_url, &schema, "ledger-race-b", 10).await;
    let expected: Vec<i64> = MigrationRunner::single()
        .migrations
        .iter()
        .map(|migration| migration.version)
        .collect();
    let first_runner = MigrationRunner::single();
    let second_runner = MigrationRunner::single();
    let (first, second) = tokio::join!(first_runner.run(&db_a), second_runner.run(&db_b));
    let first = first.expect("first migration runner");
    let second = second.expect("second migration runner");
    assert!(
        (first == expected && second.is_empty()) || (second == expected && first.is_empty()),
        "the advisory lock must let exactly one runner apply the full catalog"
    );
    assert_eq!(
        migration_ledger_row_count(&db_a).await,
        expected.len() as i64
    );
    for migration in MigrationRunner::single().migrations {
        let expected_checksum = migration_checksum(&migration, DatabaseDriver::Postgres);
        assert_eq!(
            migration_ledger_checksum(&db_a, migration.version)
                .await
                .as_deref(),
            Some(expected_checksum.as_str()),
            "migration v{} must be recorded once with its active-dialect checksum",
            migration.version
        );
    }

    drop(db_b);
    drop(db_a);
    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_incompatible_ingress_schema_drift_fails_without_recording_new_migrations() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (incompatible ingress schema drift)");
        return;
    };
    let schema = unique_postgres_schema_name("ingress_schema_drift");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Postgres), ())
        .await
        .expect("create migration ledger");
    conn.execute("CREATE TABLE ingress_messages (wrong_shape INTEGER)", ())
        .await
        .expect("create incompatible ingress_messages table");
    drop(conn);

    let error = MigrationRunner::single()
        .run(&db)
        .await
        .expect_err("incompatible ingress table must make V1008 fail");
    assert!(matches!(
        error,
        DatabaseError::MigrationApply { version: 1008, .. }
    ));

    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM _migrations WHERE version IN (?, ?, ?)",
            crate::db_params![1008_i64, 1009_i64, 1010_i64],
        )
        .await
        .expect("query ingress migration ledger rows");
    let row = rows
        .next()
        .await
        .expect("read ingress migration ledger rows")
        .expect("ingress migration ledger row");
    let recorded: i64 = row.get(0).expect("decode ingress migration ledger rows");
    assert_eq!(
        recorded, 0,
        "failed V1008 must not record V1008, V1009, or V1010"
    );
    drop(conn);

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

/// Runs without PostgreSQL: the extractor must accept the checked-in
/// ConfigMap exactly as formatted, so a parser/format drift fails here and
/// not only in the Postgres lane.
#[test]
fn ingress_monitoring_configmap_extracts_every_query() {
    let yaml_path = monitoring_configmap_path();
    let yaml = fs::read_to_string(&yaml_path).unwrap_or_else(|error| {
        panic!(
            "read ingress monitoring ConfigMap at {} (is the flake.nix postUnpack copy intact?): {error}",
            yaml_path.display()
        )
    });
    let queries = extract_monitoring_queries(&yaml).expect("extract ingress monitoring queries");
    let names: Vec<&str> = queries.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(names, EXPECTED_MONITORING_QUERIES);
    for query in &queries {
        assert!(
            !query.metrics.is_empty() && query.sql.contains("FROM"),
            "query {} extracted without metrics or SQL",
            query.name
        );
    }
}

const EXPECTED_MONITORING_QUERIES: [&str; 5] = [
    "waddle_ingress_table",
    "waddle_ingress_gc",
    "waddle_ingress_messages",
    "waddle_ingress_cohort",
    "waddle_ingress_streams",
];

fn monitoring_configmap_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../../infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml",
    )
}

#[derive(Debug)]
struct MonitoringQuery {
    name: String,
    sql: String,
    metrics: Vec<String>,
}

#[tokio::test]
async fn postgres_monitoring_queries_match_migrated_ingress_schema() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (ingress monitoring queries)");
        return;
    };
    let yaml_path = monitoring_configmap_path();
    // Never skip on a missing file: the nix test lanes materialize this
    // path through `testArgs.postUnpack` in flake.nix, and a silent skip
    // would turn this guard green-by-absence (same rule as the Mimir-rules
    // guard in waddle-xmpp).
    let yaml = fs::read_to_string(&yaml_path).unwrap_or_else(|error| {
        panic!(
            "read ingress monitoring ConfigMap at {} (is the flake.nix postUnpack copy intact?): {error}",
            yaml_path.display()
        )
    });
    let queries = extract_monitoring_queries(&yaml).expect("extract ingress monitoring queries");
    let names: Vec<&str> = queries.iter().map(|query| query.name.as_str()).collect();
    assert_eq!(
        names, EXPECTED_MONITORING_QUERIES,
        "the ConfigMap query set changed; update this test's expectations deliberately"
    );

    let schema = unique_postgres_schema_name("monitoring_queries");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("run migrations in isolated postgres schema");

    let query_pool = sqlx::PgPool::connect(db.database_url())
        .await
        .expect("connect isolated postgres query pool");
    // CloudNativePG executes custom queries with `SET ROLE pg_monitor`
    // (one transaction per query); mirror that so a missing grant on an
    // ingress table fails here instead of on the production exporter.
    let mut monitor_conn = query_pool
        .acquire()
        .await
        .expect("acquire connection for pg_monitor queries");
    sqlx::query("SET ROLE pg_monitor")
        .execute(&mut *monitor_conn)
        .await
        .expect("assume pg_monitor like the CNPG exporter");
    for monitoring_query in queries {
        // The production query pins `schemaname = 'public'`; the fixture
        // migrates into an isolated schema, so point it at that schema to
        // keep the table-name literals under test.
        let sql = if monitoring_query.name == "waddle_ingress_table" {
            assert!(
                monitoring_query.sql.contains("schemaname = 'public'"),
                "waddle_ingress_table must filter on schemaname = 'public'"
            );
            monitoring_query
                .sql
                .replace("schemaname = 'public'", "schemaname = current_schema()")
        } else {
            monitoring_query.sql.clone()
        };
        let rows = sqlx::query(&sql)
            .fetch_all(&mut *monitor_conn)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "execute monitoring query {} against migrated schema: {error}",
                    monitoring_query.name
                )
            });
        assert_declared_metric_columns(&monitoring_query, &rows);

        match monitoring_query.name.as_str() {
            "waddle_ingress_gc" | "waddle_ingress_streams" => assert_eq!(
                rows.len(),
                1,
                "{} must return one aggregate row for an empty ingress schema",
                monitoring_query.name
            ),
            "waddle_ingress_cohort" => {
                let states: Vec<(String, i64)> = rows
                    .iter()
                    .map(|row| {
                        (
                            row.try_get("state").expect("decode cohort state"),
                            row.try_get("count").expect("decode cohort count"),
                        )
                    })
                    .collect();
                assert_eq!(
                    states,
                    vec![
                        ("live".to_string(), 0),
                        ("terminal_referenced".to_string(), 0),
                        ("terminal_unreferenced".to_string(), 0),
                    ],
                    "waddle_ingress_cohort must report each empty lifecycle state"
                );
            }
            "waddle_ingress_table" => {
                let tables: Vec<String> = rows
                    .iter()
                    .map(|row| row.try_get("table").expect("decode table name"))
                    .collect();
                assert_eq!(
                    tables,
                    [
                        "ingress_deliveries",
                        "ingress_effect_intents",
                        "ingress_messages",
                        "ingress_origin_aliases",
                        "ingress_sm_refs",
                        "ingress_sm_streams",
                    ],
                    "every monitored ingress table must exist in the migrated schema"
                );
            }
            "waddle_ingress_messages" => {}
            other => panic!("unexpected ingress monitoring query {other}"),
        }
    }

    drop(monitor_conn);
    query_pool.close().await;
    drop(db);
    drop_postgres_schema(&admin, &schema).await;
}

fn assert_declared_metric_columns(query: &MonitoringQuery, rows: &[sqlx::postgres::PgRow]) {
    let Some(row) = rows.first() else {
        return;
    };
    let columns: HashSet<&str> = row.columns().iter().map(|column| column.name()).collect();
    for metric in &query.metrics {
        assert!(
            columns.contains(metric.as_str()),
            "{} result is missing declared metric column {metric}; got {columns:?}",
            query.name
        );
    }
}

/// Extract the fixed CNPG ConfigMap shape without a YAML dependency.
///
/// The `queries` literal must use four-space query names, an exact six-space
/// `query: |` field whose SQL is indented eight spaces, and an exact
/// six-space `metrics:` field whose metric maps begin with eight-space
/// `- <column>:` entries. Formatting that changes those anchors is rejected
/// so no query can be silently omitted from schema-drift coverage.
fn extract_monitoring_queries(yaml: &str) -> Result<Vec<MonitoringQuery>, String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let queries_start = lines
        .iter()
        .position(|line| *line == "  queries: |")
        .ok_or("missing exact ConfigMap `  queries: |` block")?;
    let query_end = lines[queries_start + 1..]
        .iter()
        .position(|line| !line.is_empty() && !line.starts_with(' '))
        .map_or(lines.len(), |offset| queries_start + 1 + offset);

    let mut headers = Vec::new();
    for (index, &line) in lines
        .iter()
        .enumerate()
        .take(query_end)
        .skip(queries_start + 1)
    {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with('\t') {
            return Err(format!("line {} uses tab indentation", index + 1));
        }
        if indentation(line) == 4 {
            let name = line
                .strip_prefix("    ")
                .and_then(|value| value.strip_suffix(':'))
                .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
                .ok_or_else(|| format!("line {} is not a four-space query name", index + 1))?;
            headers.push((index, name));
        }
    }
    if headers.is_empty() {
        return Err("ConfigMap queries block contains no four-space query names".to_string());
    }
    if let Some((first_header, _)) = headers.first() {
        for (offset, line) in lines[queries_start + 1..*first_header].iter().enumerate() {
            if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
                return Err(format!(
                    "queries block has content before its first query name at line {}",
                    queries_start + 2 + offset
                ));
            }
        }
    }

    headers
        .iter()
        .enumerate()
        .map(|(header_index, &(start, name))| {
            let end = headers
                .get(header_index + 1)
                .map_or(query_end, |(next, _)| *next);
            extract_query_block(&lines, start, end, name)
        })
        .collect()
}

fn extract_query_block(
    lines: &[&str],
    start: usize,
    end: usize,
    name: &str,
) -> Result<MonitoringQuery, String> {
    let query_fields: Vec<usize> = (start + 1..end)
        .filter(|&index| lines[index] == "      query: |")
        .collect();
    let metrics_fields: Vec<usize> = (start + 1..end)
        .filter(|&index| lines[index] == "      metrics:")
        .collect();
    if query_fields.len() != 1 || metrics_fields.len() != 1 {
        return Err(format!(
            "query {name} must contain exactly one `      query: |` and one `      metrics:` field"
        ));
    }

    let query_start = query_fields[0];
    let metrics_start = metrics_fields[0];
    if metrics_start <= query_start {
        return Err(format!("query {name} places metrics before its SQL"));
    }
    for (offset, line) in lines[start + 1..query_start].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with("      ") || line.starts_with("        ") {
            return Err(format!(
                "query {name} has malformed field indentation before SQL at line {}",
                start + 2 + offset
            ));
        }
    }
    let sql_end = (query_start + 1..end)
        .find(|&index| lines[index].starts_with("      ") && !lines[index].starts_with("        "))
        .unwrap_or(end);
    if sql_end > metrics_start {
        return Err(format!("query {name} has no field after its SQL literal"));
    }

    let mut sql = Vec::new();
    for (offset, line) in lines[query_start + 1..sql_end].iter().enumerate() {
        if line.is_empty() {
            sql.push(String::new());
        } else if let Some(sql_line) = line.strip_prefix("        ") {
            sql.push(sql_line.to_string());
        } else {
            return Err(format!(
                "query {name} has non-eight-space SQL indentation at line {}",
                query_start + 2 + offset
            ));
        }
    }
    if sql.iter().all(|line| line.trim().is_empty()) {
        return Err(format!("query {name} has an empty SQL literal"));
    }
    for (offset, line) in lines[sql_end..metrics_start].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with("      ") || line.starts_with("        ") {
            return Err(format!(
                "query {name} has malformed field indentation before metrics at line {}",
                sql_end + 1 + offset
            ));
        }
    }

    let mut metrics = Vec::new();
    for (offset, line) in lines[metrics_start + 1..end].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(metric) = line.strip_prefix("        - ") {
            let metric = metric
                .strip_suffix(':')
                .filter(|metric| !metric.is_empty() && !metric.contains(char::is_whitespace))
                .ok_or_else(|| {
                    format!(
                        "query {name} has malformed metric entry at line {}",
                        metrics_start + 2 + offset
                    )
                })?;
            if !metrics.iter().all(|existing| existing != metric) {
                return Err(format!(
                    "query {name} declares metric {metric} more than once"
                ));
            }
            metrics.push(metric.to_string());
        } else if line.starts_with("            ") && !metrics.is_empty() {
            continue;
        } else {
            return Err(format!(
                "query {name} has malformed metrics indentation at line {}",
                metrics_start + 2 + offset
            ));
        }
    }
    if metrics.is_empty() {
        return Err(format!("query {name} declares no metric columns"));
    }

    Ok(MonitoringQuery {
        name: name.to_string(),
        sql: sql.join("\n"),
        metrics,
    })
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

#[tokio::test]
async fn postgres_migration_runner_succeeds_with_a_single_connection_pool() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (single connection migration pool)");
        return;
    };
    let schema = unique_postgres_schema_name("ledger_pool_one");
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect postgres admin pool");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated postgres schema");
    let db = open_postgres_database_in_schema(&database_url, &schema, "ledger-pool-one", 1).await;

    assert!(!MigrationRunner::single()
        .run(&db)
        .await
        .expect("run migrations with one pooled connection")
        .is_empty());

    drop(db);
    drop_postgres_schema(&admin, &schema).await;
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
fn migration_catalog_obeys_namespace_boundary() {
    assert_eq!(WADDLE_NAMESPACE_START, 1000);

    for migration in global::all() {
        assert!(
            migration.version < WADDLE_NAMESPACE_START,
            "global migration v{} must stay below the waddle namespace boundary",
            migration.version
        );
        assert_eq!(
            MigrationNamespace::of(migration.version),
            MigrationNamespace::Global
        );
    }

    for migration in waddle::all() {
        assert!(
            migration.version >= WADDLE_NAMESPACE_START,
            "waddle migration v{} must stay inside the waddle namespace",
            migration.version
        );
        assert_eq!(
            MigrationNamespace::of(migration.version),
            MigrationNamespace::Waddle
        );
    }
}

#[test]
fn sentinel_tables_match_first_migration_sql() {
    for (namespace, catalog) in [
        (MigrationNamespace::Global, global::all()),
        (MigrationNamespace::Waddle, waddle::all()),
    ] {
        let sentinel = MigrationRunner::sentinel_table(namespace);
        let first = catalog
            .into_iter()
            .min_by_key(|migration| migration.version)
            .expect("namespace catalog is non-empty");
        for sql in [first.sql_sqlite, first.sql_postgres] {
            assert!(
                sql.contains(&format!("CREATE TABLE {sentinel} (")),
                "{namespace} sentinel table {sentinel} must be created by that \
                 namespace's first migration (v{}); the SchemaWithoutLedger \
                 guard depends on it",
                first.version
            );
        }
    }
}

#[test]
fn db_public_exports_expose_migration_foundation() {
    assert_eq!(MigrationNamespace::of(-5), MigrationNamespace::Global);
    assert_eq!(MigrationNamespace::of(1001), MigrationNamespace::Waddle);

    let checksum = migration_checksum(
        &Migration {
            version: 1234,
            description: "test export".to_string(),
            sql_sqlite: "SELECT 1;",
            sql_postgres: "SELECT 2;",
        },
        DatabaseDriver::Sqlite,
    );
    assert_eq!(checksum.len(), 64);

    let database_error: DatabaseError = MigrationLedgerError::ChecksumMismatch {
        version: 1001,
        expected: "expected".to_string(),
        found: "found".to_string(),
    }
    .into();
    assert_eq!(
        database_error.to_string(),
        "migration startup is refusing to continue rather than repairing the ledger: migration v1001 checksum expected expected, found found"
    );
}

#[test]
fn postgres_channel_pin_permission_migration_is_hot_patch_safe() {
    assert!(
        waddle::V1002_ADD_CHANNEL_PIN_PERMISSION_POSTGRES.contains("ADD COLUMN IF NOT EXISTS"),
        "Postgres v1002 must tolerate prod databases where pin_permission was hot-patched before the migration was recorded"
    );
}

#[test]
fn postgres_upload_and_attachment_sizes_are_bigint() {
    assert!(
        global::V0001_AUTH_BROKER_SCHEMA_POSTGRES.contains("size_bytes BIGINT NOT NULL"),
        "fresh Postgres upload_slots.size_bytes must be BIGINT"
    );
    assert!(
        global::V0006_UPLOAD_SIZES_BIGINT_POSTGRES.contains("ALTER COLUMN size_bytes TYPE BIGINT"),
        "existing Postgres upload_slots.size_bytes must be widened online"
    );
    assert!(
        waddle::V0001_SCHEMA_POSTGRES.contains("size_bytes BIGINT NOT NULL"),
        "fresh Postgres attachments.size_bytes must be BIGINT"
    );
    assert!(
        waddle::V1003_ATTACHMENT_SIZES_BIGINT_POSTGRES
            .contains("ALTER COLUMN size_bytes TYPE BIGINT"),
        "existing Postgres attachments.size_bytes must be widened online"
    );
}

#[test]
fn postgres_link_preview_refs_current_index_is_partial() {
    assert!(
        global::V0007_LINK_PREVIEW_MEDIA_REFS_POSTGRES.contains("WHERE state = 'current'"),
        "Postgres v0007 current-ref index must stay partial so only live preview refs are indexed"
    );
}

#[tokio::test]
async fn postgres_v0006_widens_existing_upload_slot_size_bytes() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed migration regression for upload_slots.size_bytes BIGINT)"
        );
        return;
    };

    let schema = unique_postgres_schema_name("upload_size");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Postgres), ())
        .await
        .expect("create migration table");
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 6),
        DatabaseDriver::Postgres,
    )
    .await;
    conn.execute(
        r#"
        CREATE TABLE upload_slots (
            id TEXT PRIMARY KEY,
            requester_jid TEXT NOT NULL,
            filename TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            content_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            storage_key TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
            expires_at TEXT NOT NULL,
            uploaded_at TEXT
        )
        "#,
        (),
    )
    .await
    .expect("create legacy upload_slots");
    create_legacy_session_schema(&conn).await;
    conn.execute(
        "INSERT INTO upload_slots \
         (id, requester_jid, filename, size_bytes, content_type, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "slot-legacy",
            "alice@example.com",
            "legacy.bin",
            i64::from(i32::MAX),
            "application/octet-stream",
            "2026-05-13T00:00:00Z"
        ],
    )
    .await
    .expect("seed legacy upload slot");
    drop(conn);

    let applied = MigrationRunner::global()
        .run(&db)
        .await
        .expect("run global migration");
    assert_eq!(
        applied,
        vec![6, 7, 8, 9, 10, 11, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );
    assert_postgres_column_type(&db, "upload_slots", "size_bytes", "bigint").await;

    let oversized_int4 = i64::from(i32::MAX) + 1;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(
        "INSERT INTO upload_slots \
         (id, requester_jid, filename, size_bytes, content_type, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "slot-bigint",
            "alice@example.com",
            "bigint.bin",
            oversized_int4,
            "application/octet-stream",
            "2026-05-13T00:00:00Z"
        ],
    )
    .await
    .expect("insert upload slot above int4 range");
    let stored = query_i64(
        &db,
        "SELECT size_bytes FROM upload_slots WHERE id = ?",
        "slot-bigint",
    )
    .await;
    assert_eq!(stored, oversized_int4);
    drop(conn);

    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn sqlite_v0007_tracks_link_preview_media_refs() {
    let db = Database::in_memory("test-global-v0007-link-preview-refs")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 7),
        DatabaseDriver::Sqlite,
    )
    .await;
    conn.execute(
        r#"
        CREATE TABLE upload_slots (
            id TEXT PRIMARY KEY,
            requester_jid TEXT NOT NULL,
            filename TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            content_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            storage_key TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL,
            uploaded_at TEXT
        )
        "#,
        (),
    )
    .await
    .unwrap();
    create_legacy_session_schema(&conn).await;
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    drop(conn);

    let applied = MigrationRunner::global().run(&db).await.unwrap();
    assert_eq!(
        applied,
        vec![7, 8, 9, 10, 11, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );

    let conn = db.guard().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'link_preview_media_refs'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let table_count: i64 = row.get(0).unwrap();
    assert_eq!(table_count, 1);

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN ('idx_link_preview_media_refs_current', 'idx_link_preview_media_refs_message')",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let index_count: i64 = row.get(0).unwrap();
    assert_eq!(index_count, 2);

    conn.execute(
        "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "slot-1",
            "alice@example.com",
            "link-preview-test.png",
            12_i64,
            "image/png",
            "2030-01-01T00:00:00Z"
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
        crate::db_params![
            "slot-1",
            "alice@example.com",
            "msg-1",
            "archive-1",
            "current"
        ],
    )
    .await
    .unwrap();
    let invalid_state = conn
        .execute(
            "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                "slot-1",
                "alice@example.com",
                "msg-2",
                "archive-2",
                "expired"
            ],
        )
        .await;
    assert!(invalid_state.is_err());

    conn.execute(
        "DELETE FROM upload_slots WHERE id = ?",
        crate::db_params!["slot-1"],
    )
    .await
    .unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM link_preview_media_refs", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let ref_count: i64 = row.get(0).unwrap();
    assert_eq!(ref_count, 0);
}

#[tokio::test]
async fn sqlite_v0008_repairs_marked_but_missing_global_tables() {
    let db = Database::in_memory("test-global-v0008-repair-drift")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 8),
        DatabaseDriver::Sqlite,
    )
    .await;
    seed_applied_migrations(&conn, waddle::all(), DatabaseDriver::Sqlite).await;
    conn.execute(
        r#"
        CREATE TABLE upload_slots (
            id TEXT PRIMARY KEY,
            requester_jid TEXT NOT NULL,
            filename TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            content_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            storage_key TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL,
            uploaded_at TEXT
        )
        "#,
        (),
    )
    .await
    .unwrap();
    create_legacy_session_schema(&conn).await;
    drop(conn);

    let applied = MigrationRunner::global().run(&db).await.unwrap();
    assert_eq!(applied, vec![8, 9, 10, 11]);

    let conn = db.guard().await.unwrap();
    for table in ["provider_webhook_deliveries", "link_preview_media_refs"] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                crate::db_params![table],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let table_count: i64 = row.get(0).unwrap();
        assert_eq!(table_count, 1, "{table} should be repaired");
    }

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'index' \
               AND name IN ('idx_provider_webhook_deliveries_status', \
                            'idx_link_preview_media_refs_current', \
                            'idx_link_preview_media_refs_message')",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let index_count: i64 = row.get(0).unwrap();
    assert_eq!(index_count, 3);

    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    assert_provider_delivery_conflict_target(&conn).await;
    assert_link_preview_constraints_and_cascade(&conn).await;
}

#[tokio::test]
async fn sqlite_v0010_drops_retired_isr_token_store() {
    let db = Database::in_memory("test-global-v0010-drop-isr-token-store")
        .await
        .unwrap();
    let conn = db.guard().await.unwrap();
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .unwrap();
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 10),
        DatabaseDriver::Sqlite,
    )
    .await;
    seed_applied_migrations(&conn, waddle::all(), DatabaseDriver::Sqlite).await;
    for statement in [
        "CREATE TABLE clustering_isr_tokens (sm_id TEXT PRIMARY KEY)",
        "CREATE INDEX clustering_isr_tokens_created_at_sm_id ON clustering_isr_tokens (sm_id)",
        "CREATE TABLE clustering_isr_revocation_fences (sm_id TEXT PRIMARY KEY)",
        "CREATE INDEX clustering_isr_revocation_fences_created_at_identity ON clustering_isr_revocation_fences (sm_id)",
        "CREATE TABLE clustering_isr_sweep_state (singleton INTEGER PRIMARY KEY)",
    ] {
        conn.execute(statement, ()).await.unwrap();
    }
    create_legacy_session_schema(&conn).await;
    drop(conn);

    let applied = MigrationRunner::global().run(&db).await.unwrap();
    assert_eq!(applied, vec![10, 11]);

    let conn = db.guard().await.unwrap();
    for table in [
        "clustering_isr_tokens",
        "clustering_isr_revocation_fences",
        "clustering_isr_sweep_state",
    ] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                crate::db_params![table],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let count: i64 = row.get(0).unwrap();
        assert_eq!(count, 0, "V0010 must drop {table}");
    }
}

#[tokio::test]
async fn sqlite_v0011_adds_auth_context_reference_to_existing_sessions() {
    let db = Database::in_memory("test-global-v0011-auth-context")
        .await
        .expect("in-memory database");
    let conn = db.guard().await.expect("database guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Sqlite), ())
        .await
        .expect("create migration table");
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 11),
        DatabaseDriver::Sqlite,
    )
    .await;
    seed_applied_migrations(&conn, waddle::all(), DatabaseDriver::Sqlite).await;
    create_legacy_session_schema(&conn).await;
    conn.execute(
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        crate::db_params!["alice@example.com", "alice", "alice", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
    )
    .await
    .expect("seed user");
    conn.execute(
        "INSERT INTO sessions (id, user_jid, token_hash, created_at, last_used_at) VALUES (?, ?, ?, ?, ?)",
        crate::db_params!["legacy-session", "alice@example.com", "token", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
    )
    .await
    .expect("seed legacy session");
    drop(conn);

    assert_eq!(MigrationRunner::global().run(&db).await.unwrap(), vec![11]);

    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT auth_context_id, auth_context_version, principal_auth_epoch FROM sessions WHERE id = ?",
            crate::db_params!["legacy-session"],
        )
        .await
        .expect("read migrated legacy session");
    let row = rows.next().await.expect("read row").expect("legacy row");
    let auth_context_id: Option<String> = row.get(0).expect("decode nullable context");
    let auth_context_version: i64 = row.get(1).expect("decode context version");
    let principal_auth_epoch: i64 = row.get(2).expect("decode auth epoch");
    assert_eq!(auth_context_id, None);
    assert_eq!(auth_context_version, 1);
    assert_eq!(principal_auth_epoch, 1);
    drop(rows);

    conn.execute(
        "UPDATE sessions SET auth_context_id = ? WHERE id = ?",
        crate::db_params!["e54271ba-2fe1-4632-84b0-b8895cb2f5dd", "legacy-session"],
    )
    .await
    .expect("assign durable context");
    let duplicate = conn
        .execute(
            "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?)",
            crate::db_params!["duplicate-context", "alice@example.com", "token-2", "e54271ba-2fe1-4632-84b0-b8895cb2f5dd", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
        )
        .await;
    assert!(
        duplicate.is_err(),
        "auth context must be unique when present"
    );
}

#[tokio::test]
async fn postgres_v0011_adds_auth_context_reference_to_existing_sessions() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed migration regression for auth-context reference)"
        );
        return;
    };

    let schema = unique_postgres_schema_name("auth_context");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Postgres), ())
        .await
        .expect("create migration table");
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 11),
        DatabaseDriver::Postgres,
    )
    .await;
    seed_applied_migrations(&conn, waddle::all(), DatabaseDriver::Postgres).await;
    create_legacy_session_schema(&conn).await;
    conn.execute(
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        crate::db_params!["alice@example.com", "alice", "alice", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
    )
    .await
    .expect("seed user");
    conn.execute(
        "INSERT INTO sessions (id, user_jid, token_hash, created_at, last_used_at) VALUES (?, ?, ?, ?, ?)",
        crate::db_params!["legacy-session", "alice@example.com", "token", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"],
    )
    .await
    .expect("seed legacy session");
    drop(conn);

    assert_eq!(MigrationRunner::global().run(&db).await.unwrap(), vec![11]);
    // TEXT (not UUID) on Postgres: matches the sm_sessions principal columns
    // and keeps principal resolution index-served with one shared SQL string
    // (no CAST on the indexed column — Codex PR review on #1666).
    assert_postgres_column_type(&db, "sessions", "auth_context_id", "text").await;
    assert_postgres_column_type(&db, "sessions", "auth_context_version", "bigint").await;
    assert_postgres_column_type(&db, "sessions", "principal_auth_epoch", "bigint").await;

    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(
            "SELECT auth_context_id::TEXT, auth_context_version, principal_auth_epoch FROM sessions WHERE id = ?",
            crate::db_params!["legacy-session"],
        )
        .await
        .expect("read migrated legacy session");
    let row = rows.next().await.expect("read row").expect("legacy row");
    let auth_context_id: Option<String> = row.get(0).expect("decode nullable context");
    let auth_context_version: i64 = row.get(1).expect("decode context version");
    let principal_auth_epoch: i64 = row.get(2).expect("decode auth epoch");
    assert_eq!(auth_context_id, None);
    assert_eq!(auth_context_version, 1);
    assert_eq!(principal_auth_epoch, 1);
    drop(rows);
    drop(conn);

    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_v0007_tracks_link_preview_media_refs() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed migration regression for link_preview_media_refs)"
        );
        return;
    };

    let schema = unique_postgres_schema_name("link_preview_refs");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Postgres), ())
        .await
        .expect("create migration table");
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 7),
        DatabaseDriver::Postgres,
    )
    .await;
    conn.execute(
        r#"
        CREATE TABLE upload_slots (
            id TEXT PRIMARY KEY,
            requester_jid TEXT NOT NULL,
            filename TEXT NOT NULL,
            size_bytes BIGINT NOT NULL,
            content_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            storage_key TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
            expires_at TEXT NOT NULL,
            uploaded_at TEXT
        )
        "#,
        (),
    )
    .await
    .expect("create upload_slots");
    create_legacy_session_schema(&conn).await;
    drop(conn);

    let applied = MigrationRunner::global()
        .run(&db)
        .await
        .expect("run global migration");
    assert_eq!(
        applied,
        vec![7, 8, 9, 10, 11, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );

    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = current_schema() \
               AND table_name = 'link_preview_media_refs'",
            (),
        )
        .await
        .expect("query link_preview_media_refs table");
    let row = rows
        .next()
        .await
        .expect("read table row")
        .expect("table row");
    let table_count: i64 = row.get(0).expect("decode table count");
    assert_eq!(table_count, 1);

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM pg_indexes \
             WHERE schemaname = current_schema() \
               AND indexname IN ('idx_link_preview_media_refs_current', 'idx_link_preview_media_refs_message')",
            (),
        )
        .await
        .expect("query link preview indexes");
    let row = rows
        .next()
        .await
        .expect("read index row")
        .expect("index row");
    let index_count: i64 = row.get(0).expect("decode index count");
    assert_eq!(index_count, 2);

    conn.execute(
        "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "slot-1",
            "alice@example.com",
            "link-preview-test.png",
            12_i64,
            "image/png",
            "2030-01-01T00:00:00Z"
        ],
    )
    .await
    .expect("seed upload slot");
    conn.execute(
        "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
        crate::db_params![
            "slot-1",
            "alice@example.com",
            "msg-1",
            "archive-1",
            "current"
        ],
    )
    .await
    .expect("insert valid preview ref");
    let invalid_state = conn
        .execute(
            "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                "slot-1",
                "alice@example.com",
                "msg-2",
                "archive-2",
                "expired"
            ],
        )
        .await;
    assert!(invalid_state.is_err());

    conn.execute(
        "DELETE FROM upload_slots WHERE id = ?",
        crate::db_params!["slot-1"],
    )
    .await
    .expect("delete upload slot");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM link_preview_media_refs", ())
        .await
        .expect("query refs after cascade");
    let row = rows.next().await.expect("read refs row").expect("refs row");
    let ref_count: i64 = row.get(0).expect("decode ref count");
    assert_eq!(ref_count, 0);
    drop(conn);

    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_v0008_repairs_marked_but_missing_global_tables() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed migration regression for repairing drifted global tables)"
        );
        return;
    };

    let schema = unique_postgres_schema_name("repair_global_drift");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Postgres), ())
        .await
        .expect("create migration table");
    seed_applied_migrations(
        &conn,
        global::all().into_iter().filter(|m| m.version < 8),
        DatabaseDriver::Postgres,
    )
    .await;
    seed_applied_migrations(&conn, waddle::all(), DatabaseDriver::Postgres).await;
    conn.execute(
        r#"
        CREATE TABLE upload_slots (
            id TEXT PRIMARY KEY,
            requester_jid TEXT NOT NULL,
            filename TEXT NOT NULL,
            size_bytes BIGINT NOT NULL,
            content_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            storage_key TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
            expires_at TEXT NOT NULL,
            uploaded_at TEXT
        )
        "#,
        (),
    )
    .await
    .expect("create upload_slots");
    create_legacy_session_schema(&conn).await;
    drop(conn);

    let applied = MigrationRunner::global()
        .run(&db)
        .await
        .expect("run global migration");
    assert_eq!(applied, vec![8, 9, 10, 11]);

    let conn = db.guard().await.expect("postgres guard");
    for table in ["provider_webhook_deliveries", "link_preview_media_refs"] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM information_schema.tables \
                 WHERE table_schema = current_schema() \
                   AND table_name = ?",
                crate::db_params![table],
            )
            .await
            .expect("query repaired table");
        let row = rows
            .next()
            .await
            .expect("read table row")
            .expect("table row");
        let table_count: i64 = row.get(0).expect("decode table count");
        assert_eq!(table_count, 1, "{table} should be repaired");
    }

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM pg_indexes \
             WHERE schemaname = current_schema() \
               AND indexname IN ('idx_provider_webhook_deliveries_status', \
                                'idx_link_preview_media_refs_current', \
                                'idx_link_preview_media_refs_message')",
            (),
        )
        .await
        .expect("query repaired indexes");
    let row = rows
        .next()
        .await
        .expect("read index row")
        .expect("index row");
    let index_count: i64 = row.get(0).expect("decode index count");
    assert_eq!(index_count, 3);

    assert_provider_delivery_conflict_target(&conn).await;
    assert_link_preview_constraints_and_cascade(&conn).await;
    drop(conn);

    drop_postgres_schema(&admin, &schema).await;
}

#[tokio::test]
async fn postgres_v1003_widens_existing_attachment_size_bytes() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!(
            "skipping: WADDLE_TEST_POSTGRES_URL not set \
             (postgres-backed migration regression for attachments.size_bytes BIGINT)"
        );
        return;
    };

    let schema = unique_postgres_schema_name("attachment_size");
    let (db, admin) = open_isolated_postgres_database(&database_url, &schema).await;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(sql::migrations_table_sql(DatabaseDriver::Postgres), ())
        .await
        .expect("create migration table");
    seed_applied_migrations(
        &conn,
        waddle::all().into_iter().filter(|m| m.version < 1003),
        DatabaseDriver::Postgres,
    )
    .await;
    conn.execute(
        r#"
        CREATE TABLE channels (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            channel_type TEXT NOT NULL DEFAULT 'text',
            position INTEGER NOT NULL DEFAULT 0,
            is_default INTEGER NOT NULL DEFAULT 0,
            pin_permission TEXT NOT NULL DEFAULT 'admins-only',
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT
        )
        "#,
        (),
    )
    .await
    .expect("create legacy channels");
    conn.execute(
        r#"
        CREATE TABLE attachments (
            id TEXT PRIMARY KEY,
            message_id TEXT NOT NULL,
            filename TEXT NOT NULL,
            content_type TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            storage_key TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT
        )
        "#,
        (),
    )
    .await
    .expect("create legacy attachments");
    conn.execute(
        "INSERT INTO attachments \
         (id, message_id, filename, content_type, size_bytes, storage_key) \
         VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "attachment-legacy",
            "message-1",
            "legacy.bin",
            "application/octet-stream",
            i64::from(i32::MAX),
            "legacy-key"
        ],
    )
    .await
    .expect("seed legacy attachment");
    drop(conn);

    let applied = MigrationRunner::waddle()
        .run(&db)
        .await
        .expect("run waddle migration");
    assert_eq!(
        applied,
        vec![1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011]
    );
    assert_postgres_column_type(&db, "attachments", "size_bytes", "bigint").await;

    let oversized_int4 = i64::from(i32::MAX) + 1;
    let conn = db.guard().await.expect("postgres guard");
    conn.execute(
        "INSERT INTO attachments \
         (id, message_id, filename, content_type, size_bytes, storage_key) \
         VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "attachment-bigint",
            "message-2",
            "bigint.bin",
            "application/octet-stream",
            oversized_int4,
            "bigint-key"
        ],
    )
    .await
    .expect("insert attachment above int4 range");
    let stored = query_i64(
        &db,
        "SELECT size_bytes FROM attachments WHERE id = ?",
        "attachment-bigint",
    )
    .await;
    assert_eq!(stored, oversized_int4);
    drop(conn);

    drop_postgres_schema(&admin, &schema).await;
}

async fn seed_applied_migrations(
    conn: &crate::db::ConnectionGuard,
    migrations: impl IntoIterator<Item = Migration>,
    driver: DatabaseDriver,
) {
    for migration in migrations {
        let checksum = migration_checksum(&migration, driver);
        conn.execute(
            "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
            crate::db_params![migration.version, migration.description, checksum],
        )
        .await
        .expect("seed applied migration row");
    }
}

fn migration_by_version(version: i64) -> Migration {
    MigrationRunner::single()
        .migrations
        .into_iter()
        .find(|migration| migration.version == version)
        .expect("migration exists in the catalog")
}

async fn migration_ledger_row_count(db: &Database) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM _migrations", ())
        .await
        .expect("query migration ledger count");
    let row = rows
        .next()
        .await
        .expect("read migration ledger count")
        .expect("migration ledger count row");
    row.get(0).expect("decode migration ledger count")
}

async fn migration_ledger_checksum(db: &Database, version: i64) -> Option<String> {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT checksum FROM _migrations WHERE version = ?",
            crate::db_params![version],
        )
        .await
        .expect("query migration ledger checksum");
    let row = rows
        .next()
        .await
        .expect("read migration ledger checksum")
        .expect("migration ledger checksum row");
    row.get(0).expect("decode migration ledger checksum")
}

async fn assert_all_migration_checksums(db: &Database, driver: DatabaseDriver) {
    for migration in MigrationRunner::single().migrations {
        let expected_checksum = migration_checksum(&migration, driver);
        assert_eq!(
            migration_ledger_checksum(db, migration.version)
                .await
                .as_deref(),
            Some(expected_checksum.as_str()),
            "migration v{} must be recorded with its active-dialect checksum",
            migration.version
        );
    }
}

async fn migration_ledger_namespace_row_count(db: &Database, namespace: MigrationNamespace) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let predicate = match namespace {
        MigrationNamespace::Global => "version < 1000",
        MigrationNamespace::Waddle => "version >= 1000",
    };
    let mut rows = conn
        .query(
            &format!("SELECT COUNT(*) FROM _migrations WHERE {predicate}"),
            (),
        )
        .await
        .expect("query namespace migration ledger count");
    let row = rows
        .next()
        .await
        .expect("read namespace migration ledger count")
        .expect("namespace migration ledger count row");
    row.get(0).expect("decode namespace migration ledger count")
}

async fn sqlite_schema_object_count(db: &Database) -> i64 {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table', 'index')",
            (),
        )
        .await
        .expect("query SQLite schema object count");
    let row = rows
        .next()
        .await
        .expect("read SQLite schema object count")
        .expect("SQLite schema object count row");
    row.get(0).expect("decode SQLite schema object count")
}

async fn sqlite_table_exists(db: &Database, table: &str) -> bool {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            crate::db_params![table],
        )
        .await
        .expect("query sqlite table existence");
    let row = rows
        .next()
        .await
        .expect("read sqlite table existence")
        .expect("sqlite table existence row");
    let count: i64 = row.get(0).expect("decode sqlite table existence");
    count == 1
}

async fn postgres_schema_object_count(db: &Database) -> i64 {
    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) \
             FROM information_schema.tables \
             WHERE table_schema = current_schema()",
            (),
        )
        .await
        .expect("query postgres schema object count");
    let row = rows
        .next()
        .await
        .expect("read postgres schema object count")
        .expect("postgres schema object count row");
    row.get(0).expect("decode postgres schema object count")
}

async fn postgres_table_exists(db: &Database, table: &str) -> bool {
    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) \
             FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_name = ?",
            crate::db_params![table],
        )
        .await
        .expect("query postgres table existence");
    let row = rows
        .next()
        .await
        .expect("read postgres table existence")
        .expect("postgres table existence row");
    let count: i64 = row.get(0).expect("decode postgres table existence");
    count == 1
}

/// The historical-upgrade fixtures record V0001 as applied. Their focused
/// schemas must therefore include V0001's session dependencies once a later
/// migration alters `sessions`.
async fn create_legacy_session_schema(conn: &crate::db::ConnectionGuard) {
    conn.execute(
        r#"
        CREATE TABLE users (
            jid TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            xmpp_localpart TEXT NOT NULL UNIQUE,
            display_name TEXT,
            avatar_url TEXT,
            primary_email TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
        "#,
        (),
    )
    .await
    .expect("create legacy users schema");
    conn.execute(
        r#"
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            user_jid TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE,
            expires_at TEXT,
            created_at TEXT NOT NULL,
            last_used_at TEXT NOT NULL,
            FOREIGN KEY (user_jid) REFERENCES users(jid) ON DELETE CASCADE
        )
        "#,
        (),
    )
    .await
    .expect("create legacy sessions schema");
    conn.execute(
        "CREATE INDEX idx_sessions_user_jid ON sessions(user_jid)",
        (),
    )
    .await
    .expect("create legacy sessions user index");
    conn.execute(
        "CREATE INDEX idx_sessions_expires_at ON sessions(expires_at)",
        (),
    )
    .await
    .expect("create legacy sessions expiry index");
}

async fn assert_provider_delivery_conflict_target(conn: &crate::db::ConnectionGuard) {
    let insert = "INSERT INTO provider_webhook_deliveries \
         (provider_id, delivery_id, plugin_id, event_type, payload_sha256, status) \
         VALUES (?, ?, ?, ?, ?, 'queued') \
         ON CONFLICT(provider_id, delivery_id) DO NOTHING";
    let first = conn
        .execute(
            insert,
            crate::db_params![
                "github",
                "repair-delivery-1",
                "github",
                "ping",
                "0123456789abcdef",
            ],
        )
        .await
        .expect("insert provider delivery");
    assert_eq!(first, 1);

    let duplicate = conn
        .execute(
            insert,
            crate::db_params![
                "github",
                "repair-delivery-1",
                "github",
                "ping",
                "0123456789abcdef",
            ],
        )
        .await
        .expect("dedupe provider delivery");
    assert_eq!(duplicate, 0);

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM provider_webhook_deliveries \
             WHERE provider_id = ? AND delivery_id = ?",
            crate::db_params!["github", "repair-delivery-1"],
        )
        .await
        .expect("query provider delivery");
    let row = rows.next().await.expect("read delivery row").expect("row");
    let count: i64 = row.get(0).expect("decode delivery count");
    assert_eq!(count, 1);

    let updated = conn
        .execute(
            "UPDATE provider_webhook_deliveries \
             SET status = ?, \
                 attempts = attempts + 1, \
                 last_error = ?, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE provider_id = ? AND delivery_id = ?",
            crate::db_params![
                "processed",
                Option::<String>::None,
                "github",
                "repair-delivery-1",
            ],
        )
        .await
        .expect("mark provider delivery processed");
    assert_eq!(updated, 1);

    let mut rows = conn
        .query(
            "SELECT status, attempts, last_error FROM provider_webhook_deliveries \
             WHERE provider_id = ? AND delivery_id = ?",
            crate::db_params!["github", "repair-delivery-1"],
        )
        .await
        .expect("query processed provider delivery");
    let row = rows.next().await.expect("read processed row").expect("row");
    let status: String = row.get(0).expect("decode status");
    let attempts: i64 = row.get(1).expect("decode attempts");
    let last_error: Option<String> = row.get(2).expect("decode last_error");
    assert_eq!(status, "processed");
    assert_eq!(attempts, 1);
    assert_eq!(last_error, None);
}

async fn assert_link_preview_constraints_and_cascade(conn: &crate::db::ConnectionGuard) {
    conn.execute(
        "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        crate::db_params![
            "repair-slot-1",
            "alice@example.com",
            "link-preview-test.png",
            12_i64,
            "image/png",
            "2030-01-01T00:00:00Z"
        ],
    )
    .await
    .expect("seed upload slot");
    conn.execute(
        "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
        crate::db_params![
            "repair-slot-1",
            "alice@example.com",
            "msg-1",
            "archive-1",
            "current"
        ],
    )
    .await
    .expect("insert valid preview ref");
    let invalid_state = conn
        .execute(
            "INSERT INTO link_preview_media_refs (upload_slot_id, archive_jid, message_id, current_archive_id, state) VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                "repair-slot-1",
                "alice@example.com",
                "msg-2",
                "archive-2",
                "expired"
            ],
        )
        .await;
    assert!(invalid_state.is_err());

    conn.execute(
        "DELETE FROM upload_slots WHERE id = ?",
        crate::db_params!["repair-slot-1"],
    )
    .await
    .expect("delete upload slot");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM link_preview_media_refs WHERE upload_slot_id = ?",
            crate::db_params!["repair-slot-1"],
        )
        .await
        .expect("query refs after cascade");
    let row = rows.next().await.expect("read refs row").expect("refs row");
    let ref_count: i64 = row.get(0).expect("decode ref count");
    assert_eq!(ref_count, 0);
}

async fn assert_postgres_column_type(
    db: &Database,
    table: &str,
    column: &str,
    expected_type: &str,
) {
    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(
            "SELECT data_type \
             FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = ? \
               AND column_name = ?",
            crate::db_params![table, column],
        )
        .await
        .expect("query information_schema column type");
    let row = rows
        .next()
        .await
        .expect("read column type")
        .expect("column row");
    let data_type: String = row.get(0).expect("decode column type");
    assert_eq!(data_type, expected_type);
}

async fn query_i64(db: &Database, sql: &str, id: &str) -> i64 {
    let conn = db.guard().await.expect("postgres guard");
    let mut rows = conn
        .query(sql, crate::db_params![id])
        .await
        .expect("query i64 value");
    let row = rows.next().await.expect("read i64 row").expect("i64 row");
    row.get(0).expect("decode i64 value")
}

async fn open_isolated_postgres_database(
    database_url: &str,
    schema: &str,
) -> (Database, sqlx::PgPool) {
    let admin = sqlx::PgPool::connect(database_url)
        .await
        .expect("connect postgres admin pool");
    let create_schema = format!("CREATE SCHEMA {schema}");
    sqlx::query(&create_schema)
        .execute(&admin)
        .await
        .expect("create isolated postgres schema");

    let db = open_postgres_database_in_schema(
        database_url,
        schema,
        "isolated-postgres-migration-test",
        10,
    )
    .await;
    (db, admin)
}

async fn open_postgres_database_in_schema(
    database_url: &str,
    schema: &str,
    name: &str,
    pool_size: u32,
) -> Database {
    let scoped_url = postgres_url_with_search_path(database_url, schema);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url);
    config.pool_size = pool_size;
    Database::from_config(name, &config)
        .await
        .expect("open isolated postgres database")
}

async fn drop_postgres_schema(admin: &sqlx::PgPool, schema: &str) {
    let drop_schema = format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    sqlx::query(&drop_schema)
        .execute(admin)
        .await
        .expect("drop isolated postgres schema");
}

async fn wait_for_postgres_advisory_waiter(admin: &sqlx::PgPool, key: i64) {
    let waiter = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            // Postgres exposes a single-bigint advisory key with its high
            // 32 bits in `classid`, low 32 bits in `objid`, and `objsubid = 1`.
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) \
                 FROM pg_locks \
                 WHERE locktype = 'advisory' \
                   AND granted = false \
                   AND objsubid = 1 \
                   AND ((classid::bigint << 32) | objid::bigint) = $1",
            )
            .bind(key)
            .fetch_one(admin)
            .await
            .expect("query postgres advisory lock waiters");
            if count > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        waiter.is_ok(),
        "migration runner must show an ungranted advisory waiter in pg_locks"
    );
}

fn unique_postgres_schema_name(prefix: &str) -> String {
    format!("waddle_test_{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres url");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}
