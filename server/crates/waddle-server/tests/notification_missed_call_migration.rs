//! Migration 2002 preserves existing calls and deduplicates missed-call alerts.

// Use the deployment's immutable migration SQL to construct an actual 2001
// database, with the same checksum the deployment runner verifies on upgrade.
#[path = "../src/db/migrations/after_baseline.rs"]
mod after_baseline;
#[path = "restricted_runtime_support/notification_migration.rs"]
mod support;

mod migration_types {
    mod types {
        include!("../src/db/migrations/types.rs");
    }
    pub use types::Migration;
}

use migration_types::Migration;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use waddle_server::db::{
    Database, DatabaseConfig, DatabaseDriver, MigrationRunner, BASELINE_VERSION,
};

use support::{
    assert_notification_rows_preserved, assert_rows_preserved_and_insert_call,
    install_baseline_only, seed_notification_rows,
};

fn versions_from(first: i64) -> Vec<i64> {
    std::iter::once(BASELINE_VERSION)
        .chain(
            after_baseline::all()
                .into_iter()
                .map(|migration| migration.version),
        )
        .filter(|version| *version >= first)
        .collect()
}

async fn install_2001_with_calls(db: &Database, schema: &str) {
    install_baseline_only(db, schema).await;
    seed_notification_rows(db).await;
    let migration = after_baseline::all()
        .into_iter()
        .find(|migration| migration.version == 2001)
        .expect("call migration");
    let mut tx = db.begin_immediate().await.expect("2001 transaction");
    tx.execute_batch(migration.sql_for(db.driver()))
        .await
        .expect("apply migration 2001");
    tx.execute(
        "INSERT INTO _migrations (version, description, checksum) VALUES (?, ?, ?)",
        waddle_server::db_params![
            migration.version,
            migration.description.as_str(),
            hex::encode(Sha256::digest(migration.sql_for(db.driver()).as_bytes()))
        ],
    )
    .await
    .expect("stamp migration 2001");
    tx.commit().await.expect("commit 2001");
    assert_rows_preserved_and_insert_call(db).await;
    db.guard().await.expect("call seed guard").execute(
        "UPDATE notification_candidates SET thread_id = 'call-thread', policy_error_count = 2, next_attempt_at_ms = 7000, outboxed_at_ms = 8000, suppressed_reason = 'waddle_dnd', noping = 1, no_store = 1, no_permanent_store = 1, last_message_body = 'call body', reaction = 1, delivery_id = 'call-delivery', quarantined_at_ms = 9000, quarantined_suppressed_reason = 'call quarantine' WHERE class = 'call'",
        (),
    ).await.expect("seed every call column");
}

async fn assert_call_preserved(db: &Database) {
    assert_notification_rows_preserved(db).await;
    let conn = db.guard().await.expect("preservation guard");
    let mut rows = conn.query(
        "SELECT COUNT(*) FROM notification_candidates WHERE recipient_bare_jid = 'bob@example.com' AND conversation_jid = 'alice@example.com' AND sender_jid = 'alice@example.com/phone' AND thread_id = 'call-thread' AND stanza_id_by = 'bob@example.com' AND stanza_id = 'call-stanza' AND class = 'call' AND reason = 'offline_call' AND created_at_ms = 6000 AND policy_error_count = 2 AND next_attempt_at_ms = 7000 AND outboxed_at_ms = 8000 AND suppressed_reason = 'waddle_dnd' AND noping = 1 AND no_store = 1 AND no_permanent_store = 1 AND last_message_body = 'call body' AND reaction = 1 AND delivery_id = 'call-delivery' AND quarantined_at_ms = 9000 AND quarantined_suppressed_reason = 'call quarantine' AND call_sid = 'call-session' AND call_video = 1",
        (),
    ).await.expect("preserved call query");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        1,
        "every call column, including sid and video, survives the rebuild"
    );
    drop(rows);
    let mut rows = conn.query(
        "SELECT COUNT(*) FROM notification_outbox WHERE job_id = 'call-job' AND class = 'call' AND context_xml = 'call-context' AND status = 'queued'",
        (),
    ).await.expect("preserved call outbox query");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        1
    );
}

