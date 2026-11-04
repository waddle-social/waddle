//! Read-only pending-delivery attachment against owner-prepared SQLite databases.

use std::time::Duration;

use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::pending_delivery::{
    bootstrap_pending_delivery_owner_schema, DatabasePendingDeliveryStorage,
    PendingRuntimeAttachError,
};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::QuotaPolicy;

async fn memory() -> Database {
    let config = DatabaseConfig {
        pool_size: 1,
        ..DatabaseConfig::default()
    };
    Database::from_config("pending attachment test", &config)
        .await
        .expect("one-slot SQLite")
}

async fn execute(db: &Database, sql: &str) {
    db.guard()
        .await
        .expect("guard")
        .execute(sql, ())
        .await
        .expect(sql);
}

async fn attach(
    db: &Database,
) -> Result<DatabasePendingDeliveryStorage, PendingRuntimeAttachError> {
    DatabasePendingDeliveryStorage::attach_existing(
        db.clone(),
        QuotaPolicy::CountCap { max_rows: 42 },
    )
    .await
}

async fn seed(db: &Database) {
    execute(db, "INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES ('row-a', 'alice@example.test', 1000, 'archived', 'alice@example.test', 'stanza-a')").await;
    execute(db, "INSERT INTO pending_delivery_sm_promotions (origin_stream_id, sequence, created_at_ms) VALUES ('stream-a', 1, 1000)").await;
}

#[tokio::test]
async fn attaches_fresh_one_slot_memory_and_keeps_existing_reads() {
    let db = memory().await;
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::Relation {
            relation: "pending_delivery",
            ..
        })
    ));
    let mut rows = db
        .guard()
        .await
        .expect("guard")
        .query(
            "SELECT name FROM sqlite_master WHERE name LIKE 'pending_%'",
            (),
        )
        .await
        .expect("schema query");
    assert!(
        rows.next().await.expect("result").is_none(),
        "refusal does not initialize the store"
    );
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner prepares fresh store");
    seed(&db).await;
    let storage = attach(&db).await.expect("attach prepared store");
    let recipient = "alice@example.test".parse().expect("recipient");
    let rows = storage.list(&recipient).await.expect("existing list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.as_str(), "row-a");
    assert_eq!(storage.count(&recipient).await.expect("existing count"), 1);
    assert!(matches!(
        storage.quota_policy(),
        QuotaPolicy::CountCap { max_rows: 42 }
    ));
}

#[tokio::test]
async fn attaches_upgraded_file_and_preserves_backfilled_rows() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("pending.db").display()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, url);
    config.pool_size = 1;
    let db = Database::from_config("legacy pending", &config)
        .await
        .expect("file SQLite");
    execute(&db, "CREATE TABLE pending_delivery (row_id TEXT PRIMARY KEY, recipient_jid TEXT NOT NULL, original_receipt_at INTEGER NOT NULL, payload_kind TEXT NOT NULL, archive_stanza_by TEXT, archive_stanza_id TEXT, transient_xml TEXT, flushed_in_session TEXT, legacy_note TEXT NOT NULL DEFAULT 'preserved')").await;
    execute(&db, "INSERT INTO pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id) VALUES ('row-a', 'alice@example.test', 1000, 'archived', 'alice@example.test', 'stanza-a')").await;
    assert!(attach(&db).await.is_err());
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner upgrades legacy store");
    let storage = attach(&db).await.expect("attach upgraded store");
    let recipient = "alice@example.test".parse().expect("recipient");
    assert_eq!(
        storage
            .list(&recipient)
            .await
            .expect("read upgraded row")
            .len(),
        1
    );
    let mut rows = db.guard().await.expect("guard").query("SELECT legacy_note, notification_recovery_ordinal FROM pending_delivery WHERE row_id = 'row-a'", ()).await.expect("read backfill");
    let row = rows.next().await.expect("result").expect("row");
    assert_eq!(row.get::<String>(0).expect("legacy note"), "preserved");
    assert!(row.get::<i64>(1).expect("backfilled ordinal") > 0);
    drop(rows);
    drop(storage);
    drop(db);
    let reopened = Database::from_config("reopened pending", &config)
        .await
        .expect("reopen upgraded file");
    let storage = attach(&reopened).await.expect("attach reopened store");
    assert_eq!(storage.count(&recipient).await.expect("reopened read"), 1);
}

