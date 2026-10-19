//! Boot rejects split pending stores before schema initialization.
use super::*;

#[tokio::test]
async fn postgres_pending_requires_the_same_schema_without_clustering() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping postgres_pending_requires_the_same_schema_without_clustering: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres admin");
    let global_schema = format!("ingress_colocation_{}", uuid::Uuid::new_v4().simple());
    let separate_schema = format!("pending_colocation_{}", uuid::Uuid::new_v4().simple());
    for schema in [&global_schema, &separate_schema] {
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create schema");
    }
    let mut global_url = url::Url::parse(&database_url).expect("database URL");
    global_url
        .query_pairs_mut()
        .append_pair("options", &format!("-c search_path={global_schema}"));
    let mut separate_url = url::Url::parse(&database_url).expect("database URL");
    separate_url
        .query_pairs_mut()
        .append_pair("options", &format!("-c search_path={separate_schema}"));
    let config =
        crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, global_url.to_string());
    let global = crate::db::Database::from_config("ingress-colocation", &config)
        .await
        .expect("global");
    let backend_error = create_ingress_pending_storage(Some("sqlite::memory:"), &global)
        .await
        .err()
        .expect("startup rejects a different backend");
    assert!(matches!(
        backend_error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::Backend)
    ));
    let shared = create_ingress_pending_storage(Some(global_url.as_str()), &global)
        .await
        .expect("colocated pending");
    let error = create_ingress_pending_storage(Some(separate_url.as_str()), &global)
        .await
        .err()
        .expect("startup rejects split pending schema without clustering");
    assert!(matches!(
        error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::PostgresIdentity { .. })
    ));
    let objects: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)",
        )
        .bind(&separate_schema)
        .fetch_one(&admin)
        .await
        .expect("split pending schema remains empty");
    assert_eq!(
        objects, 0,
        "rejected pending URL must not create schema objects"
    );
    drop(shared);
    drop(global);
    for schema in [&global_schema, &separate_schema] {
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop schema");
    }
}

#[tokio::test]
async fn sqlite_pending_requires_the_same_file_without_clustering() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let global =
        crate::db::Database::open_local("pending-colocation", directory.path().join("global.db"))
            .await
            .expect("global");
    create_ingress_pending_storage(Some(global.database_url()), &global)
        .await
        .expect("colocated pending");
    create_ingress_pending_storage(None, &global)
        .await
        .expect("unset override shares durable global storage");
    let memory_error = create_ingress_pending_storage(Some("sqlite::memory:"), &global)
        .await
        .err()
        .expect("durable global database rejects private memory pending storage");
    assert!(matches!(
        memory_error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::SqliteDatabase)
    ));
    let separate =
        crate::db::Database::open_local("separate-pending", directory.path().join("pending.db"))
            .await
            .expect("separate");
    let error = create_ingress_pending_storage(Some(separate.database_url()), &global)
        .await
        .err()
        .expect("startup rejects split pending database");
    assert!(matches!(
        error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::SqliteDatabase)
    ));
    let objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master")
        .fetch_one(separate.sqlite_pool().expect("split SQLite pool"))
        .await
        .expect("split pending database remains empty");
    assert_eq!(
        objects, 0,
        "rejected pending URL must not create schema objects"
    );
}

#[tokio::test]
async fn sqlite_pending_private_memory_shares_the_ingress_pool() {
    let global = crate::db::Database::in_memory("pending-colocation")
        .await
        .expect("global");
    global
        .execute("CREATE TABLE shared_pool_marker (id INTEGER)")
        .await
        .expect("marker");
    let pending = create_ingress_pending_storage(None, &global)
        .await
        .expect("shared pending");
    pending
        .database()
        .execute("INSERT INTO shared_pool_marker VALUES (1)")
        .await
        .expect("pending uses the same private database");
}
