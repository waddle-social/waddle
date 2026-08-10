use std::str::FromStr;

use super::{
    adopt_if_matched, enroll, ensure_table, verify, AdoptOutcome, DeploymentUuid, DurableStore,
    LineageError, LineageRegistryBuilder, LineageStatus, LineageUuid, PgSystemIdentifier,
};
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
    let AdoptOutcome::Adopted { attested, .. } =
        adopt_if_matched(&db, &configured(), &[original.lineage_uuid])
            .await
            .expect("adopt lineage")
    else {
        panic!("expected adoption");
    };

    assert_ne!(attested.lineage_uuid, original.lineage_uuid);
    assert_eq!(
        verify(&db, &configured()).await.expect("verify adopted"),
        attested
    );
}

#[tokio::test]
async fn sqlite_adopt_requires_expected_lineage() {
    let db = Database::in_memory("lineage-adopt-wrong-expected")
        .await
        .expect("open sqlite database");
    let enrolled = enroll(&db, &configured()).await.expect("enroll lineage");
    let wrong =
        LineageUuid::from_str("018f47b2-4b2e-7a3a-9a4c-52a5a6a90003").expect("valid expected UUID");

    // An expected UUID that matches nothing is a read-only no-op; the row
    // is untouched (the startup gate reports unmatched entries separately).
    assert_eq!(
        adopt_if_matched(&db, &configured(), &[wrong])
            .await
            .expect("unmatched adopt is read-only"),
        AdoptOutcome::NotMatched
    );
    assert_eq!(
        verify(&db, &configured())
            .await
            .expect("verify")
            .lineage_uuid,
        enrolled.lineage_uuid
    );
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

fn postgres_url_with_search_path_and_role(database_url: &str, schema: &str, role: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair(
            "options",
            &format!("-c search_path={schema} -c role={role}"),
        );
    url.to_string()
}

fn postgres_url_with_database(database_url: &str, database: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
    url.set_path(&format!("/{database}"));
    url.to_string()
}

async fn postgres_fixture(prefix: &str) -> Option<PostgresFixture> {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (lineage PostgreSQL test)");
        return None;
    };
    Some(postgres_fixture_for_url(prefix, &database_url).await)
}

async fn postgres_fixture_for_url(prefix: &str, database_url: &str) -> PostgresFixture {
    let admin = sqlx::PgPool::connect(database_url)
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
            postgres_url_with_search_path(database_url, &schema),
        ),
    )
    .await
    .expect("open isolated database");
    PostgresFixture { db, admin, schema }
}

async fn drop_postgres_fixture(fixture: PostgresFixture) {
    let PostgresFixture { db, admin, schema } = fixture;
    drop(db);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
}

