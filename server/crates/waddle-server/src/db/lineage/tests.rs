use std::str::FromStr;

use super::{adopt, enroll, ensure_table, verify, DeploymentUuid, LineageError, LineageUuid};
use crate::{
    config::LineageConfig,
    db::{Database, DatabaseConfig, DatabaseDriver, DatabaseError},
};

fn configured() -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(
            DeploymentUuid::from_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90001")
                .expect("valid deployment UUID"),
        ),
        action: None,
    }
}

fn unconfigured() -> LineageConfig {
    LineageConfig::default()
}

fn lineage_error(error: DatabaseError) -> LineageError {
    match error {
        DatabaseError::Lineage(error) => error,
        other => panic!("expected lineage error, got {other}"),
    }
}

#[tokio::test]
async fn sqlite_enroll_fresh_then_verify() {
    let db = Database::in_memory("lineage-enroll-fresh")
        .await
        .expect("open sqlite database");
    let enrolled = enroll(&db, &configured()).await.expect("enroll lineage");
    let verified = verify(&db, &configured()).await.expect("verify lineage");

    assert_eq!(verified, enrolled);
    assert_eq!(verified.postgres_identity, None);
}

#[tokio::test]
async fn sqlite_enroll_is_idempotent() {
    let db = Database::in_memory("lineage-enroll-idempotent")
        .await
        .expect("open sqlite database");
    let first = enroll(&db, &configured()).await.expect("first enrollment");
    let second = enroll(&db, &configured()).await.expect("second enrollment");

    assert_eq!(second, first);
}

#[tokio::test]
async fn sqlite_verify_missing_row_fails_closed() {
    let db = Database::in_memory("lineage-missing")
        .await
        .expect("open sqlite database");
    ensure_table(&db).await.expect("bootstrap lineage table");

    assert_eq!(
        lineage_error(
            verify(&db, &configured())
                .await
                .expect_err("missing row fails")
        ),
        LineageError::MissingRow
    );
}

#[tokio::test]
async fn sqlite_verify_rejects_deployment_mismatch() {
    let db = Database::in_memory("lineage-deployment-mismatch")
        .await
        .expect("open sqlite database");
    enroll(&db, &configured()).await.expect("enroll lineage");
    let mismatch = LineageConfig {
        deployment_uuid: Some(
            DeploymentUuid::from_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90002")
                .expect("valid mismatch UUID"),
        ),
        action: None,
    };

    assert!(matches!(
        lineage_error(verify(&db, &mismatch).await.expect_err("mismatch fails")),
        LineageError::DeploymentUuidMismatch { .. }
    ));
}

#[tokio::test]
async fn sqlite_verify_rejects_unconfigured_deployment() {
    let db = Database::in_memory("lineage-deployment-unconfigured")
        .await
        .expect("open sqlite database");
    enroll(&db, &configured()).await.expect("enroll lineage");

    assert!(matches!(
        lineage_error(
            verify(&db, &unconfigured())
                .await
                .expect_err("unconfigured deployment fails")
        ),
        LineageError::DeploymentUuidUnconfigured { .. }
    ));
}

#[tokio::test]
async fn sqlite_adopt_rotates_lineage() {
    let db = Database::in_memory("lineage-adopt")
        .await
        .expect("open sqlite database");
    let original = enroll(&db, &configured()).await.expect("enroll lineage");
    let adopted = adopt(&db, &configured(), original.lineage_uuid)
        .await
        .expect("adopt lineage");

    assert_ne!(adopted.lineage_uuid, original.lineage_uuid);
    assert_eq!(
        verify(&db, &configured()).await.expect("verify adopted"),
        adopted
    );
}

#[tokio::test]
async fn sqlite_adopt_requires_expected_lineage() {
    let db = Database::in_memory("lineage-adopt-wrong-expected")
        .await
        .expect("open sqlite database");
    enroll(&db, &configured()).await.expect("enroll lineage");
    let wrong =
        LineageUuid::from_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90003").expect("valid expected UUID");

    assert!(matches!(
        lineage_error(
            adopt(&db, &configured(), wrong)
                .await
                .expect_err("wrong expected lineage fails")
        ),
        LineageError::AdoptExpectationFailed { expected, .. } if expected == wrong
    ));
}

#[tokio::test]
async fn sqlite_enroll_requires_deployment_uuid() {
    let db = Database::in_memory("lineage-enroll-no-deployment")
        .await
        .expect("open sqlite database");

    assert!(matches!(
        lineage_error(
            enroll(&db, &unconfigured())
                .await
                .expect_err("enrollment without deployment UUID fails")
        ),
        LineageError::DeploymentUuidRequired
    ));
}

#[tokio::test]
async fn sqlite_rejects_malformed_table() {
    let db = Database::in_memory("lineage-malformed-table")
        .await
        .expect("open sqlite database");
    db.execute("CREATE TABLE _lineage (id INTEGER PRIMARY KEY)")
        .await
        .expect("create malformed table");

    assert!(matches!(
        lineage_error(ensure_table(&db).await.expect_err("malformed table fails")),
        LineageError::MalformedTable { .. }
    ));
}

