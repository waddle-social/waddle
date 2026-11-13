//! Combined-store qualification for the unselected PG17 restricted profile.
//!
//! Inbox, portable SM, and pending delivery receive legacy round trips. The
//! partial v2 diagnostics inspect PubSub, the room-effect outbox, MAM and the
//! V1009 epoch grants alongside v1 observation and, with clustering, MUC.
//! The additional sequential owner phase prepares selected Push, notification,
//! call and space tables and checks fixture ACLs. Space metadata and channel
//! links use paired owner preparation and read-only attachment diagnostics.
//! These separate checks are not an atomic all-store or serving test.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{ensure, Context};
use futures::FutureExt;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, DeploymentUuid, PgSchemaName};
use waddle_server::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig, RestrictedIngressProfileError,
    RestrictedIngressProfileObservation, RestrictedTransactionProbe,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::inbox::{bootstrap_inbox_owner_schema, DatabaseInboxStorage};
use waddle_server::ingress_substrate::INGRESS_GUARDED_TABLES;
use waddle_server::ingress_uow::{
    prepare_dark_room_observation_schema, verify_attested_room_observation_schema,
    verify_attested_room_observation_schema_v1, RoomObservationSchemaError,
};
use waddle_server::mam_runtime::DatabaseMamStorage;
#[cfg(feature = "clustering")]
use waddle_server::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
#[cfg(feature = "clustering")]
use waddle_server::muc_durable::PostgresMucRoomStore;
use waddle_server::pending_delivery::{
    bootstrap_pending_delivery_owner_schema, DatabasePendingDeliveryStorage,
};
use waddle_server::pubsub::{
    DatabasePubSubStorage, RestrictedServingProfileV1, RestrictedServingProfileV2,
};
use waddle_server::room_effect_outbox::RoomEffectOutboxStore;
use waddle_server::sm_persistence::{bootstrap_portable_sm_owner_schema, DatabaseSmPersistence};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::mam::SqlxMamStorage;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{
    PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
};
use waddle_xmpp::stream_management::persistence::{PersistedSession, SmPersistenceStorage};

use super::{test_url, CATALOG_GRANT_LOCK, C_COLLATION_DATABASE};
use pinned_pubsub_inventory::{exercise_pinned_pubsub, pinned_pubsub_inventory};

#[path = "combined_restricted_profile_extra.rs"]
mod combined_restricted_profile_extra;
#[path = "pinned_partial_profile.rs"]
mod pinned_partial_profile;
#[path = "pinned_pubsub_inventory.rs"]
mod pinned_pubsub_inventory;
#[path = "pinned_push_profile.rs"]
mod pinned_push_profile;

const RELATION_GUARD: &str =
    include_str!("../../src/db/migrations/owner/ingress_epoch_relation_guard_candidate.sql");
const CANDIDATE_API: &str =
    include_str!("../../src/db/migrations/owner/ingress_definer_api_candidate.sql");

// Resource names are generated here, and ownership markers prevent cleanup
// from touching an unrelated object after an ambiguous CREATE acknowledgement.
struct Resources {
    cluster: PgPool,
    database: Option<(String, String)>,
    roles: Vec<(String, String)>,
}

impl Resources {
    async fn role(&mut self, name: &str, attributes: &str) -> anyhow::Result<()> {
        let marker = format!("waddle-combined-profile-{}", Uuid::new_v4());
        self.roles.push((name.to_owned(), marker.clone()));
        let mut connection = provisioning_lock(&self.cluster, name).await?;
        let mut tx = connection.begin().await?;
        sqlx::query(&format!("CREATE ROLE {name} {attributes}"))
            .execute(&mut *tx)
            .await?;
        sqlx::query(&format!("COMMENT ON ROLE {name} IS '{marker}'"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        connection.close().await?;
        Ok(())
    }

    async fn database(&mut self, name: &str, owner: &str) -> anyhow::Result<()> {
        self.database = Some((name.to_owned(), owner.to_owned()));
        let mut connection = provisioning_lock(&self.cluster, name).await?;
        sqlx::query(&format!(
            "CREATE DATABASE {name} OWNER {owner} {C_COLLATION_DATABASE}"
        ))
        .execute(&mut connection)
        .await?;
        connection.close().await?;
        Ok(())
    }

    async fn cleanup(&mut self) -> anyhow::Result<()> {
        cleanup(&self.cluster, &mut self.database, &mut self.roles).await
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        if self.database.is_none() && self.roles.is_empty() {
            return;
        }
        let cluster = self.cluster.clone();
        let mut database = self.database.take();
        let mut roles = std::mem::take(&mut self.roles);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = cleanup(&cluster, &mut database, &mut roles).await;
            });
        }
    }
}

async fn provisioning_lock(cluster: &PgPool, name: &str) -> anyhow::Result<PgConnection> {
    let mut connection = PgConnection::connect_with(&cluster.connect_options()).await?;
    for statement in ["SET lock_timeout = '5s'", "SET statement_timeout = '10s'"] {
        sqlx::query(statement).execute(&mut connection).await?;
    }
    sqlx::query("SELECT pg_catalog.pg_advisory_lock(pg_catalog.hashtextextended($1, 2046))")
        .bind(name)
        .execute(&mut connection)
        .await?;
    Ok(connection)
}