#[tokio::test]
async fn refuses_missing_markers_and_recovery_without_repair() {
    for marker in [
        "ingress_v1014_pending_claim_reset_v1",
        "legacy_mam_query_frames_v1",
    ] {
        let db = memory().await;
        bootstrap_pending_delivery_owner_schema(&db)
            .await
            .expect("owner schema");
        seed(&db).await;
        execute(&db, "UPDATE pending_delivery SET flushed_in_session = 'held-session', outbound_sequence = 12, claimed_at_ms = 1000 WHERE row_id = 'row-a'").await;
        execute(
            &db,
            &format!("DELETE FROM pending_delivery_startup_migrations WHERE name = '{marker}'"),
        )
        .await;
        assert!(
            matches!(attach(&db).await, Err(PendingRuntimeAttachError::MissingMarker { marker: actual }) if actual == marker)
        );
        let mut rows = db
            .guard()
            .await
            .expect("guard")
            .query(
                &format!(
                    "SELECT 1 FROM pending_delivery_startup_migrations WHERE name = '{marker}'"
                ),
                (),
            )
            .await
            .expect("marker query");
        assert!(rows.next().await.expect("result").is_none());
        let mut rows = db.guard().await.expect("guard").query("SELECT flushed_in_session, outbound_sequence, claimed_at_ms FROM pending_delivery WHERE row_id = 'row-a'", ()).await.expect("held claim");
        let row = rows.next().await.expect("result").expect("row");
        assert_eq!(row.get::<String>(0).expect("session"), "held-session");
        assert_eq!(row.get::<i64>(1).expect("sequence"), 12);
        assert_eq!(row.get::<i64>(2).expect("claim timestamp"), 1000);
    }
    for (sql, component) in [
        (
            "DROP TABLE pending_notification_recovery_counter",
            "counter relation",
        ),
        (
            "DROP TRIGGER pending_notification_recovery_allocate",
            "pending_notification_recovery_allocate",
        ),
        (
            "DROP TRIGGER pending_notification_recovery_ordinal_immutable",
            "pending_notification_recovery_ordinal_immutable",
        ),
        (
            "DROP TRIGGER pending_notification_recovery_counter_monotone",
            "pending_notification_recovery_counter_monotone",
        ),
        (
            "DELETE FROM pending_notification_recovery_counter",
            "SQLite counter singleton",
        ),
    ] {
        let db = memory().await;
        bootstrap_pending_delivery_owner_schema(&db)
            .await
            .expect("owner schema");
        execute(&db, sql).await;
        match attach(&db).await {
            Err(PendingRuntimeAttachError::Relation {
                relation: "pending_notification_recovery_counter",
                ..
            }) if component == "counter relation" => {}
            Err(PendingRuntimeAttachError::InvalidRecovery { component: actual })
                if actual == component => {}
            Err(error) => panic!("unexpected error for {sql}: {error:?}"),
            Ok(_) => panic!("tampering accepted: {sql}"),
        }
    }
    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    execute(&db, "UPDATE pending_delivery_startup_migrations SET completed_at = 'invalid' WHERE name = 'legacy_mam_query_frames_v1'").await;
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::MissingMarker {
            marker: "legacy_mam_query_frames_v1"
        })
    ));
    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    seed(&db).await;
    execute(
        &db,
        "DROP TRIGGER pending_notification_recovery_ordinal_immutable",
    )
    .await;
    execute(
        &db,
        "UPDATE pending_delivery SET notification_recovery_ordinal = NULL",
    )
    .await;
    execute(&db, "CREATE TRIGGER pending_notification_recovery_ordinal_immutable BEFORE UPDATE OF notification_recovery_ordinal ON pending_delivery WHEN OLD.notification_recovery_ordinal IS NOT NULL AND NEW.notification_recovery_ordinal IS NOT OLD.notification_recovery_ordinal BEGIN SELECT RAISE(ABORT, 'pending recovery ordinal is immutable'); END").await;
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite ordinal backfill"
        })
    ));
    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    seed(&db).await;
    execute(
        &db,
        "DROP TRIGGER pending_notification_recovery_counter_monotone",
    )
    .await;
    execute(
        &db,
        "UPDATE pending_notification_recovery_counter SET last_ordinal = 0",
    )
    .await;
    execute(&db, "CREATE TRIGGER pending_notification_recovery_counter_monotone BEFORE UPDATE OF last_ordinal ON pending_notification_recovery_counter WHEN NEW.last_ordinal < OLD.last_ordinal BEGIN SELECT RAISE(ABORT, 'pending recovery counter cannot rewind'); END").await;
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite counter behind retained rows"
        })
    ));
}

