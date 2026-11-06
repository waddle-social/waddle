//! Combined-store qualification for the unselected PG17 restricted profile.
//!
//! Only inbox, portable SM, and pending delivery receive runtime round trips.
//! PubSub and dark room observation have independently verified profile
//! conflicts. MAM, claims, push, rooms, and outboxes are not qualified here.
//! This is not an all-store or serving test.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{ensure, Context};
use futures::FutureExt;
use sqlx::postgres::{types::Oid, PgConnectOptions, PgPool};
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
use waddle_server::ingress_substrate::EPOCH_GUARDED_TABLES;
use waddle_server::ingress_uow::{
    prepare_dark_room_observation_schema, verify_attested_room_observation_schema,
    RoomObservationSchemaError,
};
use waddle_server::pending_delivery::{
    bootstrap_pending_delivery_owner_schema, DatabasePendingDeliveryStorage,
};
use waddle_server::pubsub::DatabasePubSubStorage;
use waddle_server::sm_persistence::{bootstrap_portable_sm_owner_schema, DatabaseSmPersistence};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{
    PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
};
use waddle_xmpp::stream_management::persistence::{PersistedSession, SmPersistenceStorage};

use super::{test_url, CATALOG_GRANT_LOCK};

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
        sqlx::query(&format!("CREATE DATABASE {name} OWNER {owner}"))
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
    let installer = format!("profile_installer_{suffix}");
    let definer = format!("profile_definer_{suffix}");
    let password = format!("p{}", Uuid::new_v4().simple());
    let restricted = "NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";
    for role in [&database_owner, &owner, &api, &definer] {
        resources
            .role(role, &format!("NOLOGIN {restricted}"))
            .await?;
    }
    for role in [&migrator, &runtime, &installer] {
        resources
            .role(role, &format!("LOGIN PASSWORD '{password}' {restricted}"))
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
    // Match the established owner fixture: startup creates the original
    // observation tables/indexes before registered migrations upgrade them.
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
    let config = install(&installer_options, config, Uuid::new_v4()).await?;
    let runtime_options = base
        .database(&database)
        .username(&runtime)
        .password(&password)
        .options([("search_path", "guarded")]);
    let attested = attested_pool(runtime_options, config, 2, Duration::from_secs(5))?;
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
    let main_guard = main.guard().await?;
    let ingress_db = pool.ingress();
    let ingress_guard = ingress_db.guard().await?;
    let mut main_rows = main_guard
        .query("SELECT pg_backend_pid()::bigint", ())
        .await?;
    let main_pid: i64 = main_rows
        .next()
        .await?
        .context("main backend pid")?
        .get(0)?;
    let mut ingress_rows = ingress_guard
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
    drop(main_guard);
    drop(ingress_guard);

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
    for table in EPOCH_GUARDED_TABLES {
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
        ("sm_unacked", "SELECT, INSERT, UPDATE, DELETE"),
        ("sm_acknowledged_windows", "SELECT, INSERT, UPDATE, DELETE"),
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
        manifest_version: 2,
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
