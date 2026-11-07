//! Restricted PostgreSQL PubSub attachment and owner bootstrap.
#![cfg(feature = "test-support")]

use sqlx::postgres::{PgConnectOptions, PgPool};
use std::str::FromStr;
use std::time::Duration;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use waddle_server::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use waddle_server::muc_durable::PostgresMucRoomStore;
use waddle_server::pubsub::{
    DatabasePubSubStorage, PubSubRuntimeAttachError, RestrictedServingProfileV1,
};
use waddle_server::room_effect_outbox::RoomEffectOutboxStore;
use waddle_server::server::ResolvedXmppDatabaseUrl;
use waddle_xmpp::pubsub::{
    PubSubItem, PubSubStorage, PublicationNode, PublicationVersion, VersionedPublishResult,
};

const V1_HELPER_TABLES: [&str; 12] = [
    "dnd_projection",
    "dnd_projection_source_version",
    "notification_settings_projection",
    "notification_settings_projection_source_version",
    "pubsub_affiliations",
    "pubsub_items",
    "pubsub_nodes",
    "pubsub_owner_configured",
    "pubsub_push_publications",
    "pubsub_schema_marker",
    "pubsub_schema_version",
    "pubsub_subscriptions",
];

#[tokio::test]
async fn sqlite_pubsub_legacy_path_still_round_trips() {
    let storage = DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("open SQLite PubSub store");
    let owner = "alice@example.test".parse().expect("owner JID");
    storage
        .get_or_create_node(&owner, "urn:xmpp:bookmarks:1")
        .await
        .expect("create node");
    let item = PubSubItem {
        id: Some("room@example.test".to_owned()),
        publisher: None,
        payload: Some(
            "<conference xmlns='urn:xmpp:bookmarks:1'/>"
                .parse()
                .expect("payload"),
        ),
    };
    storage
        .publish_item(&owner, "urn:xmpp:bookmarks:1", &item, Some(&owner), false)
        .await
        .expect("publish item");
    let items = storage
        .get_items(&owner, "urn:xmpp:bookmarks:1", None, &[])
        .await
        .expect("read items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "room@example.test");
}

#[tokio::test]
async fn pg17_v1_helper_attaches_real_owner_prepared_pubsub_and_rejects_drift() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url.clone()).await;
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("v1_owner_{suffix}");
    let migrator = format!("v1_migrator_{suffix}");
    let api = format!("v1_api_{suffix}");
    let helper = format!("v1_helper_{suffix}");
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&f.admin)
        .await
        .expect("PostgreSQL version");
    assert!(
        (170_000..180_000).contains(&version),
        "requires PostgreSQL 17"
    );
    for role in [&owner, &api, &helper] {
        run_sql(&f, &format!("CREATE ROLE {role} NOLOGIN NOINHERIT")).await;
    }
    run_sql(&f, &format!("CREATE ROLE {migrator} LOGIN NOINHERIT")).await;
    run_sql(
        &f,
        &format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
    )
    .await;
    run_sql(&f, &format!("ALTER SCHEMA {} OWNER TO {owner}", f.target)).await;
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    )
    .await;
    let mut owner_url = url::Url::parse(&f.base_url).expect("owner URL");
    let retained: Vec<_> = owner_url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    owner_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", &migrator)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={}", f.target),
        );
    let owner_db = Database::from_config(
        "v1 PubSub owner",
        &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_string()),
    )
    .await
    .expect("connect owner");
    let identity = f
        .lineage
        .postgres_identity
        .as_ref()
        .expect("identity")
        .clone();
    DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
        .await
        .expect("prepare the real PubSub schema");
    PostgresMucRoomStore::initialize_owner(&owner_db, &identity)
        .await
        .expect("prepare the real MUC schema");
    MucDestroyCompletionOutboxStore::try_new(owner_db.clone())
        .await
        .expect("prepare the real MUC destroy outbox");
    RoomEffectOutboxStore::initialize_owner(&owner_db, &identity)
        .await
        .expect("prepare the real MUC effect outbox");
    for (table, rights) in [
        ("pubsub_schema_marker", "SELECT"),
        ("pubsub_schema_version", "SELECT"),
        ("pubsub_nodes", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_items", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_subscriptions", "SELECT, INSERT, DELETE"),
        ("pubsub_affiliations", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_owner_configured", "SELECT, INSERT"),
        ("dnd_projection", "SELECT, INSERT, UPDATE, DELETE"),
        ("dnd_projection_source_version", "SELECT, INSERT, UPDATE"),
        (
            "notification_settings_projection",
            "SELECT, INSERT, UPDATE, DELETE",
        ),
        (
            "notification_settings_projection_source_version",
            "SELECT, INSERT, UPDATE",
        ),
        ("pubsub_push_publications", "SELECT, INSERT, UPDATE"),
    ] {
        run_sql(
            &f,
            &format!("GRANT {rights} ON {}.{table} TO {}", f.target, f.runtime),
        )
        .await;
    }
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON SEQUENCE {}.pubsub_items_seq_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    for table in [
        "clustering_muc_rooms",
        "clustering_muc_room_affiliations",
        "clustering_muc_room_lifecycles",
        "clustering_muc_destroy_outbox",
        "clustering_muc_room_effects",
    ] {
        run_sql(
            &f,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {}",
                f.target, f.runtime
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
    run_sql(
        &f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {}.pubsub_runtime_attach_lock() TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    let config = install(&f.installer_options, f.config(), f.operation_id)
        .await
        .expect("install attestor");
    let attested = attested_pool(f.runtime_options.clone(), config, 2, Duration::from_secs(3))
        .expect("attested pool");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "v1 pubsub",
        attested,
        1,
        1,
    )
    .await
    .expect("attested database");
    let runtime = pool.global().clone();
    DatabasePubSubStorage::attach_existing(runtime.clone())
        .await
        .expect("the legacy profile remains attachable");
    RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone())
        .await
        .expect("the legacy MUC profile remains attachable");
    run_sql(
        &f,
        &format!("GRANT USAGE ON SCHEMA {} TO {api}, {helper}", f.target),
    )
    .await;
    for table in V1_HELPER_TABLES {
        run_sql(
            &f,
            &format!("GRANT UPDATE ON {}.{table} TO {helper}", f.target),
        )
        .await;
    }
    // Render the actual unregistered candidate's exact body.
    let asset = include_str!("../src/db/migrations/owner/serving_role_graph_v1_candidate.sql");
    let body = asset
        .split_once("AS $v1_pubsub_lock$")
        .unwrap()
        .1
        .split_once("$v1_pubsub_lock$")
        .unwrap()
        .0
        .replace("__SCHEMA__", &format!("\"{}\"", f.target));
    run_sql(
        &f,
        &format!("DROP FUNCTION {}.pubsub_runtime_attach_lock()", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!("GRANT CREATE ON SCHEMA {} TO {helper}", f.target),
    )
    .await;
    run_sql(&f, &format!(
        "CREATE FUNCTION {}.pubsub_runtime_attach_lock() RETURNS void LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $v1_pubsub_lock${body}$v1_pubsub_lock$",
        f.target
    )).await;
    run_sql(
        &f,
        &format!(
            "ALTER FUNCTION {}.pubsub_runtime_attach_lock() OWNER TO {helper}",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "REVOKE ALL ON FUNCTION {}.pubsub_runtime_attach_lock() FROM PUBLIC",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {}.pubsub_runtime_attach_lock() TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("REVOKE CREATE ON SCHEMA {} FROM {helper}", f.target),
    )
    .await;
    let role_oid = |name: String| {
        let admin = f.admin.clone();
        async move {
            let oid: i64 =
                sqlx::query_scalar("SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname=$1")
                    .bind(name)
                    .fetch_one(&admin)
                    .await
                    .expect("role OID");
            u32::try_from(oid).expect("valid role OID")
        }
    };
    let helper_function_oid: i64 =
        sqlx::query_scalar("SELECT pg_catalog.to_regprocedure($1)::oid::bigint")
            .bind(format!("{}.pubsub_runtime_attach_lock()", f.target))
            .fetch_one(&f.admin)
            .await
            .expect("helper OID");
    let mut relation_oids = [0_u32; 12];
    for (index, table) in V1_HELPER_TABLES.iter().enumerate() {
        let oid: i64 = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
            .bind(format!("{}.{table}", f.target))
            .fetch_one(&f.admin)
            .await
            .expect("relation OID");
        relation_oids[index] = u32::try_from(oid).expect("valid relation OID");
    }
    let profile = RestrictedServingProfileV1 {
        api: PgRoleName::new(api.clone()).unwrap(),
        api_oid: role_oid(api.clone()).await,
        helper: PgRoleName::new(helper.clone()).unwrap(),
        helper_oid: role_oid(helper.clone()).await,
        helper_function_oid: u32::try_from(helper_function_oid).unwrap(),
        helper_relation_oids: relation_oids,
    };
    DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &profile)
        .await
        .expect("the exact H-owned helper and real store verify");
    RoomEffectOutboxStore::verify_muc_room_runtime_v1(runtime.clone(), &profile)
        .await
        .expect("the A/H profile verifies the real MUC store");
    assert!(
        RoomEffectOutboxStore::verify_muc_room_runtime(runtime.clone())
            .await
            .is_err(),
        "legacy MUC schema ACL must not be silently broadened"
    );
    run_sql(
        &f,
        &format!(
            "CREATE FUNCTION {}.v1_muc_relay() RETURNS void LANGUAGE sql SECURITY DEFINER AS 'DELETE FROM {}.clustering_muc_rooms WHERE false'",
            f.target, f.target
        ),
    ).await;
    run_sql(
        &f,
        &format!(
            "ALTER FUNCTION {}.v1_muc_relay() OWNER TO {owner}",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "REVOKE ALL ON FUNCTION {}.v1_muc_relay() FROM PUBLIC",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {}.v1_muc_relay() TO {api}",
            f.target
        ),
    )
    .await;
    assert!(
        RoomEffectOutboxStore::verify_muc_room_runtime_v1(runtime.clone(), &profile)
            .await
            .is_err(),
        "a non-runtime-callable A-to-owner definer relay must be rejected"
    );
    run_sql(&f, &format!("DROP FUNCTION {}.v1_muc_relay()", f.target)).await;
    assert!(
        DatabasePubSubStorage::attach_existing(runtime.clone())
            .await
            .is_err(),
        "legacy verifier still rejects the new profile"
    );
    let mut wrong = profile.clone();
    wrong.helper_function_oid += 1;
    assert!(
        DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &wrong)
            .await
            .is_err()
    );
    let mut wrong = profile.clone();
    wrong.helper_relation_oids.swap(0, 1);
    assert!(
        DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &wrong)
            .await
            .is_err()
    );
    for statement in [
        format!("GRANT SELECT ON {}.pubsub_nodes TO {helper}", f.target),
        format!("GRANT SELECT ON {}.pubsub_nodes TO PUBLIC", f.target),
        format!("GRANT USAGE ON SCHEMA {} TO {}", f.target, f.outsider),
        format!("GRANT pg_read_all_data TO {helper}"),
        format!("GRANT TEMP ON DATABASE {} TO {api}", f.database_name),
        format!("GRANT CREATE ON DATABASE {} TO {api}", f.database_name),
        format!("GRANT CREATE ON SCHEMA public TO {api}"),
        format!("GRANT SET ON PARAMETER session_replication_role TO {api}"),
        format!(
            "GRANT EXECUTE ON FUNCTION {}.pubsub_runtime_attach_lock() TO {}",
            f.target, f.outsider
        ),
        format!(
            "GRANT TEMP ON DATABASE {} TO {}",
            f.database_name, f.runtime
        ),
    ] {
        run_sql(&f, &statement).await;
        assert!(
            DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &profile)
                .await
                .is_err(),
            "drift must fail: {statement}"
        );
        let revoke = statement
            .replacen("GRANT", "REVOKE", 1)
            .replace(" TO ", " FROM ");
        run_sql(&f, &revoke).await;
    }
    // A same-OID SECURITY DEFINER body replacement must be rejected before
    // execution, including when the stored source is oversized/compressible.
    let malicious = format!("SELECT NULL::void;{}", " ".repeat(16_384));
    run_sql(
        &f,
        &format!(
            "CREATE OR REPLACE FUNCTION {}.pubsub_runtime_attach_lock() RETURNS void LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $v1_pubsub_lock${malicious}$v1_pubsub_lock$",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &profile).await,
        Err(PubSubRuntimeAttachError::InvalidAttachLock)
    ));
    let advisory_key = i64::from_be_bytes(
        uuid::Uuid::new_v4().as_bytes()[..8]
            .try_into()
            .expect("UUID prefix"),
    );
    run_sql(
        &f,
        &format!(
            "CREATE OR REPLACE FUNCTION {}.pubsub_runtime_attach_lock() RETURNS void LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS 'SELECT pg_catalog.pg_advisory_lock({advisory_key}::pg_catalog.int8)'",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &profile).await,
        Err(PubSubRuntimeAttachError::InvalidAttachLock)
    ));
    let mut witness = f.admin.acquire().await.expect("advisory-lock witness");
    let unheld: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
        .bind(advisory_key)
        .fetch_one(&mut *witness)
        .await
        .expect("probe helper side effect");
    assert!(unheld, "tampered helper executed before validation");
    sqlx::query("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(advisory_key)
        .execute(&mut *witness)
        .await
        .expect("release witness lock");
    drop(witness);
    run_sql(
        &f,
        &format!(
            "CREATE OR REPLACE FUNCTION {}.pubsub_runtime_attach_lock() RETURNS void LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $v1_pubsub_lock${body}$v1_pubsub_lock$",
            f.target
        ),
    )
    .await;
    DatabasePubSubStorage::verify_restricted_profile_v1(&runtime, &profile)
        .await
        .expect("restored profile verifies");
    drop(runtime);
    drop(pool);
    drop(owner_db);
    f.cleanup().await;
    let cluster = PgPool::connect(&url)
        .await
        .expect("reconnect for role cleanup");
    for role in [&migrator, &owner, &api, &helper] {
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&cluster)
            .await
            .expect("drop v1 role");
    }
    cluster.close().await;
}