async fn assert_missed_calls_supported(db: &Database) {
    let conn = db.guard().await.expect("missed-call guard");
    for (stanza, reason) in [
        ("retracted", "call_retracted"),
        ("unanswered", "call_unanswered"),
    ] {
        conn.execute(
            "INSERT INTO notification_candidates (recipient_bare_jid, conversation_jid, sender_jid, stanza_id_by, stanza_id, class, reason, created_at_ms, call_sid) VALUES ('bob@example.com', 'alice@example.com', 'alice@example.com/tablet', 'bob@example.com', ?, 'missed_call', ?, 10000, ?)",
            waddle_server::db_params![stanza, reason, stanza],
        ).await.expect("missed-call candidate and reserved reason accepted");
    }
    let duplicate = conn.execute(
        "INSERT INTO notification_candidates (recipient_bare_jid, conversation_jid, sender_jid, thread_id, stanza_id_by, stanza_id, class, reason, created_at_ms, call_sid) VALUES ('bob@example.com', 'alice@example.com', 'alice@example.com/other', 'other-thread', 'bob@example.com', 'another-retract', 'missed_call', 'call_retracted', 11000, 'retracted') ON CONFLICT DO NOTHING",
        (),
    ).await.expect("untargeted conflict handling");
    assert_eq!(
        duplicate, 0,
        "one alert per caller, recipient and sid across resources and threads"
    );
    conn.execute(
        "INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, created_at_ms, updated_at_ms) VALUES ('missed-call-job', 'bob@example.com', 'push.example.com', 'node', 'alice@example.com', 'alice@example.com/phone', '[\"alice@example.com/phone\"]', 'missed_call', 1, 'alert-context', 'queued', 10000, 10000)",
        (),
    ).await.expect("missed-call outbox accepted");
}

#[tokio::test]
async fn sqlite_fresh_database_supports_missed_calls() {
    let db = Database::in_memory("missed-call-fresh")
        .await
        .expect("sqlite");
    assert_eq!(
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("fresh migrations"),
        versions_from(BASELINE_VERSION)
    );
    assert_missed_calls_supported(&db).await;
}

#[tokio::test]
async fn sqlite_2001_call_rows_survive_missed_call_migration() {
    let db = Database::in_memory("missed-call-upgrade")
        .await
        .expect("sqlite");
    install_2001_with_calls(&db, "main").await;
    assert_eq!(
        MigrationRunner::single().run(&db).await.expect("upgrade"),
        versions_from(2002)
    );
    assert_call_preserved(&db).await;
    assert_missed_calls_supported(&db).await;
    assert!(MigrationRunner::single()
        .run(&db)
        .await
        .expect("rerun")
        .is_empty());
}

async fn postgres_missed_call_migration(upgrade: bool) {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let schema = format!("notification_missed_call_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPool::connect(&url).await.expect("postgres administrator");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("isolated schema");
    let mut scoped = url::Url::parse(&url).expect("postgres URL");
    scoped
        .query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped.to_string());
    let db = Database::from_config("missed-call-migration", &config)
        .await
        .expect("postgres database");
    if upgrade {
        install_2001_with_calls(&db, &schema).await;
        assert_eq!(
            MigrationRunner::single().run(&db).await.expect("upgrade"),
            versions_from(2002)
        );
        assert_call_preserved(&db).await;
    } else {
        assert_eq!(
            MigrationRunner::single()
                .run(&db)
                .await
                .expect("fresh migrations"),
            versions_from(BASELINE_VERSION)
        );
    }
    assert_missed_calls_supported(&db).await;
    assert!(MigrationRunner::single()
        .run(&db)
        .await
        .expect("rerun")
        .is_empty());
    drop(db);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}

#[tokio::test]
async fn postgres_fresh_database_supports_missed_calls() {
    postgres_missed_call_migration(false).await;
}

#[tokio::test]
async fn postgres_2001_call_rows_survive_missed_call_migration() {
    postgres_missed_call_migration(true).await;
}
