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

    use anyhow::{bail, Context, Result};
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
            .fetch_one(&admin)
            .await
            .expect("capture disposable database control-function ACL");
            sqlx::query(&format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"
            ))
            .execute(&admin)
            .await
            .expect("allow owner to verify its physical identity in disposable database");
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
            let owner_guard = owner_database.guard().await?;
            owner_guard
                .execute(
                    "CREATE TABLE spaces_metadata (\
                       space_jid TEXT PRIMARY KEY, space_node TEXT, name TEXT NOT NULL, description TEXT, \
                       icon_url TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL\
                     )",
                    (),
                )
                .await?;
            owner_guard
                .execute(
                    "INSERT INTO spaces_metadata \
                     (space_jid, name, description, icon_url, created_at, updated_at) \
                     VALUES ('design@spaces.localhost', 'Design', NULL, NULL, 1700000000, 1700000001), \
                            ('design@archive.localhost', 'Archived Design', NULL, NULL, 1700000002, 1700000003)",
                    (),
                )
                .await?;
            owner_guard
                .execute(
                    "CREATE INDEX idx_spaces_metadata_created_at \
                     ON spaces_metadata (created_at ASC, space_jid ASC)",
                    (),
                )
                .await?;
            owner_guard
                .execute(
                    "CREATE TABLE channel_space_links (\
                       channel_jid TEXT PRIMARY KEY, space_jid TEXT NOT NULL, space_node TEXT, \
                       created_at INTEGER NOT NULL\
                     )",
                    (),
                )
                .await?;
            owner_guard
                .execute(
                    "INSERT INTO channel_space_links (channel_jid, space_jid, created_at) \
                     VALUES ('general@muc.localhost', 'design@spaces.localhost', 1700000004)",
                    (),
                )
                .await?;
            drop(owner_guard);
            let lineage = lineage::enroll(
                &owner_database,
                &LineageConfig {
                    deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
                    action: None,
                },
            )
            .await?;
            let identity = lineage
                .postgres_identity
                .clone()
                .ok_or_else(|| anyhow::anyhow!("PostgreSQL lineage has no identity"))?;
            sqlx::query(&format!("GRANT {} TO {}", self.definer, self.owner))
                .execute(&self.admin)
                .await?;
            if !matches!(
                crate::spaces_metadata::prepare_space_stores_owner(
                    owner_database.clone(),
                    &identity,
                )
                .await,
                Err(SpaceStoreAttachError::UnsafeOwnerRole)
            ) {
                bail!("owner preparation accepted an owner role with inherited privileges");
            }
            sqlx::query(&format!("REVOKE {} FROM {}", self.definer, self.owner))
                .execute(&self.admin)
                .await?;
            let owner_guard = owner_database.guard().await?;
            let mut rows = owner_guard
                .query(
                    "SELECT (SELECT COUNT(*)::bigint FROM spaces_metadata), \
                            (SELECT COUNT(*)::bigint FROM channel_space_links)",
                    (),
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("owner membership recheck returned no row"))?;
            if row.get::<i64>(0)? != 2 || row.get::<i64>(1)? != 1 {
                bail!("rejected owner membership changed legacy store rows");
            }
            drop(rows);
            drop(owner_guard);
            let owner_guard = owner_database.guard().await?;
            owner_guard
                .execute("ALTER TABLE spaces_metadata ADD COLUMN owner_note TEXT", ())
                .await?;
            owner_guard
                .execute(
                    "UPDATE spaces_metadata SET owner_note = name || '-preserve'",
                    (),
                )
                .await?;
            drop(owner_guard);
            let mut wrong_identity = identity.clone();
            wrong_identity.schema.oid = wrong_identity.schema.oid.wrapping_add(1);
            if !matches!(
                crate::spaces_metadata::prepare_space_stores_owner(
                    owner_database.clone(),
                    &wrong_identity,
                )
                .await,
                Err(SpaceStoreAttachError::OwnerIdentityMismatch)
            ) {
                bail!("owner preparation accepted a mismatched schema identity");
            }
            if crate::spaces_metadata::prepare_space_stores_owner(owner_database.clone(), &identity)
                .await
                .is_ok()
            {
                bail!("owner preparation accepted an unknown legacy metadata column");
            }
            let owner_guard = owner_database.guard().await?;
            let mut preserved_column = owner_guard
                .query(
                    "SELECT COUNT(*)::bigint FROM spaces_metadata \
                     WHERE owner_note IN ('Design-preserve', 'Archived Design-preserve')",
                    (),
                )
                .await?;
            let preserved_column_count: i64 = preserved_column
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("legacy-column recheck returned no row"))?
                .get(0)?;
            if preserved_column_count != 2 {
                bail!("failed owner prep did not preserve unknown legacy column values");
            }
            owner_guard
                .execute("ALTER TABLE spaces_metadata DROP COLUMN owner_note", ())
                .await?;
            owner_guard
                .execute(
                    "CREATE TABLE spaces_metadata_new (recoverable TEXT NOT NULL)",
                    (),
                )
                .await?;
            owner_guard
                .execute(
                    "INSERT INTO spaces_metadata_new (recoverable) VALUES ('keep')",
                    (),
                )
                .await?;
            drop(owner_guard);
            if crate::spaces_metadata::prepare_space_stores_owner(owner_database.clone(), &identity)
                .await
                .is_ok()
            {
                bail!("owner preparation overwrote existing rebuild scratch data");
            }
            let owner_guard = owner_database.guard().await?;
            let mut scratch = owner_guard
                .query("SELECT recoverable FROM spaces_metadata_new", ())
                .await?;
            let scratch_row = scratch
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("owner prep lost existing rebuild scratch data"))?;
            if scratch_row.get::<String>(0)? != "keep" {
                bail!("owner prep modified existing rebuild scratch data");
            }
            drop(scratch);
            owner_guard
                .execute("DROP TABLE spaces_metadata_new", ())
                .await?;
            drop(owner_guard);
            if crate::spaces_metadata::prepare_space_stores_owner(owner_database.clone(), &identity)
                .await
                .is_ok()
            {
                bail!("owner preparation silently dropped colliding legacy space rows");
            }
            let owner_guard = owner_database.guard().await?;
            let mut duplicate_rows = owner_guard
                .query(
                    "SELECT COUNT(*)::bigint FROM spaces_metadata WHERE name IN \
                     ('Design', 'Archived Design')",
                    (),
                )
                .await?;
            let duplicate_count: i64 = duplicate_rows
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("duplicate-row recheck returned no row"))?
                .get(0)?;
            drop(duplicate_rows);
            if duplicate_count != 2 {
                bail!("failed owner prep did not preserve both colliding legacy rows");
            }
            owner_guard
                .execute(
                    "DELETE FROM spaces_metadata WHERE space_jid = 'design@archive.localhost'",
                    (),
                )
                .await?;
            drop(owner_guard);
            crate::spaces_metadata::prepare_space_stores_owner(owner_database.clone(), &identity)
                .await?;
            let owner_guard = owner_database.guard().await?;
            let mut migrated_rows = owner_guard
                .query(
                    "SELECT space_node FROM spaces_metadata \
                     WHERE space_jid = 'design@spaces.localhost'",
                    (),
                )
                .await?;
            let metadata_node: String = migrated_rows
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("owner prep lost the metadata row"))?
                .get(0)?;
            drop(migrated_rows);
            if metadata_node != "design" {
                bail!("owner prep did not backfill metadata space_node");
            }
            let mut migrated_link = owner_guard
                .query(
                    "SELECT space_node FROM channel_space_links \
                     WHERE channel_jid = 'general@muc.localhost'",
                    (),
                )
                .await?;
            let link_node: String = migrated_link
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("owner prep lost the channel-space link"))?
                .get(0)?;
            drop(migrated_link);
            if link_node != "design" {
                bail!("owner prep did not backfill channel link space_node");
            }
            let mut lineage_row = owner_guard
                .query("SELECT lineage_uuid FROM _lineage WHERE id = 1", ())
                .await?;
            let stored_lineage: String = lineage_row
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("owner prep lost the lineage row"))?
                .get(0)?;
            drop(lineage_row);
            if stored_lineage != lineage.lineage_uuid.to_string() {
                bail!("owner prep changed the PostgreSQL lineage row");
            }
            drop(owner_guard);

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
            sqlx::query(&format!(
                "GRANT SELECT ON {}._waddle_space_store_owner TO {}",
                self.target, self.runtime
            ))
            .execute(&self.admin)
            .await?;

            let table_owners_are_no_login: bool = sqlx::query_scalar(
                "SELECT bool_and(owner.rolname = $1 AND NOT owner.rolcanlogin) \
                 FROM pg_catalog.pg_class relation \
                 JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                 JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner \
                 WHERE namespace.nspname = $2 \
                   AND relation.relname IN (\
                     'spaces_metadata', 'channel_space_links', '_waddle_space_store_owner') \
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
            sqlx::query(&format!("GRANT {} TO {}", self.definer, self.owner))
                .execute(&self.admin)
                .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::UnsafeOwnerRole) => {}
                Err(error) => bail!("owner role membership had wrong attach error: {error:?}"),
                Ok(_) => bail!("runtime attachment accepted an owner role with membership"),
            }
            sqlx::query(&format!("REVOKE {} FROM {}", self.definer, self.owner))
                .execute(&self.admin)
                .await?;
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (_resume_tx, resume_rx) = tokio::sync::oneshot::channel();
            let cancelled_owner_database = owner_database.clone();
            let cancelled_identity = identity.clone();
            let cancelled_prepare = tokio::spawn(async move {
                crate::spaces_metadata::runtime_attach::prepare_space_stores_owner_paused_for_test(
                    cancelled_owner_database,
                    &cancelled_identity,
                    entered_tx,
                    resume_rx,
                )
                .await
            });
            if let Err(entered_error) = entered_rx.await {
                let task_result = cancelled_prepare.await;
                bail!(
                    "owner preparation closed its cancellation checkpoint: {entered_error}; \
                     task result: {task_result:?}"
                );
            }
            cancelled_prepare.abort();
            if !matches!(cancelled_prepare.await, Err(error) if error.is_cancelled()) {
                bail!("owner preparation did not cancel at the invalidated-marker checkpoint");
            }

            let inherited =
                crate::server::ResolvedXmppDatabaseUrl::InheritedGlobal(boundary_url.clone());
            let (attached_spaces, attached_links, attached_global) =
                crate::server::create_space_stores(&runtime_database, &inherited, &inherited)
                    .await?;
            if !attached_global {
                bail!("inherited global selection did not attach to the attested main pool");
            }
            for (spaces, links) in [
                (
                    crate::server::ResolvedXmppDatabaseUrl::Explicit(boundary_url.clone()),
                    inherited.clone(),
                ),
                (
                    inherited.clone(),
                    crate::server::ResolvedXmppDatabaseUrl::Explicit(boundary_url.clone()),
                ),
                (
                    crate::server::ResolvedXmppDatabaseUrl::EphemeralFallback,
                    inherited.clone(),
                ),
                (
                    inherited.clone(),
                    crate::server::ResolvedXmppDatabaseUrl::EphemeralFallback,
                ),
            ] {
                if crate::server::create_space_stores(&runtime_database, &spaces, &links)
                    .await
                    .is_ok()
                {
                    bail!("attested startup accepted a non-inherited store selection");
                }
            }

            sqlx::query(&format!(
                "UPDATE {}._waddle_space_store_owner \
                 SET database_name = 'wrong' WHERE marker_id = 1",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::OwnerMarkerInvalid) => {}
                Err(error) => bail!("altered owner identity marker had wrong error: {error}"),
                Ok(_) => bail!("altered owner identity marker was accepted"),
            }
            sqlx::query(&format!(
                "UPDATE {}._waddle_space_store_owner SET database_name = $1 WHERE marker_id = 1",
                self.target
            ))
            .bind(&self.database)
            .execute(&self.admin)
            .await?;

            sqlx::query(&format!(
                "GRANT TEMPORARY ON DATABASE {} TO {}",
                self.database, self.owner
            ))
            .execute(&self.admin)
            .await?;
            let mut shadow_tx = owner_database.begin().await?;
            shadow_tx
                .execute(
                    "CREATE TEMP TABLE spaces_metadata (space_node TEXT PRIMARY KEY)",
                    (),
                )
                .await?;
            match crate::spaces_metadata::runtime_attach::verify_temporary_shadow_for_test(
                &mut shadow_tx,
            )
            .await
            {
                Err(SpaceStoreAttachError::TemporaryRelationShadow {
                    table: "spaces_metadata",
                }) => {}
                Err(error) => {
                    bail!("temporary space-store shadow detector returned wrong error: {error}")
                }
                Ok(()) => bail!("temporary spaces_metadata shadow was accepted"),
            }
            shadow_tx.rollback().await?;
            sqlx::query(&format!(
                "REVOKE TEMP ON DATABASE {} FROM {}",
                self.database, self.owner
            ))
            .execute(&self.admin)
            .await
            .context("revoke owner TEMP privilege")?;

            let metadata_row = metadata(
                space_jid("pg-space"),
                "PostgreSQL space",
                Some("owner prepared"),
                None,
                1_800_000_000,
                1_800_000_000,
            );
            attached_spaces
                .upsert(&metadata_row)
                .await
                .context("metadata runtime upsert")?;
            if attached_spaces
                .get(&metadata_row.space_jid)
                .await
                .context("metadata runtime get")?
                != Some(metadata_row.clone())
            {
                bail!("attested runtime could not round-trip spaces metadata DML");
            }
            if !attached_spaces
                .delete(&metadata_row.space_jid)
                .await
                .context("metadata runtime delete")?
            {
                bail!("attested runtime could not delete spaces metadata");
            }

            let channel: jid::BareJid = "attached@muc.localhost".parse()?;
            let space = space_jid("pg-space");
            let link = ChannelSpaceLink {
                channel_jid: channel.clone(),
                space_node: SpaceNode::from(space.node().expect("space node from JID").to_string()),
                space_jid: space,
                created_at: 1_800_000_001,
            };
            attached_links
                .set(&link)
                .await
                .context("channel-space runtime set")?;
            if attached_links
                .get(&channel)
                .await
                .context("channel-space runtime get")?
                != Some(link)
            {
                bail!("attested runtime could not round-trip channel-space link DML");
            }
            if !attached_links
                .clear(&channel)
                .await
                .context("channel-space runtime clear")?
            {
                bail!("attested runtime could not delete channel-space link");
            }

            let default_collation: String = sqlx::query_scalar(
                "SELECT pg_catalog.format('%I.%I', collation_ns.nspname, co.collname) \
                 FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class r ON r.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace table_ns ON table_ns.oid = r.relnamespace \
                 JOIN pg_catalog.pg_collation co ON co.oid = a.attcollation \
                 JOIN pg_catalog.pg_namespace collation_ns \
                   ON collation_ns.oid = co.collnamespace \
                 WHERE table_ns.nspname = $1 AND r.relname = 'spaces_metadata' \
                   AND a.attname = 'name'",
            )
            .bind(&self.target)
            .fetch_one(&self.admin)
            .await
            .context("read default spaces-metadata column collation")?;
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata ALTER COLUMN name TYPE TEXT COLLATE \"C\"",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::InvalidColumn {
                    table: "spaces_metadata",
                    column: "name",
                }) => {}
                Err(error) => bail!("non-default column collation had wrong error: {error}"),
                Ok(_) => bail!("non-default column collation was accepted"),
            }
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata ALTER COLUMN name TYPE TEXT COLLATE {}",
                self.target, default_collation
            ))
            .execute(&self.admin)
            .await?;

            sqlx::query(&format!(
                "DROP INDEX {}.idx_spaces_metadata_space_jid",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            sqlx::query(&format!(
                "CREATE INDEX idx_spaces_metadata_space_jid \
                 ON {}.spaces_metadata (space_jid COLLATE \"C\")",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::InvalidIndex {
                    index: "idx_spaces_metadata_space_jid",
                }) => {}
                Err(error) => bail!("non-default index collation had wrong error: {error}"),
                Ok(_) => bail!("non-default index collation was accepted"),
            }
            sqlx::query(&format!(
                "DROP INDEX {}.idx_spaces_metadata_space_jid",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            sqlx::query(&format!(
                "CREATE INDEX idx_spaces_metadata_space_jid \
                 ON {}.spaces_metadata (space_jid)",
                self.target
            ))
            .execute(&self.admin)
            .await?;

            sqlx::query(&format!(
                "DROP INDEX {}.idx_spaces_metadata_space_jid",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            sqlx::query(&format!(
                "CREATE INDEX idx_spaces_metadata_space_jid \
                 ON {}.spaces_metadata (space_jid text_pattern_ops)",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::InvalidIndex {
                    index: "idx_spaces_metadata_space_jid",
                }) => {}
                Err(error) => bail!("non-default index operator class had wrong error: {error}"),
                Ok(_) => bail!("non-default index operator class was accepted"),
            }
            sqlx::query(&format!(
                "DROP INDEX {}.idx_spaces_metadata_space_jid",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            sqlx::query(&format!(
                "CREATE INDEX idx_spaces_metadata_space_jid \
                 ON {}.spaces_metadata (space_jid)",
                self.target
            ))
            .execute(&self.admin)
            .await?;

            for table in ["spaces_metadata", "channel_space_links"] {
                sqlx::query(&format!(
                    "ALTER TABLE {}.{table} ALTER COLUMN space_node SET DEFAULT ''",
                    self.target
                ))
                .execute(&self.admin)
                .await?;
            }
            DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await?;
            DatabaseChannelSpaceLinkStore::attach_existing(runtime_database.clone()).await?;

            let mut primary_key_names: Vec<String> = sqlx::query_scalar(
                "SELECT con.conname::text \
                 FROM pg_catalog.pg_constraint con \
                 JOIN pg_catalog.pg_class r ON r.oid = con.conrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = r.relnamespace \
                 WHERE n.nspname = $1 AND r.relname = 'spaces_metadata' \
                   AND con.contype = 'p'",
            )
            .bind(&self.target)
            .fetch_all(&self.admin)
            .await
            .context("read rebuilt spaces-metadata primary-key name")?;
            if primary_key_names.len() != 1 {
                bail!("expected exactly one spaces-metadata primary key constraint");
            }
            let primary_key_constraint = primary_key_names.remove(0);
            let primary_key_constraint =
                crate::db::physical_attestation::quote_identifier(&primary_key_constraint);
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata DROP CONSTRAINT {primary_key_constraint}",
                self.target,
            ))
            .execute(&self.admin)
            .await?;
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata \
                 ADD CONSTRAINT spaces_metadata_pkey PRIMARY KEY (space_node) INCLUDE (name)",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::InvalidPrimaryKey {
                    table: "spaces_metadata",
                }) => {}
                Err(error) => bail!("included primary-key column had wrong error: {error}"),
                Ok(_) => bail!("primary-key INCLUDE column was accepted"),
            }
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata DROP CONSTRAINT spaces_metadata_pkey",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata \
                 ADD CONSTRAINT spaces_metadata_pkey PRIMARY KEY (space_node)",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await?;

            drop(attached_spaces);
            drop(attached_links);

            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata ALTER COLUMN name SET DEFAULT 'unexpected'",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::InvalidColumn {
                    table: "spaces_metadata",
                    column: "name",
                }) => {}
                Err(error) => bail!("unapproved name default had wrong error: {error}"),
                Ok(_) => bail!("unapproved name default was accepted"),
            }
            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata ALTER COLUMN name DROP DEFAULT",
                self.target
            ))
            .execute(&self.admin)
            .await?;

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
                "CREATE INDEX idx_spaces_metadata_created_at \
                 ON {}.spaces_metadata (created_at ASC, space_node ASC)",
                self.target
            ))
            .execute(&self.admin)
            .await?;

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
            sqlx::query(&format!(
                "REVOKE TRUNCATE ON {}.channel_space_links FROM {}",
                self.target, self.runtime
            ))
            .execute(&self.admin)
            .await?;

            let owner_guard = owner_database.guard().await?;
            owner_guard
                .execute("DROP TABLE spaces_metadata", ())
                .await?;
            owner_guard
                .execute(
                    "CREATE TABLE spaces_metadata (\
                       space_node TEXT PRIMARY KEY, \
                       space_jid TEXT NOT NULL, \
                       name TEXT GENERATED ALWAYS AS (space_jid) STORED NOT NULL, \
                       description TEXT, \
                       icon_url TEXT, \
                       created_at BIGINT NOT NULL, \
                       updated_at BIGINT NOT NULL\
                     )",
                    (),
                )
                .await?;
            for statement in [
                "CREATE UNIQUE INDEX idx_spaces_metadata_space_node \
                 ON spaces_metadata (space_node)",
                "CREATE INDEX idx_spaces_metadata_created_at \
                 ON spaces_metadata (created_at ASC, space_node ASC)",
                "CREATE INDEX idx_spaces_metadata_space_jid \
                 ON spaces_metadata (space_jid)",
            ] {
                owner_guard.execute(statement, ()).await?;
            }
            sqlx::query(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.spaces_metadata TO {}",
                self.target, self.runtime
            ))
            .execute(&self.admin)
            .await?;
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::InvalidColumn {
                    table: "spaces_metadata",
                    column: "name",
                }) => {}
                Err(error) => bail!("generated name column had wrong error: {error}"),
                Ok(_) => bail!("generated name column was accepted"),
            }

            sqlx::query(&format!(
                "ALTER TABLE {}.spaces_metadata ALTER COLUMN name DROP EXPRESSION",
                self.target
            ))
            .execute(&self.admin)
            .await?;
            DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await?;

            drop(owner_guard);
            let raw = DatabaseSpacesMetadataStore::initialize_owner(owner_database.clone()).await?;
            drop(raw);
            match DatabaseSpacesMetadataStore::attach_existing(runtime_database.clone()).await {
                Err(SpaceStoreAttachError::OwnerMarkerMissing) => {}
                Err(error) => bail!("legacy raw initialization had wrong error: {error}"),
                Ok(_) => bail!("legacy raw initialization left the marker complete"),
            }
            drop(owner_database);
            drop(runtime_database);
            Ok(())
        }

        async fn cleanup(self) {
            sqlx::query(&format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
                self.definer
            ))
            .execute(&self.admin)
            .await
            .expect("remove disposable definer function grant");
            sqlx::query(&format!(
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM {}",
                self.owner
            ))
            .execute(&self.admin)
            .await
            .expect("remove disposable owner function grant");
            let restore_public = if self.had_public_control_execute {
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO PUBLIC"
            } else {
                "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC"
            };
            sqlx::query(restore_public)
                .execute(&self.admin)
                .await
                .expect("restore disposable database control-function ACL");
            self.admin.close().await;
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