#[allow(dead_code)]
struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    admin_options: PgConnectOptions,
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
            admin_options,
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

async fn run_concurrent_pubsub_index_race(f: &Fixture, runtime: Database) {
    let target = format!("{}.pubsub_nodes", f.target);
    sqlx::query(&format!(
        "INSERT INTO {}.pubsub_nodes \
           (owner_jid, node_name, access_model, publish_model, max_items, persist_items, \
            deliver_payloads, notify_retract, notify_delete, send_last_published_item, created_at_ms) \
         SELECT 'race@example.net', 'race-' || sequence::text, 'open', 'publishers', 1, 1, \
            1, 1, 1, 'never', 1 FROM generate_series(1, 300000) AS generated(sequence)",
        f.target
    ))
    .execute(&f.admin)
    .await
    .expect("populate PubSub nodes for CIC race");

    let build_target = target.clone();
    let build_admin = f.admin.clone();
    let build = tokio::spawn(async move {
        let mut connection = build_admin.acquire().await?;
        sqlx::query("SET max_parallel_maintenance_workers = 0")
            .execute(&mut *connection)
            .await?;
        sqlx::query(&format!(
            "CREATE INDEX CONCURRENTLY idx_pubsub_nodes_race_probe \
             ON {build_target} (md5(node_name))"
        ))
        .execute(&mut *connection)
        .await
    });
    let mut observed = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let in_progress: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_progress_create_index \
                 WHERE relid = $1::regclass)",
            )
            .bind(&target)
            .fetch_one(&f.admin)
            .await
            .expect("observe PubSub CIC progress");
            if in_progress {
                observed = true;
                break;
            }
            if build.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("wait for PubSub CIC progress");
    if !observed {
        build
            .await
            .expect("join PubSub CIC build")
            .expect("complete PubSub CIC build");
        panic!("could not observe concurrent PubSub index creation");
    }

    let attach_database = runtime.clone();
    let mut attach =
        tokio::spawn(async move { DatabasePubSubStorage::attach_existing(attach_database).await });
    let refusal = tokio::time::timeout(Duration::from_secs(10), &mut attach)
        .await
        .expect("PubSub attach did not refuse active CIC")
        .expect("join PubSub attach");
    match refusal {
        Err(PubSubRuntimeAttachError::ConcurrentSchemaChange {
            table: "pubsub_nodes",
        }) => {}
        Err(error) => panic!("PubSub CIC race had wrong refusal: {error:?}"),
        Ok(_) => panic!("PubSub attachment accepted an active CIC"),
    }
    build
        .await
        .expect("join PubSub CIC build")
        .expect("complete PubSub CIC build");
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime).await,
        Err(PubSubRuntimeAttachError::InvalidIndex {
            table: "pubsub_nodes"
        })
    ));
    run_sql(
        f,
        &format!("DROP INDEX {}.idx_pubsub_nodes_race_probe", f.target),
    )
    .await;
    run_sql(
        f,
        &format!(
            "DELETE FROM {}.pubsub_nodes WHERE owner_jid = 'race@example.net'",
            f.target
        ),
    )
    .await;
}