#[tokio::test]
async fn refuses_altered_index_and_temporary_objects() {
    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    execute(&db, "CREATE TABLE sm_sessions (id INTEGER)").await;
    for (create, remove) in [
        ("CREATE TEMP TABLE Pending_Delivery (shadow TEXT)", "DROP TABLE temp.pending_delivery"),
        ("CREATE TEMP VIEW Pending_Delivery_Sm_Promotions AS SELECT 1 AS shadow", "DROP VIEW temp.pending_delivery_sm_promotions"),
        ("CREATE TEMP TABLE pending_delivery_startup_migrations (name TEXT)", "DROP TABLE temp.pending_delivery_startup_migrations"),
        ("CREATE TEMP TABLE pending_notification_recovery_counter (singleton INTEGER)", "DROP TABLE temp.pending_notification_recovery_counter"),
        ("CREATE TEMP VIEW Sm_Unacked AS SELECT 1 AS shadow", "DROP VIEW temp.sm_unacked"),
        ("CREATE TEMP TABLE mam_messages (shadow TEXT)", "DROP TABLE temp.mam_messages"),
        ("CREATE TEMP TABLE sm_ingress_appends (shadow TEXT)", "DROP TABLE temp.sm_ingress_appends"),
        ("CREATE TEMP TRIGGER malicious AFTER INSERT ON main.pending_delivery BEGIN SELECT 1; END", "DROP TRIGGER temp.malicious"),
        ("CREATE TEMP TRIGGER malicious_sm AFTER UPDATE ON main.sm_sessions BEGIN SELECT 1; END", "DROP TRIGGER temp.malicious_sm"),
    ] {
        execute(&db, create).await;
        assert!(matches!(attach(&db).await, Err(PendingRuntimeAttachError::SqliteTemporaryObject)), "{create}");
        execute(&db, remove).await;
    }
    attach(&db)
        .await
        .expect("attach after temporary objects removed");
    execute(&db, "CREATE TEMP TABLE unrelated (id INTEGER)").await;
    execute(
        &db,
        "CREATE TEMP TRIGGER unrelated_trigger AFTER INSERT ON unrelated BEGIN SELECT 1; END",
    )
    .await;
    attach(&db)
        .await
        .expect("unrelated temporary trigger is permitted");
    execute(&db, "DROP INDEX idx_pending_delivery_archived_unique").await;
    execute(&db, "CREATE INDEX idx_pending_delivery_archived_unique ON pending_delivery (recipient_jid, archive_stanza_id) WHERE payload_kind = 'archived'").await;
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::InvalidIndex {
            index: "idx_pending_delivery_archived_unique"
        })
    ));
    let mut rows = db
        .guard()
        .await
        .expect("guard")
        .query(
            "SELECT sql FROM sqlite_master WHERE name = 'idx_pending_delivery_archived_unique'",
            (),
        )
        .await
        .expect("index query");
    assert!(
        !rows
            .next()
            .await
            .expect("result")
            .expect("index")
            .get::<String>(0)
            .expect("DDL")
            .contains("UNIQUE"),
        "attachment did not repair the index"
    );
}

#[tokio::test]
async fn rejects_multi_connection_memory_database() {
    let db = Database::from_config("multi-connection memory", &DatabaseConfig::default())
        .await
        .expect("memory database");
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::SqliteMemoryPoolMustBeSingleConnection)
    ));
}

#[tokio::test]
async fn cancelled_attachment_preserves_one_slot_private_memory() {
    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    seed(&db).await;
    let tx = db.begin().await.expect("occupy only connection");
    let cloned = db.clone();
    let task = tokio::spawn(async move { attach(&cloned).await });
    tokio::task::yield_now().await;
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    drop(tx);
    for _ in 0..25 {
        let cloned = db.clone();
        let task = tokio::spawn(async move { attach(&cloned).await });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;
    }
    let storage = tokio::time::timeout(Duration::from_secs(10), attach(&db))
        .await
        .expect("verification tasks release pool")
        .expect("private database survives cancellation");
    let recipient = "alice@example.test".parse().expect("recipient");
    assert_eq!(
        storage
            .count(&recipient)
            .await
            .expect("existing row survives"),
        1
    );
}

