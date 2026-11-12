//! Real PostgreSQL characterization for the unwired migration-owner lab.

#[path = "../src/test_support/postgres_proxy.rs"]
pub mod postgres_proxy;

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AttestedLineage, DeploymentUuid};
use waddle_server::db::physical_attestation::PgRoleName;
use waddle_server::db::{
    run_dark_postgres_owner, DarkMigrationOwnerConfig, DarkMigrationOwnerError, Database,
    DatabaseConfig, DatabaseDriver, MigrationRunner,
};

#[path = "migration_owner_lab/catalog_fence.rs"]
mod catalog_fence;
#[path = "migration_owner_lab/epoch_acl.rs"]
mod epoch_acl;
#[cfg(feature = "test-support")]
#[path = "migration_owner_lab/mam.rs"]
mod mam;
#[path = "migration_owner_lab/serving_role_graph.rs"]
mod serving_role_graph;
#[cfg(feature = "test-support")]
#[path = "migration_owner_lab/transition.rs"]
mod transition;

struct Fixture {
    cluster: PgPool,
    admin: PgPool,
    admin_options: PgConnectOptions,
    owner_options: PgConnectOptions,
    migrator_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    database: String,
    schema: String,
    owner: String,
    migrator: String,
    runtime: String,
    lineage: AttestedLineage,
}

impl Fixture {
    async fn new(url: &str, migrate: bool) -> Self {
        let cluster = PgPool::connect(url).await.expect("cluster administrator");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("migration_lab_db_{suffix}");
        let schema = format!("migration_lab_s_{suffix}");
        let owner = format!("migration_lab_o_{suffix}");
        let migrator = format!("migration_lab_m_{suffix}");
        let runtime = format!("migration_lab_r_{suffix}");
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&cluster)
            .await
            .expect("create isolated database");
        for role in [&owner, &migrator, &runtime] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
                .execute(&cluster).await.expect("create separated login");
        }
        let base = url_with_database(url, &database);
        let admin = PgPool::connect(&base)
            .await
            .expect("database administrator");
        for statement in [
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
            format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}"),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("provision lab roles");
        }
        let scoped = url_with_option(&base, "options", &format!("-c search_path={schema}"));
        let owner_url = url_with_option(&scoped, "user", &owner);
        let admin_options = PgConnectOptions::from_str(&scoped).expect("admin options");
        let owner_options = PgConnectOptions::from_str(&owner_url).expect("owner options");
        let migrator_options = owner_options.clone().username(&migrator);
        let runtime_options = owner_options.clone().username(&runtime);
        let db = Database::from_config(
            "migration owner fixture",
            &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url),
        )
        .await
        .expect("owner database");
        let lineage = lineage::enroll(
            &db,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await
        .expect("enroll owner lineage");
        if migrate {
            MigrationRunner::single()
                .run(&db)
                .await
                .expect("baseline actual migrations");
        }
        drop(db);
        Self {
            cluster,
            admin,
            admin_options,
            owner_options,
            migrator_options,
            runtime_options,
            database,
            schema,
            owner,
            migrator,
            runtime,
            lineage,
        }
    }

    fn config(&self) -> DarkMigrationOwnerConfig {
        DarkMigrationOwnerConfig::new(
            self.lineage.clone(),
            PgRoleName::new(&self.owner).expect("owner name"),
            PgRoleName::new(&self.runtime).expect("runtime name"),
        )
        .expect("owner configuration")
    }

    fn migrator_role(&self) -> PgRoleName {
        PgRoleName::new(&self.migrator).expect("migrator name")
    }

    async fn enable_owner_handoff(&self) {
        for statement in [
            format!("REVOKE TEMP ON DATABASE {} FROM PUBLIC", self.database),
            format!("ALTER ROLE {} NOLOGIN", self.owner),
            format!(
                "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
                self.owner, self.migrator
            ),
        ] {
            sqlx::query(&statement)
                .execute(&self.admin)
                .await
                .expect("provision three-role handoff");
        }
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster)
            .await
            .expect("drop test database");
        for role in [&self.runtime, &self.migrator, &self.owner] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.cluster)
                .await
                .expect("drop test role");
        }
        self.cluster.close().await;
    }
}

fn url_with_database(base: &str, database: &str) -> String {
    url_with_option(base, "dbname", database)
}

fn url_with_option(base: &str, key: &str, value: &str) -> String {
    let mut url = url::Url::parse(base).expect("test PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(existing, _)| existing != key)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(k, v)| (k, v)))
        .append_pair(key, value);
    url.to_string()
}

async fn blocked_by(pool: &PgPool, waiting_pid: i32, blocker_pid: i32) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool =
                sqlx::query_scalar("SELECT $1 = ANY(pg_catalog.pg_blocking_pids($2))")
                    .bind(blocker_pid)
                    .bind(waiting_pid)
                    .fetch_one(pool)
                    .await
                    .expect("inspect wait graph");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected PostgreSQL wait edge");
}

async fn wait_for_application(pool: &PgPool, application: &str, blocker_pid: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND $2 = ANY(pg_catalog.pg_blocking_pids(pid))",
            )
            .bind(application)
            .bind(blocker_pid)
            .fetch_optional(pool)
            .await
            .expect("find owner wait");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner blocked on ledger")
}