async fn drop_postgres_database_fixture(
    fixture: PostgresFixture,
    control: sqlx::PgPool,
    database: &str,
) {
    let PostgresFixture { db, admin, schema } = fixture;
    drop(db);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema before database cleanup");
    admin.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&control)
        .await
        .expect("drop isolated PostgreSQL database");
    control.close().await;
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
async fn postgres_reports_system_identifier_unavailable_without_execute_privilege() {
    let Ok(base_database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (lineage PostgreSQL test)");
        return;
    };
    let control = sqlx::PgPool::connect(&base_database_url)
        .await
        .expect("connect PostgreSQL database-control pool");
    let database = format!(
        "waddle_lineage_pg_control_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&control)
        .await
        .expect("create isolated PostgreSQL database for catalog ACL test");
    let isolated_database_url = postgres_url_with_database(&base_database_url, &database);
    let fixture =
        postgres_fixture_for_url("pg_control_system_privilege", &isolated_database_url).await;
    enroll(&fixture.db, &configured())
        .await
        .expect("enroll lineage before testing restricted role");

    let role = format!(
        "waddle_lineage_no_pg_control_{}",
        uuid::Uuid::new_v4().simple()
    );
    let public_execute: bool = sqlx::query_scalar(
        "SELECT has_function_privilege('public', 'pg_catalog.pg_control_system()', 'EXECUTE')",
    )
    .fetch_one(&fixture.admin)
    .await
    .expect("read existing pg_control_system public privilege");

    // PostgreSQL grants this catalog function to PUBLIC by default, so revoking
    // only from the restricted role would not test the permission-error path.
    // This test owns a disposable database, so its catalog ACL cannot affect
    // parallel tests; keep the original ACL state and restore it on every path.
    let result = async {
        sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
            .execute(&fixture.admin)
            .await
            .map_err(|error| format!("create restricted role: {error}"))?;
        sqlx::query(&format!("GRANT {role} TO CURRENT_USER"))
            .execute(&fixture.admin)
            .await
            .map_err(|error| format!("permit test process to SET ROLE: {error}"))?;
        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {role}",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .map_err(|error| format!("grant schema usage: {error}"))?;
        sqlx::query(&format!(
            "GRANT SELECT ON TABLE {}._lineage TO {role}",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .map_err(|error| format!("grant lineage read: {error}"))?;
        sqlx::query("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC")
            .execute(&fixture.admin)
            .await
            .map_err(|error| format!("revoke public pg_control_system execute: {error}"))?;
        sqlx::query(&format!(
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {role}"
        ))
        .execute(&fixture.admin)
        .await
        .map_err(|error| format!("revoke restricted-role pg_control_system execute: {error}"))?;

        let restricted = Database::from_config(
            "lineage-postgres-restricted-role",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                postgres_url_with_search_path_and_role(
                    &isolated_database_url,
                    &fixture.schema,
                    &role,
                ),
            ),
        )
        .await
        .map_err(|error| format!("open restricted database: {error}"))?;
        let verification = verify(&restricted, &configured()).await;
        drop(restricted);
        match verification {
            Err(DatabaseError::Lineage(LineageError::SystemIdentifierUnavailable)) => Ok(()),
            Err(other) => Err(format!(
                "expected typed system identifier error, got {other}"
            )),
            Ok(_) => Err("restricted role unexpectedly read pg_control_system".to_string()),
        }
    }
    .await;

    if public_execute {
        sqlx::query("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO PUBLIC")
            .execute(&fixture.admin)
            .await
            .expect("restore public pg_control_system execute privilege");
    } else {
        sqlx::query("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC")
            .execute(&fixture.admin)
            .await
            .expect("restore absent public pg_control_system execute privilege");
    }
    sqlx::query(&format!("REVOKE {role} FROM CURRENT_USER"))
        .execute(&fixture.admin)
        .await
        .expect("revoke restricted-role membership");
    // The role still holds GRANTs on the disposable schema/table; a bare
    // DROP ROLE fails with 2BP01 until those dependent privileges go.
    sqlx::query(&format!("DROP OWNED BY {role}"))
        .execute(&fixture.admin)
        .await
        .expect("drop restricted-role privileges");
    sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
        .execute(&fixture.admin)
        .await
        .expect("drop restricted role");
    drop_postgres_database_fixture(fixture, control, &database).await;

    result.expect("restricted role must receive typed privilege error");
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
    let AdoptOutcome::Adopted { attested, .. } =
        adopt_if_matched(&db, &configured(), &[first.lineage_uuid])
            .await
            .expect("adopt lineage")
    else {
        panic!("expected adoption");
    };
    assert_ne!(attested.lineage_uuid, first.lineage_uuid);

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

#[tokio::test]
async fn clustered_schema_boundaries_report_colocation_mismatch() {
    let Some(fixture) = postgres_fixture("colocation_global").await else {
        return;
    };
    let database_url = std::env::var("WADDLE_TEST_POSTGRES_URL")
        .expect("postgres URL remains available for gated test");
    let mam_schema = format!(
        "waddle_test_lineage_colocation_mam_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!("CREATE SCHEMA {mam_schema}"))
        .execute(&fixture.admin)
        .await
        .expect("create MAM schema");
    let mam = Database::from_config(
        "lineage-postgres-colocation-mam",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            postgres_url_with_search_path(&database_url, &mam_schema),
        ),
    )
    .await
    .expect("open MAM schema database");
    enroll(&fixture.db, &configured())
        .await
        .expect("enroll global boundary");
    enroll(&mam, &configured())
        .await
        .expect("enroll MAM boundary");

    let mut builder = LineageRegistryBuilder::default();
    builder.register_database(DurableStore::Global, fixture.db.clone());
    builder.register_database(DurableStore::Mam, mam.clone());
    let report = builder.seal().attest(&configured(), true).await;
    assert!(report
        .failures()
        .contains(&(DurableStore::Mam, LineageStatus::ColocationMismatch)));

    drop(mam);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {mam_schema} CASCADE"))
        .execute(&fixture.admin)
        .await
        .expect("drop MAM schema");
    drop_postgres_fixture(fixture).await;
}

#[tokio::test]
async fn sqlite_adopt_if_matched_rotates_only_the_named_boundary() {
    let db = Database::in_memory("lineage-adopt-if-matched")
        .await
        .expect("open sqlite database");
    let original = enroll(&db, &configured()).await.expect("enroll lineage");

    // Matched: rotates.
    let outcome = adopt_if_matched(&db, &configured(), &[original.lineage_uuid])
        .await
        .expect("adopt matched boundary");
    let AdoptOutcome::Adopted { matched, attested } = outcome else {
        panic!("expected adoption, got {outcome:?}");
    };
    assert_eq!(matched, original.lineage_uuid);
    assert_ne!(attested.lineage_uuid, original.lineage_uuid);

    // Re-applying the SAME expected list against the now-rotated row is the
    // shared-database second-store case: a read-only no-op, not an error.
    let outcome = adopt_if_matched(&db, &configured(), &[original.lineage_uuid])
        .await
        .expect("second application is read-only");
    assert_eq!(outcome, AdoptOutcome::NotMatched);
    assert_eq!(
        verify(&db, &configured())
            .await
            .expect("verify")
            .lineage_uuid,
        attested.lineage_uuid,
    );
}

#[tokio::test]
async fn sqlite_rejects_multiple_lineage_rows() {
    let db = Database::in_memory("lineage-multi-row")
        .await
        .expect("open sqlite database");
    // A pre-existing table without the singleton PK/CHECK can hold several
    // rows; build that shape by hand.
    db.execute(
        "CREATE TABLE _lineage (id INTEGER, format INTEGER NOT NULL, lineage_uuid TEXT NOT NULL,          deployment_uuid TEXT NOT NULL, pg_system_identifier TEXT, pg_database_oid TEXT,          pg_database_name TEXT, pg_schema_oid TEXT, pg_schema_name TEXT,          stamped_at TEXT NOT NULL DEFAULT (datetime('now')))",
    )
    .await
    .expect("create constraint-free lineage table");
    for _ in 0..2 {
        db.execute(
            "INSERT INTO _lineage (id, format, lineage_uuid, deployment_uuid)              VALUES (1, 1, '018f47b2-4b2e-7a3a-9a4c-52a5a6a90001',              '018f47b2-4b2e-7a3a-9a4c-52a5a6a90001')",
        )
        .await
        .expect("insert duplicate lineage row");
    }

    assert!(matches!(
        lineage_error(
            verify(&db, &configured())
                .await
                .expect_err("multiple rows fail closed")
        ),
        LineageError::MalformedTable { .. }
    ));
}

#[tokio::test]
async fn sqlite_tolerates_additive_future_columns() {
    let db = Database::in_memory("lineage-extra-column")
        .await
        .expect("open sqlite database");
    enroll(&db, &configured()).await.expect("enroll lineage");
    db.execute("ALTER TABLE _lineage ADD COLUMN future_field TEXT")
        .await
        .expect("add future column");

    // Format-1 columns are a subset: bootstrap validation and verify still
    // work, preserving the format-based evolution story.
    ensure_table(&db).await.expect("extra column tolerated");
    verify(&db, &configured())
        .await
        .expect("verify tolerates additive columns");
}

#[test]
fn pg_system_identifier_accepts_negative_bigint_rendering() {
    // From 2038 initdb-derived identifiers exceed i64::MAX bit patterns and
    // PostgreSQL renders the bigint as a negative decimal.
    let negative: PgSystemIdentifier = "-9223372036854775808".parse().expect("negative parses");
    assert_eq!(negative.0, 9_223_372_036_854_775_808);
    let positive: PgSystemIdentifier = "7373622067229815983".parse().expect("positive parses");
    assert_eq!(positive.0, 7_373_622_067_229_815_983);
    assert!("not-a-number".parse::<PgSystemIdentifier>().is_err());
}

mod sticky_success {
    use std::sync::{Arc, Mutex};

    use super::super::{
        AttestedLineage, DurableStore, LineageAttestor, LineageError, LineageRegistryBuilder,
        LineageStatus,
    };
    use crate::{
        config::LineageConfig,
        db::{DatabaseDriver, DatabaseError},
    };
    use std::collections::VecDeque;
    use std::str::FromStr;

    fn attested() -> AttestedLineage {
        AttestedLineage {
            lineage_uuid: super::super::LineageUuid::from_str(
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a90010",
            )
            .expect("lineage uuid"),
            deployment_uuid: super::super::DeploymentUuid::from_str(
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a90001",
            )
            .expect("deployment uuid"),
            postgres_identity: None,
        }
    }

    struct ScriptedAttestor {
        script: Mutex<VecDeque<Result<AttestedLineage, DatabaseError>>>,
    }

    impl ScriptedAttestor {
        fn new(script: Vec<Result<AttestedLineage, DatabaseError>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
            })
        }
    }

    #[async_trait::async_trait]
    impl LineageAttestor for ScriptedAttestor {
        async fn attest(&self, _config: &LineageConfig) -> Result<AttestedLineage, DatabaseError> {
            self.script
                .lock()
                .expect("script lock")
                .pop_front()
                .expect("scripted attestation available")
        }

        fn driver(&self) -> DatabaseDriver {
            DatabaseDriver::Sqlite
        }
    }

    fn transport_error() -> DatabaseError {
        DatabaseError::Internal(sqlx::Error::PoolTimedOut)
    }

    fn registry_with(
        script: Vec<Result<AttestedLineage, DatabaseError>>,
    ) -> super::super::LineageRegistry {
        let mut builder = LineageRegistryBuilder::default();
        builder.register_probe(DurableStore::Global, ScriptedAttestor::new(script));
        builder.seal()
    }

    #[tokio::test]
    async fn transport_error_after_proven_attestation_stays_attested() {
        let registry = registry_with(vec![Ok(attested()), Err(transport_error())]);
        let config = LineageConfig::default();
        assert!(registry.attest(&config, false).await.is_attested());
        assert!(registry.attest(&config, false).await.is_attested());
    }

    #[tokio::test]
    async fn typed_lineage_error_always_fails_even_after_proven_attestation() {
        let registry = registry_with(vec![
            Ok(attested()),
            Err(DatabaseError::Lineage(LineageError::MissingRow)),
        ]);
        let config = LineageConfig::default();
        assert!(registry.attest(&config, false).await.is_attested());
        let report = registry.attest(&config, false).await;
        assert_eq!(
            report.failures(),
            &[(DurableStore::Global, LineageStatus::MissingLineage)]
        );
        assert!(!report.is_transient_only());
    }

    #[tokio::test]
    async fn database_level_error_is_definitive_not_sticky() {
        // A dropped `_lineage` table / revoked SELECT surfaces as a
        // database-level error, NOT a transport error: it must fail even on
        // a previously proven boundary.
        let registry = registry_with(vec![
            Ok(attested()),
            Err(DatabaseError::QueryFailed(
                "relation _lineage does not exist".to_string(),
            )),
        ]);
        let config = LineageConfig::default();
        assert!(registry.attest(&config, false).await.is_attested());
        let report = registry.attest(&config, false).await;
        assert_eq!(
            report.failures(),
            &[(DurableStore::Global, LineageStatus::VerificationFailed)]
        );
        assert!(!report.is_transient_only());
    }

    #[tokio::test]
    async fn transport_error_without_prior_proof_is_transient_only() {
        let registry = registry_with(vec![Err(transport_error())]);
        let config = LineageConfig::default();
        let report = registry.attest(&config, false).await;
        assert_eq!(
            report.failures(),
            &[(DurableStore::Global, LineageStatus::ProbeError)]
        );
        assert!(report.is_transient_only());
    }
}

#[tokio::test]
async fn adopt_list_rotates_each_matching_boundary() {
    // Two distinct durable boundaries, one comma-list action naming both
    // old lineage UUIDs: each boundary rotates on its own match.
    let db_a = Database::in_memory("lineage-adopt-list-a")
        .await
        .expect("open boundary a");
    let db_b = Database::in_memory("lineage-adopt-list-b")
        .await
        .expect("open boundary b");
    let enrolled_a = enroll(&db_a, &configured()).await.expect("enroll a");
    let enrolled_b = enroll(&db_b, &configured()).await.expect("enroll b");
    let expected = [enrolled_a.lineage_uuid, enrolled_b.lineage_uuid];

    let AdoptOutcome::Adopted { matched, .. } = adopt_if_matched(&db_a, &configured(), &expected)
        .await
        .expect("adopt boundary a")
    else {
        panic!("boundary a should match");
    };
    assert_eq!(matched, enrolled_a.lineage_uuid);
    let AdoptOutcome::Adopted { matched, .. } = adopt_if_matched(&db_b, &configured(), &expected)
        .await
        .expect("adopt boundary b")
    else {
        panic!("boundary b should match");
    };
    assert_eq!(matched, enrolled_b.lineage_uuid);

    let rotated_a = verify(&db_a, &configured()).await.expect("verify a");
    let rotated_b = verify(&db_b, &configured()).await.expect("verify b");
    assert_ne!(rotated_a.lineage_uuid, enrolled_a.lineage_uuid);
    assert_ne!(rotated_b.lineage_uuid, enrolled_b.lineage_uuid);
    assert_ne!(rotated_a.lineage_uuid, rotated_b.lineage_uuid);
}
