//! Boot rejects split SM stores before schema initialization or session hydration.
use super::*;

#[tokio::test]
async fn postgres_sm_requires_the_same_schema_without_clustering() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping postgres_sm_requires_the_same_schema_without_clustering: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("postgres admin");
    let global_schema = format!("ingress_colocation_{}", uuid::Uuid::new_v4().simple());
    let separate_schema = format!("sm_colocation_{}", uuid::Uuid::new_v4().simple());
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
    let backend_error = create_ingress_sm_storage(Some("sqlite::memory:"), false, None, &global)
        .await
        .err()
        .expect("startup rejects a different backend");
    assert!(matches!(
        backend_error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::Backend)
    ));
    let shared = create_ingress_sm_storage(Some(global_url.as_str()), false, None, &global)
        .await
        .expect("colocated sm");
    let mut alias_url = global_url.clone();
    alias_url
        .query_pairs_mut()
        .append_pair("application_name", "sm-colocation-alias");
    create_ingress_sm_storage(Some(alias_url.as_str()), false, None, &global)
        .await
        .expect("different URL with the same live identity is colocated");
    create_ingress_sm_storage(None, false, None, &global)
        .await
        .expect("unset override shares the global PostgreSQL pool");
    let error = create_ingress_sm_storage(Some(separate_url.as_str()), false, None, &global)
        .await
        .err()
        .expect("startup rejects split sm schema without clustering");
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
        .expect("split sm schema remains empty");
    assert_eq!(objects, 0, "rejected sm URL must not create schema objects");
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
async fn sqlite_sm_requires_the_same_file_without_clustering() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let global =
        crate::db::Database::open_local("sm-colocation", directory.path().join("global.db"))
            .await
            .expect("global");
    create_ingress_sm_storage(Some(global.database_url()), false, None, &global)
        .await
        .expect("colocated sm");
    create_ingress_sm_storage(None, false, None, &global)
        .await
        .expect("unset override shares durable global storage");
    let memory_error = create_ingress_sm_storage(Some("sqlite::memory:"), false, None, &global)
        .await
        .err()
        .expect("durable global database rejects private memory sm storage");
    assert!(matches!(
        memory_error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::SqliteDatabase)
    ));
    let separate = crate::db::Database::open_local("separate-sm", directory.path().join("sm.db"))
        .await
        .expect("separate");
    let error = create_ingress_sm_storage(Some(separate.database_url()), false, None, &global)
        .await
        .err()
        .expect("startup rejects split sm database");
    assert!(matches!(
        error.downcast_ref::<IngressColocationError>(),
        Some(IngressColocationError::SqliteDatabase)
    ));
    let objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master")
        .fetch_one(separate.sqlite_pool().expect("split SQLite pool"))
        .await
        .expect("split sm database remains empty");
    assert_eq!(objects, 0, "rejected sm URL must not create schema objects");
}

#[tokio::test]
async fn sqlite_sm_private_memory_shares_the_ingress_pool() {
    let global = crate::db::Database::in_memory("sm-colocation")
        .await
        .expect("global");
    let sm = create_ingress_sm_storage(None, false, None, &global)
        .await
        .expect("shared SM");
    global
        .execute("INSERT INTO sm_sessions SELECT * FROM sm_sessions WHERE 1 = 0")
        .await
        .expect("SM schema initialized on the same private ingress database");
    assert!(sm.list_all_sessions().await.expect("sessions").is_empty());
}