/// Return a fully migrated fixture to its coherent V1027 state so V1028 onward
/// is pending. V1027's ACK table and ledger row stay intact.
async fn make_v1028_pending(fixture: &Fixture) {
    let mut tx = fixture
        .admin
        .begin()
        .await
        .expect("begin V1028 fixture rollback");
    let schema = &fixture.schema;
    let statements = [
        format!(
            "ALTER TABLE {schema}.ingress_epoch_guard_manifest DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row"
        ),
        format!(
            "DELETE FROM {schema}.ingress_epoch_guard_manifest WHERE table_name IN ('ingress_mailbox_lanes', 'ingress_mailbox_entries', 'ingress_room_requests', 'ingress_room_responses', 'ingress_room_occupants')"
        ),
        format!(
            "ALTER TABLE {schema}.ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row"
        ),
        format!("DROP TABLE {schema}.ingress_room_occupants"),
        format!("DROP TABLE {schema}.ingress_room_responses"),
        format!("DROP TABLE {schema}.ingress_mailbox_entries"),
        format!("DROP TABLE {schema}.ingress_mailbox_lanes"),
        format!("DROP TABLE {schema}.ingress_room_requests"),
        format!(
            "DROP FUNCTION {schema}.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA)"
        ),
        format!("DROP TABLE {schema}.p21_receipt_operation_results"),
        format!("ALTER TABLE {schema}.sm_unacked DROP COLUMN provenance"),
        format!(
            "DROP TRIGGER ingress_messages_source_binding_guard ON {schema}.ingress_messages"
        ),
        format!(
            "DROP TRIGGER ingress_messages_source_insert_guard ON {schema}.ingress_messages"
        ),
        format!(
            "DROP TRIGGER ingress_owner_rejections_update_guard ON {schema}.ingress_owner_rejections"
        ),
        format!(
            "DROP TRIGGER ingress_owner_rejections_insert_guard ON {schema}.ingress_owner_rejections"
        ),
        format!(
            "DROP TRIGGER ingress_owner_rejections_delete_guard ON {schema}.ingress_owner_rejections"
        ),
        format!(
            "DROP TRIGGER ingress_owner_rejections_epoch_guard_dml ON {schema}.ingress_owner_rejections"
        ),
        format!(
            "DROP TRIGGER ingress_owner_rejections_epoch_guard_truncate ON {schema}.ingress_owner_rejections"
        ),
        format!("DROP FUNCTION {schema}.waddle_ingress_source_binding_guard()"),
        format!("DROP FUNCTION {schema}.waddle_ingress_source_insert_guard()"),
        format!("DROP FUNCTION {schema}.waddle_ingress_owner_rejection_immutable()"),
        format!("DROP FUNCTION {schema}.waddle_ingress_owner_rejection_insert_guard()"),
        format!("DROP FUNCTION {schema}.waddle_ingress_owner_rejection_delete_guard()"),
        format!(
            "ALTER TABLE {schema}.ingress_epoch_guard_manifest DISABLE TRIGGER ingress_epoch_guard_manifest_append_only_row"
        ),
        format!(
            "DELETE FROM {schema}.ingress_epoch_guard_manifest WHERE table_name = 'ingress_owner_rejections'"
        ),
        format!(
            "ALTER TABLE {schema}.ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row"
        ),
        format!("DROP TABLE {schema}.ingress_owner_rejections"),
        format!(
            "ALTER TABLE {schema}.ingress_messages DROP CONSTRAINT ingress_messages_source_binding_check"
        ),
        format!(
            "ALTER TABLE {schema}.ingress_messages DROP CONSTRAINT ingress_messages_owner_decision_check"
        ),
        format!(
            "ALTER TABLE {schema}.ingress_messages DROP COLUMN source_binding_version, DROP COLUMN source_bare_jid, DROP COLUMN source_auth_context_id, DROP COLUMN source_auth_context_version, DROP COLUMN source_auth_epoch, DROP COLUMN source_sm_ingress_id, DROP COLUMN source_ingress_ordinal, DROP COLUMN owner_decision"
        ),
        format!("DELETE FROM {schema}._migrations WHERE version >= 1028"),
    ];
    for statement in statements {
        sqlx::query(&statement)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|error| panic!("revert V1028 fixture state: {error}"));
    }
    tx.commit()
        .await
        .expect("commit coherent V1027 fixture state");
}

