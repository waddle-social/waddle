//! Round-trip tests for the SQLite-backed [`SpacesMetadataStore`].

use jid::BareJid;

use crate::space_identity::SpaceNode;

use super::{
    DatabaseSpacesMetadataStore, SpaceMetadata, SpaceStoreAttachError, SpacesMetadataStore,
};

fn space_jid(local: &str) -> BareJid {
    format!("{local}@spaces.localhost")
        .parse()
        .expect("valid space JID")
}

fn metadata(
    space_jid: BareJid,
    name: &str,
    description: Option<&str>,
    icon_url: Option<&str>,
    created_at: i64,
    updated_at: i64,
) -> SpaceMetadata {
    let space_node = SpaceNode::from(space_jid.node().expect("space node").to_string());
    SpaceMetadata {
        space_jid,
        space_node,
        name: name.to_string(),
        description: description.map(str::to_string),
        icon_url: icon_url.map(str::to_string),
        created_at,
        updated_at,
    }
}

async fn fresh_store() -> DatabaseSpacesMetadataStore {
    DatabaseSpacesMetadataStore::open(Some("sqlite::memory:"))
        .await
        .expect("open in-memory spaces_metadata store")
}

#[tokio::test]
async fn upsert_then_get_round_trips_full_row() {
    let store = fresh_store().await;
    let row = metadata(
        space_jid("design"),
        "Design",
        Some("Shared design space"),
        Some("https://cdn.example/design.png"),
        1_700_000_000,
        1_700_000_000,
    );

    store.upsert(&row).await.expect("upsert");

    let fetched = store
        .get(&row.space_jid)
        .await
        .expect("get")
        .expect("row present after upsert");
    assert_eq!(fetched, row);

    let fetched_by_node = store
        .get_by_node(&row.space_node)
        .await
        .expect("get_by_node")
        .expect("row present after upsert");
    assert_eq!(fetched_by_node, row);
}

#[tokio::test]
async fn upsert_overwrites_existing_row_and_updates_timestamp() {
    let store = fresh_store().await;
    let initial = metadata(
        space_jid("design"),
        "Design",
        Some("v1 desc"),
        Some("https://cdn.example/v1.png"),
        1_700_000_000,
        1_700_000_000,
    );
    store.upsert(&initial).await.expect("initial upsert");

    let updated = SpaceMetadata {
        name: "Design Studio".to_string(),
        description: Some("v2 desc".to_string()),
        icon_url: None,
        updated_at: 1_700_000_500,
        // created_at is left at its original value — admin V2 callers
        // pass through the existing value on update.
        ..initial.clone()
    };
    store.upsert(&updated).await.expect("re-upsert");

    let fetched = store
        .get(&initial.space_jid)
        .await
        .expect("get")
        .expect("row present after re-upsert");
    assert_eq!(fetched.name, "Design Studio");
    assert_eq!(fetched.description.as_deref(), Some("v2 desc"));
    assert!(fetched.icon_url.is_none(), "icon cleared on update");
    assert_eq!(fetched.updated_at, 1_700_000_500);
    assert_eq!(
        fetched.created_at, 1_700_000_000,
        "created_at preserved across overwrite"
    );
}

#[tokio::test]
async fn delete_returns_true_when_row_present() {
    let store = fresh_store().await;
    let row = metadata(
        space_jid("ops"),
        "Ops",
        None,
        None,
        1_700_000_000,
        1_700_000_000,
    );
    store.upsert(&row).await.expect("upsert");

    let removed = store.delete(&row.space_jid).await.expect("delete");
    assert!(removed, "delete returns true when a row was removed");
    let after = store.get(&row.space_jid).await.expect("get");
    assert!(after.is_none(), "row absent after delete");
}

#[tokio::test]
async fn delete_by_node_returns_true_when_row_present() {
    let store = fresh_store().await;
    let row = metadata(
        space_jid("ops"),
        "Ops",
        None,
        None,
        1_700_000_000,
        1_700_000_000,
    );
    store.upsert(&row).await.expect("upsert");

    let removed = store
        .delete_by_node(&row.space_node)
        .await
        .expect("delete_by_node");
    assert!(
        removed,
        "delete_by_node returns true when a row was removed"
    );
    let after = store
        .get_by_node(&row.space_node)
        .await
        .expect("get_by_node");
    assert!(after.is_none(), "row absent after delete_by_node");
}

#[tokio::test]
async fn delete_returns_false_when_row_absent() {
    let store = fresh_store().await;
    let removed = store
        .delete(&space_jid("ghost"))
        .await
        .expect("delete missing");
    assert!(!removed, "delete on missing row returns false");
}

#[tokio::test]
async fn get_returns_none_for_unknown_jid() {
    let store = fresh_store().await;
    let result = store.get(&space_jid("ghost")).await.expect("get");
    assert!(result.is_none(), "get returns None for unknown space JID");
}