async fn marked(connection: &mut PgConnection, name: &str, marker: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=$1 AND pg_catalog.shobj_description(oid, 'pg_authid')=$2)")
        .bind(name).bind(marker).fetch_one(connection).await?)
}

async fn owner_sql(database: &Database, statement: &str) -> anyhow::Result<()> {
    database.guard().await?.execute(statement, ()).await?;
    Ok(())
}

async fn cleanup(
    cluster: &PgPool,
    database: &mut Option<(String, String)>,
    roles: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    if let Some((name, owner)) = database.as_ref() {
        let mut connection = provisioning_lock(cluster, name).await?;
        let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_database d JOIN pg_catalog.pg_roles r ON r.oid=d.datdba WHERE d.datname=$1 AND r.rolname=$2)")
            .bind(name).bind(owner).fetch_one(&mut connection).await?;
        if owned {
            let (_, marker) = roles
                .iter()
                .find(|(role, _)| role == owner)
                .context("fixture database owner is not tracked")?;
            ensure!(
                marked(&mut connection, owner, marker).await?,
                "fixture database owner marker changed"
            );
            sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
                .execute(&mut connection)
                .await?;
        }
        connection.close().await?;
        *database = None;
    }
    while let Some((name, marker)) = roles.last() {
        let mut connection = provisioning_lock(cluster, name).await?;
        if marked(&mut connection, name, marker).await? {
            sqlx::query(&format!("DROP ROLE {name}"))
                .execute(&mut connection)
                .await?;
        }
        connection.close().await?;
        roles.pop();
    }
    Ok(())
}

fn owner_url(
    base: &str,
    database: &str,
    migrator: &str,
    password: &str,
    owner: &str,
) -> anyhow::Result<String> {
    let mut parsed = url::Url::parse(base)?;
    let values: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "user" | "password" | "dbname" | "options"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    parsed
        .query_pairs_mut()
        .clear()
        .extend_pairs(values.iter().map(|(k, v)| (k, v)))
        .append_pair("dbname", database)
        .append_pair("user", migrator)
        .append_pair("password", password)
        .append_pair(
            "options",
            &format!("-c role={owner} -c search_path=guarded"),
        );
    Ok(parsed.to_string())
}

