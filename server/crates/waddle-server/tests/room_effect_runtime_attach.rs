//! Restricted PostgreSQL room-effect outbox attachment.
#![cfg(feature = "test-support")]

use sqlx::postgres::{PgConnectOptions, PgPool};
use std::future::Future;
use std::str::FromStr;
use std::time::Duration;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use waddle_server::db::physical_attestation::{
    CATALOG_MUTATION_ADVISORY_LOCK_KEY, PgRoleName, PhysicalAttestationConfig, attested_pool,
    install,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabaseError};
use waddle_server::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use waddle_server::muc_durable::{MucOwnerPreparationError, PostgresMucRoomStore};
use waddle_server::room_effect_outbox::{
    RoomEffectOutboxError, RoomEffectOutboxStore, RoomEffectRuntimeAttachError,
};
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    installer_options: PgConnectOptions,
    runtime_options: PgConnectOptions,
    base_url: String,
    database_name: String,
    lineage: AttestedLineage,
    target: String,
    trusted: String,
    definer: String,
    installer: String,
    runtime: String,
    outsider: String,
    operation_id: uuid::Uuid,
}

impl Fixture {
    async fn new(url: String) -> Self {
        let cluster_admin = PgPool::connect(&url).await.expect("admin PostgreSQL pool");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database_name = format!("attest_db_{suffix}");
        let target = format!("waddle_attest_t_{suffix}");
        let trusted = format!("waddle_attest_f_{suffix}");
        let definer = format!("waddle_attest_d_{suffix}");
        let installer = format!("waddle_attest_i_{suffix}");
        let runtime = format!("waddle_attest_r_{suffix}");
        let outsider = format!("waddle_attest_o_{suffix}");
        sqlx::query(&format!("CREATE DATABASE {database_name}"))
            .execute(&cluster_admin)
            .await
            .expect("isolated test database");
        sqlx::query(&format!(
            "REVOKE TEMPORARY ON DATABASE {database_name} FROM PUBLIC"
        ))
        .execute(&cluster_admin)
        .await
        .expect("revoke public TEMP in isolated database");
        sqlx::query(&format!("CREATE ROLE {definer} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
            .execute(&cluster_admin).await.expect("preprovision definer");
        for role in [&runtime, &outsider, &installer] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
                .execute(&cluster_admin)
                .await
                .expect("test login role");
        }
        sqlx::query(&format!(
            "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("grant restricted installer role handoff");
        let base_url = url_with_database(&url, &database_name);
        let admin = PgPool::connect(&base_url)
            .await
            .expect("isolated database administrator");
        sqlx::query("REVOKE ALL ON SCHEMA public FROM PUBLIC")
            .execute(&admin)
            .await
            .expect("restrict public schema");
        sqlx::query(&format!("CREATE SCHEMA {target}"))
            .execute(&admin)
            .await
            .expect("target schema");
        sqlx::query(&format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"))
            .execute(&admin)
            .await
            .expect("preprovision trusted schema");
        sqlx::query(&format!("GRANT USAGE ON SCHEMA {target} TO {runtime}"))
            .execute(&admin)
            .await
            .expect("runtime schema usage");

        let boundary_url = url_with_schema(&base_url, &target);
        let database = Database::from_config(
            "physical attestation fixture",
            &DatabaseConfig::new(DatabaseDriver::Postgres, boundary_url.clone()),
        )
        .await
        .expect("boundary database");
        let lineage = lineage::enroll(
            &database,
            &LineageConfig {
                deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                action: None,
            },
        )
        .await
        .expect("enroll real lineage");
        drop(database);
        for statement in [
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {definer}"),
            format!("GRANT USAGE ON SCHEMA {target} TO {definer}"),
            format!("GRANT SELECT ON {target}._lineage TO {definer}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("preprovision definer grant");
        }

        let admin_options = PgConnectOptions::from_str(&boundary_url).expect("admin options");
        let installer_options = admin_options.clone().username(&installer);
        let runtime_options = admin_options.clone().username(&runtime);
        Self {
            cluster_admin,
            admin,
            installer_options,
            runtime_options,
            base_url,
            database_name,
            lineage,
            target,
            trusted,
            definer,
            installer,
            runtime,
            outsider,
            operation_id: uuid::Uuid::new_v4(),
        }
    }

    fn config(&self) -> PhysicalAttestationConfig {
        self.with_lineage(self.lineage.clone())
    }

    fn with_lineage(&self, lineage: AttestedLineage) -> PhysicalAttestationConfig {
        PhysicalAttestationConfig::new(
            lineage,
            PgRoleName::new(self.installer.clone()).expect("test installer role"),
            PgRoleName::new(self.runtime.clone()).expect("test runtime role"),
            PgRoleName::new(self.definer.clone()).expect("test definer role"),
            PgSchemaName(self.trusted.clone()),
        )
        .expect("valid attestation configuration")
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.database_name
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("drop isolated test database");
        for role in [
            &self.runtime,
            &self.outsider,
            &self.installer,
            &self.definer,
        ] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.cluster_admin)
                .await
                .expect("drop test role");
        }
        self.cluster_admin.close().await;
    }
}

fn url_with_database(base: &str, database: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "dbname" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database);
    url.to_string()
}

fn url_with_schema(base: &str, schema: &str) -> String {
    url_with_search_path(base, schema)
}

fn url_with_search_path(base: &str, search_path: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={search_path}"));
    url.to_string()
}

async fn run_sql(f: &Fixture, statement: &str) {
    sqlx::query(statement)
        .execute(&f.admin)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

async fn relation_exists(admin: &PgPool, schema: &str, relation: &str) -> bool {
    let qualified = format!("{schema}.{relation}");
    sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
        .bind(qualified)
        .fetch_one(admin)
        .await
        .expect("inspect fixture relation")
}

fn room_effect_pg_error_code(error: &RoomEffectOutboxError) -> Option<String> {
    match error {
        RoomEffectOutboxError::Database(DatabaseError::Internal(sqlx::Error::Database(error))) => {
            error.code().map(|code| code.into_owned())
        }
        _ => None,
    }
}

const MUC_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_994;
const MUC_DESTROY_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_995;
const ROOM_EFFECT_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_996;

/// Exercise a public owner entry while its family gate is held. Observe the
/// backend's acknowledged ROLLBACK after a failed lower-gate attempt, confirm
/// catalog-X is released while the operation remains pending, then verify
/// relation visibility after releasing the family gate.
async fn assert_owner_gate_releases_catalog<T>(
    admin: PgPool,
    schema: String,
    relation: &'static str,
    family_gate: i64,
    application_name: &str,
    operation: impl Future<Output = T> + Send + 'static,
) -> T
where
    T: Send + 'static,
{
    let qualified = format!("{schema}.{relation}");
    assert!(!relation_exists(&admin, &schema, relation).await);

    let mut gate = admin.begin().await.expect("begin owner-family gate");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(family_gate)
        .execute(&mut *gate)
        .await
        .expect("hold owner-family gate");

    let rollback_baseline: String = sqlx::query_scalar("SELECT pg_catalog.clock_timestamp()::text")
        .fetch_one(&admin)
        .await
        .expect("owner rollback timestamp baseline");
    let operation = tokio::spawn(operation);
    let retry_rollback_seen =
        wait_for_owner_retry_rollback(&admin, application_name, &rollback_baseline).await;

    let catalog_released = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let shared: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&admin)
                    .await
                    .expect("probe catalog lock release");
            if shared {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    let remained_pending = !operation.is_finished();
    let visible_while_gate_held = relation_exists(&admin, &schema, relation).await;

    gate.rollback().await.expect("release owner-family gate");
    let output = tokio::time::timeout(Duration::from_secs(10), operation)
        .await
        .expect("public owner entry completed after gate release")
        .expect("join public owner entry");
    assert!(
        retry_rollback_seen,
        "public owner transaction did not expose its acknowledged retry rollback"
    );
    assert!(
        remained_pending,
        "public owner entry bypassed its family gate"
    );
    assert!(
        catalog_released,
        "owner retry retained catalog-X while the family gate was busy"
    );
    assert!(
        !visible_while_gate_held,
        "owner DDL became visible before the family gate was released"
    );
    assert!(
        relation_exists(&admin, &schema, relation).await,
        "{} became visible after the public owner entry completed",
        qualified
    );
    output
}

/// This public-entry check only proves the reader completes after catalog-X
/// is released. It does not independently prove that the operation reached its
/// catalog-S attempt before the release.
async fn assert_public_reader_completes_after_catalog_x_release<T>(
    admin: PgPool,
    operation: impl Future<Output = T> + Send + 'static,
) -> T
where
    T: Send + 'static,
{
    let mut catalog = admin.begin().await.expect("begin catalog-X reader witness");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *catalog)
        .await
        .expect("hold catalog-X");

    let operation = tokio::spawn(operation);

    catalog.rollback().await.expect("release catalog-X");
    let output = tokio::time::timeout(Duration::from_secs(10), operation)
        .await
        .expect("public reader completed after catalog-X release")
        .expect("join public reader");
    output
}

async fn wait_for_owner_retry_rollback(
    admin: &PgPool,
    application_name: &str,
    baseline: &str,
) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observed: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' \
                   AND a.query LIKE 'ROLLBACK%' \
                   AND a.state_change > $2::timestamptz)",
            )
            .bind(application_name)
            .bind(baseline)
            .fetch_one(admin)
            .await
            .expect("observe acknowledged owner retry rollback");
            if observed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn restricted_room_effect_outbox_preserves_and_fences_effect_rows() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url).await;
    let identity = f.lineage.postgres_identity.as_ref().unwrap().clone();
    let owner = format!("room_owner_{}", uuid::Uuid::new_v4().simple());
    let migrator = format!("room_migrator_{}", uuid::Uuid::new_v4().simple());
    run_sql(&f, &format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS")).await;
    run_sql(&f, &format!("CREATE ROLE {migrator} LOGIN")).await;
    run_sql(
        &f,
        &format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
    )
    .await;
    run_sql(&f, &format!("ALTER SCHEMA {} OWNER TO {owner}", f.target)).await;
    let config = install(&f.installer_options, f.config(), f.operation_id)
        .await
        .expect("attestor");
    let attested =
        attested_pool(f.runtime_options.clone(), config, 2, Duration::from_secs(3)).unwrap();
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "room effects",
        attested,
        1,
        1,
    )
    .await
    .unwrap();
    let runtime = pool.global().clone();
    assert!(matches!(
        RoomEffectOutboxStore::attach_existing(pool.ingress()).await,
        Err(RoomEffectRuntimeAttachError::MainLaneRequired)
    ));
    assert!(matches!(
        RoomEffectOutboxStore::attach_existing(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::Relation { .. })
    ));
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::Relation {
            table: "clustering_muc_rooms",
            ..
        })
    ));
    let absent_room_table: Option<String> = sqlx::query_scalar(&format!(
        "SELECT pg_catalog.to_regclass('{}.clustering_muc_rooms')::text",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert!(
        absent_room_table.is_none(),
        "runtime verification must not create schema"
    );
    assert!(
        RoomEffectOutboxStore::try_new(runtime.clone())
            .await
            .is_err()
    );
    assert!(
        RoomEffectOutboxStore::initialize_owner(&runtime, &identity)
            .await
            .is_err()
    );
    assert!(
        PostgresMucRoomStore::open(
            runtime.clone(),
            tokio_util::sync::CancellationToken::new(),
            SharedNodeIdentity::new(NodeIdentity::new("node", "epoch")),
        )
        .await
        .is_err()
    );
    let owner_app_name = format!("muc_owner_{}", f.operation_id.simple());
    let mut url = url::Url::parse(&f.base_url).unwrap();
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(k, v)| (k, v)))
        .append_pair("user", &migrator)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={},public", f.target),
        )
        .append_pair("application_name", &owner_app_name);
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    )
    .await;
    run_sql(
        &f,
        "CREATE TABLE public.clustering_muc_room_effects (sentinel integer)",
    )
    .await;
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    config.pool_size = 1;
    let owner_db = Database::from_config("room effect owner", &config)
        .await
        .unwrap();
    let mut wrong = identity.clone();
    wrong.database.oid += 1;
    assert!(
        RoomEffectOutboxStore::initialize_owner(&owner_db, &wrong)
            .await
            .is_err()
    );
    run_sql(
        &f,
        &format!("GRANT TEMPORARY ON DATABASE {} TO {owner}", f.database_name),
    )
    .await;
    owner_db
        .guard()
        .await
        .unwrap()
        .execute(
            "CREATE TEMP TABLE clustering_muc_room_effects (sentinel integer)",
            (),
        )
        .await
        .unwrap();
    assert!(
        RoomEffectOutboxStore::initialize_owner(&owner_db, &identity)
            .await
            .is_err()
    );
    // The catalog-fenced refusal discards this checkout; closing that backend
    // also removes the temporary shadow before the next owner checkout.
    run_sql(
        &f,
        &format!(
            "REVOKE TEMPORARY ON DATABASE {} FROM {owner}",
            f.database_name
        ),
    )
    .await;
    // Retained effect or occupancy authority must prevent recreating rooms.
    RoomEffectOutboxStore::initialize_owner(&owner_db, &identity)
        .await
        .unwrap();
    run_sql(&f, &format!("INSERT INTO {}.clustering_muc_room_effects (lifecycle_id, revision, ordinal, room_jid, kind, terminal, payload_json, available_at_ms, origin_instance_id, producing_node, created_at_ms) VALUES ('life', 1, 0, 'room@conference.example.test', 'config_changed', false, '{{}}', 0, 'instance', 'node', 0)", f.target)).await;
    assert!(matches!(
        PostgresMucRoomStore::initialize_owner(&owner_db, &identity).await,
        Err(MucOwnerPreparationError::ExistingRoomSchema)
    ));
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.clustering_muc_room_effects",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(retained, 1);
    run_sql(
        &f,
        &format!("DROP TABLE {}.clustering_muc_room_effects", f.target),
    )
    .await;
    run_sql(&f, &format!("CREATE TABLE {}.xmpp_occupancy_authority (full_jid text primary key, generation text not null)", f.target)).await;
    run_sql(
        &f,
        &format!(
            "ALTER TABLE {}.xmpp_occupancy_authority OWNER TO {owner}",
            f.target
        ),
    )
    .await;
    run_sql(&f, &format!("INSERT INTO {}.xmpp_occupancy_authority VALUES ('alice@example.test/device', 'generation')", f.target)).await;
    assert!(matches!(
        PostgresMucRoomStore::initialize_owner(&owner_db, &identity).await,
        Err(MucOwnerPreparationError::ExistingRoomSchema)
    ));
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.xmpp_occupancy_authority",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(retained, 1);
    run_sql(
        &f,
        &format!("DELETE FROM {}.xmpp_occupancy_authority", f.target),
    )
    .await;
    let database = owner_db.clone();
    let room_identity = identity.clone();
    assert_owner_gate_releases_catalog(
        f.admin.clone(),
        f.target.clone(),
        "clustering_muc_rooms",
        MUC_SCHEMA_ADVISORY_LOCK_KEY,
        &owner_app_name,
        async move { PostgresMucRoomStore::initialize_owner(&database, &room_identity).await },
    )
    .await
    .expect("prepare fresh room schema after the MUC gate is released");
    let database = owner_db.clone();
    assert_owner_gate_releases_catalog(
        f.admin.clone(),
        f.target.clone(),
        "clustering_muc_destroy_outbox",
        MUC_DESTROY_SCHEMA_ADVISORY_LOCK_KEY,
        &owner_app_name,
        async move { MucDestroyCompletionOutboxStore::try_new(database).await },
    )
    .await
    .expect("prepare destroy completion outbox after its gate is released");
    run_sql(&f, &format!("INSERT INTO {}.clustering_muc_rooms (room_jid, waddle_id, channel_id, config_json) VALUES ('room@conference.example.test', 'space', 'channel', '{{}}')", f.target)).await;
    assert!(matches!(
        PostgresMucRoomStore::initialize_owner(&owner_db, &identity).await,
        Err(MucOwnerPreparationError::ExistingRoomSchema)
    ));
    let preserved: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.clustering_muc_rooms",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(preserved, 1);
    let database = owner_db.clone();
    let effect_identity = identity.clone();
    assert_owner_gate_releases_catalog(
        f.admin.clone(),
        f.target.clone(),
        "clustering_muc_room_effects",
        ROOM_EFFECT_SCHEMA_ADVISORY_LOCK_KEY,
        &owner_app_name,
        async move { RoomEffectOutboxStore::initialize_owner(&database, &effect_identity).await },
    )
    .await
    .expect("prepare room-effect outbox after its gate is released");
    PostgresMucRoomStore::open(
        owner_db.clone(),
        tokio_util::sync::CancellationToken::new(),
        SharedNodeIdentity::new(NodeIdentity::new("node", "epoch")),
    )
    .await
    .expect("open the raw MUC composition across its three fenced commits");
    let sentinel: Option<String> = sqlx::query_scalar(
        "SELECT pg_catalog.to_regclass('public.clustering_muc_room_effects')::text",
    )
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert!(sentinel.is_some());
    run_sql(
        &f,
        &format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.clustering_muc_room_effects TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    for table in [
        "clustering_muc_rooms",
        "clustering_muc_room_affiliations",
        "clustering_muc_room_lifecycles",
        "clustering_muc_destroy_outbox",
    ] {
        run_sql(
            &f,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{} TO {}",
                f.target, table, f.runtime
            ),
        )
        .await;
    }
    run_sql(
        &f,
        &format!(
            "GRANT SELECT, INSERT, DELETE ON {}.clustering_muc_admin_receipts TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    let verified_room_runtime = assert_public_reader_completes_after_catalog_x_release(
        f.admin.clone(),
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()),
    )
    .await
    .expect("verify complete room runtime schema after acquiring catalog-S");
    assert_public_reader_completes_after_catalog_x_release(
        f.admin.clone(),
        MucDestroyCompletionOutboxStore::attach_existing(runtime.clone()),
    )
    .await
    .expect("attach destroy outbox after acquiring catalog-S");
    assert!(verified_room_runtime.belongs_to_main_pool(&runtime));
    assert!(!verified_room_runtime.belongs_to_main_pool(&pool.ingress()));
    assert!(
        verified_room_runtime
            .effect_outbox()
            .belongs_to_main_pool(&runtime)
    );
    let mut mismatched_lineage = f.lineage.clone();
    mismatched_lineage
        .postgres_identity
        .as_mut()
        .expect("fixture PostgreSQL identity")
        .schema
        .oid += 1;
    let mismatched_attested = attested_pool(
        f.runtime_options.clone(),
        f.with_lineage(mismatched_lineage),
        2,
        Duration::from_secs(3),
    )
    .expect("construct mismatched identity test pool");
    let mismatched_pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "mismatched room runtime identity",
        mismatched_attested,
        1,
        1,
    )
    .await
    .expect("wrap mismatched identity test pool");
    assert!(
        RoomEffectOutboxStore::verify_muc_room_runtime(mismatched_pool.global().clone())
            .await
            .is_err()
    );
    drop(mismatched_pool);
    // The main lane has one slot. Returning from verification with a usable
    // checkout proves the read-only preflight released its transaction.
    runtime
        .guard()
        .await
        .expect("single-slot main pool is available after verification")
        .execute("SELECT 1", ())
        .await
        .expect("preflight did not retain the pool slot");
    drop(verified_room_runtime);
    let attached = RoomEffectOutboxStore::attach_existing(runtime.clone())
        .await
        .expect("verify outbox");
    let conn = runtime.guard().await.unwrap();
    conn.execute("INSERT INTO clustering_muc_room_effects (lifecycle_id, revision, ordinal, room_jid, kind, terminal, payload_json, available_at_ms, origin_instance_id, producing_node, created_at_ms) VALUES ('life', 1, 0, 'room@conference.example.test', 'config_changed', false, '{}', 0, 'instance', 'node', 0)", ()).await.expect("restricted insert");
    conn.execute(
        "UPDATE clustering_muc_room_effects SET available_at_ms = 42 WHERE lifecycle_id = 'life'",
        (),
    )
    .await
    .expect("restricted update");
    drop(conn);
    RoomEffectOutboxStore::initialize_owner(&owner_db, &identity)
        .await
        .expect("preserve existing rows");
    run_sql(
        &f,
        &format!(
            "ALTER TABLE {}.clustering_muc_rooms ADD COLUMN verifier_drift TEXT",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::InvalidTable {
            table: "clustering_muc_rooms"
        })
    ));
    run_sql(
        &f,
        &format!(
            "ALTER TABLE {}.clustering_muc_rooms DROP COLUMN verifier_drift",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT TRUNCATE ON {}.clustering_muc_room_lifecycles TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "clustering_muc_room_lifecycles"
        })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE TRUNCATE ON {}.clustering_muc_room_lifecycles FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT UPDATE ON {}.clustering_muc_admin_receipts TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "clustering_muc_admin_receipts"
        })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE UPDATE ON {}.clustering_muc_admin_receipts FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT UPDATE (removed_sessions_json) ON {}.clustering_muc_admin_receipts TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "clustering_muc_admin_receipts"
        })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE UPDATE (removed_sessions_json) ON {}.clustering_muc_admin_receipts FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("GRANT CREATE ON SCHEMA {} TO {}", f.target, f.definer),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "runtime role"
        })
    ));
    run_sql(
        &f,
        &format!("REVOKE CREATE ON SCHEMA {} FROM {}", f.target, f.definer),
    )
    .await;
    run_sql(
        &f,
        &format!("GRANT USAGE ON SCHEMA {} TO {}", f.target, f.outsider),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "runtime role"
        })
    ));
    run_sql(
        &f,
        &format!("REVOKE USAGE ON SCHEMA {} FROM {}", f.target, f.outsider),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON {}.clustering_muc_admin_receipts TO {}",
            f.target, f.outsider
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "clustering_muc_admin_receipts"
        })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE SELECT ON {}.clustering_muc_admin_receipts FROM {}",
            f.target, f.outsider
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX muc_lifecycle_extra ON {}.clustering_muc_room_lifecycles (updated_at)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::InvalidIndex {
            table: "clustering_muc_room_lifecycles"
        })
    ));
    run_sql(&f, &format!("DROP INDEX {}.muc_lifecycle_extra", f.target)).await;
    // The opaque attachment has no methods for draining or unbranded writes,
    // even when lifecycle authority is unavailable.
    run_sql(
        &f,
        &format!("DROP TABLE {}.clustering_muc_admin_receipts", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!("DROP TABLE {}.clustering_muc_room_lifecycles", f.target),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::Relation {
            table: "clustering_muc_room_lifecycles",
            ..
        })
    ));
    let _still_verified = RoomEffectOutboxStore::attach_existing(runtime.clone())
        .await
        .expect("verify outbox without claiming lifecycle authority");
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.clustering_muc_room_effects",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(retained, 1);
    run_sql(
        &f,
        &format!(
            "GRANT TRUNCATE ON {}.clustering_muc_room_effects TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::attach_existing(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges { .. })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE TRUNCATE ON {}.clustering_muc_room_effects FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON {}.clustering_muc_room_effects TO {}",
            f.target, f.outsider
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::attach_existing(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges { .. })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE SELECT ON {}.clustering_muc_room_effects FROM {}",
            f.target, f.outsider
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX room_effect_extra ON {}.clustering_muc_room_effects (created_at_ms)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        RoomEffectOutboxStore::attach_existing(runtime.clone()).await,
        Err(RoomEffectRuntimeAttachError::InvalidIndex { .. })
    ));
    run_sql(&f, &format!("DROP INDEX {}.room_effect_extra", f.target)).await;
    run_sql(
        &f,
        &format!(
            "ALTER TABLE {}.clustering_muc_room_effects DROP COLUMN unowned_since_ms",
            f.target
        ),
    )
    .await;
    assert!(
        RoomEffectOutboxStore::initialize_owner(&owner_db, &identity)
            .await
            .is_err()
    );
    assert!(
        RoomEffectOutboxStore::attach_existing(runtime.clone())
            .await
            .is_err()
    );
    let mut relation_holder = f.admin.begin().await.expect("begin relation lock holder");
    sqlx::query(&format!(
        "LOCK TABLE {}.clustering_muc_room_effects IN ACCESS EXCLUSIVE MODE",
        f.target
    ))
    .execute(&mut *relation_holder)
    .await
    .expect("hold room-effect relation lock");
    let mut bootstrap = tokio::spawn(RoomEffectOutboxStore::try_new(owner_db.clone()));
    let bootstrap_result = tokio::time::timeout(Duration::from_secs(8), &mut bootstrap).await;
    relation_holder
        .rollback()
        .await
        .expect("release room-effect relation lock");
    let bootstrap_result = match bootstrap_result {
        Ok(result) => result.expect("room-effect bootstrap task"),
        Err(_) => {
            bootstrap.abort();
            let _ = bootstrap.await;
            panic!("room-effect bootstrap exceeded its bounded relation-lock wait");
        }
    };
    let error = match bootstrap_result {
        Ok(_) => panic!("room-effect bootstrap unexpectedly passed the held relation lock"),
        Err(error) => error,
    };
    assert_eq!(
        room_effect_pg_error_code(&error).as_deref(),
        Some("55P03"),
        "room-effect owner bootstrap should fail with PostgreSQL lock_timeout"
    );
    drop(attached);
    drop(runtime);
    drop(pool);
    drop(owner_db);
    run_sql(&f, &format!("DROP SCHEMA {} CASCADE", f.target)).await;
    run_sql(
        &f,
        &format!("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {owner}"),
    )
    .await;
    run_sql(&f, &format!("DROP ROLE {migrator}")).await;
    run_sql(&f, &format!("DROP ROLE {owner}")).await;
    f.cleanup().await;
}