async fn verify_pubsub_owner_gate_sees_committed_schema_state(
    f: &Fixture,
    owner_database: &Database,
    identity: &lineage::PgIdentity,
    application: &str,
) {
    // Keep synchronized with the production PubSub schema gate.
    const PUBSUB_SCHEMA_ADVISORY_LOCK_KEY: i64 = 7_534_519_881_230_429_064;

    let mut catalog = f.admin.begin().await.expect("begin catalog holder");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *catalog)
        .await
        .expect("hold catalog-S");
    let database = owner_database.clone();
    let identity_copy = identity.clone();
    let initialize = tokio::spawn(async move {
        DatabasePubSubStorage::initialize_owner(database, &identity_copy).await
    });
    wait_for_owner_retry(f, application).await;
    let mut gate_probe = f.admin.begin().await.expect("begin gate probe");
    let acquired: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(PUBSUB_SCHEMA_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *gate_probe)
        .await
        .expect("probe PubSub gate");
    assert!(acquired, "owner must not take its gate before catalog-X");
    gate_probe.rollback().await.expect("release gate probe");
    catalog.rollback().await.expect("release catalog-S");
    tokio::time::timeout(Duration::from_secs(30), initialize)
        .await
        .expect("catalog-blocked owner completes")
        .expect("join catalog-blocked owner")
        .expect("owner initializes after catalog release");

    // Publish an incomplete marker and old schema version while the owner is
    // retrying its lower gate. It must see that committed state on retry.
    let mut gate = f.admin.begin().await.expect("begin PubSub owner gate");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(PUBSUB_SCHEMA_ADVISORY_LOCK_KEY)
        .execute(&mut *gate)
        .await
        .expect("hold PubSub owner gate");
    let initial_version: i32 = sqlx::query_scalar(&format!(
        "SELECT version FROM {}.pubsub_schema_version",
        f.target
    ))
    .fetch_one(&mut *gate)
    .await
    .expect("read current PubSub schema version");
    sqlx::query(&format!(
        "UPDATE {}.pubsub_schema_version SET version = version - 1",
        f.target
    ))
    .execute(&mut *gate)
    .await
    .expect("stage prior schema version");
    sqlx::query(&format!(
        "UPDATE {}.pubsub_schema_marker SET owner_phase = 0 WHERE marker_id = 1",
        f.target
    ))
    .execute(&mut *gate)
    .await
    .expect("stage incomplete PubSub marker");

    let database = owner_database.clone();
    let identity = identity.clone();
    let initialize =
        tokio::spawn(
            async move { DatabasePubSubStorage::initialize_owner(database, &identity).await },
        );
    wait_for_owner_retry(f, application).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut probe = f.admin.begin().await.expect("begin catalog probe");
            let acquired: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *probe)
                    .await
                    .expect("probe released catalog-X");
            probe.rollback().await.expect("release catalog probe");
            if acquired {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("contended lower gate must release catalog-X between retries");
    gate.commit()
        .await
        .expect("publish prior PubSub schema state");
    tokio::time::timeout(Duration::from_secs(30), initialize)
        .await
        .expect("PubSub owner initializer remained blocked")
        .expect("join PubSub owner initializer")
        .expect("initialize from post-gate committed schema");

    let version: i32 = sqlx::query_scalar(&format!(
        "SELECT version FROM {}.pubsub_schema_version",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .expect("read PubSub schema version after gate wait");
    let owner_phase: i64 = sqlx::query_scalar(&format!(
        "SELECT owner_phase FROM {}.pubsub_schema_marker WHERE marker_id = 1",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .expect("read PubSub marker after gate wait");
    assert_eq!(version, initial_version, "the owner saw the prior version");
    assert_eq!(
        owner_phase, 1,
        "the owner completed the marker after waiting"
    );
}

async fn wait_for_owner_retry(f: &Fixture, application: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rolled_back: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'idle' \
                   AND a.query = 'ROLLBACK' AND a.xact_start IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
                     WHERE l.pid = a.pid AND l.locktype = 'advisory'))",
            )
            .bind(application)
            .fetch_one(&f.admin)
            .await
            .expect("observe acknowledged PubSub owner rollback");
            if rolled_back {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("observe an acknowledged PubSub owner retry");
}

#[tokio::test]
async fn postgres_security_definer_table_lock_works_in_read_only_attach_transaction() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping PostgreSQL security-definer lock probe: test URL not set");
        return;
    };
    let f = Fixture::new(url).await;
    let table = format!("{}.pubsub_attach_lock_probe", f.target);
    let function = format!("{}.pubsub_attach_lock_probe", f.target);
    run_sql(
        &f,
        &format!("GRANT CREATE ON SCHEMA {} TO {}", f.target, f.definer),
    )
    .await;
    run_sql(
        &f,
        &format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY)"),
    )
    .await;
    run_sql(&f, &format!("ALTER TABLE {table} OWNER TO {}", f.definer)).await;
    run_sql(
        &f,
        &format!(
            "CREATE FUNCTION {function}() RETURNS void LANGUAGE sql SECURITY DEFINER \
             SET search_path = pg_catalog, pg_temp AS $$ \
             LOCK TABLE {table} IN SHARE UPDATE EXCLUSIVE MODE NOWAIT; \
             SELECT NULL::void; $$"
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("ALTER FUNCTION {function}() OWNER TO {}", f.definer),
    )
    .await;
    run_sql(
        &f,
        &format!("REVOKE ALL ON FUNCTION {function}() FROM PUBLIC"),
    )
    .await;
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION {function}() TO {}", f.runtime),
    )
    .await;

    let runtime = PgPool::connect_with(f.runtime_options.clone())
        .await
        .expect("restricted runtime pool");
    let mut tx = runtime.begin().await.expect("begin read-only attach probe");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY")
        .execute(&mut *tx)
        .await
        .expect("set read-only attach transaction");
    sqlx::query(&format!("SELECT {function}()"))
        .execute(&mut *tx)
        .await
        .expect("security-definer lock helper in read-only transaction");
    let lock_held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l \
         JOIN pg_catalog.pg_class c ON c.oid = l.relation \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE l.pid = pg_catalog.pg_backend_pid() AND l.granted \
           AND l.mode = 'ShareUpdateExclusiveLock' AND n.nspname = $1 \
           AND c.relname = 'pubsub_attach_lock_probe')",
    )
    .bind(&f.target)
    .fetch_one(&mut *tx)
    .await
    .expect("observe transaction lock");
    assert!(
        lock_held,
        "the helper did not hold the requested relation lock"
    );
    tx.rollback().await.expect("release probe lock");
    runtime.close().await;

    let runtime_has_table_rights: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT, INSERT, UPDATE, DELETE')",
    )
    .bind(&f.runtime)
    .bind(&table)
    .fetch_one(&f.admin)
    .await
    .expect("inspect restricted runtime table rights");
    assert!(
        !runtime_has_table_rights,
        "the runtime unexpectedly has table rights"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn restricted_pubsub_attachment_and_guarded_owner_bootstrap() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url).await;
    let identity = f
        .lineage
        .postgres_identity
        .as_ref()
        .expect("physical identity")
        .clone();
    let owner = format!("pubsub_owner_{}", uuid::Uuid::new_v4().simple());
    let migrator = format!("pubsub_migrator_{}", uuid::Uuid::new_v4().simple());
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
        .expect("install physical attestor");
    let attested = attested_pool(f.runtime_options.clone(), config, 2, Duration::from_secs(3))
        .expect("attested pool");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "pubsub runtime",
        attested,
        1,
        1,
    )
    .await
    .expect("main/ingress lanes");
    let runtime = pool.global().clone();
    let same_global_url = url_with_schema(&f.base_url, &f.target);
    for selection in [
        ResolvedXmppDatabaseUrl::Explicit(same_global_url.clone()),
        ResolvedXmppDatabaseUrl::EphemeralFallback,
    ] {
        let Err(error) = waddle_server::server::create_pubsub_storage(&runtime, &selection).await
        else {
            panic!("attested PubSub must reject explicit and fallback origins");
        };
        assert!(error
            .to_string()
            .contains("inherit the global database location"));
    }
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(pool.ingress()).await,
        Err(PubSubRuntimeAttachError::MainLaneRequired)
    ));
    assert!(DatabasePubSubStorage::attach_existing(runtime.clone())
        .await
        .is_err());
    assert!(
        DatabasePubSubStorage::initialize_owner(runtime.clone(), &identity)
            .await
            .is_err()
    );
    let missing: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.pubsub_nodes", f.target))
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert!(missing.is_none(), "runtime refusal must not create schema");
    let mut owner_url = url::Url::parse(&f.base_url).unwrap();
    let retained: Vec<_> = owner_url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    owner_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(k, v)| (k, v)))
        .append_pair("user", &migrator)
        .append_pair("application_name", &owner)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path={},public", f.target),
        );
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    )
    .await;
    run_sql(&f, "CREATE TABLE public.pubsub_nodes (sentinel integer)").await;
    let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_string());
    owner_config.pool_size = 1;
    let owner_db = Database::from_config("pubsub owner", &owner_config)
        .await
        .expect("owner database");
    run_sql(
        &f,
        &format!("GRANT USAGE ON SCHEMA {} TO {migrator}", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {migrator}"),
    )
    .await;
    let mut wrong_role_url = url::Url::parse(&f.base_url).unwrap();
    let retained: Vec<_> = wrong_role_url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    wrong_role_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", &migrator)
        .append_pair("options", &format!("-c search_path={},public", f.target));
    let mut wrong_role_config =
        DatabaseConfig::new(DatabaseDriver::Postgres, wrong_role_url.to_string());
    wrong_role_config.pool_size = 1;
    let wrong_role_db = Database::from_config("wrong PubSub owner role", &wrong_role_config)
        .await
        .expect("wrong role database");
    assert!(
        DatabasePubSubStorage::initialize_owner(wrong_role_db, &identity)
            .await
            .is_err(),
        "a different current role must not prepare the owner schema"
    );
    let mut wrong_schema_url = url::Url::parse(&f.base_url).unwrap();
    run_sql(&f, &format!("GRANT USAGE ON SCHEMA public TO {owner}")).await;
    let retained: Vec<_> = wrong_schema_url
        .query_pairs()
        .filter(|(key, _)| key != "user" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    wrong_schema_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("user", &migrator)
        .append_pair("options", &format!("-c role={owner} -c search_path=public"));
    let mut wrong_schema_config =
        DatabaseConfig::new(DatabaseDriver::Postgres, wrong_schema_url.to_string());
    wrong_schema_config.pool_size = 1;
    let wrong_schema_db = Database::from_config("wrong PubSub owner schema", &wrong_schema_config)
        .await
        .expect("wrong schema database");
    assert!(
        DatabasePubSubStorage::initialize_owner(wrong_schema_db, &identity)
            .await
            .is_err(),
        "a different active schema must not prepare the owner schema"
    );
    let mut wrong_identity = identity.clone();
    wrong_identity.database.oid += 1;
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &wrong_identity)
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
        .execute("CREATE TEMP TABLE pubsub_nodes (sentinel integer)", ())
        .await
        .unwrap();
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err(),
        "temporary shadow is refused"
    );
    owner_db
        .guard()
        .await
        .unwrap()
        .execute("DROP TABLE pg_temp.pubsub_nodes", ())
        .await
        .unwrap();
    run_sql(
        &f,
        &format!(
            "REVOKE TEMPORARY ON DATABASE {} FROM {owner}",
            f.database_name
        ),
    )
    .await;
    DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
        .await
        .expect("owner setup");
    verify_pubsub_owner_gate_sees_committed_schema_state(&f, &owner_db, &identity, &owner).await;
    let public_sentinel: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass('public.pubsub_nodes')::text")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert!(public_sentinel.is_some(), "fallback table was not touched");
    for (table, rights) in [
        ("pubsub_schema_marker", "SELECT"),
        ("pubsub_schema_version", "SELECT"),
        ("pubsub_nodes", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_items", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_subscriptions", "SELECT, INSERT, DELETE"),
        ("pubsub_affiliations", "SELECT, INSERT, UPDATE, DELETE"),
        ("pubsub_owner_configured", "SELECT, INSERT"),
        ("dnd_projection", "SELECT, INSERT, UPDATE, DELETE"),
        ("dnd_projection_source_version", "SELECT, INSERT, UPDATE"),
        (
            "notification_settings_projection",
            "SELECT, INSERT, UPDATE, DELETE",
        ),
        (
            "notification_settings_projection_source_version",
            "SELECT, INSERT, UPDATE",
        ),
        ("pubsub_push_publications", "SELECT, INSERT, UPDATE"),
    ] {
        run_sql(
            &f,
            &format!("GRANT {rights} ON {}.{table} TO {}", f.target, f.runtime),
        )
        .await;
    }
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON SEQUENCE {}.pubsub_items_seq_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    let attach_lock_function = format!("{}.pubsub_runtime_attach_lock()", f.target);
    run_sql(
        &f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {attach_lock_function} TO {}",
            f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {attach_lock_function} TO {}",
            f.outsider
        ),
    )
    .await;
    match DatabasePubSubStorage::attach_existing(runtime.clone()).await {
        Err(PubSubRuntimeAttachError::InvalidAttachLock) => {}
        Err(error) => panic!("outsider attach-lock grant had wrong refusal: {error:?}"),
        Ok(_) => panic!("runtime attachment accepted an outsider execute grant"),
    }
    run_sql(
        &f,
        &format!(
            "REVOKE EXECUTE ON FUNCTION {attach_lock_function} FROM {}",
            f.outsider
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION {attach_lock_function} TO PUBLIC"),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidAttachLock)
    ));
    run_sql(
        &f,
        &format!("REVOKE EXECUTE ON FUNCTION {attach_lock_function} FROM PUBLIC"),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "REVOKE EXECUTE ON FUNCTION {attach_lock_function} FROM {}",
            f.runtime
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidAttachLock)
    ));
    run_sql(
        &f,
        &format!(
            "GRANT EXECUTE ON FUNCTION {attach_lock_function} TO {}",
            f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("ALTER FUNCTION {attach_lock_function} RESET search_path"),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidAttachLock)
    ));
    run_sql(
        &f,
        &format!("ALTER FUNCTION {attach_lock_function} SET search_path TO pg_catalog, pg_temp"),
    )
    .await;

    let (storage, attached_global) = waddle_server::server::create_pubsub_storage(
        &runtime,
        &ResolvedXmppDatabaseUrl::InheritedGlobal(same_global_url.clone()),
    )
    .await
    .expect("attach inherited global owner-prepared schema");
    assert!(attached_global, "PubSub must share the exact main pool");
    let concurrent_runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "second one-slot PubSub runtime",
        attested_pool(
            f.runtime_options.clone(),
            f.config(),
            1,
            Duration::from_secs(3),
        )
        .expect("second one-slot attested pool"),
    );
    let (first_attach, second_attach) = tokio::join!(
        DatabasePubSubStorage::attach_existing(runtime.clone()),
        DatabasePubSubStorage::attach_existing(concurrent_runtime.clone())
    );
    first_attach.expect("first concurrent runtime attachment");
    second_attach.expect("serialized concurrent runtime attachment");
    run_sql(
        &f,
        &format!("GRANT CREATE ON SCHEMA {} TO {}", f.target, f.runtime),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "runtime role"
        })
    ));
    run_sql(
        &f,
        &format!("REVOKE CREATE ON SCHEMA {} FROM {}", f.target, f.runtime),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT UPDATE ON {}.pubsub_schema_marker TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "pubsub_schema_marker"
        })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE UPDATE ON {}.pubsub_schema_marker FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "UPDATE {}.pubsub_schema_marker SET owner_phase = 0 WHERE marker_id = 1",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidMarker)
    ));
    run_sql(
        &f,
        &format!(
            "UPDATE {}.pubsub_schema_marker SET owner_phase = 1 WHERE marker_id = 1",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "UPDATE {}.pubsub_schema_marker SET schema_name = 'wrong-schema' WHERE marker_id = 1",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidMarker)
    ));
    run_sql(
        &f,
        &format!(
            "UPDATE {}.pubsub_schema_marker SET schema_name = '{}' WHERE marker_id = 1",
            f.target, f.target
        ),
    )
    .await;

    run_concurrent_pubsub_index_race(&f, runtime.clone()).await;

    run_sql(
        &f,
        &format!("DROP INDEX {}.idx_pubsub_items_owner_item", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX idx_pubsub_items_owner_item ON {}.pubsub_items (owner_jid text_pattern_ops, item_id)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidIndex {
            table: "pubsub_items"
        })
    ));
    run_sql(
        &f,
        &format!("DROP INDEX {}.idx_pubsub_items_owner_item", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX idx_pubsub_items_owner_item ON {}.pubsub_items (owner_jid, item_id)",
            f.target
        ),
    )
    .await;

    run_sql(
        &f,
        &format!(
            "CREATE COLLATION {}.pubsub_test_collation (provider = libc, locale = 'C')",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("DROP INDEX {}.idx_pubsub_items_owner_item", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX idx_pubsub_items_owner_item ON {}.pubsub_items (owner_jid COLLATE {}.pubsub_test_collation, item_id)",
            f.target, f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidIndex {
            table: "pubsub_items"
        })
    ));
    run_sql(
        &f,
        &format!("DROP INDEX {}.idx_pubsub_items_owner_item", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX idx_pubsub_items_owner_item ON {}.pubsub_items (owner_jid, item_id)",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "ALTER TABLE {}.pubsub_nodes ALTER COLUMN access_model TYPE text COLLATE {}.pubsub_test_collation",
            f.target, f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidColumn {
            table: "pubsub_nodes",
            column: "access_model"
        })
    ));
    run_sql(
        &f,
        &format!(
            "ALTER TABLE {}.pubsub_nodes ALTER COLUMN access_model TYPE text COLLATE \"default\"",
            f.target
        ),
    )
    .await;

    let alice = "alice@example.com".parse().unwrap();
    storage
        .get_or_create_node(&alice, "ordinary")
        .await
        .expect("create node");
    storage
        .mark_owner_configured(&alice, "ordinary")
        .await
        .expect("mark owner configured");
    let item = PubSubItem {
        id: Some("item-a".to_owned()),
        publisher: None,
        payload: Some(minidom::Element::builder("body", "urn:waddle:test").build()),
    };
    storage
        .publish_item(&alice, "ordinary", &item, Some(&alice), false)
        .await
        .expect("publish");
    storage
        .publish_item(&alice, "ordinary", &item, Some(&alice), false)
        .await
        .expect("republish");
    assert_eq!(
        storage
            .get_items(&alice, "ordinary", None, &[])
            .await
            .unwrap()
            .len(),
        1
    );
    let subscriber = "bob@example.com".parse().unwrap();
    storage
        .subscribe(&alice, "ordinary", &subscriber)
        .await
        .expect("subscribe");
    assert!(storage
        .delete_node(&alice, "ordinary")
        .await
        .expect("delete with FK cascades"));
    let node = PublicationNode::try_new("push-device").unwrap();
    storage
        .get_or_create_node(&alice, node.as_str())
        .await
        .unwrap();
    let push = PubSubItem {
        id: Some("notification".into()),
        publisher: None,
        payload: Some(
            minidom::Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build(),
        ),
    };
    let version = PublicationVersion::try_new(1, uuid::Uuid::new_v4()).unwrap();
    storage
        .publish_push_item_versioned(&alice, &alice, &node, &push, version)
        .await
        .expect("push watermark");
    assert!(matches!(
        storage
            .publish_push_item_versioned(&alice, &alice, &node, &push, version)
            .await
            .unwrap(),
        VersionedPublishResult::AlreadyApplied
    ));
    DatabasePubSubStorage::attach_existing(runtime.clone())
        .await
        .expect("reattach after writes");
    let dnd_node = "urn:waddle:dnd:0";
    storage
        .get_or_create_node(&alice, dnd_node)
        .await
        .expect("DND node");
    let dnd = PubSubItem {
        id: Some("current".into()),
        publisher: None,
        payload: Some(
            minidom::Element::builder("dnd", dnd_node)
                .attr(minidom::rxml::xml_ncname!("timezone").to_owned(), "UTC")
                .build(),
        ),
    };
    storage
        .publish_item(&alice, dnd_node, &dnd, Some(&alice), false)
        .await
        .expect("DND projection write");
    let bookmarks = "urn:xmpp:bookmarks:1";
    storage
        .get_or_create_node(&alice, bookmarks)
        .await
        .expect("bookmark node");
    let bookmark = PubSubItem {
        id: Some("room@muc.example.com".into()),
        publisher: None,
        payload: Some(minidom::Element::builder("conference", bookmarks).build()),
    };
    storage
        .publish_item(&alice, bookmarks, &bookmark, Some(&alice), false)
        .await
        .expect("bookmark projection write");
    DatabasePubSubStorage::attach_existing(runtime.clone())
        .await
        .expect("reattach after projections");

    // Existing inbound FKs may change or block node deletion through internal triggers.
    run_sql(&f, &format!("CREATE TABLE {}.outside_fk (owner_jid text, node_name text, FOREIGN KEY (owner_jid, node_name) REFERENCES {}.pubsub_nodes (owner_jid, node_name) ON DELETE CASCADE)", f.target, f.target)).await;
    run_sql(
        &f,
        &format!(
            "INSERT INTO {}.outside_fk VALUES ('alice@example.com','push-device')",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidTable {
            table: "pubsub_nodes"
        })
    ));
    run_sql(&f, &format!("DROP TABLE {}.outside_fk", f.target)).await;
    for counter in [
        "dnd_projection_source_version",
        "notification_settings_projection_source_version",
    ] {
        run_sql(
            &f,
            &format!(
                "UPDATE {}.{counter} SET current_version = 9223372036854775807",
                f.target
            ),
        )
        .await;
        assert!(
            matches!(DatabasePubSubStorage::attach_existing(runtime.clone()).await, Err(PubSubRuntimeAttachError::InvalidCounter { component }) if component == counter)
        );
        run_sql(
            &f,
            &format!("UPDATE {}.{counter} SET current_version = 1", f.target),
        )
        .await;
    }
    run_sql(
        &f,
        &format!(
            "SELECT pg_catalog.setval('{}.pubsub_items_seq_seq', 1, true)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidSequence)
    ));
    run_sql(&f, &format!("SELECT pg_catalog.setval('{}.pubsub_items_seq_seq', (SELECT max(seq) FROM {}.pubsub_items), true)", f.target, f.target)).await;
    run_sql(
        &f,
        &format!(
            "CREATE INDEX extra_pubsub_index ON {}.pubsub_nodes (created_at_ms)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidIndex {
            table: "pubsub_nodes"
        })
    ));
    run_sql(&f, &format!("DROP INDEX {}.extra_pubsub_index", f.target)).await;
    run_sql(
        &f,
        &format!(
            "GRANT UPDATE ON SEQUENCE {}.pubsub_items_seq_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidSequence)
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE UPDATE ON SEQUENCE {}.pubsub_items_seq_seq FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT DELETE ON {}.pubsub_push_publications TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::UnsafeRuntimePrivileges { .. })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE DELETE ON {}.pubsub_push_publications FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON {}.pubsub_items TO {}",
            f.target, f.outsider
        ),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::UnsafeRuntimePrivileges { .. })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE SELECT ON {}.pubsub_items FROM {}",
            f.target, f.outsider
        ),
    )
    .await;
    run_sql(
        &f,
        &format!("INSERT INTO {}.pubsub_schema_version VALUES (7)", f.target),
    )
    .await;
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::InvalidVersion)
    ));
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err()
    );
    run_sql(
        &f,
        &format!(
            "DELETE FROM {}.pubsub_schema_version WHERE version = 8",
            f.target
        ),
    )
    .await;
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err(),
        "persisted items/watermarks prohibit reset"
    );
    let marker: i32 = sqlx::query_scalar(&format!(
        "SELECT version FROM {}.pubsub_schema_version",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(marker, 7, "owner refusal did not modify version");
    // Remove all core and projection rows, leaving only the push watermark.
    for table in [
        "pubsub_nodes",
        "dnd_projection",
        "dnd_projection_source_version",
        "notification_settings_projection",
        "notification_settings_projection_source_version",
    ] {
        run_sql(&f, &format!("DELETE FROM {}.{table}", f.target)).await;
    }
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err(),
        "watermark alone must prevent reset"
    );
    run_sql(
        &f,
        &format!("UPDATE {}.pubsub_schema_version SET version = 8", f.target),
    )
    .await;
    DatabasePubSubStorage::attach_existing(runtime.clone())
        .await
        .expect("attach after restored grants");
    run_sql(&f, &format!("DROP TABLE {}.pubsub_items", f.target)).await;
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err(),
        "do not recreate items with a surviving watermark"
    );
    let missing: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.pubsub_items", f.target))
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert!(missing.is_none());
    run_sql(
        &f,
        &format!("DELETE FROM {}.pubsub_push_publications", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!("UPDATE {}.pubsub_schema_version SET version = 7", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "CREATE VIEW {}.pubsub_items AS SELECT 1::bigint AS seq WHERE false",
            f.target
        ),
    )
    .await;
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err(),
        "unexpected view must fail before reset"
    );
    let preserved: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.dnd_projection", f.target))
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert!(preserved.is_some(), "existing tables are preserved");
    let marker: i32 = sqlx::query_scalar(&format!(
        "SELECT version FROM {}.pubsub_schema_version",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(marker, 7, "marker is unchanged");
    run_sql(&f, &format!("DROP VIEW {}.pubsub_items", f.target)).await;
    DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
        .await
        .expect("reset an empty old schema");
    let marker: i32 = sqlx::query_scalar(&format!(
        "SELECT version FROM {}.pubsub_schema_version",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(marker, 8);
    run_sql(
        &f,
        &format!("UPDATE {}.pubsub_schema_version SET version = 9", f.target),
    )
    .await;
    assert!(
        DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
            .await
            .is_err(),
        "unknown future version must not reset"
    );
    run_sql(
        &f,
        &format!("UPDATE {}.pubsub_schema_version SET version = 7", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "INSERT INTO {}.pubsub_nodes \
             (owner_jid, node_name, access_model, publish_model, node_type, max_items, \
              persist_items, deliver_payloads, notify_retract, notify_delete, \
              send_last_published_item, created_at_ms) \
             VALUES ('alice@example.com', 'push-device', 'open', 'publish', NULL, 1, 1, 1, 0, 0, 'never', 1)",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "INSERT INTO {}.pubsub_items \
             (owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms) \
             VALUES ('alice@example.com', 'push-device', 'notification', '<notification/>', \
                     'alice@example.com', 1)",
            f.target
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "INSERT INTO {}.pubsub_push_publications \
             (service_jid, node_name, revision, job_token, payload_hash) \
             VALUES ('alice@example.com', 'push-device', 1, '{}', 'digest')",
            f.target,
            uuid::Uuid::new_v4()
        ),
    )
    .await;
    let raw_reset = DatabasePubSubStorage::open(Some(owner_url.as_str()))
        .await
        .expect("legacy raw initialization resets the schema");
    let items_after_reset: i64 =
        sqlx::query_scalar(&format!("SELECT count(*) FROM {}.pubsub_items", f.target))
            .fetch_one(&f.admin)
            .await
            .unwrap();
    let watermarks_after_reset: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.pubsub_push_publications",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(items_after_reset, 0, "schema reset cleared backing items");
    assert_eq!(
        watermarks_after_reset, 0,
        "schema reset cleared replay watermarks with backing items"
    );
    let owner_phase: i64 = sqlx::query_scalar(&format!(
        "SELECT owner_phase FROM {}.pubsub_schema_marker WHERE marker_id = 1",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(owner_phase, 0, "raw initialization revoked completion");
    raw_reset
        .get_or_create_node(&alice, node.as_str())
        .await
        .expect("recreate push node after reset");
    assert!(matches!(
        raw_reset
            .publish_push_item_versioned(&alice, &alice, &node, &push, version)
            .await
            .expect("retry after an empty reset"),
        VersionedPublishResult::Applied(_)
    ));
    let owner_phase: i64 = sqlx::query_scalar(&format!(
        "SELECT owner_phase FROM {}.pubsub_schema_marker WHERE marker_id = 1",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(owner_phase, 0, "raw DML cannot complete owner preparation");
    drop(raw_reset);
    drop(storage);
    drop(concurrent_runtime);
    drop(runtime);
    drop(pool);
    drop(owner_db);
    run_sql(
        &f,
        &format!("REVOKE USAGE ON SCHEMA {} FROM {migrator}", f.target),
    )
    .await;
    run_sql(
        &f,
        &format!("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {migrator}"),
    )
    .await;
    run_sql(&f, &format!("REVOKE {owner} FROM {migrator}")).await;
    run_sql(&f, &format!("DROP SCHEMA {} CASCADE", f.target)).await;
    run_sql(
        &f,
        &format!("REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {owner}"),
    )
    .await;
    run_sql(&f, &format!("REVOKE USAGE ON SCHEMA public FROM {owner}")).await;
    run_sql(&f, &format!("DROP ROLE {migrator}")).await;
    run_sql(&f, &format!("DROP ROLE {owner}")).await;
    f.cleanup().await;
}