#[tokio::test]
async fn v1009_epoch_id_grant_requires_exact_guard_and_valid_pending_ledger() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    fixture.enable_owner_handoff().await;
    make_v1028_pending(&fixture).await;
    let epoch = format!("{}.ingress_protocol_epoch", fixture.schema);
    async fn run(fixture: &Fixture) -> Result<Vec<i64>, DarkMigrationOwnerError> {
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role(),
        )
        .await
    }
    for statement in [
        format!("GRANT SELECT ON {epoch} TO {}", fixture.runtime),
        format!("GRANT UPDATE (id) ON {epoch} TO {}", fixture.runtime),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("V1009 scoped grant");
    }

    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("restricted login");
    let mut row_lock = runtime.begin().await.expect("row lock transaction");
    sqlx::query(&format!("SELECT id FROM {epoch} WHERE id = 1 FOR SHARE"))
        .execute(&mut *row_lock)
        .await
        .expect("id update permits FOR SHARE");
    row_lock.rollback().await.expect("release row lock");
    let mut exclusive = runtime.begin().await.expect("exclusive lock transaction");
    assert!(
        sqlx::query(&format!("LOCK TABLE {epoch} IN ACCESS EXCLUSIVE MODE"))
            .execute(&mut *exclusive)
            .await
            .is_err(),
        "column UPDATE must not permit ACCESS EXCLUSIVE"
    );
    exclusive
        .rollback()
        .await
        .expect("release failed transaction");
    assert!(
        sqlx::query(&format!("UPDATE {epoch} SET epoch = 1 WHERE id = 1"))
            .execute(&mut runtime)
            .await
            .is_err(),
        "runtime cannot update epoch"
    );
    assert!(
        sqlx::query(&format!("UPDATE {epoch} SET id = id WHERE id = 1"))
            .execute(&mut runtime)
            .await
            .is_err(),
        "V1009 trigger rejects id mutation"
    );
    drop(runtime);

    let disabled = format!("ALTER TABLE {epoch} DISABLE TRIGGER ingress_protocol_epoch_guard_row");
    sqlx::query(&disabled)
        .execute(&fixture.admin)
        .await
        .expect("disable guard");
    assert!(matches!(
        run(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    sqlx::query(&format!(
        "ALTER TABLE {epoch} ENABLE ALWAYS TRIGGER ingress_protocol_epoch_guard_row"
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore guard");

    let original: String =
        sqlx::query_scalar("SELECT pg_catalog.pg_get_functiondef($1::regprocedure)")
            .bind(format!(
                "{}.waddle_ingress_protocol_epoch_guard()",
                fixture.schema
            ))
            .fetch_one(&fixture.admin)
            .await
            .expect("original guard definition");
    sqlx::query(&format!("CREATE OR REPLACE FUNCTION {}.waddle_ingress_protocol_epoch_guard() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$", fixture.schema))
        .execute(&fixture.admin).await.expect("tamper guard body");
    assert!(matches!(
        run(&fixture).await,
        Err(DarkMigrationOwnerError::EpochCatalogDrift)
    ));
    sqlx::query(&original)
        .execute(&fixture.admin)
        .await
        .expect("restore guard body");

    let runtime_role = &fixture.runtime;
    for (grant, revoke) in [
        (
            format!("GRANT UPDATE ON {epoch} TO {runtime_role}"),
            format!("REVOKE UPDATE ON {epoch} FROM {runtime_role}"),
        ),
        (
            format!("GRANT UPDATE (epoch) ON {epoch} TO {runtime_role}"),
            format!("REVOKE UPDATE (epoch) ON {epoch} FROM {runtime_role}"),
        ),
        (
            format!("GRANT UPDATE (id) ON {epoch} TO {runtime_role} WITH GRANT OPTION"),
            format!("REVOKE UPDATE (id) ON {epoch} FROM {runtime_role}"),
        ),
        (
            format!("GRANT UPDATE (id) ON {epoch} TO PUBLIC"),
            format!("REVOKE UPDATE (id) ON {epoch} FROM PUBLIC"),
        ),
        (
            format!("GRANT SELECT ON {epoch} TO PUBLIC"),
            format!("REVOKE SELECT ON {epoch} FROM PUBLIC"),
        ),
        (
            format!("GRANT SELECT ON {epoch} TO {runtime_role} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR SELECT ON {epoch} FROM {runtime_role}"),
        ),
        (
            format!(
                "GRANT CREATE ON SCHEMA {} TO {runtime_role}",
                fixture.schema
            ),
            format!(
                "REVOKE CREATE ON SCHEMA {} FROM {runtime_role}",
                fixture.schema
            ),
        ),
        (
            format!(
                "GRANT TEMP ON DATABASE {} TO {runtime_role}",
                fixture.database
            ),
            format!(
                "REVOKE TEMP ON DATABASE {} FROM {runtime_role}",
                fixture.database
            ),
        ),
    ] {
        sqlx::query(&grant)
            .execute(&fixture.admin)
            .await
            .expect("install unsafe grant");
        assert!(run(&fixture).await.is_err(), "accepted {grant}");
        sqlx::query(&revoke)
            .execute(&fixture.admin)
            .await
            .expect("remove unsafe grant");
        sqlx::query(&format!("GRANT UPDATE (id) ON {epoch} TO {runtime_role}"))
            .execute(&fixture.admin)
            .await
            .expect("restore scoped update");
    }

    let delegated = format!("{}_delegate", fixture.runtime);
    sqlx::query(&format!("CREATE ROLE {delegated} NOLOGIN"))
        .execute(&fixture.admin)
        .await
        .expect("create delegated role");
    sqlx::query(&format!(
        "GRANT {delegated} TO {runtime_role} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant SET ROLE");
    assert!(run(&fixture).await.is_err(), "runtime role escalation");
    sqlx::query(&format!("REVOKE {delegated} FROM {runtime_role}"))
        .execute(&fixture.admin)
        .await
        .expect("revoke SET ROLE");
    sqlx::query(&format!("DROP ROLE {delegated}"))
        .execute(&fixture.admin)
        .await
        .expect("drop delegated role");

    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&fixture.admin)
        .await
        .expect("server version");
    if version >= 170000 {
        for with_id in [false, true] {
            let verb = if with_id { "GRANT" } else { "REVOKE" };
            let direction = if with_id { "TO" } else { "FROM" };
            sqlx::query(&format!(
                "{verb} UPDATE (id) ON {epoch} {direction} {runtime_role}"
            ))
            .execute(&fixture.admin)
            .await
            .expect("configure scoped UPDATE");
            for table in [&epoch, &format!("{}._migrations", fixture.schema)] {
                sqlx::query(&format!("GRANT MAINTAIN ON {table} TO {runtime_role}"))
                    .execute(&fixture.admin)
                    .await
                    .expect("grant MAINTAIN");
                assert!(
                    matches!(run(&fixture).await, Err(DarkMigrationOwnerError::Roles)),
                    "accepted MAINTAIN on {table}"
                );
                sqlx::query(&format!("REVOKE MAINTAIN ON {table} FROM {runtime_role}"))
                    .execute(&fixture.admin)
                    .await
                    .expect("revoke MAINTAIN");
            }
        }
    }

    let checksum: String = sqlx::query_scalar(&format!(
        "SELECT checksum FROM {}._migrations WHERE version = 1009",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("original V1009 checksum");
    sqlx::query(&format!(
        "UPDATE {}._migrations SET checksum = 'tampered' WHERE version = 1009",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("tamper V1009 ledger");
    assert!(run(&fixture).await.is_err(), "unchecked V1009 ledger");
    sqlx::query(&format!(
        "UPDATE {}._migrations SET checksum = $1 WHERE version = 1009",
        fixture.schema
    ))
    .bind(checksum)
    .execute(&fixture.admin)
    .await
    .expect("restore V1009 ledger");
    assert_eq!(
        run(&fixture)
            .await
            .expect("pending catalog from V1028 with scoped V1009 ACL"),
        vec![1028, 1029, 1030, 1031, 1032]
    );
    sqlx::query(&format!(
        "UPDATE {epoch} SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1"
    ))
    .bind(fixture.lineage.lineage_uuid.0)
    .execute(&fixture.admin)
    .await
    .expect("advance epoch");
    assert!(matches!(
        run(&fixture).await,
        Err(DarkMigrationOwnerError::UnsupportedEpoch)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn owner_uses_one_session_takes_both_epoch_locks_and_applies_pending_catalog() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let mut hostile = PgConnection::connect_with(&fixture.owner_options)
        .await
        .expect("hostile session");
    sqlx::query("CREATE TEMP TABLE _migrations (version BIGINT PRIMARY KEY, description TEXT, checksum TEXT)")
        .execute(&mut hostile).await.expect("temporary ledger");
    sqlx::query("INSERT INTO _migrations VALUES (999999, 'temporary decoy', 'bad')")
        .execute(&mut hostile)
        .await
        .expect("temporary decoy row");
    let contaminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_my_temp_schema() <> 0")
        .fetch_one(&mut hostile)
        .await
        .expect("detect backend TEMP namespace");
    assert!(contaminated);
    // Prove PostgreSQL lookup order in an actually contaminated session.
    let mut path_tx = hostile.begin().await.expect("path transaction");
    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(format!("\"{}\", pg_temp", fixture.schema))
        .execute(&mut *path_tx)
        .await
        .expect("trusted path");
    let (schema, real_ledger): (String, i64) = sqlx::query_as(
        "SELECT pg_catalog.current_schema()::text, count(*) FROM _migrations WHERE version = 1008",
    )
    .fetch_one(&mut *path_tx)
    .await
    .expect("trusted lookup");
    assert_eq!(schema, fixture.schema);
    assert_eq!(real_ledger, 1);
    path_tx.rollback().await.expect("reset path");

    fixture.enable_owner_handoff().await;
    make_v1028_pending(&fixture).await;
    let mut blocker = fixture.admin.begin().await.expect("ledger blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}._migrations IN ROW EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *blocker)
    .await
    .expect("hold ledger DDL");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .expect("blocker PID");
    let application = format!("migration_owner_{}", uuid::Uuid::new_v4().simple());
    let options = fixture
        .migrator_options
        .clone()
        .application_name(&application)
        .options([("search_path", "pg_temp,public")]);
    let config = fixture.config();
    let observe = async {
        let pid = wait_for_application(&fixture.admin, &application, blocker_pid).await;
        let session_login: String = sqlx::query_scalar(
            "SELECT usename::text FROM pg_catalog.pg_stat_activity WHERE pid = $1",
        )
        .bind(pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect migration session login");
        assert_eq!(session_login, fixture.migrator);
        let (exclusive, share): (bool, bool) = sqlx::query_as(
            "SELECT COALESCE(bool_or(mode = 'AccessExclusiveLock'), false), \
                    COALESCE(bool_or(mode = 'AccessShareLock'), false) \
             FROM pg_catalog.pg_locks WHERE pid = $1 AND relation = $2::pg_catalog.regclass AND granted",
        ).bind(pid).bind(format!("{}.ingress_protocol_epoch", fixture.schema))
            .fetch_one(&fixture.admin).await.expect("observe same session's epoch locks");
        assert!(exclusive && share, "owner needs both recorded lock modes");
        blocker.rollback().await.expect("release ledger");
    };
    let migrator = fixture.migrator_role();
    let (result, ()) = tokio::join!(
        run_dark_postgres_owner(&options, &config, &migrator),
        observe
    );
    assert_eq!(
        result.expect("owner commits"),
        vec![1028, 1029, 1030, 1031, 1032]
    );
    let (v1027, v1028, ack_table, rejection_table): (i64, i64, bool, bool) =
        sqlx::query_as(&format!(
            "SELECT (SELECT count(*) FROM {}._migrations WHERE version = 1027), \
         (SELECT count(*) FROM {}._migrations WHERE version = 1028), \
         pg_catalog.to_regclass($1) IS NOT NULL, pg_catalog.to_regclass($2) IS NOT NULL",
            fixture.schema, fixture.schema,
        ))
        .bind(format!("{}.sm_acknowledged_windows", fixture.schema))
        .bind(format!("{}.ingress_owner_rejections", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("committed catalog");
    assert_eq!((v1027, v1028), (1, 1));
    assert!(ack_table, "V1027 schema remains installed");
    assert!(
        rejection_table,
        "owner committed the pending V1028 relation"
    );
    let relation_owner: String = sqlx::query_scalar(
        "SELECT pg_catalog.pg_get_userbyid(c.relowner)::text \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = 'ingress_owner_rejections'",
    )
    .bind(&fixture.schema)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect migrated relation owner");
    assert_eq!(relation_owner, fixture.owner);
    let epoch: i64 = sqlx::query_scalar(&format!(
        "SELECT epoch FROM {}.ingress_protocol_epoch WHERE id = 1",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect unchanged epoch");
    assert_eq!(epoch, 0);
    let decoy: i64 = sqlx::query_scalar("SELECT count(*) FROM _migrations WHERE version = 999999")
        .fetch_one(&mut hostile)
        .await
        .expect("independent TEMP decoy remains");
    assert_eq!(decoy, 1);
    drop(hostile);

    sqlx::query(&format!(
        "GRANT SET ON PARAMETER session_replication_role TO {}",
        fixture.migrator
    ))
    .execute(&fixture.admin)
    .await
    .expect("model unsafe owner parameter grant");
    let replica_options = fixture
        .migrator_options
        .clone()
        .options([("session_replication_role", "replica")]);
    assert!(matches!(
        run_dark_postgres_owner(
            &replica_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::ContaminatedSession)
    ));
    sqlx::query(&format!(
        "REVOKE SET ON PARAMETER session_replication_role FROM {}",
        fixture.migrator
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke test parameter grant");

    for (table, privilege) in [
        ("ingress_protocol_epoch", "UPDATE"),
        ("_migrations", "INSERT"),
    ] {
        sqlx::query(&format!(
            "GRANT {privilege} ON {}.{table} TO {}",
            fixture.schema, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("model unsafe runtime grant");
        assert!(matches!(
            run_dark_postgres_owner(
                &fixture.migrator_options,
                &fixture.config(),
                &fixture.migrator_role()
            )
            .await,
            Err(DarkMigrationOwnerError::Roles)
        ));
        sqlx::query(&format!(
            "REVOKE {privilege} ON {}.{table} FROM {}",
            fixture.schema, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("remove unsafe runtime grant");
    }

    for (table, grant) in [
        ("ingress_protocol_epoch", "UPDATE (id)"),
        (
            "ingress_protocol_epoch",
            "UPDATE (epoch, activated_at, lineage_uuid)",
        ),
        ("_migrations", "INSERT (version)"),
    ] {
        sqlx::query(&format!(
            "GRANT {grant} ON {}.{table} TO {}",
            fixture.schema, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("model unsafe column grant");
        assert!(matches!(
            run_dark_postgres_owner(
                &fixture.migrator_options,
                &fixture.config(),
                &fixture.migrator_role()
            )
            .await,
            Err(DarkMigrationOwnerError::Roles)
        ));
        sqlx::query(&format!(
            "REVOKE {grant} ON {}.{table} FROM {}",
            fixture.schema, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("remove unsafe column grant");
    }

    // A conflicting epoch holder times out before any ledger bootstrap or
    // catalog application, leaving the intentionally pending catalog intact.
    make_v1028_pending(&fixture).await;
    let mut epoch_holder = fixture.admin.begin().await.expect("epoch holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *epoch_holder)
    .await
    .expect("hold epoch transition");
    let blocked = tokio::time::timeout(
        Duration::from_secs(6),
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role(),
        ),
    )
    .await
    .expect("server lock timeout bounds owner");
    assert!(blocked.is_err());
    epoch_holder.rollback().await.expect("release epoch");
    let (v1027, ledger, ack_table, rejection_table): (i64, i64, bool, bool) =
        sqlx::query_as(&format!(
            "SELECT (SELECT count(*) FROM {}._migrations WHERE version = 1027), \
         (SELECT count(*) FROM {}._migrations WHERE version = 1028), \
         pg_catalog.to_regclass($1) IS NOT NULL, pg_catalog.to_regclass($2) IS NOT NULL",
            fixture.schema, fixture.schema,
        ))
        .bind(format!("{}.sm_acknowledged_windows", fixture.schema))
        .bind(format!("{}.ingress_owner_rejections", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("read timed-out state");
    assert_eq!((v1027, ledger), (1, 0));
    assert!(ack_table, "timeout preserves V1027 schema");
    assert!(!rejection_table, "timeout leaves V1028 unapplied");

    // Only this disposable fixture advances: the binary accepts a supported
    // nonzero epoch and refuses the first epoch past the supported one before
    // applying the catalog.
    let update_epoch = format!("UPDATE {}.ingress_protocol_epoch SET epoch = $1, activated_at = now(), lineage_uuid = $2 WHERE id = 1", fixture.schema);
    sqlx::query(&update_epoch)
        .bind(1_i64)
        .bind(fixture.lineage.lineage_uuid.0)
        .execute(&fixture.admin)
        .await
        .expect("model supported epoch");
    assert_eq!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await
        .expect("apply at supported epoch"),
        vec![1028, 1029, 1030, 1031, 1032]
    );
    make_v1028_pending(&fixture).await;
    for epoch in [2_i64, 3] {
        sqlx::query(&update_epoch)
            .bind(epoch)
            .bind(fixture.lineage.lineage_uuid.0)
            .execute(&fixture.admin)
            .await
            .expect("model future epoch");
    }
    assert!(matches!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::UnsupportedEpoch)
    ));
    let (pending, ack_table, rejection_table): (i64, bool, bool) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {}._migrations WHERE version = 1028), \
         pg_catalog.to_regclass($1) IS NOT NULL, pg_catalog.to_regclass($2) IS NOT NULL",
        fixture.schema
    ))
    .bind(format!("{}.sm_acknowledged_windows", fixture.schema))
    .bind(format!("{}.ingress_owner_rejections", fixture.schema))
    .fetch_one(&fixture.admin)
    .await
    .expect("read future-epoch ledger");
    assert_eq!(pending, 0);
    assert!(ack_table, "future epoch preserves V1027 schema");
    assert!(!rejection_table, "future epoch leaves V1028 pending");
    fixture.cleanup().await;
}

#[tokio::test]
async fn owner_refuses_missing_epoch_wrong_identity_and_runtime_role_escalation() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, false).await;
    fixture.enable_owner_handoff().await;
    assert!(matches!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::MissingEpochBaseline)
    ));
    let mut wrong = fixture.lineage.clone();
    wrong
        .postgres_identity
        .as_mut()
        .expect("postgres identity")
        .schema
        .oid += 1;
    let wrong = DarkMigrationOwnerConfig::new(
        wrong,
        PgRoleName::new(&fixture.owner).expect("owner"),
        PgRoleName::new(&fixture.runtime).expect("runtime"),
    )
    .expect("config");
    assert!(matches!(
        run_dark_postgres_owner(&fixture.migrator_options, &wrong, &fixture.migrator_role()).await,
        Err(DarkMigrationOwnerError::Identity)
    ));
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    assert!(sqlx::query(&format!(
        "CREATE TABLE {}.forbidden (id int)",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    assert!(sqlx::query(&format!(
        "ALTER TABLE {}._lineage ADD COLUMN forbidden int",
        fixture.schema
    ))
    .execute(&mut runtime)
    .await
    .is_err());
    for role in [&fixture.owner, &fixture.migrator] {
        assert!(
            sqlx::query(&format!("SET ROLE {role}"))
                .execute(&mut runtime)
                .await
                .is_err(),
            "runtime must not assume {role}"
        );
    }
    drop(runtime);
    sqlx::query(&format!(
        "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
        fixture.owner, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("model runtime SET ROLE escalation");
    assert!(matches!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.owner, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove test escalation");
    fixture.cleanup().await;
}

#[tokio::test]
async fn owner_handoff_rejects_unsafe_role_graph_before_migration() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    fixture.enable_owner_handoff().await;
    make_v1028_pending(&fixture).await;
    let owner = &fixture.owner;
    let migrator = &fixture.migrator;
    let runtime = &fixture.runtime;
    let database = &fixture.database;
    let schema = &fixture.schema;
    let safe_grant =
        format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE");
    let cases = [
        (
            format!("ALTER ROLE {owner} LOGIN"),
            format!("ALTER ROLE {owner} NOLOGIN"),
        ),
        (
            format!("ALTER ROLE {migrator} CREATEROLE"),
            format!("ALTER ROLE {migrator} NOCREATEROLE"),
        ),
        (
            format!("ALTER ROLE {runtime} BYPASSRLS"),
            format!("ALTER ROLE {runtime} NOBYPASSRLS"),
        ),
        (
            format!("REVOKE {owner} FROM {migrator}"),
            safe_grant.clone(),
        ),
        (
            format!("GRANT {owner} TO {migrator} WITH INHERIT TRUE, SET TRUE, ADMIN FALSE"),
            safe_grant.clone(),
        ),
        (
            format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET FALSE, ADMIN FALSE"),
            safe_grant.clone(),
        ),
        (
            format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN TRUE"),
            safe_grant,
        ),
        (
            format!("GRANT {owner} TO {runtime} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
            format!("REVOKE {owner} FROM {runtime}"),
        ),
        (
            format!("GRANT {migrator} TO {runtime} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
            format!("REVOKE {migrator} FROM {runtime}"),
        ),
        (
            format!("GRANT pg_read_all_stats TO {migrator}"),
            format!("REVOKE pg_read_all_stats FROM {migrator}"),
        ),
        (
            format!("GRANT TEMP ON DATABASE {database} TO {migrator}"),
            format!("REVOKE TEMP ON DATABASE {database} FROM {migrator}"),
        ),
        (
            format!("GRANT TEMP ON DATABASE {database} TO {runtime}"),
            format!("REVOKE TEMP ON DATABASE {database} FROM {runtime}"),
        ),
        (
            format!("GRANT CREATE ON SCHEMA {schema} TO {migrator}"),
            format!("REVOKE CREATE ON SCHEMA {schema} FROM {migrator}"),
        ),
        (
            format!("GRANT CREATE ON SCHEMA {schema} TO {runtime}"),
            format!("REVOKE CREATE ON SCHEMA {schema} FROM {runtime}"),
        ),
    ];
    for (unsafe_change, restore) in cases {
        sqlx::query(&unsafe_change)
            .execute(&fixture.admin)
            .await
            .expect("inject unsafe role graph");
        assert!(
            matches!(
                run_dark_postgres_owner(
                    &fixture.migrator_options,
                    &fixture.config(),
                    &fixture.migrator_role(),
                )
                .await,
                Err(DarkMigrationOwnerError::Roles)
            ),
            "accepted unsafe change: {unsafe_change}"
        );
        let (pending, relation): (i64, bool) = sqlx::query_as(&format!(
            "SELECT (SELECT count(*) FROM {schema}._migrations WHERE version = 1028), \
                    pg_catalog.to_regclass($1) IS NOT NULL"
        ))
        .bind(format!("{schema}.ingress_owner_rejections"))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect refused migration");
        assert_eq!((pending, relation), (0, false), "after {unsafe_change}");
        sqlx::query(&restore)
            .execute(&fixture.admin)
            .await
            .expect("restore role graph");
    }

    // An injected startup role must not substitute for the transaction-local handoff.
    let assumed_at_login = fixture
        .migrator_options
        .clone()
        .options([("role", owner.as_str())]);
    assert!(matches!(
        run_dark_postgres_owner(
            &assumed_at_login,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    assert!(matches!(
        run_dark_postgres_owner(
            &fixture.runtime_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await,
        Err(DarkMigrationOwnerError::Roles)
    ));
    assert_eq!(
        run_dark_postgres_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role()
        )
        .await
        .expect("restored graph applies migration"),
        vec![1028, 1029, 1030, 1031, 1032]
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn old_trigger_can_invert_lock_order_against_an_epoch_first_migrator() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let mut writer = fixture.admin.begin().await.expect("old writer");
    sqlx::query("SET LOCAL deadlock_timeout = '5s'")
        .execute(&mut *writer)
        .await
        .expect("observation window");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_messages IN ROW EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *writer)
    .await
    .expect("old target lock");
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .expect("writer PID");
    let mut owner = fixture.admin.begin().await.expect("migrator");
    sqlx::query("SET LOCAL deadlock_timeout = '5s'")
        .execute(&mut *owner)
        .await
        .expect("observation window");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *owner)
    .await
    .expect("epoch-first owner");
    let owner_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *owner)
        .await
        .expect("owner PID");
    let writer_sql = format!(
        "DELETE FROM {}.ingress_messages WHERE false",
        fixture.schema
    );
    let writer_task = tokio::spawn(async move {
        let result = sqlx::query(&writer_sql).execute(&mut *writer).await;
        writer.rollback().await.expect("rollback writer");
        result
    });
    blocked_by(&fixture.admin, writer_pid, owner_pid).await;
    let owner_sql = format!(
        "ALTER TABLE {}.ingress_messages ADD COLUMN lab_marker int",
        fixture.schema
    );
    let owner_task = tokio::spawn(async move {
        let result = sqlx::query(&owner_sql).execute(&mut *owner).await;
        owner.rollback().await.expect("rollback owner");
        result
    });
    blocked_by(&fixture.admin, owner_pid, writer_pid).await;
    let cancelled: bool = sqlx::query_scalar("SELECT pg_catalog.pg_cancel_backend($1)")
        .bind(owner_pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("cancel test owner");
    assert!(cancelled);
    assert!(owner_task.await.expect("owner task").is_err());
    writer_task
        .await
        .expect("writer task")
        .expect("writer unblocked");
    fixture.cleanup().await;
}

#[tokio::test]
async fn experimental_guard_requires_recorded_access_share_even_with_access_exclusive() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let mut admin = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("admin session");
    sqlx::raw_sql(include_str!(
        "../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql"
    ))
    .execute(&mut admin)
    .await
    .expect("install experimental guard");
    let mut only_exclusive = admin.begin().await.expect("first owner");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *only_exclusive)
    .await
    .expect("exclusive lock");
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut *only_exclusive).await.expect("epoch proof");
    let error = sqlx::query(&format!(
        "DELETE FROM {}.ingress_messages WHERE false",
        fixture.schema
    ))
    .execute(&mut *only_exclusive)
    .await
    .expect_err("exclusive lock alone must fail the candidate guard");
    assert!(error
        .to_string()
        .contains("guarded writes require an epoch ACCESS SHARE prelock"));
    only_exclusive
        .rollback()
        .await
        .expect("rollback rejected write");
    let mut both = admin.begin().await.expect("second owner");
    for mode in ["ACCESS EXCLUSIVE", "ACCESS SHARE"] {
        sqlx::query(&format!(
            "LOCK TABLE ONLY {}.ingress_protocol_epoch IN {mode} MODE",
            fixture.schema
        ))
        .execute(&mut *both)
        .await
        .expect("record lock mode");
    }
    sqlx::query("SELECT pg_catalog.set_config('waddle.protocol_epoch', '0', true), pg_catalog.set_config('waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::text, true)")
        .execute(&mut *both).await.expect("epoch proof");
    sqlx::query(&format!(
        "DELETE FROM {}.ingress_messages WHERE false",
        fixture.schema
    ))
    .execute(&mut *both)
    .await
    .expect("both locks satisfy guard");
    both.rollback().await.expect("rollback lab write");
    drop(admin);
    fixture.cleanup().await;
}

#[tokio::test]
async fn legacy_guard_requires_epoch_update_privilege_that_can_advance_epoch() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    for statement in [
        format!(
            "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
            fixture.schema, fixture.runtime
        ),
        format!(
            "GRANT INSERT ON {}.ingress_messages TO {}",
            fixture.schema, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .expect("runtime write grants");
    }
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    let insert = format!(
        "INSERT INTO {}.ingress_messages (message_key, digest_version, digest) VALUES ($1, 1, $2)",
        fixture.schema,
    );
    let error = sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect_err("SELECT alone cannot satisfy legacy FOR SHARE");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    sqlx::query(&format!(
        "GRANT UPDATE ON {}.ingress_protocol_epoch TO {}",
        fixture.schema, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant legacy row-lock privilege");
    sqlx::query(&insert)
        .bind(uuid::Uuid::new_v4())
        .bind([0_u8; 32].as_slice())
        .execute(&mut runtime)
        .await
        .expect("legacy guarded write now succeeds");
    let update = format!("UPDATE {}.ingress_protocol_epoch SET epoch = 1, activated_at = now(), lineage_uuid = $1 WHERE id = 1", fixture.schema);
    assert_eq!(
        sqlx::query(&update)
            .bind(fixture.lineage.lineage_uuid.0)
            .execute(&mut runtime)
            .await
            .expect("runtime can also advance epoch")
            .rows_affected(),
        1
    );
    drop(runtime);
    fixture.cleanup().await;
}

#[tokio::test]
async fn owner_reports_terminated_commit_as_unknown_without_replaying() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    make_v1028_pending(&fixture).await;
    for statement in [
        format!("CREATE FUNCTION {}.lab_delay_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.pg_sleep(10); RETURN NEW; END $$", fixture.schema),
        format!("CREATE CONSTRAINT TRIGGER lab_delay_commit AFTER INSERT ON {}._migrations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {}.lab_delay_commit()", fixture.schema, fixture.schema),
    ] {
        sqlx::query(&statement).execute(&fixture.admin).await.expect("prepare delayed COMMIT");
    }
    fixture.enable_owner_handoff().await;
    let application = format!("migration_commit_{}", uuid::Uuid::new_v4().simple());
    let options = fixture
        .migrator_options
        .clone()
        .application_name(&application);
    let config = fixture.config();
    let migrator = fixture.migrator_role();
    let terminate = async {
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let found: Option<i32> = sqlx::query_scalar(
                    "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1 \
                     AND wait_event = 'PgSleep' AND query = 'COMMIT'",
                )
                .bind(&application)
                .fetch_optional(&fixture.admin)
                .await
                .expect("find delayed COMMIT");
                if let Some(pid) = found {
                    break pid;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("COMMIT dispatched and delayed");
        let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("terminate committing backend");
        assert!(terminated);
    };
    let (result, ()) = tokio::join!(
        run_dark_postgres_owner(&options, &config, &migrator),
        terminate
    );
    assert!(matches!(
        result,
        Err(DarkMigrationOwnerError::CommitUnknown)
    ));
    let (v1027, ledger, ack_table, rejection_table): (i64, i64, bool, bool) =
        sqlx::query_as(&format!(
            "SELECT (SELECT count(*) FROM {}._migrations WHERE version = 1027), \
         (SELECT count(*) FROM {}._migrations WHERE version = 1028), \
         pg_catalog.to_regclass($1) IS NOT NULL, pg_catalog.to_regclass($2) IS NOT NULL",
            fixture.schema, fixture.schema,
        ))
        .bind(format!("{}.sm_acknowledged_windows", fixture.schema))
        .bind(format!("{}.ingress_owner_rejections", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("observe terminated transaction");
    assert_eq!((v1027, ledger), (1, 0));
    assert!(ack_table, "commit uncertainty preserves V1027 schema");
    assert!(!rejection_table, "rollback leaves V1028 relation absent");
    fixture.cleanup().await;
}

#[tokio::test]
async fn sqlite_runner_still_applies_and_rechecks_the_catalog() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:");
    config.pool_size = 1;
    let db = Database::from_config("sqlite migration regression", &config)
        .await
        .expect("open SQLite");
    let runner = MigrationRunner::single();
    let applied = runner.run(&db).await.expect("apply SQLite catalog");
    assert!(applied.contains(&1008) && applied.contains(&1027) && applied.contains(&1030));
    assert!(runner.run(&db).await.expect("recheck catalog").is_empty());
    let conn = db.guard().await.expect("SQLite guard");
    let mut rows = conn
        .query(
            "SELECT count(*) FROM _migrations WHERE checksum IS NULL",
            (),
        )
        .await
        .expect("check ledger");
    let count: i64 = rows
        .next()
        .await
        .expect("read result")
        .expect("count row")
        .get(0)
        .expect("count");
    assert_eq!(count, 0);
}