#[tokio::test]
async fn list_all_returns_rows_in_created_at_ascending_order() {
    let store = fresh_store().await;
    let alpha = metadata(
        space_jid("alpha"),
        "Alpha",
        None,
        None,
        1_700_000_000,
        1_700_000_000,
    );
    let beta = metadata(
        space_jid("beta"),
        "Beta",
        None,
        None,
        1_700_000_100,
        1_700_000_100,
    );
    let gamma = metadata(
        space_jid("gamma"),
        "Gamma",
        None,
        None,
        1_700_000_200,
        1_700_000_200,
    );

    // Insert out of order to confirm ordering is by `created_at`, not by
    // physical insertion order.
    store.upsert(&beta).await.expect("upsert beta");
    store.upsert(&gamma).await.expect("upsert gamma");
    store.upsert(&alpha).await.expect("upsert alpha");

    let rows = store.list_all().await.expect("list_all");
    assert_eq!(rows, vec![alpha, beta, gamma]);
}

#[tokio::test]
async fn list_all_on_empty_store_returns_empty_vec() {
    let store = fresh_store().await;
    let rows = store.list_all().await.expect("list_all empty");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn initialize_owner_uses_the_supplied_database() {
    let database = crate::db::Database::in_memory("spaces-metadata-owner-test")
        .await
        .expect("create owner database");
    let store = DatabaseSpacesMetadataStore::initialize_owner(database)
        .await
        .expect("initialize exact owner database");

    assert_eq!(store.database().name(), "spaces-metadata-owner-test");
    let row = metadata(
        space_jid("design"),
        "Design",
        None,
        None,
        1_700_000_000,
        1_700_000_000,
    );
    store
        .upsert(&row)
        .await
        .expect("write owner-prepared table");
    assert_eq!(
        store.get(&row.space_jid).await.expect("read row"),
        Some(row)
    );
}

#[tokio::test]
async fn attach_existing_rejects_sqlite_without_creating_schema() {
    let database = crate::db::Database::in_memory("spaces-metadata-attach-test")
        .await
        .expect("create database");

    assert!(matches!(
        DatabaseSpacesMetadataStore::attach_existing(database.clone()).await,
        Err(SpaceStoreAttachError::PostgresRequired)
    ));

    let mut rows = database
        .guard()
        .await
        .expect("acquire database")
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'spaces_metadata'",
            (),
        )
        .await
        .expect("inspect schema");
    assert!(rows.next().await.expect("read schema").is_none());
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn postgres_attach_existing_accepts_owner_prepared_stores_and_rejects_drift() {
    use std::str::FromStr;
    use std::time::Duration;

    use anyhow::{bail, Result};
    use sqlx::postgres::{PgConnectOptions, PgPool};

    use crate::channel_space_links::{
        ChannelSpaceLink, ChannelSpaceLinkStore, DatabaseChannelSpaceLinkStore,
    };
    use crate::config::LineageConfig;
    use crate::db::lineage::{self, DeploymentUuid};
    use crate::db::physical_attestation::{
        attested_pool, install, PgRoleName, PhysicalAttestationConfig,
    };
    use crate::db::{Database, DatabaseConfig, DatabaseDriver};
    use crate::space_identity::SpaceNode;

    struct Fixture {
        cluster_admin: PgPool,
        admin: PgPool,
        base_url: String,
        admin_login: String,
        database: String,
        owner: String,
        definer: String,
        installer: String,
        runtime: String,
        target: String,
        trusted: String,
        had_public_control_execute: bool,
        operation_id: uuid::Uuid,
    }

    impl Fixture {
        async fn new(url: String) -> Self {
            let cluster_admin = PgPool::connect(&url)
                .await
                .expect("connect disposable PostgreSQL cluster");
            let admin_login: String = sqlx::query_scalar("SELECT session_user::text")
                .fetch_one(&cluster_admin)
                .await
                .expect("read test cluster login");
            let public_control_execute: bool = sqlx::query_scalar(
                "SELECT EXISTS (\
                   SELECT 1 FROM pg_catalog.pg_proc p \
                   CROSS JOIN LATERAL pg_catalog.aclexplode( \
                     COALESCE(p.proacl, pg_catalog.acldefault('f', p.proowner)) \
                   ) acl \
                   WHERE p.oid = 'pg_catalog.pg_control_system()'::regprocedure \
                     AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE' \
                 )",
            )
            .fetch_one(&cluster_admin)
            .await
            .expect("capture original control-function public ACL");

            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let database = format!("space_store_attach_{suffix}");
            let owner = format!("space_store_o_{suffix}");
            let definer = format!("space_store_d_{suffix}");
            let installer = format!("space_store_i_{suffix}");
            let runtime = format!("space_store_r_{suffix}");
            let target = format!("space_store_t_{suffix}");
            let trusted = format!("space_store_f_{suffix}");

            sqlx::query(&format!("CREATE DATABASE {database}"))
                .execute(&cluster_admin)
                .await
                .expect("create isolated test database");
            sqlx::query(&format!(
                "REVOKE CREATE, TEMPORARY ON DATABASE {database} FROM PUBLIC"
            ))
            .execute(&cluster_admin)
            .await
            .expect("restrict database defaults");
            for role in [&owner, &definer] {
                sqlx::query(&format!(
                    "CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
                ))
                .execute(&cluster_admin)
                .await
                .expect("create no-login owner or definer role");
            }
            for role in [&installer, &runtime] {
                sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
                    .execute(&cluster_admin)
                    .await
                    .expect("create test login role");
            }
            sqlx::query(&format!(
                "GRANT {owner} TO \"{}\"",
                admin_login.replace('"', "\"\"")
            ))
            .execute(&cluster_admin)
            .await
            .expect("allow test administrator to assume owner role");
            sqlx::query(&format!(
                "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
            ))
            .execute(&cluster_admin)
            .await
            .expect("grant restricted installer handoff");

            let base_url = space_attach_url_with_database(&url, &database);
            let admin = PgPool::connect(&base_url)
                .await
                .expect("connect isolated database administrator");
            sqlx::query("REVOKE ALL ON SCHEMA public FROM PUBLIC")
                .execute(&admin)
                .await
                .expect("restrict public schema");
            sqlx::query(&format!("CREATE SCHEMA {target} AUTHORIZATION {owner}"))
                .execute(&admin)
                .await
                .expect("create owner-controlled schema");
            sqlx::query(&format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"))
                .execute(&admin)
                .await
                .expect("create trusted definer schema");
            for role in [&runtime, &definer] {
                sqlx::query(&format!("GRANT USAGE ON SCHEMA {target} TO {role}"))
                    .execute(&admin)
                    .await
                    .expect("grant only target-schema usage");
            }

            Self {
                cluster_admin,
                admin,
                base_url,
                admin_login,
                database,
                owner,
                definer,
                installer,
                runtime,
                target,
                trusted,
                had_public_control_execute: public_control_execute,
                operation_id: uuid::Uuid::new_v4(),
            }
        }

        async fn exercise(&self) -> Result<()> {
            let owner_url =
                space_attach_url_with_role_and_schema(&self.base_url, &self.owner, &self.target);
            let owner_database = Database::from_config(
                "space-store owner",
                &DatabaseConfig::new(DatabaseDriver::Postgres, owner_url),
            )
            .await?;
            let lineage = lineage::enroll(
                &owner_database,
                &LineageConfig {
                    deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                    action: None,
                },
            )
            .await?;
            let owner_spaces =
                DatabaseSpacesMetadataStore::initialize_owner(owner_database.clone()).await?;
            let owner_links =
                DatabaseChannelSpaceLinkStore::initialize_owner(owner_database.clone()).await?;

            let attestation_config = PhysicalAttestationConfig::new(
                lineage,
                PgRoleName::new(self.installer.clone())?,
                PgRoleName::new(self.runtime.clone())?,
                PgRoleName::new(self.definer.clone())?,
                crate::db::lineage::PgSchemaName(self.trusted.clone()),
            )?;
            for statement in [
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
                format!(
                    "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}",
                    self.definer
                ),
                format!(
                    "GRANT SELECT ON {}._lineage TO {}",
                    self.target, self.definer
                ),
            ] {
                sqlx::query(&statement).execute(&self.admin).await?;
            }
            let boundary_url = space_attach_url_with_schema(&self.base_url, &self.target);
            let installer_options =
                PgConnectOptions::from_str(&boundary_url)?.username(&self.installer);
            install(
                &installer_options,
                attestation_config.clone(),
                self.operation_id,
            )
            .await?;

            for table in ["spaces_metadata", "channel_space_links"] {
                sqlx::query(&format!(
                    "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {}",
                    self.target, self.runtime
                ))
                .execute(&self.admin)
                .await?;
            }

            let table_owners_are_no_login: bool = sqlx::query_scalar(
                "SELECT bool_and(owner.rolname = $1 AND NOT owner.rolcanlogin) \
                 FROM pg_catalog.pg_class relation \
                 JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                 JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                 WHERE namespace.nspname = $2 \
                   AND relation.relname IN ('spaces_metadata', 'channel_space_links') \
                   AND relation.relkind = 'r'",
            )
            .bind(&self.owner)
            .bind(&self.target)
            .fetch_one(&self.admin)
            .await?;
            if !table_owners_are_no_login {
                bail!("owner-prepared space tables are not owned by the NOLOGIN role");
            }

            let runtime_options =
                PgConnectOptions::from_str(&boundary_url)?.username(&self.runtime);
            let attested = attested_pool(
                runtime_options,
                attestation_config,
                1,
                Duration::from_secs(3),
            )?;
            let runtime_database = crate::test_support::database_from_attested_postgres_pool(
                "space-store runtime",
                attested,
            );
            let attached_spaces =
                DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await?;
            let attached_links =
                DatabaseChannelSpaceLinkStore::attach_existing(runtime_database.clone()).await?;

            let metadata_row = metadata(
                space_jid("pg-space"),
                "PostgreSQL space",
                Some("owner prepared"),
                None,
                1_800_000_000,
                1_800_000_000,
            );
            attached_spaces.upsert(&metadata_row).await?;
            if attached_spaces.get(&metadata_row.space_jid).await? != Some(metadata_row.clone()) {
                bail!("attested runtime could not round-trip spaces metadata DML");
            }
            if !attached_spaces.delete(&metadata_row.space_jid).await? {
                bail!("attested runtime could not delete spaces metadata");
            }

            let channel: jid::BareJid = "general@muc.localhost".parse()?;
            let space = space_jid("pg-space");
            let link = ChannelSpaceLink {
                channel_jid: channel.clone(),
                space_node: SpaceNode::from(space.node().expect("space node from JID").to_string()),
                space_jid: space,
                created_at: 1_800_000_001,
            };
            attached_links.set(&link).await?;
            if attached_links.get(&channel).await? != Some(link) {
                bail!("attested runtime could not round-trip channel-space link DML");
            }
            if !attached_links.clear(&channel).await? {
                bail!("attested runtime could not delete channel-space link");
            }

            drop(owner_spaces);
            drop(owner_links);
            drop(owner_database);
            drop(attached_spaces);
            drop(attached_links);

            sqlx::query(&format!(
                "DROP INDEX {}.idx_spaces_metadata_created_at",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::UnexpectedIndex {
                    table: "spaces_metadata",
                }) => {}
                Err(error) => bail!("missing spaces_metadata index had wrong error: {error}"),
                Ok(_) => bail!("missing spaces_metadata index was accepted"),
            }

            sqlx::query(&format!(
                "GRANT TRUNCATE ON {}.channel_space_links TO {}",
                self.target, self.runtime
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseChannelSpaceLinkStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::UnsafeRuntimeRole) => {}
                Err(error) => bail!("unsafe channel_space_links grant had wrong error: {error}"),
                Ok(_) => bail!("unsafe channel_space_links grant was accepted"),
            }

            drop(runtime_database);
            Ok(())
        }

        async fn cleanup(self) {
            self.admin.close().await;
            sqlx::query(&format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
                self.definer
            ))
            .execute(&self.cluster_admin)
            .await
            .expect("remove disposable definer function grant");
            let restore_public = if self.had_public_control_execute {
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO PUBLIC"
            } else {
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC"
            };
            sqlx::query(restore_public)
                .execute(&self.cluster_admin)
                .await
                .expect("restore original control-function public ACL");
            sqlx::query(&format!(
                "REVOKE {} FROM \"{}\"",
                self.owner,
                self.admin_login.replace('"', "\"\"")
            ))
            .execute(&self.cluster_admin)
            .await
            .expect("remove disposable owner membership from test login");
            sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
                .execute(&self.cluster_admin)
                .await
                .expect("drop isolated test database");
            for role in [&self.runtime, &self.installer, &self.owner, &self.definer] {
                sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                    .execute(&self.cluster_admin)
                    .await
                    .expect("drop disposable test role");
            }
            self.cluster_admin.close().await;
        }
    }

    static CONTROL_CATALOG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _serial = CONTROL_CATALOG_LOCK.lock().await;
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping PostgreSQL space-store attachment test: test URL not set");
        return;
    };
    let fixture = Fixture::new(url).await;
    let result = fixture.exercise().await;
    fixture.cleanup().await;
    result.expect("owner-prepared PostgreSQL space-store attach contract");
}

#[cfg(feature = "test-support")]
fn space_attach_url_with_database(base: &str, database: &str) -> String {
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

#[cfg(feature = "test-support")]
fn space_attach_url_with_schema(base: &str, schema: &str) -> String {
    space_attach_url_with_options(base, &format!("-c search_path={schema}"))
}

#[cfg(feature = "test-support")]
fn space_attach_url_with_role_and_schema(base: &str, role: &str, schema: &str) -> String {
    space_attach_url_with_options(base, &format!("-c role={role} -c search_path={schema}"))
}

#[cfg(feature = "test-support")]
fn space_attach_url_with_options(base: &str, options: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", options);
    url.to_string()
}
