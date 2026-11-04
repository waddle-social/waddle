//! Restricted PostgreSQL PubSub attachment and owner bootstrap.
#![cfg(feature = "test-support")]

use sqlx::postgres::{PgConnectOptions, PgPool};
use std::str::FromStr;
use std::time::Duration;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use waddle_server::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::pubsub::{DatabasePubSubStorage, PubSubRuntimeAttachError};
use waddle_xmpp::pubsub::{
    PubSubItem, PubSubStorage, PublicationNode, PublicationVersion, VersionedPublishResult,
};

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
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(pool.ingress()).await,
        Err(PubSubRuntimeAttachError::MainLaneRequired)
    ));
    assert!(matches!(
        DatabasePubSubStorage::attach_existing(runtime.clone()).await,
        Err(PubSubRuntimeAttachError::Relation { .. })
    ));
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
    let public_sentinel: Option<String> =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass('public.pubsub_nodes')::text")
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert!(public_sentinel.is_some(), "fallback table was not touched");
    for (table, rights) in [
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
    let storage = DatabasePubSubStorage::attach_existing(runtime.clone())
        .await
        .expect("attach owner-prepared schema");
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
    drop(storage);
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
