//! Schema v1 preserves call rows and deduplicates missed-call alerts.

#[path = "restricted_runtime_support/notification_migration.rs"]
mod support;

use sqlx::PgPool;
use waddle_server::db::{
    Database, DatabaseConfig, DatabaseDriver, MigrationRunner, INITIAL_SCHEMA_VERSION,
};

use support::{
    assert_rows_preserved_and_insert_call, install_baseline_only, seed_notification_rows,
};

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
async fn sqlite_fresh_database_supports_calls_and_missed_calls() {
    let db = Database::in_memory("missed-call-fresh")
        .await
        .expect("sqlite");
    install_baseline_only(&db, "main").await;
    assert_eq!(
        MigrationRunner::single().latest_version(),
        Some(INITIAL_SCHEMA_VERSION)
    );
    seed_notification_rows(&db).await;
    assert_rows_preserved_and_insert_call(&db).await;
    assert_missed_calls_supported(&db).await;
    assert!(MigrationRunner::single()
        .run(&db)
        .await
        .expect("rerun")
        .is_empty());
}

#[tokio::test]
async fn postgres_fresh_database_supports_calls_and_missed_calls() {
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
    assert_eq!(
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("fresh migrations"),
        vec![INITIAL_SCHEMA_VERSION]
    );
    seed_notification_rows(&db).await;
    assert_rows_preserved_and_insert_call(&db).await;
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
