//! Restricted PostgreSQL claim epoch and occupancy attachment.
#![cfg(all(feature = "test-support", feature = "clustering"))]

use sqlx::postgres::{PgConnectOptions, PgPool};
use std::str::FromStr;
use std::time::Duration;
use waddle_server::clustering::claims::{
    attach_main_claim_occupancy, initialize_fresh_claims_owner, ClaimOccupancyAttachError,
    ClaimOwnerPreparationError, PostgresClaimStore,
};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use waddle_server::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::occupancy_authority;
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, NodeIdentity};
use waddle_xmpp_core::OccupancySessionGeneration;

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
#[tokio::test]
async fn claims_and_occupancy_attach_on_exact_main_pool_and_preserve_epochs() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url).await;
    let identity = f.lineage.postgres_identity.as_ref().unwrap().clone();
    let owner = format!("claim_owner_{}", uuid::Uuid::new_v4().simple());
    let migrator = format!("claim_migrator_{}", uuid::Uuid::new_v4().simple());
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
        .unwrap();
    let main = attested_pool(
        f.runtime_options.clone(),
        config.clone(),
        3,
        Duration::from_secs(3),
    )
    .unwrap();
    let control = attested_pool(
        f.runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(3),
    )
    .unwrap();
    let other =
        attested_pool(f.runtime_options.clone(), config, 2, Duration::from_secs(3)).unwrap();
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pools(
        "claim and occupancy",
        main,
        Some((control, 1)),
        2,
        1,
    )
    .await
    .unwrap();
    let other_pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "other", other, 1, 1,
    )
    .await
    .unwrap();
    let runtime = pool.global().clone();
    assert!(matches!(
        attach_main_claim_occupancy(pool.ingress()).await,
        Err(ClaimOccupancyAttachError::MainLaneRequired)
    ));
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::Relation { .. })
    ));
    assert!(PostgresClaimStore::new(runtime.clone())
        .ensure_schema()
        .await
        .is_err());
    assert!(initialize_fresh_claims_owner(&runtime, &identity)
        .await
        .is_err());
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
        );
    run_sql(
        &f,
        &format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
    )
    .await;
    run_sql(
        &f,
        "CREATE TABLE public.clustering_claims (sentinel integer)",
    )
    .await;
    let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    owner_config.pool_size = 1;
    let owner_db = Database::from_config("claim owner", &owner_config)
        .await
        .unwrap();
    let mut wrong = identity.clone();
    wrong.database.oid += 1;
    assert!(initialize_fresh_claims_owner(&owner_db, &wrong)
        .await
        .is_err());
    run_sql(
        &f,
        &format!("GRANT TEMPORARY ON DATABASE {} TO {owner}", f.database_name),
    )
    .await;
    owner_db
        .guard()
        .await
        .unwrap()
        .execute("CREATE TEMP TABLE clustering_claims (sentinel integer)", ())
        .await
        .unwrap();
    assert!(initialize_fresh_claims_owner(&owner_db, &identity)
        .await
        .is_err());
    owner_db
        .guard()
        .await
        .unwrap()
        .execute("DROP TABLE pg_temp.clustering_claims", ())
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
    owner_db.guard().await.unwrap().execute("CREATE TABLE xmpp_occupancy_authority (full_jid TEXT PRIMARY KEY, generation TEXT NOT NULL)", ()).await.unwrap();
    owner_db
        .guard()
        .await
        .unwrap()
        .execute(
            "INSERT INTO xmpp_occupancy_authority VALUES ('alice@example.test/device', 'retained')",
            (),
        )
        .await
        .unwrap();
    run_sql(
        &f,
        &format!(
            "CREATE TABLE {}.clustering_nodes (sentinel integer)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        initialize_fresh_claims_owner(&owner_db, &identity).await,
        Err(ClaimOwnerPreparationError::ExistingClaimAuthority)
    ));
    run_sql(&f, &format!("DROP TABLE {}.clustering_nodes", f.target)).await;
    assert!(matches!(
        initialize_fresh_claims_owner(&owner_db, &identity).await,
        Err(ClaimOwnerPreparationError::ExistingClaimAuthority)
    ));
    let retained: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.xmpp_occupancy_authority",
        f.target
    ))
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(retained, 1);
    owner_db
        .guard()
        .await
        .unwrap()
        .execute("DELETE FROM xmpp_occupancy_authority", ())
        .await
        .unwrap();
    initialize_fresh_claims_owner(&owner_db, &identity)
        .await
        .expect("fresh claim authority");
    assert!(matches!(
        initialize_fresh_claims_owner(&owner_db, &identity).await,
        Err(ClaimOwnerPreparationError::ExistingClaimAuthority)
    ));
    let sentinel: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass('public.clustering_claims')::text")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert!(sentinel.is_some());
    // V0013 owner-migration shape was installed above; this node table is
    // only the acquire fixture and is not part of the verified capability.
    owner_db.guard().await.unwrap().execute("CREATE TABLE clustering_nodes (node_id TEXT PRIMARY KEY, node_epoch TEXT NOT NULL, draining BOOLEAN NOT NULL DEFAULT FALSE)", ()).await.unwrap();
    run_sql(
        &f,
        &format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.clustering_claims TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT, INSERT, UPDATE ON {}.xmpp_occupancy_authority TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON {}.clustering_nodes TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT, USAGE ON SEQUENCE {}.clustering_claim_epoch_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    let proof = attach_main_claim_occupancy(runtime.clone())
        .await
        .expect("verified main authority");
    assert!(proof.belongs_to_main_pool(&runtime));
    assert!(!proof.belongs_to_main_pool(&pool.ingress()));
    assert!(!proof.belongs_to_main_pool(other_pool.global()));
    let store = std::sync::Arc::new(PostgresClaimStore::new(runtime.clone()));
    let entity = Entity::new(
        EntityType::RoomActor,
        "room@conference.example.test".to_owned(),
    );
    let node = NodeIdentity::new("node", "epoch");
    let first = store
        .acquire(&entity, &node)
        .await
        .expect("acquire first epoch");
    let mut tx = runtime.begin().await.unwrap();
    tx.query(
        "SELECT 1 FROM clustering_claims WHERE entity = ? AND claim_epoch = ? FOR SHARE",
        waddle_server::db_params!["room_actor:room@conference.example.test", first.0],
    )
    .await
    .unwrap();
    let release_store = store.clone();
    let release_entity = entity.clone();
    let release_node = node.clone();
    let mut release = tokio::spawn(async move {
        release_store
            .release_exact(&release_entity, &release_node, first)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut release)
            .await
            .is_err(),
        "claim release waits for main transaction"
    );
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), release)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let second = store.acquire(&entity, &node).await.expect("reacquire");
    assert!(second.0 > first.0);
    assert!(!store.fence(&entity, &node, first).await.unwrap());
    let jid = "alice@example.test/device".parse().unwrap();
    let g1 = OccupancySessionGeneration::mint();
    let g2 = OccupancySessionGeneration::mint();
    occupancy_authority::publish(&runtime, &jid, g1)
        .await
        .unwrap();
    let held = occupancy_authority::acquire_current(&runtime, &jid, g1)
        .await
        .unwrap()
        .unwrap();
    let later_db = runtime.clone();
    let later_jid = jid.clone();
    let mut later =
        tokio::spawn(async move { occupancy_authority::publish(&later_db, &later_jid, g2).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut later)
            .await
            .is_err(),
        "new generation waits for held authority"
    );
    drop(held);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), later)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Some(g1)
    );
    assert!(!occupancy_authority::is_current(&runtime, &jid, g1)
        .await
        .unwrap());
    assert!(occupancy_authority::is_current(&runtime, &jid, g2)
        .await
        .unwrap());
    run_sql(
        &f,
        &format!(
            "SELECT pg_catalog.setval('{}.clustering_claim_epoch_seq', 1, true)",
            f.target
        ),
    )
    .await;
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::InvalidClaimSequence)
    ));
    run_sql(
        &f,
        &format!(
            "SELECT pg_catalog.setval('{}.clustering_claim_epoch_seq', {}, true)",
            f.target, second.0
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "SELECT pg_catalog.setval('{}.clustering_claim_epoch_seq', {}, false)",
            f.target, second.0
        ),
    )
    .await;
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::InvalidClaimSequence)
    ));
    run_sql(
        &f,
        &format!(
            "SELECT pg_catalog.setval('{}.clustering_claim_epoch_seq', {}, true)",
            f.target, second.0
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "REVOKE USAGE ON SEQUENCE {}.clustering_claim_epoch_seq FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::InvalidClaimSequence)
    ));
    run_sql(
        &f,
        &format!(
            "GRANT USAGE ON SEQUENCE {}.clustering_claim_epoch_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT UPDATE ON SEQUENCE {}.clustering_claim_epoch_seq TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::InvalidClaimSequence)
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE UPDATE ON SEQUENCE {}.clustering_claim_epoch_seq FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "REVOKE UPDATE ON {}.xmpp_occupancy_authority FROM {}",
            f.target, f.runtime
        ),
    )
    .await;
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::UnsafeRuntimePrivileges { .. })
    ));
    run_sql(
        &f,
        &format!(
            "GRANT UPDATE ON {}.xmpp_occupancy_authority TO {}",
            f.target, f.runtime
        ),
    )
    .await;
    run_sql(
        &f,
        &format!(
            "GRANT SELECT ON {}.clustering_claims TO {}",
            f.target, f.outsider
        ),
    )
    .await;
    assert!(matches!(
        attach_main_claim_occupancy(runtime.clone()).await,
        Err(ClaimOccupancyAttachError::UnsafeRuntimePrivileges { .. })
    ));
    run_sql(
        &f,
        &format!(
            "REVOKE SELECT ON {}.clustering_claims FROM {}",
            f.target, f.outsider
        ),
    )
    .await;
    attach_main_claim_occupancy(runtime.clone())
        .await
        .expect("restored authority");
    run_sql(&f, &format!("DROP TABLE {}.clustering_claims", f.target)).await;
    assert!(matches!(
        initialize_fresh_claims_owner(&owner_db, &identity).await,
        Err(ClaimOwnerPreparationError::ExistingClaimAuthority)
    ));
    let remaining: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.clustering_claim_epoch_seq", f.target))
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert!(remaining.is_some());
    drop(proof);
    drop(store);
    drop(runtime);
    drop(pool);
    drop(other_pool);
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
