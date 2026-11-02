//! PostgreSQL catalog and real separated-login checks for the dark diagnostic.

use super::{url_with_database, url_with_option, DarkMigrationOwnerError, Fixture};
use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::db::{inspect_dark_postgres_epoch_acl, DarkEpochAclStage};
use waddle_server::ingress_substrate::EPOCH_GUARDED_TABLES;

async fn admin(fixture: &Fixture, sql: &str) {
    sqlx::query(sql)
        .execute(&fixture.admin)
        .await
        .unwrap_or_else(|error| panic!("fixture SQL failed: {sql}: {error}"));
}

async fn prepare(fixture: &Fixture) {
    admin(
        fixture,
        &format!("REVOKE TEMP ON DATABASE {} FROM PUBLIC", fixture.database),
    )
    .await;
    admin(
        fixture,
        &format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
            fixture.schema, fixture.runtime
        ),
    )
    .await;
}

async fn grant_id(fixture: &Fixture) {
    admin(
        fixture,
        &format!(
            "GRANT UPDATE (id) ON {}.ingress_protocol_epoch TO {}",
            fixture.schema, fixture.runtime
        ),
    )
    .await;
}

async fn inspect(fixture: &Fixture) -> Result<DarkEpochAclStage, DarkMigrationOwnerError> {
    inspect_dark_postgres_epoch_acl(
        &fixture.owner_options,
        &fixture.runtime_options,
        &fixture.config(),
    )
    .await
}

async fn assert_system_column_acls_rejected(fixture: &Fixture, stage: DarkEpochAclStage) {
    let table = format!("{}.ingress_protocol_epoch", fixture.schema);
    for (grant, revoke) in [
        (
            format!("GRANT SELECT (tableoid) ON {table} TO PUBLIC"),
            format!("REVOKE SELECT (tableoid) ON {table} FROM PUBLIC"),
        ),
        (
            format!("GRANT UPDATE (tableoid) ON {table} TO {}", fixture.runtime),
            format!(
                "REVOKE UPDATE (tableoid) ON {table} FROM {}",
                fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT UPDATE (tableoid) ON {table} TO {} WITH GRANT OPTION",
                fixture.runtime
            ),
            format!(
                "REVOKE UPDATE (tableoid) ON {table} FROM {}",
                fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT SELECT (tableoid) ON {table} TO {} WITH GRANT OPTION",
                fixture.runtime
            ),
            format!(
                "REVOKE SELECT (tableoid) ON {table} FROM {}",
                fixture.runtime
            ),
        ),
    ] {
        admin(fixture, &grant).await;
        let (attnum, has_acl): (i16, bool) = sqlx::query_as(
            "SELECT attnum, attacl IS NOT NULL FROM pg_catalog.pg_attribute \
             WHERE attrelid = pg_catalog.to_regclass($1) AND attname = 'tableoid'",
        )
        .bind(&table)
        .fetch_one(&fixture.admin)
        .await
        .expect("observe system-column grant");
        assert_eq!(attnum, -6);
        assert!(has_acl, "no system-column ACL after {grant}");
        assert!(
            matches!(inspect(fixture).await, Err(DarkMigrationOwnerError::Roles)),
            "accepted {grant} at {stage:?}"
        );
        admin(fixture, &revoke).await;
        assert_eq!(
            inspect(fixture).await.expect("restored system-column ACL"),
            stage,
            "after {revoke}"
        );
    }
}