#[tokio::test]
async fn sqlite_busy_wait_is_bounded_restored_and_in_flight_cancellation_releases_pool() {
    use sqlx::Connection;

    let directory = tempfile::tempdir().expect("fixture directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("locked.db").display()
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, &url);
    config.pool_size = 1;
    let db = Database::from_config("locked pending", &config)
        .await
        .expect("file SQLite");
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    seed(&db).await;
    execute(&db, "PRAGMA journal_mode = DELETE").await;
    execute(&db, "PRAGMA busy_timeout = 60000").await;
    let mut blocker = sqlx::SqliteConnection::connect(&url)
        .await
        .expect("independent SQLite connection");
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut blocker)
        .await
        .expect("exclusive file lock");
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(8), attach(&db))
        .await
        .expect("lock wait is bounded");
    assert!(result.is_err(), "locked file must refuse attachment");
    assert!(started.elapsed() < Duration::from_secs(8));
    let cloned = db.clone();
    let task = tokio::spawn(async move { attach(&cloned).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !task.is_finished(),
        "inspection is blocked on the external file lock"
    );
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    sqlx::query("ROLLBACK")
        .execute(&mut blocker)
        .await
        .expect("release file lock");
    let storage = tokio::time::timeout(Duration::from_secs(3), attach(&db))
        .await
        .expect("inspection releases pool")
        .expect("attach after unlock");
    let recipient = "alice@example.test".parse().expect("recipient");
    assert_eq!(storage.count(&recipient).await.expect("row survives"), 1);
    let mut rows = db
        .guard()
        .await
        .expect("guard")
        .query("PRAGMA busy_timeout", ())
        .await
        .expect("inspect restored timeout");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("value")
            .get::<i64>(0)
            .expect("timeout"),
        60000
    );
}

#[tokio::test]
async fn rejects_ordinal_check_hidden_only_in_a_schema_comment() {
    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    let guard = db.guard().await.expect("guard");
    let mut rows = guard
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pending_delivery'",
            (),
        )
        .await
        .expect("table schema");
    let ddl: String = rows
        .next()
        .await
        .expect("result")
        .expect("table")
        .get(0)
        .expect("DDL");
    let check = "CHECK (notification_recovery_ordinal IS NULL OR (typeof(notification_recovery_ordinal) = 'integer' AND notification_recovery_ordinal > 0))";
    assert!(ddl.contains(check));
    let tampered = ddl.replacen(check, &format!("/* {check} */"), 1);
    guard
        .execute("PRAGMA writable_schema = ON", ())
        .await
        .expect("allow disposable catalog tamper");
    guard
        .execute(
            "UPDATE sqlite_master SET sql = ? WHERE type = 'table' AND name = 'pending_delivery'",
            waddle_server::db_params![tampered],
        )
        .await
        .expect("tamper disposable schema");
    guard
        .execute("PRAGMA writable_schema = OFF", ())
        .await
        .expect("disable writable schema");
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite ordinal constraint"
        })
    ));
}

#[tokio::test]
async fn rejects_wrong_column_type_and_promotion_primary_key() {
    let db = memory().await;
    execute(&db, "CREATE TABLE pending_delivery (row_id INTEGER PRIMARY KEY, recipient_jid TEXT NOT NULL, original_receipt_at INTEGER NOT NULL, payload_kind TEXT NOT NULL, archive_stanza_by TEXT, archive_stanza_id TEXT, transient_xml TEXT, flushed_in_session TEXT)").await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("legacy owner initialization");
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::InvalidColumn {
            table: "pending_delivery",
            column: "row_id"
        })
    ));

    let db = memory().await;
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("owner schema");
    execute(&db, "DROP TABLE pending_delivery_sm_promotions").await;
    execute(&db, "CREATE TABLE pending_delivery_sm_promotions (origin_stream_id TEXT NOT NULL, sequence BIGINT NOT NULL, created_at_ms BIGINT NOT NULL DEFAULT 0, PRIMARY KEY (sequence, origin_stream_id))").await;
    assert!(matches!(
        attach(&db).await,
        Err(PendingRuntimeAttachError::InvalidPrimaryKey {
            table: "pending_delivery_sm_promotions"
        })
    ));
}
