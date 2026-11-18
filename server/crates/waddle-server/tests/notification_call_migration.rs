//! Migration 2001 preserves queued notifications and admits offline call candidates.

#[path = "restricted_runtime_support/notification_migration.rs"]
mod support;

use sqlx::PgPool;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};

use support::{
    assert_rows_preserved_and_insert_call, install_baseline_only, seed_notification_rows,
};

#[tokio::test]
async fn sqlite_baseline_notification_rows_survive_call_migration() {
    let db = Database::in_memory("call-migration").await.expect("sqlite");
    install_baseline_only(&db, "main").await;
    seed_notification_rows(&db).await;
    let indexes = sqlite_indexes(&db).await;
    assert_eq!(indexes.len(), 8);
    assert_eq!(
        MigrationRunner::single().run(&db).await.expect("upgrade"),
        [2001, 2002]
    );
    assert_rows_preserved_and_insert_call(&db).await;
    let upgraded_indexes = sqlite_indexes(&db).await;
    assert_eq!(upgraded_indexes.len(), indexes.len() + 1);
    for index in indexes {
        assert!(
            upgraded_indexes.contains(&index),
            "existing index survives: {index:?}"
        );
    }
    assert!(MigrationRunner::single()
        .run(&db)
        .await
        .expect("rerun")
        .is_empty());
}

async fn sqlite_indexes(db: &Database) -> Vec<(String, String)> {
    let conn = db.guard().await.expect("sqlite connection");
    let mut rows = conn.query(
        "SELECT name, sql FROM sqlite_master WHERE type = 'index' AND tbl_name IN ('notification_candidates', 'notification_outbox') AND sql IS NOT NULL ORDER BY name",
        (),
    ).await.expect("read notification indexes");
    let mut indexes = Vec::new();
    while let Some(row) = rows.next().await.expect("index row") {
        indexes.push((row.get(0).expect("name"), row.get(1).expect("definition")));
    }
    indexes
}

#[tokio::test]
async fn postgres_baseline_notification_rows_survive_call_migration() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let schema = format!("notification_call_{}", uuid::Uuid::new_v4().simple());
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
    let db = Database::from_config("call-migration", &config)
        .await
        .expect("postgres database");
    install_baseline_only(&db, &schema).await;
    seed_notification_rows(&db).await;
    assert_eq!(
        MigrationRunner::single().run(&db).await.expect("upgrade"),
        [2001, 2002]
    );
    assert_rows_preserved_and_insert_call(&db).await;
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