#[tokio::test]
async fn sqlite_rejects_unknown_format_and_invalid_uuid() {
    let db = Database::in_memory("lineage-invalid-record")
        .await
        .expect("open sqlite database");
    enroll(&db, &configured()).await.expect("enroll lineage");
    db.execute("UPDATE _lineage SET format = 2")
        .await
        .expect("tamper format");
    assert_eq!(
        lineage_error(verify(&db, &configured()).await.expect_err("format fails")),
        LineageError::UnknownFormat { found: 2 }
    );

    db.execute("UPDATE _lineage SET format = 1, lineage_uuid = 'not-a-uuid'")
        .await
        .expect("tamper UUID");
    assert!(matches!(
        lineage_error(verify(&db, &configured()).await.expect_err("UUID fails")),
        LineageError::InvalidUuid {
            field: "lineage_uuid",
            ..
        }
    ));
}

struct PostgresFixture {
    db: Database,
    admin: sqlx::PgPool,
    schema: String,
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
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

async fn postgres_fixture(prefix: &str) -> Option<PostgresFixture> {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (lineage PostgreSQL test)");
        return None;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect postgres admin pool");
    let schema = format!(
        "waddle_test_lineage_{prefix}_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let db = Database::from_config(
        "lineage-postgres-test",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            postgres_url_with_search_path(&database_url, &schema),
        ),
    )
    .await
    .expect("open isolated database");
    Some(PostgresFixture { db, admin, schema })
}

async fn drop_postgres_fixture(fixture: PostgresFixture) {
    let PostgresFixture { db, admin, schema } = fixture;
    drop(db);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
}

#[tokio::test]
async fn postgres_detects_tampered_identity_fields() {
    let Some(fixture) = postgres_fixture("identity").await else {
        return;
    };
    let db = fixture.db.clone();
    enroll(&db, &configured()).await.expect("enroll lineage");

    for (column, expected) in [
        ("pg_system_identifier", "SystemIdentifierMismatch"),
        ("pg_database_oid", "DatabaseIdentityMismatch"),
        ("pg_schema_oid", "SchemaIdentityMismatch"),
    ] {
        db.execute(&format!("UPDATE _lineage SET {column} = '0'"))
            .await
            .expect("tamper identity");
        let error = lineage_error(
            verify(&db, &configured())
                .await
                .expect_err("tampering fails"),
        );
        match (expected, error) {
            ("SystemIdentifierMismatch", LineageError::SystemIdentifierMismatch { .. })
            | ("DatabaseIdentityMismatch", LineageError::DatabaseIdentityMismatch { .. })
            | ("SchemaIdentityMismatch", LineageError::SchemaIdentityMismatch { .. }) => {}
            (_, other) => panic!("unexpected lineage error {other}"),
        }
        db.execute("UPDATE _lineage SET pg_system_identifier = (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), pg_database_oid = (SELECT oid::text FROM pg_catalog.pg_database WHERE datname = current_database()), pg_schema_oid = (SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = current_schema())")
            .await
            .expect("restore identity");
    }
    drop_postgres_fixture(fixture).await;
}

#[tokio::test]
async fn postgres_adopt_rotates_lineage_and_concurrent_enrollment_serializes() {
    let Some(fixture) = postgres_fixture("adopt_race").await else {
        return;
    };
    let db = fixture.db.clone();
    let db_second = Database::from_config(
        "lineage-postgres-race-second",
        &DatabaseConfig::new(DatabaseDriver::Postgres, db.database_url().to_string()),
    )
    .await
    .expect("open second pool");
    let config = configured();
    let (first, second) = tokio::join!(enroll(&db, &config), enroll(&db_second, &config));
    let first = first.expect("first concurrent enrollment");
    let second = second.expect("second concurrent enrollment");
    assert_eq!(first, second);
    let adopted = adopt(&db, &configured(), first.lineage_uuid)
        .await
        .expect("adopt lineage");
    assert_ne!(adopted.lineage_uuid, first.lineage_uuid);

    drop(db_second);
    drop_postgres_fixture(fixture).await;
}

#[tokio::test]
async fn postgres_search_path_boundaries_have_independent_lineages() {
    let Some(fixture) = postgres_fixture("search_path_first").await else {
        return;
    };
    let database_url = std::env::var("WADDLE_TEST_POSTGRES_URL")
        .expect("postgres URL remains available for gated test");
    let second_schema = format!(
        "waddle_test_lineage_search_path_second_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!("CREATE SCHEMA {second_schema}"))
        .execute(&fixture.admin)
        .await
        .expect("create second isolated schema");
    let second = Database::from_config(
        "lineage-postgres-search-path-second",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            postgres_url_with_search_path(&database_url, &second_schema),
        ),
    )
    .await
    .expect("open second schema database");

    let first = enroll(&fixture.db, &configured())
        .await
        .expect("enroll first boundary");
    let second_lineage = enroll(&second, &configured())
        .await
        .expect("enroll second boundary");
    assert_ne!(first.lineage_uuid, second_lineage.lineage_uuid);

    drop(second);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {second_schema} CASCADE"))
        .execute(&fixture.admin)
        .await
        .expect("drop second isolated schema");
    drop_postgres_fixture(fixture).await;
}