async fn exercise(resources: &mut Resources, base_url: &str) -> anyhow::Result<()> {
    let suffix = Uuid::new_v4().simple().to_string();
    let database = format!("profile_db_{suffix}");
    let database_owner = format!("profile_dbowner_{suffix}");
    let owner = format!("profile_owner_{suffix}");
    let migrator = format!("profile_migrator_{suffix}");
    let runtime = format!("profile_runtime_{suffix}");
    let api = format!("profile_api_{suffix}");
    let helper = format!("profile_helper_{suffix}");
    let installer = format!("profile_installer_{suffix}");
    let definer = format!("profile_definer_{suffix}");
    let password = format!("p{}", Uuid::new_v4().simple());
    let restricted = "NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";
    for role in [&database_owner, &owner, &api, &definer, &helper] {
        resources
            .role(role, &format!("NOLOGIN NOINHERIT {restricted}"))
            .await?;
    }
    for role in [&migrator, &runtime, &installer] {
        resources
            .role(
                role,
                &format!("LOGIN NOINHERIT PASSWORD '{password}' {restricted}"),
            )
            .await?;
    }
    for statement in [
        format!("GRANT {owner} TO {migrator} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
        format!("GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
    ] {
        sqlx::query(&statement).execute(&resources.cluster).await?;
    }
    resources.database(&database, &database_owner).await?;
    sqlx::query(&format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE {database} FROM PUBLIC"
    ))
    .execute(&resources.cluster)
    .await?;
    sqlx::query(&format!(
        "GRANT CONNECT ON DATABASE {database} TO {migrator}, {runtime}, {installer}"
    ))
    .execute(&resources.cluster)
    .await?;

    let base = PgConnectOptions::from_str(base_url)?;
    let admin_options = base
        .clone()
        .database(&database)
        .options([("search_path", "guarded")]);
    let admin = PgPool::connect_with(admin_options).await?;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&admin)
        .await?;
    ensure!(
        (170_000..180_000).contains(&version),
        "qualification requires PostgreSQL 17"
    );
    // pg_control_system's ACL lives in pg_proc. Refuse to change that ACL
    // unless this disposable database has a database-local pg_proc catalog.
    let proc_is_local: bool = sqlx::query_scalar(
        "SELECT NOT relisshared FROM pg_catalog.pg_class WHERE oid='pg_catalog.pg_proc'::regclass",
    )
    .fetch_one(&admin)
    .await?;
    ensure!(proc_is_local, "pg_proc catalog is not database-local");
    for statement in [
        "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
        format!("CREATE SCHEMA guarded AUTHORIZATION {owner}"),
        format!("CREATE SCHEMA attestor AUTHORIZATION {definer}"),
        "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
        format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}, {definer}"),
    ] {
        sqlx::query(&statement).execute(&admin).await?;
    }
    let owner_url = owner_url(base_url, &database, &migrator, &password, &owner)?;
    let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url);
    owner_config.pool_size = 1;
    let owner_db = Database::from_config("combined profile owner", &owner_config).await?;
    let lineage = lineage::enroll(
        &owner_db,
        &LineageConfig {
            deployment_uuid: Some(DeploymentUuid(Uuid::new_v4())),
            action: None,
        },
    )
    .await?;
    // Model a pre-existing observation schema upgraded by registered
    // migrations, as in the established owner fixture.
    waddle_server::test_support::initialize_room_observations_for_test(&owner_db).await?;
    MigrationRunner::single().run(&owner_db).await?;
    bootstrap_pending_delivery_owner_schema(&owner_db).await?;
    bootstrap_inbox_owner_schema(&owner_db).await?;
    bootstrap_portable_sm_owner_schema(&owner_db).await?;
    let identity = lineage
        .postgres_identity
        .clone()
        .context("PostgreSQL identity")?;
    let migrator_role = PgRoleName::new(&migrator)?;
    let runtime_role = PgRoleName::new(&runtime)?;
    let api_role = PgRoleName::new(&api)?;
    let definer_role = PgRoleName::new(&definer)?;
    for statement in [
        format!("GRANT USAGE ON SCHEMA guarded TO {runtime}, {definer}"),
        format!("GRANT SELECT ON guarded._lineage TO {runtime}, {definer}"),
        format!("GRANT SELECT ON guarded._migrations TO {runtime}"),
    ] {
        owner_sql(&owner_db, &statement).await?;
    }
    prepare_dark_room_observation_schema(
        &owner_db,
        &lineage,
        &migrator_role,
        &definer_role,
        &runtime_role,
    )
    .await
    .context("prepare dark room observation")?;

    let installer_options = base
        .clone()
        .database(&database)
        .username(&installer)
        .password(&password)
        .options([("search_path", "guarded")]);
    let config = PhysicalAttestationConfig::new(
        lineage.clone(),
        PgRoleName::new(&installer)?,
        runtime_role.clone(),
        definer_role,
        PgSchemaName("attestor".to_owned()),
    )?;
    let installation_operation = Uuid::new_v4();
    let config = install(&installer_options, config, installation_operation).await?;
    let runtime_options = base
        .database(&database)
        .username(&runtime)
        .password(&password)
        .options([("search_path", "guarded")]);
    let attested = attested_pool(
        runtime_options.clone(),
        config.clone(),
        2,
        Duration::from_secs(5),
    )?;
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "combined profile",
        attested,
        1,
        1,
    )
    .await?;
    let main = pool.global();

    // A controlled counterfactual isolates the API-owner namespace grant as
    // the reason room observation cannot verify under the strict profile.
    verify_attested_room_observation_schema(main, &lineage, &migrator_role)
        .await
        .context("observation verifies before API grant")?;
    owner_sql(
        &owner_db,
        &format!("GRANT USAGE ON SCHEMA guarded TO {api}"),
    )
    .await?;
    ensure!(
        matches!(
            verify_attested_room_observation_schema(main, &lineage, &migrator_role).await,
            Err(RoomObservationSchemaError::UnsafeRuntime)
        ),
        "observation must refuse the API schema grant"
    );
    owner_sql(
        &owner_db,
        &format!("REVOKE USAGE ON SCHEMA guarded FROM {api}"),
    )
    .await?;
    verify_attested_room_observation_schema(main, &lineage, &migrator_role)
        .await
        .context("observation verifies after API grant removal")?;

    // Two simultaneously held distinct backend PIDs establish real slots,
    // not just independent semaphore budgets over one connection.
    let mut main_transaction = main.begin().await?;
    let ingress_db = pool.ingress();
    let mut ingress_transaction = ingress_db.begin().await?;
    let mut main_rows = main_transaction
        .query("SELECT pg_backend_pid()::bigint", ())
        .await?;
    let main_pid: i64 = main_rows
        .next()
        .await?
        .context("main backend pid")?
        .get(0)?;
    let mut ingress_rows = ingress_transaction
        .query("SELECT pg_backend_pid()::bigint", ())
        .await?;
    let ingress_pid: i64 = ingress_rows
        .next()
        .await?
        .context("ingress backend pid")?
        .get(0)?;
    ensure!(
        main_pid != ingress_pid,
        "main and ingress did not reserve distinct physical slots"
    );
    drop(main_rows);
    drop(ingress_rows);
    main_transaction
        .rollback()
        .await
        .context("acknowledge main-lane rollback")?;
    ingress_transaction
        .rollback()
        .await
        .context("acknowledge ingress-lane rollback")?;

    // Preserve the registered V1009 definitions before the separate legacy
    // relation-guard diagnostic below. The v2 qualification restores them.
    let mut v1009_definitions = Vec::new();
    for function in [
        "waddle_ingress_epoch_guard",
        "waddle_ingress_protocol_epoch_guard",
    ] {
        let definition: String = sqlx::query_scalar(
            "SELECT pg_catalog.pg_get_functiondef(pg_catalog.to_regprocedure($1))",
        )
        .bind(format!("guarded.{function}()"))
        .fetch_one(&admin)
        .await?;
        v1009_definitions.push(definition);
    }

    let mut owner_connection = PgConnection::connect_with(
        &PgConnectOptions::from_str(base_url)?
            .database(&database)
            .username(&migrator)
            .password(&password)
            .options([("role", owner.as_str()), ("search_path", "guarded")]),
    )
    .await?;
    sqlx::raw_sql(RELATION_GUARD)
        .execute(&mut owner_connection)
        .await?;
    for statement in [
        format!("GRANT USAGE, CREATE ON SCHEMA guarded TO {api}"),
        format!("GRANT SELECT ON guarded.ingress_protocol_epoch, guarded.sessions, guarded.pending_delivery TO {api}"),
        format!("GRANT UPDATE(id) ON guarded.ingress_protocol_epoch, guarded.sessions TO {api}"),
        format!("GRANT SELECT ON guarded.ingress_protocol_epoch, guarded.ingress_epoch_guard_manifest TO {runtime}"),
    ] { sqlx::query(&statement).execute(&mut owner_connection).await?; }
    for table in INGRESS_GUARDED_TABLES {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.{table} TO {api}"
        ))
        .execute(&mut owner_connection)
        .await?;
    }
    drop(owner_connection);
    let candidate = CANDIDATE_API
        .replace("__SCHEMA_PATH__", "\"guarded\"")
        .replace("__SCHEMA__", "\"guarded\"")
        .replace("__API_OWNER_LITERAL__", &api)
        .replace("__API_OWNER_IDENT__", &api)
        .replace("__RUNTIME_ROLE_LITERAL__", &runtime)
        .replace("__RUNTIME_ROLE_IDENT__", &runtime);
    sqlx::raw_sql(&candidate).execute(&admin).await?;
    // The candidate's trigger helper is created by the installer but is
    // specified to remain owned by the schema owner, unlike its definer API.
    sqlx::query(&format!(
        "ALTER FUNCTION guarded.waddle_ingress_definer_epoch_guard() OWNER TO {owner}"
    ))
    .execute(&admin)
    .await?;
    owner_sql(
        &owner_db,
        &format!("REVOKE CREATE ON SCHEMA guarded FROM {api}"),
    )
    .await?;

    for (table, rights) in [
        ("pending_delivery", "SELECT, INSERT, UPDATE, DELETE"),
        ("pending_delivery_sm_promotions", "SELECT, INSERT, DELETE"),
        ("pending_delivery_startup_migrations", "SELECT"),
        ("inbox_entries", "SELECT, INSERT, UPDATE"),
        (
            "groupchat_notification_recovery",
            "SELECT, INSERT, UPDATE, DELETE",
        ),
        ("sm_sessions", "SELECT, INSERT, UPDATE, DELETE"),
        ("sm_unacked", "SELECT, INSERT, DELETE"),
        ("sm_acknowledged_windows", "SELECT, INSERT, DELETE"),
        ("sm_ingress_appends", "SELECT, INSERT, UPDATE, DELETE"),
    ] {
        owner_sql(
            &owner_db,
            &format!("GRANT {rights} ON guarded.{table} TO {runtime}"),
        )
        .await?;
    }
    let sequence: String = sqlx::query_scalar("SELECT pg_catalog.pg_get_serial_sequence('guarded.pending_delivery','notification_recovery_ordinal')")
        .fetch_one(&admin).await?;
    owner_sql(
        &owner_db,
        &format!("GRANT USAGE ON SEQUENCE {sequence} TO {runtime}"),
    )
    .await?;
    let epoch: Oid = sqlx::query_scalar("SELECT 'guarded.ingress_protocol_epoch'::regclass::oid")
        .fetch_one(&admin)
        .await?;
    let runtime_oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_roles WHERE rolname=$1")
        .bind(&runtime)
        .fetch_one(&admin)
        .await?;
    let api_oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_roles WHERE rolname=$1")
        .bind(&api)
        .fetch_one(&admin)
        .await?;
    let expected = RestrictedIngressProfileObservation {
        manifest_version: 3,
        schema_oid: identity.schema.oid,
        epoch_relation_oid: epoch.0,
        runtime_role_oid: runtime_oid.0,
        api_owner_oid: api_oid.0,
    };
    let probe =
        RestrictedTransactionProbe::new(main.clone(), main, api_role.clone(), expected.clone())?;
    ensure!(
        probe.inspect().await?.profile == expected,
        "initial strict profile differs"
    );

    let inbox = DatabaseInboxStorage::attach_existing(main.clone()).await?;
    let sm = DatabaseSmPersistence::attach_existing_postgres(main.clone()).await?;
    let pending = DatabasePendingDeliveryStorage::attach_existing(
        main.clone(),
        QuotaPolicy::CountCap { max_rows: 42 },
    )
    .await?;
    let user = "alice@example.test".parse()?;
    let partner = "bob@example.test".parse()?;
    let entry = InboxEntry::new(partner, ConversationKind::Direct, "profile-message", 1);
    inbox.upsert(&user, entry, true).await?;
    ensure!(inbox.list(&user).await?.len() == 1, "inbox round trip");
    let session_id = SmSessionId::new("profile-stream");
    sm.upsert_session(PersistedSession {
        stream_id: session_id.clone(),
        user_id: "alice".to_owned(),
        jid: "alice@example.test/device".parse()?,
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(60),
        detached_at: chrono::Utc::now(),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
    })
    .await?;
    ensure!(
        sm.get_session(&session_id).await?.is_some(),
        "SM round trip"
    );
    let row = PendingRowId::new("profile-pending");
    pending
        .insert(PendingRow {
            id: row.clone(),
            recipient: user.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                "profile-message",
                user.clone().into(),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await?;
    ensure!(pending.count(&user).await? == 1, "pending round trip");
    ensure!(pending.delete_row(&row).await? == 1, "pending deletion");
    ensure!(
        probe.inspect().await?.profile == expected,
        "strict profile changed after store operations"
    );

    // PubSub owner preparation creates the real owner-owned definer helper.
    // The exact observer must reject that authority; do not widen its manifest.
    DatabasePubSubStorage::initialize_owner(owner_db.clone(), &identity)
        .await
        .map_err(|error| anyhow::anyhow!("prepare PubSub: {error}"))?;
    ensure!(
        matches!(
            probe.inspect().await,
            Err(RestrictedIngressProfileError::Mismatch(
                "alternate callable definer authority"
            ))
        ),
        "strict profile must reject the PubSub owner definer"
    );
    // Replace only the legacy S-owned helper with the exact H-owned v1 body,
    // and supply real PubSub rights alongside the existing observation store.
    let helper_tables = [
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
    for (table, rights) in [
        ("dnd_projection", "SELECT,INSERT,UPDATE,DELETE"),
        ("dnd_projection_source_version", "SELECT,INSERT,UPDATE"),
        (
            "notification_settings_projection",
            "SELECT,INSERT,UPDATE,DELETE",
        ),
        (
            "notification_settings_projection_source_version",
            "SELECT,INSERT,UPDATE",
        ),
        ("pubsub_affiliations", "SELECT,INSERT,UPDATE,DELETE"),
        ("pubsub_items", "SELECT,INSERT,UPDATE,DELETE"),
        ("pubsub_nodes", "SELECT,INSERT,UPDATE,DELETE"),
        ("pubsub_owner_configured", "SELECT,INSERT"),
        ("pubsub_push_publications", "SELECT,INSERT,UPDATE"),
        ("pubsub_schema_marker", "SELECT"),
        ("pubsub_schema_version", "SELECT"),
        ("pubsub_subscriptions", "SELECT,INSERT,DELETE"),
    ] {
        owner_sql(
            &owner_db,
            &format!("GRANT {rights} ON guarded.{table} TO {runtime}"),
        )
        .await?;
        owner_sql(
            &owner_db,
            &format!("GRANT UPDATE ON guarded.{table} TO {helper}"),
        )
        .await?;
    }
    owner_sql(
        &owner_db,
        &format!("GRANT SELECT ON SEQUENCE guarded.pubsub_items_seq_seq TO {runtime}"),
    )
    .await?;
    owner_sql(
        &owner_db,
        &format!("GRANT USAGE, CREATE ON SCHEMA guarded TO {helper}"),
    )
    .await?;
    let asset = include_str!("../../src/db/migrations/owner/serving_role_graph_v1_candidate.sql");
    let body = asset
        .split_once("AS $v1_pubsub_lock$")
        .context("helper source")?
        .1
        .split_once("$v1_pubsub_lock$")
        .context("helper source end")?
        .0
        .replace("__SCHEMA__", "\"guarded\"");
    for statement in [
        "DROP FUNCTION guarded.pubsub_runtime_attach_lock()".to_owned(),
        format!("CREATE FUNCTION guarded.pubsub_runtime_attach_lock() RETURNS void LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $v1_pubsub_lock${body}$v1_pubsub_lock$"),
        format!("ALTER FUNCTION guarded.pubsub_runtime_attach_lock() OWNER TO {helper}"),
        "REVOKE ALL ON FUNCTION guarded.pubsub_runtime_attach_lock() FROM PUBLIC".to_owned(),
        format!("GRANT EXECUTE ON FUNCTION guarded.pubsub_runtime_attach_lock() TO {runtime}"),
    ] {
        sqlx::query(&statement).execute(&admin).await?;
    }
    owner_sql(
        &owner_db,
        &format!("REVOKE CREATE ON SCHEMA guarded FROM {helper}"),
    )
    .await?;
    let helper_oid: Oid =
        sqlx::query_scalar("SELECT oid FROM pg_catalog.pg_roles WHERE rolname=$1")
            .bind(&helper)
            .fetch_one(&admin)
            .await?;
    let helper_function_oid: Oid = sqlx::query_scalar(
        "SELECT 'guarded.pubsub_runtime_attach_lock()'::pg_catalog.regprocedure::pg_catalog.oid",
    )
    .fetch_one(&admin)
    .await?;
    let mut helper_relation_oids = [0_u32; 12];
    for (index, table) in helper_tables.iter().enumerate() {
        let oid: Oid = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::pg_catalog.oid")
            .bind(format!("guarded.{table}"))
            .fetch_one(&admin)
            .await?;
        helper_relation_oids[index] = oid.0;
    }
    let serving_profile = RestrictedServingProfileV1 {
        api: api_role.clone(),
        api_oid: api_oid.0,
        helper: PgRoleName::new(&helper)?,
        helper_oid: helper_oid.0,
        helper_function_oid: helper_function_oid.0,
        helper_relation_oids,
    };
    DatabasePubSubStorage::verify_restricted_profile_v1(main, &serving_profile).await?;
    verify_attested_room_observation_schema_v1(main, &lineage, &migrator_role, &serving_profile)
        .await
        .context("combined A/H room observation profile")?;
    ensure!(
        verify_attested_room_observation_schema(main, &lineage, &migrator_role)
            .await
            .is_err(),
        "legacy observation verification must retain its exact schema ACL"
    );
    // A may call this S-owned definer, although R may not call it directly.
    // The composed profile must reject the relay before returning evidence.
    sqlx::query("CREATE FUNCTION guarded.v1_observation_relay() RETURNS void LANGUAGE sql SECURITY DEFINER AS 'DELETE FROM guarded.extension_room_observers WHERE false'")
        .execute(&admin).await?;
    sqlx::query(&format!(
        "ALTER FUNCTION guarded.v1_observation_relay() OWNER TO {owner}"
    ))
    .execute(&admin)
    .await?;
    sqlx::query("REVOKE ALL ON FUNCTION guarded.v1_observation_relay() FROM PUBLIC")
        .execute(&admin)
        .await?;
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION guarded.v1_observation_relay() TO {api}"
    ))
    .execute(&admin)
    .await?;
    ensure!(
        verify_attested_room_observation_schema_v1(
            main,
            &lineage,
            &migrator_role,
            &serving_profile
        )
        .await
        .is_err(),
        "A-to-owner definer relay must be rejected"
    );
    sqlx::query("DROP FUNCTION guarded.v1_observation_relay()")
        .execute(&admin)
        .await?;

    // Return the guards to the registered V1009 bodies before qualifying the
    // partial profile. The legacy relation-guard experiment is not selected.
    for definition in &v1009_definitions {
        owner_sql(&owner_db, definition).await?;
    }
    owner_sql(
        &owner_db,
        &format!("REVOKE UPDATE (id) ON guarded.ingress_protocol_epoch FROM {runtime}"),
    )
    .await?;
    let mut mam_connection = PgConnection::connect_with(
        &PgConnectOptions::from_str(base_url)?
            .database(&database)
            .username(&migrator)
            .password(&password)
            .options([("role", owner.as_str()), ("search_path", "guarded")]),
    )
    .await?;
    mam_connection = SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(
        mam_connection,
        "guarded",
        &owner,
    )
    .await?;
    mam_connection.close().await?;
    #[cfg(feature = "clustering")]
    {
        PostgresMucRoomStore::initialize_owner(&owner_db, &identity).await?;
        MucDestroyCompletionOutboxStore::try_new(owner_db.clone()).await?;
    }
    RoomEffectOutboxStore::initialize_owner(&owner_db, &identity).await?;
    for statement in [
        format!("GRANT SELECT, INSERT ON guarded.mam_messages, guarded.mam_archive_sequences TO {runtime}"),
        format!("GRANT UPDATE (body, stanza_xml, thread_id, parent_thread_id, reply_to_id, reply_to_jid, rich_payload) ON guarded.mam_messages TO {runtime}"),
        format!("GRANT UPDATE (next_seq) ON guarded.mam_archive_sequences TO {runtime}"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.clustering_muc_room_effects TO {runtime}"),
    ] {
        owner_sql(&owner_db, &statement).await?;
    }
    #[cfg(feature = "clustering")]
    for statement in [
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON guarded.clustering_muc_rooms, guarded.clustering_muc_room_affiliations, guarded.clustering_muc_destroy_outbox TO {runtime}"),
        format!("GRANT SELECT, INSERT, UPDATE ON guarded.clustering_muc_room_lifecycles TO {runtime}"),
        format!("GRANT SELECT, INSERT, DELETE ON guarded.clustering_muc_admin_receipts TO {runtime}"),
    ] {
        owner_sql(&owner_db, &statement).await?;
    }
    let mut pins = Vec::new();
    for table in [
        "clustering_muc_room_effects",
        "mam_messages",
        "mam_archive_sequences",
    ] {
        let oid: Oid = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid")
            .bind(format!("guarded.{table}"))
            .fetch_one(&admin)
            .await?;
        pins.push(oid.0);
    }
    let v2 = RestrictedServingProfileV2 {
        v1: serving_profile.clone(),
        epoch_relation_oid: epoch.0,
        effect_relation_oid: pins[0],
        mam_relation_oids: [pins[1], pins[2]],
    };
    let pinned = pinned_pubsub_inventory(
        &admin,
        &runtime_options,
        &config,
        installation_operation,
        (&runtime, &installer, &definer),
        serving_profile.helper_relation_oids,
    )
    .await?;
    exercise_pinned_pubsub(&admin, &runtime, &owner, &definer, &pinned).await?;
    pinned_partial_profile::exercise(
        &admin,
        &pinned,
        (&runtime, &api, &helper, &migrator, &owner),
    )
    .await?;
    #[cfg(feature = "clustering")]
    super::coverage_gap_gate_tests::exercise_combined_fixture(
        &admin,
        &database_owner,
        &owner,
        main,
    )
    .await?;
    let owner_role = PgRoleName::new(&owner)?;
    DatabasePubSubStorage::verify_restricted_profile_v2(main, &v2).await?;
    #[cfg(feature = "clustering")]
    RoomEffectOutboxStore::verify_muc_room_runtime_v1(main.clone(), &serving_profile).await?;
    RoomEffectOutboxStore::verify_effect_outbox_v2(main.clone(), &v2).await?;
    DatabaseMamStorage::verify_restricted_profile_v2(main, &owner_role, &v2)
        .await
        .context("initial v2 MAM inspection")?;

    #[cfg(not(feature = "clustering"))]
    {
        let schema = PgSchemaName("guarded".to_owned());
        ensure!(
            matches!(
                DatabaseMamStorage::attach(main, owner_role.clone(), schema.clone()).await,
                Err(waddle_server::mam_runtime::DatabaseMamStorageError::UnsafeMamPrivileges)
            ),
            "legacy MAM unexpectedly accepted A/H grants"
        );
        owner_sql(
            &owner_db,
            &format!("REVOKE USAGE ON SCHEMA guarded FROM {api}, {helper}, pg_monitor"),
        )
        .await?;
        let legacy = DatabaseMamStorage::attach(main, owner_role.clone(), schema)
            .await
            .context("legacy MAM after A/H revocation")?;
        drop(legacy);
        owner_sql(
            &owner_db,
            &format!("GRANT USAGE ON SCHEMA guarded TO {api}, {helper}, pg_monitor"),
        )
        .await?;
    }

    let mut wrong = v2.clone();
    wrong.epoch_relation_oid = v2.epoch_relation_oid.wrapping_add(1);
    ensure!(
        DatabasePubSubStorage::verify_restricted_profile_v2(main, &wrong)
            .await
            .is_err(),
        "wrong epoch pin accepted"
    );
    let mut wrong = v2.clone();
    wrong.effect_relation_oid = v2.mam_relation_oids[0];
    ensure!(
        RoomEffectOutboxStore::verify_effect_outbox_v2(main.clone(), &wrong)
            .await
            .is_err(),
        "wrong effect pin accepted"
    );
    let mut wrong = v2.clone();
    wrong.mam_relation_oids.swap(0, 1);
    ensure!(
        DatabaseMamStorage::verify_restricted_profile_v2(main, &owner_role, &wrong)
            .await
            .is_err(),
        "swapped MAM pins accepted"
    );

    // V1009 uses an invoker FOR SHARE: prove A can take that row lock while
    // R cannot, with the original definitions still installed.
    for (role, succeeds) in [(&api, true), (&runtime, false)] {
        let mut role_tx = admin.begin().await?;
        sqlx::query(&format!("SET LOCAL ROLE {role}"))
            .execute(&mut *role_tx)
            .await?;
        let result =
            sqlx::query("SELECT epoch FROM guarded.ingress_protocol_epoch WHERE id=1 FOR SHARE")
                .execute(&mut *role_tx)
                .await;
        ensure!(
            result.is_ok() == succeeds,
            "V1009 row-lock privilege for {role}"
        );
        role_tx.rollback().await?;
    }
    for (function, expected) in [
        "waddle_ingress_epoch_guard",
        "waddle_ingress_protocol_epoch_guard",
    ]
    .into_iter()
    .zip(&v1009_definitions)
    {
        let actual: String = sqlx::query_scalar(
            "SELECT pg_catalog.pg_get_functiondef(pg_catalog.to_regprocedure($1))",
        )
        .bind(format!("guarded.{function}()"))
        .fetch_one(&admin)
        .await?;
        ensure!(
            &actual == expected,
            "registered V1009 function changed: {function}"
        );
    }

    // Each drift is introduced and removed in the same owner-prepared schema;
    // no verifier may return evidence while the graph is unsafe.
    for (grant, revoke) in [
        (format!("GRANT UPDATE(id) ON guarded.ingress_protocol_epoch TO {runtime}"), format!("REVOKE UPDATE(id) ON guarded.ingress_protocol_epoch FROM {runtime}")),
        (format!("GRANT UPDATE(epoch) ON guarded.ingress_protocol_epoch TO {api}"), format!("REVOKE UPDATE(epoch) ON guarded.ingress_protocol_epoch FROM {api}")),
        (format!("GRANT UPDATE(id) ON guarded.ingress_protocol_epoch TO {api} WITH GRANT OPTION"), format!("REVOKE GRANT OPTION FOR UPDATE(id) ON guarded.ingress_protocol_epoch FROM {api}")),
        ("GRANT SELECT ON guarded.ingress_protocol_epoch TO PUBLIC".to_owned(), "REVOKE SELECT ON guarded.ingress_protocol_epoch FROM PUBLIC".to_owned()),
        (format!("GRANT SELECT ON guarded.mam_messages TO {api}"), format!("REVOKE SELECT ON guarded.mam_messages FROM {api}")),
        (format!("GRANT SELECT ON guarded.mam_messages TO {helper}"), format!("REVOKE SELECT ON guarded.mam_messages FROM {helper}")),
        ("GRANT SELECT ON guarded.clustering_muc_room_effects TO PUBLIC".to_owned(), "REVOKE SELECT ON guarded.clustering_muc_room_effects FROM PUBLIC".to_owned()),
        (format!("GRANT UPDATE ON guarded.clustering_muc_room_effects TO {runtime} WITH GRANT OPTION"), format!("REVOKE GRANT OPTION FOR UPDATE ON guarded.clustering_muc_room_effects FROM {runtime}")),
        (format!("GRANT pg_read_all_data TO {helper}"), format!("REVOKE pg_read_all_data FROM {helper}")),
        (format!("GRANT TEMP ON DATABASE {database} TO {api}"), format!("REVOKE TEMP ON DATABASE {database} FROM {api}")),
        (format!("GRANT USAGE ON SCHEMA public TO {api}"), format!("REVOKE USAGE ON SCHEMA public FROM {api}")),
        (format!("GRANT CREATE ON SCHEMA public TO {runtime}"), format!("REVOKE CREATE ON SCHEMA public FROM {runtime}")),
    ] {
        sqlx::query(&grant).execute(&admin).await?;
        let pubsub = DatabasePubSubStorage::verify_restricted_profile_v2(main, &v2).await;
        let effect = RoomEffectOutboxStore::verify_effect_outbox_v2(main.clone(), &v2).await;
        let mam = DatabaseMamStorage::verify_restricted_profile_v2(main, &owner_role, &v2).await;
        // Store-local ACL drift need not be rejected by an unrelated store.
        if grant.contains("mam_messages") {
            ensure!(mam.is_err(), "MAM accepted {grant}");
        } else if grant.contains("clustering_muc_room_effects") {
            ensure!(effect.is_err(), "effect outbox accepted {grant}");
        } else {
            ensure!(pubsub.is_err() && effect.is_err() && mam.is_err(), "graph accepted {grant}");
        }
        sqlx::query(&revoke).execute(&admin).await?;
    }
    for table in ["mam_messages", "clustering_muc_room_effects"] {
        sqlx::query(&format!("CREATE FUNCTION guarded.v2_relay() RETURNS void LANGUAGE sql SECURITY DEFINER AS 'DELETE FROM guarded.{table} WHERE false'"))
            .execute(&admin).await?;
        sqlx::query(&format!(
            "ALTER FUNCTION guarded.v2_relay() OWNER TO {owner}"
        ))
        .execute(&admin)
        .await?;
        sqlx::query("REVOKE ALL ON FUNCTION guarded.v2_relay() FROM PUBLIC")
            .execute(&admin)
            .await?;
        sqlx::query(&format!(
            "GRANT EXECUTE ON FUNCTION guarded.v2_relay() TO {api}"
        ))
        .execute(&admin)
        .await?;
        ensure!(
            RoomEffectOutboxStore::verify_effect_outbox_v2(main.clone(), &v2)
                .await
                .is_err(),
            "effect accepted owner definer relay"
        );
        ensure!(
            DatabaseMamStorage::verify_restricted_profile_v2(main, &owner_role, &v2)
                .await
                .is_err(),
            "MAM accepted owner definer relay"
        );
        sqlx::query("DROP FUNCTION guarded.v2_relay()")
            .execute(&admin)
            .await?;
    }
    sqlx::query("CREATE TABLE public.v2_unrelated(id integer)")
        .execute(&admin)
        .await?;
    for role in [&api, &runtime] {
        sqlx::query(&format!("GRANT SELECT ON public.v2_unrelated TO {role}"))
            .execute(&admin)
            .await?;
        ensure!(
            DatabasePubSubStorage::verify_restricted_profile_v2(main, &v2)
                .await
                .is_err(),
            "{role} gained another schema's table authority"
        );
        sqlx::query(&format!("REVOKE SELECT ON public.v2_unrelated FROM {role}"))
            .execute(&admin)
            .await?;
    }
    sqlx::query("DROP TABLE public.v2_unrelated")
        .execute(&admin)
        .await?;
    DatabasePubSubStorage::verify_restricted_profile_v2(main, &v2).await?;
    RoomEffectOutboxStore::verify_effect_outbox_v2(main.clone(), &v2).await?;
    DatabaseMamStorage::verify_restricted_profile_v2(main, &owner_role, &v2).await?;

    // Run only after the older partial profile checks, which intentionally
    // begin with these additional families absent.
    combined_restricted_profile_extra::prepare_and_check(
        &owner_db,
        main,
        &identity,
        &admin,
        &pinned,
        &serving_profile,
        combined_restricted_profile_extra::FixtureRoles {
            owner: &owner,
            api: &api,
            helper: &helper,
            migrator: &migrator,
            runtime: &runtime,
        },
    )
    .await?;

    // The extra owner preparation is a later sequential phase. Recheck the
    // closed MAM profile afterward while keeping its clustering-disabled
    // selector refusal and all serving paths unchanged.
    DatabaseMamStorage::verify_restricted_profile_v2(main, &owner_role, &v2)
        .await
        .context("closed MAM profile after sequential extra owner phase")?;

    // A complete V1030 ledger is checked again for every pinned diagnostic.
    sqlx::query("DELETE FROM guarded._migrations WHERE version = 2000")
        .execute(&admin)
        .await?;
    ensure!(
        DatabasePubSubStorage::start_pinned_relation_inventory(&pinned.inventory)?
            .finish()
            .await
            .is_err(),
        "pinned diagnostic accepted an incomplete migration ledger"
    );
    drop(pinned);
    drop(probe);
    drop(pending);
    drop(sm);
    drop(inbox);
    drop(pool);
    drop(owner_db);
    admin.close().await;
    Ok(())
}

#[tokio::test]
async fn pg17_combined_stores_and_profile_conflicts() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let cluster = PgPool::connect(&url)
        .await
        .expect("connect disposable PostgreSQL cluster");
    let mut resources = Resources {
        cluster,
        database: None,
        roles: Vec::new(),
    };
    let result = std::panic::AssertUnwindSafe(exercise(&mut resources, &url))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("combined profile fixture panicked")));
    let cleanup = resources.cleanup().await;
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => {
            panic!("qualification failed: {error:#}; cleanup failed: {cleanup:#}")
        }
        (Err(error), Ok(())) => panic!("qualification failed: {error:#}"),
        (Ok(()), Err(cleanup)) => panic!("fixture cleanup failed: {cleanup:#}"),
        (Ok(()), Ok(())) => {}
    }
}