#[tokio::test]
async fn exact_guard_stages_have_distinct_effective_acl_and_real_login_behavior() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    grant_id(&fixture).await;
    assert_eq!(
        inspect(&fixture).await.expect("V1009 observation"),
        DarkEpochAclStage::V1009EpochZero
    );
    assert_system_column_acls_rejected(&fixture, DarkEpochAclStage::V1009EpochZero).await;
    admin(
        &fixture,
        &format!(
            "GRANT INSERT ON {}.ingress_messages TO {}",
            fixture.schema, fixture.runtime
        ),
    )
    .await;
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("separate runtime login");
    let (session, current): (String, String) =
        sqlx::query_as("SELECT session_user::text, current_user::text")
            .fetch_one(&mut runtime)
            .await
            .expect("runtime identity");
    assert_eq!(
        (session.as_str(), current.as_str()),
        (fixture.runtime.as_str(), fixture.runtime.as_str())
    );
    let lock = format!(
        "SELECT epoch FROM {}.ingress_protocol_epoch WHERE id = 1 FOR SHARE",
        fixture.schema
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(&lock)
            .fetch_one(&mut runtime)
            .await
            .expect("id UPDATE permits V1009 row lock"),
        0
    );
    let insert = format!(
        "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.schema
    );
    sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect("legacy guard can lock epoch with UPDATE(id)");
    let error = sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET id = id WHERE id = 1",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .expect_err("V1009 row trigger rejects id-only UPDATE");
    assert!(
        error.to_string().contains("must advance exactly one step"),
        "{error}"
    );
    let error = sqlx::query(&format!(
        "UPDATE {}.ingress_protocol_epoch SET epoch = 1 WHERE id = 1",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .expect_err("runtime lacks UPDATE(epoch)");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501")
    );
    drop(runtime);

    let mut installer = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("fixture owner");
    sqlx::raw_sql(include_str!(
        "../../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&mut installer)
    .await
    .expect("install candidate on disposable fixture");
    drop(installer);
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    admin(
        &fixture,
        &format!(
            "REVOKE UPDATE (id) ON {}.ingress_protocol_epoch FROM {}",
            fixture.schema, fixture.runtime
        ),
    )
    .await;
    assert_eq!(
        inspect(&fixture).await.expect("relation-guard observation"),
        DarkEpochAclStage::RelationGuardEpochZero
    );
    assert_system_column_acls_rejected(&fixture, DarkEpochAclStage::RelationGuardEpochZero).await;
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    let error = sqlx::query(&lock)
        .execute(&mut runtime)
        .await
        .expect_err("SELECT alone does not permit FOR SHARE");
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501")
    );
    let mut tx = runtime.begin().await.expect("runtime transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS SHARE MODE",
        fixture.schema
    ))
    .execute(&mut *tx)
    .await
    .expect("SELECT permits relation prelock");
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut *tx).await.expect("candidate proof");
    sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut *tx)
        .await
        .expect("candidate guard works without UPDATE");
    tx.rollback().await.expect("discard candidate write");
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn scoped_acl_rejects_broad_public_grant_options_and_role_paths() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    // TEMP is PUBLIC by default in PostgreSQL and is an explicit refusal.
    admin(
        &fixture,
        &format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
            fixture.schema, fixture.runtime
        ),
    )
    .await;
    grant_id(&fixture).await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    prepare(&fixture).await;
    assert_eq!(
        inspect(&fixture).await.expect("scoped ACL"),
        DarkEpochAclStage::V1009EpochZero
    );
    let table = format!("{}.ingress_protocol_epoch", fixture.schema);
    for (grant, revoke) in [
        (
            format!("GRANT UPDATE ON {table} TO {}", fixture.runtime),
            format!("REVOKE UPDATE ON {table} FROM {}", fixture.runtime),
        ),
        (
            format!("GRANT UPDATE (epoch) ON {table} TO {}", fixture.runtime),
            format!("REVOKE UPDATE (epoch) ON {table} FROM {}", fixture.runtime),
        ),
        (
            format!("GRANT INSERT (id) ON {table} TO {}", fixture.runtime),
            format!("REVOKE INSERT (id) ON {table} FROM {}", fixture.runtime),
        ),
        (
            format!("GRANT REFERENCES (id) ON {table} TO {}", fixture.runtime),
            format!("REVOKE REFERENCES (id) ON {table} FROM {}", fixture.runtime),
        ),
        (
            format!("GRANT MAINTAIN ON {table} TO {}", fixture.runtime),
            format!("REVOKE MAINTAIN ON {table} FROM {}", fixture.runtime),
        ),
        (
            format!(
                "GRANT SELECT ON {table} TO {} WITH GRANT OPTION",
                fixture.runtime
            ),
            format!(
                "REVOKE GRANT OPTION FOR SELECT ON {table} FROM {}",
                fixture.runtime
            ),
        ),
        (
            format!("GRANT SELECT ON {table} TO PUBLIC"),
            format!("REVOKE SELECT ON {table} FROM PUBLIC"),
        ),
        (
            format!("GRANT UPDATE (id) ON {table} TO PUBLIC"),
            format!("REVOKE UPDATE (id) ON {table} FROM PUBLIC"),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {} TO PUBLIC", fixture.schema),
            format!("REVOKE USAGE ON SCHEMA {} FROM PUBLIC", fixture.schema),
        ),
        (
            format!(
                "GRANT CREATE ON SCHEMA {} TO {}",
                fixture.schema, fixture.runtime
            ),
            format!(
                "REVOKE CREATE ON SCHEMA {} FROM {}",
                fixture.schema, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT INSERT (version) ON {}._migrations TO {}",
                fixture.schema, fixture.runtime
            ),
            format!(
                "REVOKE INSERT (version) ON {}._migrations FROM {}",
                fixture.schema, fixture.runtime
            ),
        ),
        (
            format!("GRANT SELECT ON {}._lineage TO PUBLIC", fixture.schema),
            format!("REVOKE SELECT ON {}._lineage FROM PUBLIC", fixture.schema),
        ),
    ] {
        admin(&fixture, &grant).await;
        assert!(
            matches!(inspect(&fixture).await, Err(DarkMigrationOwnerError::Roles)),
            "accepted {grant}"
        );
        admin(&fixture, &revoke).await;
        // PostgreSQL's table-level REVOKE UPDATE also removes column grants.
        grant_id(&fixture).await;
        assert_eq!(
            inspect(&fixture).await.expect("restored scoped ACL"),
            DarkEpochAclStage::V1009EpochZero,
            "after {revoke}"
        );
    }
    let delegated = format!("{}_delegate", fixture.runtime);
    admin(&fixture, &format!("CREATE ROLE {delegated} NOLOGIN")).await;
    admin(
        &fixture,
        &format!("GRANT USAGE ON SCHEMA {} TO {delegated}", fixture.schema),
    )
    .await;
    admin(
        &fixture,
        &format!("GRANT UPDATE (epoch) ON {table} TO {delegated}"),
    )
    .await;
    admin(
        &fixture,
        &format!(
            "GRANT {delegated} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            fixture.runtime
        ),
    )
    .await;
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    sqlx::query(&format!("SET ROLE {delegated}"))
        .execute(&mut runtime)
        .await
        .expect("SET ROLE works despite NOINHERIT");
    let dangerous: bool = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.has_column_privilege(current_user, '{table}', 'epoch', 'UPDATE')"
    ))
    .fetch_one(&mut runtime)
    .await
    .expect("delegated effective UPDATE");
    assert!(dangerous);
    drop(runtime);
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    admin(
        &fixture,
        &format!("REVOKE {delegated} FROM {}", fixture.runtime),
    )
    .await;
    admin(&fixture, &format!("DROP OWNED BY {delegated}")).await;
    admin(&fixture, &format!("DROP ROLE {delegated}")).await;
    admin(
        &fixture,
        &format!("ALTER ROLE {} CREATEROLE", fixture.runtime),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    admin(
        &fixture,
        &format!("ALTER ROLE {} NOCREATEROLE", fixture.runtime),
    )
    .await;
    admin(
        &fixture,
        &format!(
            "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
            fixture.owner, fixture.runtime
        ),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    admin(
        &fixture,
        &format!("REVOKE {} FROM {}", fixture.owner, fixture.runtime),
    )
    .await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn catalog_observation_refuses_changed_bodies_constraints_and_each_guarded_trigger() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    assert_eq!(
        inspect(&fixture).await.expect("baseline"),
        DarkEpochAclStage::V1009EpochZero
    );

    let function = format!("{}.waddle_ingress_epoch_guard()", fixture.schema);
    let original: String =
        sqlx::query_scalar("SELECT pg_catalog.pg_get_functiondef($1::regprocedure)")
            .bind(&function)
            .fetch_one(&fixture.admin)
            .await
            .expect("original function");
    let changed = original.replacen("DECLARE", "DECLARE\n    -- drift", 1);
    assert_ne!(original, changed);
    admin(&fixture, &changed).await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(&fixture, &original).await;
    admin(
        &fixture,
        &format!("ALTER FUNCTION {function} SECURITY DEFINER"),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(
        &fixture,
        &format!("ALTER FUNCTION {function} SECURITY INVOKER"),
    )
    .await;

    for table in EPOCH_GUARDED_TABLES {
        let trigger = format!("{table}_epoch_guard_dml");
        admin(
            &fixture,
            &format!(
                "ALTER TABLE {}.{table} DISABLE TRIGGER {trigger}",
                fixture.schema
            ),
        )
        .await;
        assert!(
            matches!(
                inspect(&fixture).await,
                Err(DarkMigrationOwnerError::EpochCatalogDrift)
            ),
            "disabled trigger on {table}"
        );
        admin(
            &fixture,
            &format!(
                "ALTER TABLE {}.{table} ENABLE ALWAYS TRIGGER {trigger}",
                fixture.schema
            ),
        )
        .await;
    }
    admin(&fixture, &format!("ALTER TABLE {}.ingress_protocol_epoch DISABLE TRIGGER ingress_protocol_epoch_guard_row", fixture.schema)).await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(&fixture, &format!("ALTER TABLE {}.ingress_protocol_epoch ENABLE ALWAYS TRIGGER ingress_protocol_epoch_guard_row", fixture.schema)).await;
    admin(
        &fixture,
        &format!(
            "ALTER TABLE {}.ingress_protocol_epoch DROP CONSTRAINT ingress_protocol_epoch_id_check",
            fixture.schema
        ),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(&fixture, &format!("ALTER TABLE {}.ingress_protocol_epoch ADD CONSTRAINT ingress_protocol_epoch_id_check CHECK (id = 1)", fixture.schema)).await;
    admin(&fixture, &format!("ALTER TABLE {}.ingress_epoch_guard_manifest DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row", fixture.schema)).await;
    admin(
        &fixture,
        &format!(
            "DELETE FROM {}.ingress_epoch_guard_manifest WHERE table_name = 'ingress_messages'",
            fixture.schema
        ),
    )
    .await;
    admin(&fixture, &format!("ALTER TABLE {}.ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row", fixture.schema)).await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(
        &fixture,
        &format!(
            "INSERT INTO {}.ingress_epoch_guard_manifest (table_name) VALUES ('ingress_messages')",
            fixture.schema
        ),
    )
    .await;
    assert_eq!(
        inspect(&fixture).await.expect("restored catalog"),
        DarkEpochAclStage::V1009EpochZero
    );
    admin(
        &fixture,
        &format!(
            "CREATE TABLE {}.ingress_unexpected (id integer)",
            fixture.schema
        ),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(
        &fixture,
        &format!("DROP TABLE {}.ingress_unexpected", fixture.schema),
    )
    .await;
    admin(
        &fixture,
        &format!(
            "CREATE VIEW {}.ingress_unexpected AS SELECT 1 AS id",
            fixture.schema
        ),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    admin(
        &fixture,
        &format!("DROP VIEW {}.ingress_unexpected", fixture.schema),
    )
    .await;
    admin(
        &fixture,
        &format!("ALTER TABLE {}.ingress_protocol_epoch DROP CONSTRAINT ingress_protocol_epoch_check, DROP CONSTRAINT ingress_protocol_epoch_epoch_check, ADD CONSTRAINT duplicate_id_1 CHECK (id = 1), ADD CONSTRAINT duplicate_id_2 CHECK (id = 1)", fixture.schema),
    )
    .await;
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn observations_do_not_rule_out_callable_security_definer_bypass() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    let mut owner = PgConnection::connect_with(&fixture.owner_options)
        .await
        .expect("actual owner login");
    sqlx::query(&format!(
        "CREATE FUNCTION {}.lab_privileged_advance() RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$ BEGIN UPDATE {}.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = '{}'::uuid WHERE id = 1; END $$",
        fixture.schema, fixture.schema, fixture.lineage.lineage_uuid.0
    )).execute(&mut owner).await.expect("fixture definer function, default PUBLIC EXECUTE");
    drop(owner);
    assert_eq!(
        inspect(&fixture).await.expect("narrow ACL observation"),
        DarkEpochAclStage::V1009EpochZero
    );
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    sqlx::query(&format!(
        "SELECT {}.lab_privileged_advance()",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .expect("PUBLIC definer function bypasses direct ACL scope");
    let epoch: i64 = sqlx::query_scalar(&format!(
        "SELECT epoch FROM {}.ingress_protocol_epoch WHERE id = 1",
        fixture.schema
    ))
    .fetch_one(&mut runtime)
    .await
    .expect("observe bypass");
    assert_eq!(epoch, 1);
    assert!(matches!(
        inspect(&fixture).await,
        Err(DarkMigrationOwnerError::UnsupportedEpoch)
    ));
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn runtime_session_must_match_role_database_and_observed_cluster() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    let wrong_role = fixture.runtime_options.clone().username(&fixture.owner);
    assert!(matches!(
        inspect_dark_postgres_epoch_acl(&fixture.owner_options, &wrong_role, &fixture.config())
            .await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    let wrong_database =
        PgConnectOptions::from_str(&url_with_option(&url, "user", &fixture.runtime))
            .expect("same role in other database");
    assert!(matches!(
        inspect_dark_postgres_epoch_acl(&fixture.owner_options, &wrong_database, &fixture.config())
            .await,
        Err(DarkMigrationOwnerError::RuntimeSessionUnverified)
    ));
    let caller_application = fixture
        .runtime_options
        .clone()
        .application_name("caller-controlled-value");
    assert_eq!(
        inspect_dark_postgres_epoch_acl(
            &fixture.owner_options,
            &caller_application,
            &fixture.config()
        )
        .await
        .expect("caller name is replaced by internal nonce"),
        DarkEpochAclStage::V1009EpochZero
    );

    // CI can provide one cluster; the local two-cluster run exercises this
    // additional negative when a second independent cluster is available.
    if let Ok(other_url) = std::env::var("WADDLE_TEST_POSTGRES_SECOND_URL") {
        let other = PgPool::connect(&other_url)
            .await
            .expect("independent cluster admin");
        sqlx::query(&format!("CREATE ROLE {} LOGIN", fixture.runtime))
            .execute(&other)
            .await
            .expect("matching login on other cluster");
        sqlx::query(&format!("CREATE DATABASE {}", fixture.database))
            .execute(&other)
            .await
            .expect("matching database on other cluster");
        let other_database = url_with_database(&other_url, &fixture.database);
        let other_runtime =
            PgConnectOptions::from_str(&url_with_option(&other_database, "user", &fixture.runtime))
                .expect("other-cluster options");
        assert!(matches!(
            inspect_dark_postgres_epoch_acl(
                &fixture.owner_options,
                &other_runtime,
                &fixture.config()
            )
            .await,
            Err(DarkMigrationOwnerError::RuntimeSessionUnverified)
        ));
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", fixture.database))
            .execute(&other)
            .await
            .expect("drop other-cluster database");
        sqlx::query(&format!("DROP ROLE {}", fixture.runtime))
            .execute(&other)
            .await
            .expect("drop other-cluster role");
        other.close().await;
    } else {
        eprintln!("second-cluster correlation test not configured");
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn terminated_runtime_during_scan_cannot_yield_an_observation() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    assert_eq!(
        inspect(&fixture)
            .await
            .expect("baseline before interruption"),
        DarkEpochAclStage::V1009EpochZero
    );
    let mut blocker = fixture.admin.begin().await.expect("epoch lock holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *blocker)
    .await
    .expect("block final epoch read");
    let application = format!("epoch-acl-owner-{}", uuid::Uuid::new_v4().simple());
    let owner = fixture.owner_options.clone().application_name(&application);
    let runtime = fixture.runtime_options.clone();
    let config = fixture.config();
    let task =
        tokio::spawn(
            async move { inspect_dark_postgres_epoch_acl(&owner, &runtime, &config).await },
        );
    let pid = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND wait_event_type = 'Lock')",
            )
            .bind(&application)
            .fetch_one(&fixture.admin)
            .await
            .expect("find blocked owner");
            if blocked {
                let pid: Option<i32> = sqlx::query_scalar(
                    "SELECT pid FROM pg_catalog.pg_stat_activity WHERE usename = $1 \
                     AND application_name LIKE 'waddle-epoch-acl-%'",
                )
                .bind(&fixture.runtime)
                .fetch_optional(&fixture.admin)
                .await
                .expect("find live runtime");
                if let Some(pid) = pid {
                    break pid;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner passed its first activity correlation and blocked");
    let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("terminate runtime backend");
    assert!(terminated);
    blocker.rollback().await.expect("release epoch read");
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("preflight completes")
        .expect("task");
    assert!(
        matches!(
            result,
            Err(DarkMigrationOwnerError::Postgres(_))
                | Err(DarkMigrationOwnerError::RuntimeSessionUnverified)
        ),
        "unexpected result after runtime termination: {result:?}"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn owner_respects_tighter_lock_and_statement_timeouts() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    assert_eq!(
        inspect(&fixture).await.expect("baseline"),
        DarkEpochAclStage::V1009EpochZero
    );
    let mut blocker = fixture.admin.begin().await.expect("epoch lock holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold epoch relation");
    for options in [
        [("lock_timeout", "200ms"), ("statement_timeout", "5s")],
        [("lock_timeout", "5s"), ("statement_timeout", "200ms")],
    ] {
        let owner = fixture.owner_options.clone().options(options);
        let started = std::time::Instant::now();
        let result =
            inspect_dark_postgres_epoch_acl(&owner, &fixture.runtime_options, &fixture.config())
                .await;
        assert!(result.is_err(), "blocked scan unexpectedly succeeded");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "tight session timeout was loosened: {:?}",
            started.elapsed()
        );
    }
    blocker.rollback().await.expect("release epoch relation");
    fixture.cleanup().await;
}

#[tokio::test]
async fn epoch_transition_while_first_read_waits_is_refused() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    let mut owner_transition = fixture.admin.begin().await.expect("epoch owner");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *owner_transition)
    .await
    .expect("hold epoch transition lock");
    let application = format!("epoch-acl-race-{}", uuid::Uuid::new_v4().simple());
    let owner = fixture.owner_options.clone().application_name(&application);
    let runtime = fixture.runtime_options.clone();
    let config = fixture.config();
    let task =
        tokio::spawn(
            async move { inspect_dark_postgres_epoch_acl(&owner, &runtime, &config).await },
        );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name = $1 AND wait_event_type = 'Lock')")
                .bind(&application).fetch_one(&fixture.admin).await.expect("find blocked observation");
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("observation is waiting on the epoch relation");
    sqlx::query(&format!("UPDATE {}.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1", fixture.schema))
        .bind(fixture.lineage.lineage_uuid.0).execute(&mut *owner_transition).await.expect("commit epoch transition");
    owner_transition.commit().await.expect("release transition");
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("preflight completes")
        .expect("task");
    assert!(
        matches!(result, Err(DarkMigrationOwnerError::UnsupportedEpoch)),
        "{result:?}"
    );
    fixture.cleanup().await;
}

#[cfg(unix)]
async fn relay_until_setup_stalls<S>(
    client: tokio::net::TcpStream,
    upstream: S,
    stalled: tokio::sync::oneshot::Sender<()>,
) -> std::io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut server_read, mut server_write) = tokio::io::split(upstream);
    let server_to_client =
        tokio::spawn(async move { tokio::io::copy(&mut server_read, &mut client_write).await });
    let mut startup_length = [0_u8; 4];
    client_read.read_exact(&mut startup_length).await?;
    let length = u32::from_be_bytes(startup_length) as usize;
    let mut startup = vec![0; length - 4];
    client_read.read_exact(&mut startup).await?;
    server_write.write_all(&startup_length).await?;
    server_write.write_all(&startup).await?;
    loop {
        let mut header = [0_u8; 5];
        client_read.read_exact(&mut header).await?;
        let length = u32::from_be_bytes(header[1..5].try_into().expect("message length")) as usize;
        let mut payload = vec![0; length - 4];
        client_read.read_exact(&mut payload).await?;
        if payload
            .windows(b"SET TRANSACTION READ ONLY".len())
            .any(|window| window == b"SET TRANSACTION READ ONLY")
        {
            let _ = stalled.send(());
            let mut byte = [0_u8; 1];
            while client_read.read(&mut byte).await? != 0 {}
            break;
        }
        server_write.write_all(&header).await?;
        server_write.write_all(&payload).await?;
    }
    server_to_client.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn stalled_runtime_setup_times_out_and_discards_connection() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let proxy_address = listener.local_addr().expect("proxy address");
    let target = fixture.runtime_options.clone();
    let (stalled, observed) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await?;
        let upstream = crate::postgres_proxy::connect_local_postgres(&target).await?;
        relay_until_setup_stalls(client, upstream, stalled).await
    });
    let runtime = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(proxy_address.port())
        .username(&fixture.runtime)
        .database(&fixture.database)
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    let owner = fixture.owner_options.clone();
    let config = fixture.config();
    let task =
        tokio::spawn(
            async move { inspect_dark_postgres_epoch_acl(&owner, &runtime, &config).await },
        );
    tokio::time::timeout(Duration::from_secs(5), observed)
        .await
        .expect("setup statement reached proxy")
        .expect("proxy is running");
    let result = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("stalled setup is bounded")
        .expect("task");
    assert!(
        matches!(result, Err(DarkMigrationOwnerError::PreflightTimeout)),
        "{result:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("discard closes runtime socket")
        .expect("proxy task")
        .expect("proxy connection closes cleanly");
    fixture.cleanup().await;
}

#[tokio::test]
async fn v1028_trigger_sets_are_exact_at_both_epoch_guard_stages() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;
    let schema = &fixture.schema;
    let triggers = [
        (
            "ingress_messages",
            "ingress_messages_source_binding_guard",
            "waddle_ingress_source_binding_guard",
        ),
        (
            "ingress_messages",
            "ingress_messages_source_insert_guard",
            "waddle_ingress_source_insert_guard",
        ),
        (
            "ingress_owner_rejections",
            "ingress_owner_rejections_update_guard",
            "waddle_ingress_owner_rejection_immutable",
        ),
        (
            "ingress_owner_rejections",
            "ingress_owner_rejections_insert_guard",
            "waddle_ingress_owner_rejection_insert_guard",
        ),
        (
            "ingress_owner_rejections",
            "ingress_owner_rejections_delete_guard",
            "waddle_ingress_owner_rejection_delete_guard",
        ),
    ];

    for stage in [
        DarkEpochAclStage::V1009EpochZero,
        DarkEpochAclStage::RelationGuardEpochZero,
    ] {
        if stage == DarkEpochAclStage::RelationGuardEpochZero {
            let mut installer = PgConnection::connect_with(&fixture.admin_options)
                .await
                .expect("candidate installer");
            sqlx::raw_sql(include_str!(
                "../../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql"
            ))
            .execute(&mut installer)
            .await
            .expect("install unregistered candidate on disposable fixture");
            drop(installer);
            admin(
                &fixture,
                &format!(
                    "REVOKE UPDATE (id) ON {schema}.ingress_protocol_epoch FROM {}",
                    fixture.runtime
                ),
            )
            .await;
        }
        assert_eq!(
            inspect(&fixture).await.expect("real migration triggers"),
            stage
        );

        for (table, trigger, function) in triggers {
            let original_trigger: String = sqlx::query_scalar(
                "SELECT pg_catalog.pg_get_triggerdef(t.oid) FROM pg_catalog.pg_trigger t \
                 WHERE t.tgrelid = pg_catalog.to_regclass($1) AND t.tgname = $2",
            )
            .bind(format!("{schema}.{table}"))
            .bind(trigger)
            .fetch_one(&fixture.admin)
            .await
            .expect("original trigger definition");

            admin(
                &fixture,
                &format!("DROP TRIGGER {trigger} ON {schema}.{table}"),
            )
            .await;
            assert_v1028_catalog_drift(&fixture, stage, &format!("missing {trigger}")).await;
            admin(&fixture, &original_trigger).await;

            admin(
                &fixture,
                &format!("ALTER TABLE {schema}.{table} DISABLE TRIGGER {trigger}"),
            )
            .await;
            assert_v1028_catalog_drift(&fixture, stage, &format!("disabled {trigger}")).await;
            admin(
                &fixture,
                &format!("ALTER TABLE {schema}.{table} ENABLE TRIGGER {trigger}"),
            )
            .await;

            let function_name = format!("{schema}.{function}()");
            let original_function: String = sqlx::query_scalar(
                "SELECT pg_catalog.pg_get_functiondef($1::pg_catalog.regprocedure)",
            )
            .bind(&function_name)
            .fetch_one(&fixture.admin)
            .await
            .expect("original function definition");
            let altered = original_function.replacen("BEGIN", "BEGIN\n    -- catalog drift", 1);
            assert_ne!(original_function, altered);
            admin(&fixture, &altered).await;
            assert_v1028_catalog_drift(&fixture, stage, &format!("changed {function}")).await;
            admin(&fixture, &original_function).await;
        }

        for table in [
            "ingress_messages",
            "ingress_owner_rejections",
            "ingress_deliveries",
        ] {
            admin(&fixture, &format!("CREATE TRIGGER unrelated BEFORE UPDATE ON {schema}.{table} FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_source_binding_guard()" )).await;
            assert_v1028_catalog_drift(&fixture, stage, &format!("extra trigger on {table}")).await;
            admin(
                &fixture,
                &format!("DROP TRIGGER unrelated ON {schema}.{table}"),
            )
            .await;
        }

        // Preserve the count and name while altering the trigger's definition.
        for (event, condition, function, case) in [
            (
                "UPDATE",
                "",
                "waddle_ingress_source_insert_guard",
                "wrong function",
            ),
            (
                "INSERT",
                "",
                "waddle_ingress_source_binding_guard",
                "wrong event",
            ),
            (
                "UPDATE",
                "WHEN (false)",
                "waddle_ingress_source_binding_guard",
                "conditional trigger",
            ),
        ] {
            admin(&fixture, &format!("DROP TRIGGER ingress_messages_source_binding_guard ON {schema}.ingress_messages")).await;
            admin(&fixture, &format!("CREATE TRIGGER ingress_messages_source_binding_guard BEFORE {event} ON {schema}.ingress_messages FOR EACH ROW {condition} EXECUTE FUNCTION {schema}.{function}()")).await;
            assert_v1028_catalog_drift(&fixture, stage, case).await;
            admin(&fixture, &format!("DROP TRIGGER ingress_messages_source_binding_guard ON {schema}.ingress_messages")).await;
            admin(&fixture, &format!("CREATE TRIGGER ingress_messages_source_binding_guard BEFORE UPDATE ON {schema}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_source_binding_guard()")).await;
        }

        // An unrelated trigger cannot make up for a missing epoch guard.
        admin(
            &fixture,
            &format!("DROP TRIGGER ingress_messages_epoch_guard_dml ON {schema}.ingress_messages"),
        )
        .await;
        admin(&fixture, &format!("CREATE TRIGGER unrelated BEFORE UPDATE ON {schema}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_source_binding_guard()")).await;
        assert_v1028_catalog_drift(
            &fixture,
            stage,
            "missing epoch guard masked by extra trigger",
        )
        .await;
        admin(
            &fixture,
            &format!("DROP TRIGGER unrelated ON {schema}.ingress_messages"),
        )
        .await;
        admin(&fixture, &format!("CREATE TRIGGER ingress_messages_epoch_guard_dml BEFORE INSERT OR UPDATE OR DELETE ON {schema}.ingress_messages FOR EACH STATEMENT EXECUTE FUNCTION {schema}.waddle_ingress_epoch_guard()")).await;
        admin(&fixture, &format!("ALTER TABLE {schema}.ingress_messages ENABLE ALWAYS TRIGGER ingress_messages_epoch_guard_dml")).await;

        // Swap two names between tables, preserving both per-table counts.
        admin(
            &fixture,
            &format!(
                "DROP TRIGGER ingress_messages_source_binding_guard ON {schema}.ingress_messages"
            ),
        )
        .await;
        admin(&fixture, &format!("DROP TRIGGER ingress_owner_rejections_update_guard ON {schema}.ingress_owner_rejections")).await;
        admin(&fixture, &format!("CREATE TRIGGER ingress_messages_source_binding_guard BEFORE UPDATE ON {schema}.ingress_owner_rejections FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_source_binding_guard()")).await;
        admin(&fixture, &format!("CREATE TRIGGER ingress_owner_rejections_update_guard BEFORE UPDATE ON {schema}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_owner_rejection_immutable()")).await;
        assert_v1028_catalog_drift(&fixture, stage, "triggers moved to wrong tables").await;
        admin(&fixture, &format!("DROP TRIGGER ingress_messages_source_binding_guard ON {schema}.ingress_owner_rejections")).await;
        admin(
            &fixture,
            &format!(
                "DROP TRIGGER ingress_owner_rejections_update_guard ON {schema}.ingress_messages"
            ),
        )
        .await;
        admin(&fixture, &format!("CREATE TRIGGER ingress_messages_source_binding_guard BEFORE UPDATE ON {schema}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_source_binding_guard()")).await;
        admin(&fixture, &format!("CREATE TRIGGER ingress_owner_rejections_update_guard BEFORE UPDATE ON {schema}.ingress_owner_rejections FOR EACH ROW EXECUTE FUNCTION {schema}.waddle_ingress_owner_rejection_immutable()")).await;
        assert_eq!(
            inspect(&fixture).await.expect("restored trigger set"),
            stage
        );
    }
    fixture.cleanup().await;
}

async fn assert_v1028_catalog_drift(fixture: &Fixture, stage: DarkEpochAclStage, case: &str) {
    let result = inspect(fixture).await;
    assert!(
        matches!(result, Err(DarkMigrationOwnerError::EpochCatalogDrift)),
        "{case} at {stage:?}: {result:?}"
    );
}

async fn install_relation_guard_stage(fixture: &Fixture) {
    let mut installer = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("candidate installer");
    sqlx::raw_sql(include_str!(
        "../../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&mut installer)
    .await
    .expect("install unregistered candidate on disposable fixture");
    drop(installer);
    admin(
        fixture,
        &format!(
            "REVOKE UPDATE (id) ON {}.ingress_protocol_epoch FROM {}",
            fixture.schema, fixture.runtime
        ),
    )
    .await;
}

#[tokio::test]
async fn runtime_replication_role_and_direct_set_privilege_are_rejected() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    prepare(&fixture).await;
    grant_id(&fixture).await;

    for stage in [
        DarkEpochAclStage::V1009EpochZero,
        DarkEpochAclStage::RelationGuardEpochZero,
    ] {
        if stage == DarkEpochAclStage::RelationGuardEpochZero {
            install_relation_guard_stage(&fixture).await;
        }
        assert_eq!(inspect(&fixture).await.expect("baseline"), stage);

        // The administrator can configure a replica default even though the
        // runtime has no privilege to change the setting itself.
        admin(
            &fixture,
            &format!(
                "ALTER ROLE {} SET session_replication_role = replica",
                fixture.runtime
            ),
        )
        .await;
        let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
            .await
            .expect("runtime with replica default");
        let (replication_role, can_set): (String, bool) = sqlx::query_as(
            "SELECT pg_catalog.current_setting('session_replication_role'), \
             pg_catalog.has_parameter_privilege(current_user, 'session_replication_role', 'SET')",
        )
        .fetch_one(&mut runtime)
        .await
        .expect("observe replica default and privilege");
        assert_eq!(replication_role, "replica");
        assert!(!can_set);
        let error = sqlx::query("SET session_replication_role = origin")
            .execute(&mut runtime)
            .await
            .expect_err("runtime cannot change parameter");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("42501")
        );
        drop(runtime);
        let replica_result = inspect(&fixture).await;
        admin(
            &fixture,
            &format!(
                "ALTER ROLE {} RESET session_replication_role",
                fixture.runtime
            ),
        )
        .await;
        assert!(
            matches!(
                replica_result,
                Err(DarkMigrationOwnerError::RuntimeSessionUnverified)
            ),
            "{replica_result:?}"
        );
        assert_eq!(inspect(&fixture).await.expect("restored origin"), stage);

        // Parameter ACLs are cluster-wide. Verify PostgreSQL's PUBLIC-grant
        // semantics inside an uncommitted transaction so other sessions never
        // receive the privilege. The direct grant below exercises the full
        // diagnostic across its separate connections.
        let mut public_grant = fixture
            .admin
            .begin()
            .await
            .expect("PUBLIC probe transaction");
        sqlx::query("GRANT SET ON PARAMETER session_replication_role TO PUBLIC")
            .execute(&mut *public_grant)
            .await
            .expect("transactional PUBLIC grant");
        let privilege_query = "SELECT pg_catalog.has_parameter_privilege($1::pg_catalog.name, 'session_replication_role', 'SET')";
        let visible_inside: bool = sqlx::query_scalar(privilege_query)
            .bind(&fixture.runtime)
            .fetch_one(&mut *public_grant)
            .await
            .expect("PUBLIC grant in transaction");
        let visible_outside: bool = sqlx::query_scalar(privilege_query)
            .bind(&fixture.runtime)
            .fetch_one(&fixture.admin)
            .await
            .expect("PUBLIC grant outside transaction");
        public_grant
            .rollback()
            .await
            .expect("rollback PUBLIC grant");
        assert!(
            visible_inside,
            "PUBLIC grant must be effective within the transaction"
        );
        assert!(
            !visible_outside,
            "uncommitted PUBLIC grant must not affect other sessions"
        );

        admin(
            &fixture,
            &format!(
                "GRANT SET ON PARAMETER session_replication_role TO {}",
                fixture.runtime
            ),
        )
        .await;
        let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
            .await
            .expect("runtime with direct SET privilege");
        let replication_role: String =
            sqlx::query_scalar("SELECT pg_catalog.current_setting('session_replication_role')")
                .fetch_one(&mut runtime)
                .await
                .expect("initial replication role");
        assert_eq!(replication_role, "origin");
        sqlx::query("SET session_replication_role = replica")
            .execute(&mut runtime)
            .await
            .expect("direct SET privilege works");
        drop(runtime);
        let granted_result = inspect(&fixture).await;
        admin(
            &fixture,
            &format!(
                "REVOKE SET ON PARAMETER session_replication_role FROM {}",
                fixture.runtime
            ),
        )
        .await;
        assert!(
            matches!(granted_result, Err(DarkMigrationOwnerError::Roles)),
            "{granted_result:?}"
        );
        assert_eq!(inspect(&fixture).await.expect("revoked SET"), stage);
    }
    fixture.cleanup().await;
}
