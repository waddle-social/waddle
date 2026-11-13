//! Restricted PostgreSQL runtime role: owner phase, then runtime attachment.
//!
//! A fresh database gets separate owner roles (a NOLOGIN schema owner and
//! its migrator login) and a runtime login that holds no DDL privilege. The
//! library owner phase prepares every schema; the runtime login then runs the
//! pinned preflight, attaches the attested serving pool, and attaches each
//! startup store read-only. A missing or drifted relation fails closed.
//!
//! `restricted_runtime_role_serves_without_ddl` then starts the real server
//! under the runtime login and runs a WebSocket session between two accounts.
//!
//! PostgreSQL is optional in local development, so the tests skip unless
//! `WADDLE_TEST_POSTGRES_URL` is configured. Publish-gating lanes require
//! PostgreSQL through `WADDLE_TEST_REQUIRE_POSTGRES=1` and fail if it is missing.
#![cfg(feature = "test-support")]

#[path = "restricted_runtime_startup/authority_guard.rs"]
mod authority_guard;
#[cfg(feature = "clustering")]
#[path = "restricted_runtime_startup/clustered.rs"]
mod clustered;
#[path = "restricted_runtime_startup/owner_entrypoint.rs"]
mod owner_entrypoint;
#[path = "restricted_runtime_support/mod.rs"]
mod support;

use std::str::FromStr;
use std::time::Duration;

use minidom::Element;
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::call_teardown_outbox::CallTeardownOutboxStore;
use waddle_server::channel_space_links::DatabaseChannelSpaceLinkStore;
use waddle_server::config::{IngressConfig, LineageConfig};
use waddle_server::db::bootstrap::RuntimePoolLimits;
use waddle_server::db::lineage::PgSchemaName;
use waddle_server::db::physical_attestation::DarkPinnedEntryError;
use waddle_server::db::runtime_startup::{RuntimeStartupError, RuntimeStartupIdentity};
use waddle_server::db::DatabasePool;
use waddle_server::ingress_substrate::{
    AliasGcBudget, AliasGcProgress, PostgresIngressSubstrate, ALIAS_RETENTION,
};
use waddle_server::ingress_uow::{
    verify_attested_room_observation_schema, RoomObservationSchemaError,
};
use waddle_server::mam_runtime::{DatabaseMamStorage, DatabaseMamStorageError};
use waddle_server::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use waddle_server::room_effect_outbox::RoomEffectOutboxStore;
use waddle_server::server::push_notification_runtime::verify_attested_push_notification_stores;
use waddle_server::server::runtime_owner::RestrictedRuntimeOwnerError;
use waddle_server::server::{ResolvedXmppDatabaseUrl, WebhookDeliveryAttachError};
use waddle_server::spaces_metadata::DatabaseSpacesMetadataStore;
use waddle_ws_test_support::iq::{round_trip, send_iq};
use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};
use waddle_xmpp::ingress::{MessageKey, SemanticDigest};
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::MessageType;

use support::{
    assert_catalog_unchanged, catalog_entries, changes_catalog, chat_message, collect_until,
    contains_text, drop_fixture, execute, exercise_stream_management, has_descendant, iq_request,
    mam_query, muc_presence, probe, role, run_owner_phase, send_element, Prepared, Provisioned,
    SocketEndpoint, Wire, DOMAIN, RELEASE_GIT_SHA, SCHEMA, SESSION_KEY,
};

impl SocketEndpoint {
    /// Forget what was recorded so far, such as the deliberate refusals a
    /// test provoked before the interval it asserts on.
    fn reset_wire(&self) {
        *self.wire.lock().expect("wire record") = Wire::default();
    }
}

impl Provisioned {
    async fn cleanup(self) {
        drop_fixture(self.cluster, self.admin, &self.database, &self.roles).await;
    }
}

impl Prepared {
    /// Environment that starts the server under the runtime login.
    fn runtime_env(&self) -> Vec<(&'static str, String)> {
        self.runtime_env_for(&self.runtime_url, &self.handoff)
    }

    async fn new(url: &str) -> Self {
        Self::from_provisioned(Provisioned::new(url).await, url).await
    }

    /// Run the binary to completion, for a startup that must refuse.
    async fn refused_startup(&self, extra: &[(&str, &str)]) -> (String, bool) {
        let temp = tempfile::tempdir().expect("startup directory");
        let port_file = temp.path().join("port");
        let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if key_text.starts_with("WADDLE_") || key_text.starts_with("OTEL_") {
                server.env_remove(key);
            }
        }
        server
            .env(
                "WADDLE_OCCUPANT_ID_SECRET",
                "integration-test-occupant-id-secret-32-bytes-long",
            )
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_ENABLED", "true")
            .env(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD",
                "refused-startup-password",
            )
            .env("WADDLE_SERVER_OWNER_LOCALPARTS", "admin")
            .env("WADDLE_XMPP_DOMAIN", "localhost")
            .env("WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY", "true")
            .env("WADDLE_UPLOAD_DIR", temp.path().join("uploads"))
            .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
            .env("WADDLE_HTTP_PORT_FILE", &port_file)
            .kill_on_drop(true);
        for (key, value) in self.runtime_env() {
            server.env(key, value);
        }
        for (key, value) in extra {
            server.env(key, value);
        }
        let output = tokio::time::timeout(Duration::from_secs(90), server.output())
            .await
            .expect("refused startup exits")
            .expect("run server binary");
        assert!(
            !output.status.success(),
            "startup must refuse, but the server exited successfully"
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (text, port_file.exists())
    }
}

/// A record of the client sessions connected to one database while a step
/// runs, sampled through a session on another database of the cluster so
/// that the watch is not one of the sessions it records.
///
/// An owner phase that requires the database to itself refuses when it sees
/// another session. The record names that session: its backend, login,
/// `application_name`, when it started and when it was last seen.
#[cfg(feature = "clustering")]
struct SessionWatch {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    samples: tokio::task::JoinHandle<Vec<String>>,
}

#[cfg(feature = "clustering")]
impl SessionWatch {
    /// Start recording the sessions of `database` through `cluster`, a pool
    /// on another database.
    fn start(cluster: PgPool, database: String) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = std::sync::Arc::clone(&stop);
        let samples = tokio::spawn(async move {
            struct Seen {
                pid: i32,
                login: String,
                application_name: String,
                backend_start: String,
                state: String,
                query: String,
                first: Duration,
                last: Duration,
            }
            let started = std::time::Instant::now();
            let mut seen: Vec<Seen> = Vec::new();
            while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                type Row = (
                    i32,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                );
                let rows: Vec<Row> = sqlx::query_as(
                    "SELECT pid, usename::text, application_name, backend_start::text, state, \
                            pg_catalog.left(query, 120) \
                       FROM pg_catalog.pg_stat_activity \
                      WHERE datname = $1 AND backend_type = 'client backend'",
                )
                .bind(&database)
                .fetch_all(&cluster)
                .await
                .unwrap_or_default();
                let at = started.elapsed();
                for (pid, login, application_name, backend_start, state, query) in rows {
                    let backend_start = backend_start.unwrap_or_default();
                    let state = state.unwrap_or_default();
                    let query = query.unwrap_or_default();
                    match seen
                        .iter_mut()
                        .find(|known| known.pid == pid && known.backend_start == backend_start)
                    {
                        Some(known) => {
                            known.last = at;
                            known.state = state;
                            known.query = query;
                        }
                        None => seen.push(Seen {
                            pid,
                            login: login.unwrap_or_default(),
                            application_name: application_name.unwrap_or_default(),
                            backend_start,
                            state,
                            query,
                            first: at,
                            last: at,
                        }),
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let ended = started.elapsed();
            seen.into_iter()
                .map(|session| {
                    format!(
                        "pid {} login {} application_name {:?} backend_start {} seen +{:?}..+{:?} \
                         of {:?}; last state {:?}, last query {:?}",
                        session.pid,
                        session.login,
                        session.application_name,
                        session.backend_start,
                        session.first,
                        session.last,
                        ended,
                        session.state,
                        session.query
                    )
                })
                .collect()
        });
        Self { stop, samples }
    }

    /// Stop recording and return one line per session that was seen.
    async fn finish(self) -> Vec<String> {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.samples.await.unwrap_or_default()
    }
}

/// End every session the database administrator has on `database`: close
/// `admin`, drop it, and wait until none of the administrator's backends is
/// left there.
///
/// `PgPool::close` leaves a connection open when that connection was being
/// returned to the pool while `close` ran: it is put back as idle after
/// `close` has emptied the idle queue, and stays connected for as long as a
/// handle to the pool exists. Dropping the pool closes its socket, and the
/// backend exits some time after that, so the sessions are awaited where
/// the server reports them. `cluster` is a pool of the same login on
/// another database; its sessions are not counted.
#[cfg(feature = "clustering")]
async fn end_admin_sessions(admin: PgPool, cluster: &PgPool, database: &str) {
    admin.close().await;
    drop(admin);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining: Vec<(i32, Option<String>)> = sqlx::query_as(
            "SELECT pid, pg_catalog.left(query, 120) FROM pg_catalog.pg_stat_activity \
              WHERE datname = $1 AND usename = session_user \
                AND backend_type = 'client backend'",
        )
        .bind(database)
        .fetch_all(cluster)
        .await
        .expect("read the administrator's sessions");
        if remaining.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the administrator's sessions on {database} did not end: {remaining:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Disconnect the fixture's administrator from its database before a step
/// that needs the database to itself. The fixture keeps a pool that
/// connects on its next use.
#[cfg(feature = "clustering")]
async fn disconnect_admin(provisioned: &mut Provisioned) {
    let reconnecting = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy_with(provisioned.base.clone().database(&provisioned.database));
    let admin = std::mem::replace(&mut provisioned.admin, reconnecting);
    end_admin_sessions(admin, &provisioned.cluster, &provisioned.database).await;
}

async fn runtime_connection(prepared: &Prepared) -> PgConnection {
    PgConnection::connect_with(
        &PgConnectOptions::from_str(&prepared.runtime_url).expect("runtime URL"),
    )
    .await
    .expect("runtime login connects")
}

fn sqlstate(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .map(|code| code.into_owned())
}

const POOL_LIMITS: RuntimePoolLimits = RuntimePoolLimits {
    main_connections: 2,
    ingress_connections: 1,
    acquire_timeout: Duration::from_secs(10),
};

fn startup_identity(prepared: &Prepared) -> RuntimeStartupIdentity {
    let lineage = LineageConfig {
        deployment_uuid: Some(prepared.lineage.deployment_uuid),
        action: None,
    };
    RuntimeStartupIdentity::from_json(&prepared.handoff, &lineage, RELEASE_GIT_SHA)
        .expect("operator handoff")
}

/// Run the library half of restricted startup: the pinned read-only
/// preflight, then the attested serving pool it authorizes.
async fn attach_serving_pool(prepared: &Prepared) -> DatabasePool {
    let identity = startup_identity(prepared);
    let preparation = identity
        .preflight(&prepared.runtime_url, POOL_LIMITS)
        .await
        .expect("pinned preflight under the runtime login");
    identity
        .attach_serving(preparation, &prepared.runtime_url, POOL_LIMITS, None)
        .await
        .expect("attested serving pool under the runtime login")
}

async fn assert_runtime_role_cannot_run_ddl(prepared: &Prepared) {
    let mut runtime = runtime_connection(prepared).await;
    for statement in [
        format!("CREATE TABLE {SCHEMA}.runtime_probe (id integer)"),
        "CREATE TEMPORARY TABLE runtime_probe (id integer)".to_owned(),
        format!("ALTER TABLE {SCHEMA}.webhook_deliveries ADD COLUMN probe integer"),
        format!("CREATE INDEX runtime_probe ON {SCHEMA}.webhook_deliveries (attempt_count)"),
        format!("DROP TABLE {SCHEMA}.webhook_deliveries"),
        format!("TRUNCATE {SCHEMA}.webhook_deliveries"),
    ] {
        let error = sqlx::query(&statement)
            .execute(&mut runtime)
            .await
            .expect_err("the runtime role must not run DDL");
        assert_eq!(
            sqlstate(&error).as_deref(),
            Some("42501"),
            "{statement} must fail with insufficient privilege: {error}"
        );
    }
    runtime.close().await.expect("close runtime probe");
}

async fn assert_runtime_owns_nothing(prepared: &Prepared) {
    let owned: i64 = sqlx::query_scalar(
        "SELECT (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c \
                  JOIN pg_catalog.pg_roles r ON r.oid = c.relowner WHERE r.rolname = $1) \
              + (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p \
                  JOIN pg_catalog.pg_roles r ON r.oid = p.proowner WHERE r.rolname = $1) \
              + (SELECT pg_catalog.count(*) FROM pg_catalog.pg_namespace n \
                  JOIN pg_catalog.pg_roles r ON r.oid = n.nspowner WHERE r.rolname = $1)",
    )
    .bind(&prepared.roles.runtime)
    .fetch_one(&prepared.admin)
    .await
    .expect("count runtime-owned objects");
    assert_eq!(owned, 0, "the runtime role must own no catalog object");
}

/// Attach every store a restricted runtime attaches whether or not it is
/// clustered, in startup order, through the selection functions startup
/// calls. MAM and the ingress authority are attached by the caller, which
/// knows the topology.
async fn attach_single_node_stores(
    prepared: &Prepared,
    pool: &DatabasePool,
    inherited: &ResolvedXmppDatabaseUrl,
) {
    let main = pool.global();
    MucDestroyCompletionOutboxStore::attach_existing(main.clone())
        .await
        .expect("attach MUC destroy completion outbox");
    waddle_server::test_support::create_inbox_startup_for_test(inherited, main)
        .await
        .expect("attach inbox");
    DatabaseSpacesMetadataStore::attach_existing(main.clone())
        .await
        .expect("attach spaces metadata");
    DatabaseChannelSpaceLinkStore::attach_existing(main.clone())
        .await
        .expect("attach channel-space links");
    waddle_server::server::create_pubsub_storage(main, inherited)
        .await
        .expect("attach PubSub");
    CallTeardownOutboxStore::attach_existing(
        main.clone(),
        SharedNodeIdentity::new(NodeIdentity::local()),
    )
    .await
    .expect("attach call teardown outbox");
    RoomEffectOutboxStore::attach_serving(main.clone())
        .await
        .expect("attach room effect outbox");
    waddle_server::test_support::attach_pending_startup(inherited, main)
        .await
        .expect("attach pending delivery");
    waddle_server::test_support::attach_sm_startup(inherited, main, false)
        .await
        .expect("attach stream management");
    // Push registrations, Push Service, VAPID, notification outbox and
    // notification activity, with the root key the owner phase sealed under.
    let _push = verify_attested_push_notification_stores(
        main.clone(),
        SESSION_KEY.as_bytes(),
        "push.localhost".parse().expect("push service JID"),
    )
    .await
    .expect("attach Push and notification stores");
    waddle_server::test_support::attach_webhook_delivery_ledger(main)
        .await
        .expect("attach webhook delivery ledger");
    verify_attested_room_observation_schema(
        main,
        &prepared.lineage,
        &role(&prepared.roles.migrator),
    )
    .await
    .expect("verify room observation schema");
}

/// The owner phase prepares every store the non-clustered runtime attaches
/// at startup, and the runtime role attaches each one read-only on its
/// attested pool. The attachments run in startup order through the same
/// selection functions startup calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_prepares_the_stores_the_runtime_role_attaches() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    prepared.endpoint.reset_wire();
    assert_runtime_role_cannot_run_ddl(&prepared).await;
    // The relay in front of PostgreSQL records exactly those attempts, which
    // is what lets the serving test assert that it recorded none.
    let wire = prepared.endpoint.take_wire();
    assert_eq!(
        wire.catalog_statements.len(),
        6,
        "{:#?}",
        wire.catalog_statements
    );
    assert_eq!(
        wire.errors
            .iter()
            .filter(|(sqlstate, _)| sqlstate == "42501")
            .count(),
        6,
        "{:#?}",
        wire.errors
    );
    let before = catalog_entries(&prepared.admin).await;

    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal(prepared.runtime_url.clone());

    attach_single_node_stores(&prepared, &pool, &inherited).await;
    DatabaseMamStorage::attach_single_node(
        main,
        role(&prepared.roles.schema_owner),
        PgSchemaName(SCHEMA.to_owned()),
    )
    .await
    .expect("attach MAM");
    let ingress = waddle_server::test_support::create_ingress_authority_at_startup(
        &ingress_config(),
        &pool,
        &runtime_lineage(&prepared),
    )
    .await
    .expect("ingress takes the epoch row lock under the runtime role");
    drop(ingress);

    // Attachment is read-only: no DDL ran and no ACL changed, so the catalog
    // is byte-for-byte what the owner phase left, and the runtime owns nothing.
    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "runtime attachment changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;

    drop(pool);
    prepared.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_runtime_refuses_instead_of_creating_a_missing_relation() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;

    // The webhook ledger refuses a privilege the owner phase did not grant,
    // and a missing ledger is an attachment error, not something to create.
    let pool = attach_serving_pool(&prepared).await;
    execute(
        &prepared.admin,
        &format!(
            "GRANT TRUNCATE ON {SCHEMA}.webhook_deliveries TO {}",
            prepared.roles.runtime
        ),
    )
    .await;
    assert!(matches!(
        waddle_server::test_support::attach_webhook_delivery_ledger(pool.global()).await,
        Err(WebhookDeliveryAttachError::UnsafeRuntimePrivileges {
            component: "webhook_deliveries"
        })
    ));
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.webhook_deliveries"),
    )
    .await;
    assert!(matches!(
        waddle_server::test_support::attach_webhook_delivery_ledger(pool.global()).await,
        Err(WebhookDeliveryAttachError::OwnerPreparationMissing(_))
    ));
    drop(pool);
    assert!(
        !relation_exists(&prepared.admin, "webhook_deliveries").await,
        "the runtime created the missing webhook ledger"
    );

    // One relation the server attaches at startup is missing. The real
    // binary must fail closed on that attachment and must not create it.
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.clustering_muc_destroy_outbox"),
    )
    .await;
    let before = catalog_entries(&prepared.admin).await;
    let (output, listener_published) = prepared.refused_startup(&[]).await;
    assert!(
        output.contains("failed to attach MUC destroy completion outbox"),
        "a missing owner-prepared relation must fail its attachment: {output}"
    );
    assert!(
        !listener_published,
        "a refused startup published a listener"
    );
    assert!(
        !relation_exists(&prepared.admin, "clustering_muc_destroy_outbox").await,
        "the runtime created the missing relation"
    );
    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "a refused startup changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;

    prepared.cleanup().await;
}

fn ingress_config() -> IngressConfig {
    IngressConfig {
        pool_size: POOL_LIMITS.ingress_connections,
        ..IngressConfig::default()
    }
}

fn runtime_lineage(prepared: &Prepared) -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(prepared.lineage.deployment_uuid),
        action: None,
    }
}

async fn preflight(prepared: &Prepared) -> Result<(), RuntimeStartupError> {
    startup_identity(prepared)
        .preflight(&prepared.runtime_url, POOL_LIMITS)
        .await
        .map(|_| ())
}

/// The runtime holds `UPDATE (id)` on the epoch row so that it can lock it
/// `FOR SHARE`. That grant cannot change the epoch, and the startup
/// preflight requires exactly it: SELECT alone, or anything wider, refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_locks_the_epoch_row_but_cannot_change_it() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let runtime = &prepared.roles.runtime;
    let epoch = format!("{SCHEMA}.ingress_protocol_epoch");

    // The owner phase ends by stepping the epoch to the supported one and
    // stamping the step with the deployment's lineage; that row is what the
    // runtime must leave exactly as it found it.
    let read_row = format!(
        "SELECT id, epoch, activated_at::text, lineage_uuid::text FROM {epoch} ORDER BY id"
    );
    let activated: Vec<(i32, i64, Option<String>, Option<String>)> = sqlx::query_as(&read_row)
        .fetch_all(&prepared.admin)
        .await
        .expect("read the activated epoch row");
    let [(1, 2, Some(_), Some(lineage))] = activated.as_slice() else {
        panic!("the owner phase leaves one row at epoch 2 with its stamp: {activated:?}");
    };
    assert_eq!(lineage, &prepared.lineage.lineage_uuid.0.to_string());

    let mut connection = runtime_connection(&prepared).await;
    let locked: i64 =
        sqlx::query_scalar(&format!("SELECT epoch FROM {epoch} WHERE id = 1 FOR SHARE"))
            .fetch_one(&mut connection)
            .await
            .expect("the runtime locks the epoch row for share");
    assert_eq!(locked, 2);
    // The column grant reaches the guard trigger, which refuses every
    // change that is not an owner-run epoch step; every other statement is
    // refused for lack of privilege before any trigger or constraint runs.
    for (statement, sqlstate) in [
        (format!("UPDATE {epoch} SET id = 2"), "P0001"),
        (format!("UPDATE {epoch} SET id = 1"), "P0001"),
        (format!("UPDATE {epoch} SET epoch = 1"), "42501"),
        (format!("UPDATE {epoch} SET epoch = 3"), "42501"),
        (
            format!("UPDATE {epoch} SET activated_at = pg_catalog.now()"),
            "42501",
        ),
        (
            format!("UPDATE {epoch} SET lineage_uuid = pg_catalog.gen_random_uuid()"),
            "42501",
        ),
        (
            format!("INSERT INTO {epoch} (id, epoch) VALUES (2, 0)"),
            "42501",
        ),
        (format!("DELETE FROM {epoch}"), "42501"),
    ] {
        let error = sqlx::query(&statement)
            .execute(&mut connection)
            .await
            .expect_err("the runtime must not change the epoch row");
        assert_eq!(
            self::sqlstate(&error).as_deref(),
            Some(sqlstate),
            "{statement}: {error}"
        );
    }
    connection.close().await.expect("close runtime probe");
    let unchanged: Vec<(i32, i64, Option<String>, Option<String>)> = sqlx::query_as(&read_row)
        .fetch_all(&prepared.admin)
        .await
        .expect("read the epoch row");
    assert_eq!(unchanged, activated);

    preflight(&prepared)
        .await
        .expect("the preflight accepts the serving epoch grant");
    // Each deviation is applied and then undone; the grant the owner phase
    // made is restored at the end of every round.
    let restore = format!("GRANT UPDATE (id) ON {epoch} TO {runtime}");
    for (deviation, undo) in [
        (
            format!("REVOKE UPDATE (id) ON {epoch} FROM {runtime}"),
            restore.clone(),
        ),
        (
            format!("GRANT UPDATE ON {epoch} TO {runtime}"),
            format!("REVOKE UPDATE ON {epoch} FROM {runtime}; {restore}"),
        ),
        (
            format!("GRANT UPDATE (epoch) ON {epoch} TO {runtime}"),
            format!("REVOKE UPDATE (epoch) ON {epoch} FROM {runtime}"),
        ),
        (
            format!("GRANT UPDATE (id) ON {epoch} TO {runtime} WITH GRANT OPTION"),
            format!("REVOKE UPDATE (id) ON {epoch} FROM {runtime}; {restore}"),
        ),
        (
            format!("GRANT SELECT (id) ON {epoch} TO {runtime}"),
            format!("REVOKE SELECT (id) ON {epoch} FROM {runtime}"),
        ),
        (
            format!("GRANT DELETE ON {epoch} TO {runtime}"),
            format!("REVOKE DELETE ON {epoch} FROM {runtime}"),
        ),
        (
            format!(
                "GRANT UPDATE (table_name) ON {SCHEMA}.ingress_epoch_guard_manifest TO {runtime}"
            ),
            format!(
                "REVOKE UPDATE (table_name) ON {SCHEMA}.ingress_epoch_guard_manifest FROM {runtime}"
            ),
        ),
    ] {
        execute(&prepared.admin, &deviation).await;
        assert!(
            matches!(
                preflight(&prepared).await,
                Err(RuntimeStartupError::PinnedPreflightIndeterminate(
                    DarkPinnedEntryError::Catalog(_)
                ))
            ),
            "the preflight must refuse after: {deviation}"
        );
        for statement in undo.split("; ") {
            execute(&prepared.admin, statement).await;
        }
        preflight(&prepared).await.unwrap_or_else(|error| {
            panic!("the preflight must pass after undoing {deviation}: {error}")
        });
    }

    prepared.cleanup().await;
}

/// Alias garbage collection runs on the serving pool under the runtime role.
/// A terminal message past its retention is recorded and collected through
/// the production code path; the collection and the orphan-proof sweep that
/// follows it each begin through the typed ingress entry, and the runtime
/// grants cover every statement they issue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alias_gc_collects_an_expired_message_under_the_runtime_role() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let store = PostgresIngressSubstrate::open(pool.global().clone()).expect("ingress substrate");
    let digest = |byte: u8| SemanticDigest::from_storage(1, [byte; 32]).expect("fixture digest");
    let expired = MessageKey::new();
    let retained = MessageKey::new();
    let mut tx = store.begin().await.expect("attested ingress writer");
    for (key, byte) in [(expired, 1), (retained, 2)] {
        store
            .record_message(&mut tx, key, &digest(byte), None)
            .await
            .expect("record a canonical message");
    }
    store
        .terminalize_message(
            &mut tx,
            expired,
            chrono::Utc::now() - ALIAS_RETENTION - chrono::Duration::seconds(1),
        )
        .await
        .expect("terminalize past the retention cutoff");
    tx.commit().await.expect("commit the fixture messages");
    prepared.endpoint.reset_wire();

    let outcome = store
        .gc_expired_aliases(
            chrono::Utc::now(),
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + Duration::from_secs(30),
                lock_timeout: Duration::from_secs(5),
                statement_timeout: Duration::from_secs(10),
                scan_timeout: Duration::from_secs(10),
                progress: AliasGcProgress::default(),
            },
        )
        .await
        .expect("alias GC under the runtime role");
    assert_eq!(outcome.deleted_messages, 1);
    assert!(outcome.completed, "the pass and its orphan sweep finish");

    let remaining: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT message_key FROM {SCHEMA}.ingress_messages ORDER BY message_key"
    ))
    .fetch_all(&prepared.admin)
    .await
    .expect("read the remaining canonical messages");
    assert_eq!(remaining, vec![retained.to_storage()]);
    let wire = prepared.endpoint.take_wire();
    assert!(
        wire.errors.is_empty(),
        "alias GC drew errors on a runtime connection: {:#?}",
        wire.errors
    );

    drop(store);
    drop(pool);
    prepared.cleanup().await;
}

/// MAM admits `pg_monitor` in the schema ACL only for the owner-granted,
/// non-grantable USAGE that room observation preparation issues.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mam_admits_only_the_exact_monitor_schema_grant() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let mam = || {
        DatabaseMamStorage::attach_single_node(
            main,
            role(&prepared.roles.schema_owner),
            PgSchemaName(SCHEMA.to_owned()),
        )
    };
    mam().await.expect("MAM attaches with the monitor grant");
    let owner = &prepared.roles.schema_owner;
    let database_owner = &prepared.roles.database_owner;
    let monitor_grantor = |from: &str, to: &str| {
        format!(
            "UPDATE pg_catalog.pg_namespace SET nspacl = pg_catalog.array_replace(nspacl, \
             'pg_monitor=U/{from}'::pg_catalog.aclitem, 'pg_monitor=U/{to}'::pg_catalog.aclitem) \
             WHERE nspname = '{SCHEMA}'"
        )
    };

    // A clustered selection without a live claim pair refuses before any
    // attachment; the single-node selection is the one that attaches.
    #[cfg(feature = "clustering")]
    {
        let handoff = waddle_server::server::MamOwnerClaimHandoff::new(
            role(owner),
            PgSchemaName(SCHEMA.to_owned()),
        )
        .expect("MAM owner handoff");
        let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal(prepared.runtime_url.clone());
        let refused = waddle_server::test_support::select_mam_storage_for_test(
            &inherited,
            Some(&handoff),
            true,
            false,
            main,
        )
        .await
        .expect_err("clustered attested MAM must refuse without a claim pair");
        assert_eq!(
            refused.to_string(),
            "clustered attested MAM storage requires live claim handles"
        );
        waddle_server::test_support::select_mam_storage_for_test(
            &inherited,
            Some(&handoff),
            false,
            false,
            main,
        )
        .await
        .expect("single-node attested MAM selection attaches");
    }

    for (deviation, undo) in [
        (
            format!("GRANT CREATE ON SCHEMA {SCHEMA} TO pg_monitor"),
            format!("REVOKE CREATE ON SCHEMA {SCHEMA} FROM pg_monitor"),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO pg_monitor WITH GRANT OPTION"),
            format!(
                "REVOKE USAGE ON SCHEMA {SCHEMA} FROM pg_monitor; GRANT USAGE ON SCHEMA {SCHEMA} TO pg_monitor"
            ),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO pg_read_all_stats"),
            format!("REVOKE USAGE ON SCHEMA {SCHEMA} FROM pg_read_all_stats"),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO {}", prepared.roles.old_runtime),
            format!(
                "REVOKE USAGE ON SCHEMA {SCHEMA} FROM {}",
                prepared.roles.old_runtime
            ),
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.mam_messages TO pg_monitor"),
            format!("REVOKE SELECT ON {SCHEMA}.mam_messages FROM pg_monitor"),
        ),
        // The same USAGE item, recorded as granted by a role that is not
        // the schema owner. Only a direct catalog edit produces that item
        // without also adding the grantor to the ACL.
        (monitor_grantor(owner, database_owner), monitor_grantor(database_owner, owner)),
    ] {
        execute(&prepared.admin, &deviation).await;
        assert!(
            matches!(
                mam().await,
                Err(DatabaseMamStorageError::UnsafeMamPrivileges)
            ),
            "MAM must refuse after: {deviation}"
        );
        for statement in undo.split("; ") {
            execute(&prepared.admin, statement).await;
        }
        mam()
            .await
            .unwrap_or_else(|error| panic!("MAM must attach after undoing {deviation}: {error}"));
    }

    drop(pool);
    prepared.cleanup().await;
}

/// Room observation exempts one owner-owned `SECURITY DEFINER` routine, the
/// PubSub attach-lock function, and only in its exact owner-prepared form.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_observation_exempts_only_the_exact_pubsub_lock_function() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let migrator = role(&prepared.roles.migrator);
    let observation =
        || verify_attested_room_observation_schema(main, &prepared.lineage, &migrator);
    observation()
        .await
        .expect("room observation verifies beside the PubSub lock function");

    let owner = &prepared.roles.schema_owner;
    let runtime = &prepared.roles.runtime;
    let lock = format!("{SCHEMA}.pubsub_runtime_attach_lock()");
    let original: String =
        sqlx::query_scalar("SELECT pg_catalog.pg_get_functiondef(pg_catalog.to_regprocedure($1))")
            .bind(&lock)
            .fetch_one(&prepared.admin)
            .await
            .expect("read the owner-prepared lock function");
    let definer = "LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp";
    let relay = format!("{SCHEMA}.runtime_relay()");
    let overload = format!("{SCHEMA}.pubsub_runtime_attach_lock(integer)");
    let elsewhere = "public.pubsub_runtime_attach_lock()";
    for (deviation, undo) in [
        // A different body under the same name, signature and ACL.
        (
            vec![format!(
                "CREATE OR REPLACE FUNCTION {lock} RETURNS void {definer} AS 'SELECT NULL::void'"
            )],
            vec![original.clone()],
        ),
        // The same body with a different configuration.
        (
            vec![format!(
                "ALTER FUNCTION {lock} SET search_path = pg_catalog, {SCHEMA}, pg_temp"
            )],
            vec![format!(
                "ALTER FUNCTION {lock} SET search_path = pg_catalog, pg_temp"
            )],
        ),
        // An extra grantee, and a grantable runtime grant.
        (
            vec![format!(
                "GRANT EXECUTE ON FUNCTION {lock} TO {}",
                prepared.roles.old_runtime
            )],
            vec![format!(
                "REVOKE EXECUTE ON FUNCTION {lock} FROM {}",
                prepared.roles.old_runtime
            )],
        ),
        (
            vec![format!(
                "GRANT EXECUTE ON FUNCTION {lock} TO {runtime} WITH GRANT OPTION"
            )],
            vec![
                format!("REVOKE EXECUTE ON FUNCTION {lock} FROM {runtime}"),
                format!("GRANT EXECUTE ON FUNCTION {lock} TO {runtime}"),
            ],
        ),
        // An overload of the same name, and an unrelated definer routine.
        (
            vec![
                format!("CREATE FUNCTION {overload} RETURNS void {definer} AS 'SELECT NULL::void'"),
                format!("ALTER FUNCTION {overload} OWNER TO {owner}"),
                format!("REVOKE ALL ON FUNCTION {overload} FROM PUBLIC"),
                format!("GRANT EXECUTE ON FUNCTION {overload} TO {runtime}"),
            ],
            vec![format!("DROP FUNCTION {overload}")],
        ),
        // EXECUTE reaching every role through PUBLIC.
        (
            vec![format!("GRANT EXECUTE ON FUNCTION {lock} TO PUBLIC")],
            vec![format!("REVOKE EXECUTE ON FUNCTION {lock} FROM PUBLIC")],
        ),
        // The same name and shape in another schema.
        (
            vec![
                format!(
                    "CREATE FUNCTION {elsewhere} RETURNS void {definer} AS 'SELECT NULL::void'"
                ),
                format!("ALTER FUNCTION {elsewhere} OWNER TO {owner}"),
                format!("REVOKE ALL ON FUNCTION {elsewhere} FROM PUBLIC"),
                format!("GRANT EXECUTE ON FUNCTION {elsewhere} TO {runtime}"),
            ],
            vec![format!("DROP FUNCTION {elsewhere}")],
        ),
        (
            vec![
                format!("CREATE FUNCTION {relay} RETURNS void {definer} AS 'SELECT NULL::void'"),
                format!("ALTER FUNCTION {relay} OWNER TO {owner}"),
                format!("REVOKE ALL ON FUNCTION {relay} FROM PUBLIC"),
                format!("GRANT EXECUTE ON FUNCTION {relay} TO {runtime}"),
            ],
            vec![format!("DROP FUNCTION {relay}")],
        ),
    ] {
        for statement in &deviation {
            execute(&prepared.admin, statement).await;
        }
        assert!(
            matches!(
                observation().await,
                Err(RoomObservationSchemaError::UnsafeRuntime)
            ),
            "room observation must refuse after: {deviation:?}"
        );
        for statement in &undo {
            execute(&prepared.admin, statement).await;
        }
        observation().await.unwrap_or_else(|error| {
            panic!("room observation must verify after undoing {deviation:?}: {error}")
        });
    }

    drop(pool);
    prepared.cleanup().await;
}

async fn iq_result(client: &mut WsXmppClient, request: Iq) -> Option<Element> {
    match round_trip(client, request).await {
        Iq::Result { payload, .. } => payload,
        other => panic!("expected an IQ result, received {other:?}"),
    }
}

/// The relay's statement classifier sees through leading whitespace and
/// comments, treats an anonymous code block as a catalog change, and leaves
/// data statements alone.
#[test]
fn relay_classifies_catalog_changing_statements() {
    for statement in [
        "CREATE TABLE probe (id integer)",
        "  \n\talter table probe add column other integer",
        "-- a comment\nDROP TABLE probe",
        "/* a comment */ /* another */ GRANT SELECT ON probe TO PUBLIC",
        "SELECT 1; TRUNCATE probe",
        "DO $$ BEGIN EXECUTE 'CREATE TABLE probe (id integer)'; END $$",
        "REASSIGN OWNED BY old_role TO new_role",
    ] {
        assert!(changes_catalog(statement), "{statement}");
    }
    for statement in [
        "SELECT 1",
        "WITH moved AS (DELETE FROM probe RETURNING id) INSERT INTO other SELECT id FROM moved",
        "INSERT INTO probe (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET id = 2",
        "-- CREATE TABLE in a comment\nSELECT 1",
        "UPDATE probe SET note = 'created'",
        "",
    ] {
        assert!(!changes_catalog(statement), "{statement}");
    }
}

/// Catalog entries a repeated owner phase must leave alone. Pending delivery
/// owner preparation drops and recreates its ordinal trigger on every run, so
/// that one trigger is compared by name and relation, without its OID.
async fn owner_phase_stable_entries(admin: &PgPool) -> Vec<String> {
    const RECREATED: &str = "pending_notification_recovery_ordinal_immutable";
    let mut entries: Vec<String> = catalog_entries(admin)
        .await
        .into_iter()
        .map(|entry| {
            let mut fields = entry.split(' ');
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some("trigger"), Some(_), Some(RECREATED), Some(relation)) => {
                    format!("trigger {RECREATED} {relation}")
                }
                _ => entry,
            }
        })
        .collect();
    entries.sort();
    entries
}

/// The handoff the fixtures start the server with is composed by
/// `compose_handoff`. This reads every database-derived value of it
/// separately, as the administrator, with its own catalog query, and
/// requires the composed document to carry exactly those values.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn composed_handoff_carries_the_pins_an_administrator_reads() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let composed: serde_json::Value =
        serde_json::from_str(&prepared.handoff).expect("handoff is JSON");

    let pins: Vec<i64> = sqlx::query_scalar(
        "SELECT pin FROM pg_catalog.unnest(ARRAY[ \
           (SELECT datdba::bigint FROM pg_catalog.pg_database \
             WHERE datname = pg_catalog.current_database()), \
           (SELECT nspowner::bigint FROM pg_catalog.pg_namespace WHERE nspname = $1), \
           (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = $3), \
           (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = $4), \
           (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = $5), \
           (SELECT oid::bigint FROM pg_catalog.pg_namespace WHERE nspname = $2), \
           pg_catalog.to_regclass($1 || '._lineage')::oid::bigint, \
           (SELECT indexrelid::bigint FROM pg_catalog.pg_index \
             WHERE indisprimary AND indrelid = pg_catalog.to_regclass($1 || '._lineage')), \
           pg_catalog.to_regprocedure($2 || '.read_physical_identity()')::oid::bigint, \
           pg_catalog.to_regclass($1 || '._migrations')::oid::bigint, \
           (SELECT indexrelid::bigint FROM pg_catalog.pg_index \
             WHERE indisprimary AND indrelid = pg_catalog.to_regclass($1 || '._migrations')), \
           pg_catalog.to_regclass($1 || '.ingress_protocol_epoch')::oid::bigint, \
           (SELECT indexrelid::bigint FROM pg_catalog.pg_index \
             WHERE indisprimary \
               AND indrelid = pg_catalog.to_regclass($1 || '.ingress_protocol_epoch')), \
           pg_catalog.to_regclass($1 || '.ingress_epoch_guard_manifest')::oid::bigint \
         ]) WITH ORDINALITY AS pins(pin, position) ORDER BY position",
    )
    .bind(SCHEMA)
    .bind(support::TRUSTED_SCHEMA)
    .bind(&prepared.roles.runtime)
    .bind(&prepared.roles.installer)
    .bind(&prepared.roles.definer)
    .fetch_all(&prepared.admin)
    .await
    .expect("administrator reads the pins");
    assert_eq!(
        composed["pins"],
        serde_json::json!({
            "database_owner_role_oid": pins[0],
            "schema_owner_role_oid": pins[1],
            "runtime_role_oid": pins[2],
            "installer_role_oid": pins[3],
            "definer_role_oid": pins[4],
            "trusted_schema_oid": pins[5],
            "lineage_relation_oid": pins[6],
            "lineage_primary_index_oid": pins[7],
            "attestor_function_oid": pins[8],
            "installation_operation_id": prepared.operation_id.to_string(),
            "ledger_relation_oid": pins[9],
            "ledger_primary_index_oid": pins[10],
            "epoch_relation_oid": pins[11],
            "epoch_primary_index_oid": pins[12],
            "guard_manifest_relation_oid": pins[13],
        }),
        "the composed pins must be the catalog's"
    );

    let (lineage_uuid, deployment_uuid, system_identifier, database_oid, schema_oid): (
        String,
        String,
        String,
        i64,
        i64,
    ) = sqlx::query_as(&format!(
        "SELECT l.lineage_uuid, l.deployment_uuid, \
                (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
                (SELECT oid::bigint FROM pg_catalog.pg_database \
                  WHERE datname = pg_catalog.current_database()), \
                (SELECT oid::bigint FROM pg_catalog.pg_namespace WHERE nspname = $1) \
           FROM {SCHEMA}._lineage l"
    ))
    .bind(SCHEMA)
    .fetch_one(&prepared.admin)
    .await
    .expect("administrator reads the lineage and physical identity");
    for (field, expected) in [
        ("version", serde_json::json!(3)),
        ("release_git_sha", serde_json::json!(RELEASE_GIT_SHA)),
        ("lineage_uuid", serde_json::json!(lineage_uuid)),
        ("deployment_uuid", serde_json::json!(deployment_uuid)),
        ("system_identifier", serde_json::json!(system_identifier)),
        ("database_oid", serde_json::json!(database_oid)),
        ("database_name", serde_json::json!(prepared.database)),
        ("schema_oid", serde_json::json!(schema_oid)),
        ("schema_name", serde_json::json!(SCHEMA)),
        (
            "installer_login",
            serde_json::json!(prepared.roles.installer),
        ),
        ("runtime_login", serde_json::json!(prepared.roles.runtime)),
        ("definer_role", serde_json::json!(prepared.roles.definer)),
        ("trusted_schema", serde_json::json!(support::TRUSTED_SCHEMA)),
    ] {
        assert_eq!(composed[field], expected, "{field}");
    }
    prepared.cleanup().await;
}

/// The owner phase can run again on a prepared database, classifies every
/// relation before granting, and reports a privilege it did not grant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_is_idempotent_and_fails_closed_on_unclassified_state() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let runtime = &prepared.roles.runtime;
    let before = owner_phase_stable_entries(&prepared.admin).await;
    let lineage = prepared
        .rerun_owner_phase()
        .await
        .expect("a second owner phase succeeds on a prepared database");
    assert_eq!(lineage, prepared.lineage);
    assert_eq!(
        owner_phase_stable_entries(&prepared.admin).await,
        before,
        "a repeated owner phase changed the catalog"
    );

    // A relation the matrix does not name fails the phase and gets nothing.
    execute(
        &prepared.admin,
        &format!("CREATE TABLE {SCHEMA}.unclassified_probe (id integer)"),
    )
    .await;
    execute(
        &prepared.admin,
        &format!(
            "ALTER TABLE {SCHEMA}.unclassified_probe OWNER TO {}",
            prepared.roles.schema_owner
        ),
    )
    .await;
    assert!(matches!(
        prepared.rerun_owner_phase().await,
        Err(RestrictedRuntimeOwnerError::UnclassifiedRelation { relation })
            if relation == "unclassified_probe"
    ));
    let granted: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT') \
             OR pg_catalog.has_table_privilege($1, $2, 'INSERT') \
             OR pg_catalog.has_table_privilege($1, $2, 'UPDATE') \
             OR pg_catalog.has_table_privilege($1, $2, 'DELETE')",
    )
    .bind(runtime)
    .bind(format!("{SCHEMA}.unclassified_probe"))
    .fetch_one(&prepared.admin)
    .await
    .expect("inspect the unclassified relation");
    assert!(!granted, "the runtime was granted an unclassified relation");
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.unclassified_probe"),
    )
    .await;

    // A privilege beyond the matrix, or any privilege held with the grant
    // option, is reported and left in place.
    let sequence = format!("{SCHEMA}.roster_items_id_seq");
    for (grant, undo, relation, privilege) in [
        (
            format!("GRANT DELETE ON {SCHEMA}.users TO {runtime}"),
            format!("REVOKE DELETE ON {SCHEMA}.users FROM {runtime}"),
            "users",
            "DELETE",
        ),
        (
            format!("GRANT UPDATE ON {SCHEMA}.ingress_owner_rejections TO {runtime}"),
            format!("REVOKE UPDATE ON {SCHEMA}.ingress_owner_rejections FROM {runtime}"),
            "ingress_owner_rejections",
            "UPDATE",
        ),
        (
            format!("GRANT UPDATE (epoch) ON {SCHEMA}.ingress_protocol_epoch TO {runtime}"),
            format!("REVOKE UPDATE (epoch) ON {SCHEMA}.ingress_protocol_epoch FROM {runtime}"),
            "ingress_protocol_epoch",
            "UPDATE(epoch)",
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.messages TO {runtime}"),
            format!("REVOKE SELECT ON {SCHEMA}.messages FROM {runtime}"),
            "messages",
            "SELECT",
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.users TO {runtime} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR SELECT ON {SCHEMA}.users FROM {runtime}"),
            "users",
            "SELECT WITH GRANT OPTION",
        ),
        (
            format!(
                "GRANT UPDATE (next_seq) ON {SCHEMA}.mam_archive_sequences TO {runtime} \
                 WITH GRANT OPTION"
            ),
            format!(
                "REVOKE GRANT OPTION FOR UPDATE (next_seq) ON {SCHEMA}.mam_archive_sequences \
                 FROM {runtime}"
            ),
            "mam_archive_sequences",
            "UPDATE WITH GRANT OPTION",
        ),
        (
            format!("GRANT USAGE ON SEQUENCE {sequence} TO {runtime} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR USAGE ON SEQUENCE {sequence} FROM {runtime}"),
            "roster_items_id_seq",
            "USAGE WITH GRANT OPTION",
        ),
    ] {
        execute(&prepared.admin, &grant).await;
        let result = prepared.rerun_owner_phase().await;
        assert!(
            matches!(
                &result,
                Err(RestrictedRuntimeOwnerError::ExcessRuntimePrivilege {
                    relation: found,
                    privilege: excess,
                }) if found == relation && excess == privilege
            ),
            "the owner phase must report {grant}: {:?}",
            result.err()
        );
        execute(&prepared.admin, &undo).await;
    }

    // A runtime role that is not restricted is refused the same way the
    // store attachments would refuse it.
    let owner = &prepared.roles.schema_owner;
    for (deviation, undo, component) in [
        (
            format!(
                "GRANT TEMPORARY ON DATABASE {} TO {runtime}",
                prepared.database
            ),
            format!(
                "REVOKE TEMPORARY ON DATABASE {} FROM {runtime}",
                prepared.database
            ),
            "database privileges",
        ),
        (
            format!("GRANT CREATE ON SCHEMA {SCHEMA} TO {runtime}"),
            format!("REVOKE CREATE ON SCHEMA {SCHEMA} FROM {runtime}"),
            "schema privileges",
        ),
        (
            format!("GRANT pg_read_all_stats TO {runtime}"),
            format!("REVOKE pg_read_all_stats FROM {runtime}"),
            "role membership",
        ),
        (
            format!("ALTER ROLE {runtime} CREATEDB"),
            format!("ALTER ROLE {runtime} NOCREATEDB"),
            "login attributes",
        ),
        (
            format!(
                "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA {SCHEMA} \
                 GRANT SELECT ON TABLES TO {runtime}"
            ),
            format!(
                "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA {SCHEMA} \
                 REVOKE SELECT ON TABLES FROM {runtime}"
            ),
            "default privileges",
        ),
    ] {
        execute(&prepared.admin, &deviation).await;
        let result = prepared.rerun_owner_phase().await;
        assert!(
            matches!(
                &result,
                Err(RestrictedRuntimeOwnerError::UnsafeRuntimeRole { component: found })
                    if *found == component
            ),
            "the owner phase must refuse {deviation}: {:?}",
            result.err()
        );
        execute(&prepared.admin, &undo).await;
    }
    prepared
        .rerun_owner_phase()
        .await
        .expect("the owner phase succeeds once the excess privileges are gone");

    prepared.cleanup().await;
}

/// PUBLIC privileges on the target schema, as the default `public` schema
/// carries, are refused before the owner phase runs any DDL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_refuses_a_schema_public_can_use() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let provisioned = Provisioned::new(&url).await;
    execute(
        &provisioned.admin,
        &format!("GRANT USAGE ON SCHEMA {SCHEMA} TO PUBLIC"),
    )
    .await;
    assert!(matches!(
        run_owner_phase(&provisioned.owner_url, &provisioned.plan).await,
        Err(RestrictedRuntimeOwnerError::PublicSchemaPrivilege { schema }) if schema == SCHEMA
    ));
    let relations: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1",
    )
    .bind(SCHEMA)
    .fetch_one(&provisioned.admin)
    .await
    .expect("count relations");
    assert_eq!(relations, 0, "a refused owner phase created relations");
    provisioned.cleanup().await;
}

async fn relation_exists(admin: &PgPool, relation: &str) -> bool {
    sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
        .bind(format!("{SCHEMA}.{relation}"))
        .fetch_one(admin)
        .await
        .expect("inspect relation")
}

const NS_ROSTER: &str = "jabber:iq:roster";

const NS_VCARD: &str = "vcard-temp";

const NS_PRIVATE: &str = "jabber:iq:private";

const NS_PREFS: &str = "urn:waddle:test:restricted-prefs";

const NS_BLOCKING: &str = "urn:xmpp:blocking";

const NS_UPLOAD: &str = "urn:xmpp:http:upload:0";

const NS_PUSH: &str = "urn:xmpp:push:0";

const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";

const NS_DATA_FORMS: &str = "jabber:x:data";

fn text_element(name: &str, namespace: &str, text: &str) -> Element {
    Element::builder(name, namespace)
        .append(text.to_owned())
        .build()
}

/// A roster get marks the resource interested, so the following roster set
/// produces a push to it; a second get returns the stored contact.
async fn exercise_roster(alice: &mut WsXmppClient) {
    iq_result(
        alice,
        iq_request(
            false,
            "roster-interest",
            None,
            Element::builder("query", NS_ROSTER).build(),
        ),
    )
    .await;
    let item = Element::builder("item", NS_ROSTER)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "bob@localhost",
        )
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Bob")
        .build();
    send_iq(
        alice,
        iq_request(
            true,
            "roster-set",
            None,
            Element::builder("query", NS_ROSTER).append(item).build(),
        ),
    )
    .await;
    let is_push = |stanza: &Element| {
        stanza.name() == "iq"
            && stanza.attr("type") == Some("set")
            && has_descendant(stanza, "item", NS_ROSTER)
    };
    collect_until(alice, "roster set result and push", |stanzas| {
        stanzas.iter().any(is_push)
            && stanzas.iter().any(|stanza| {
                stanza.attr("id") == Some("roster-set") && stanza.attr("type") == Some("result")
            })
    })
    .await;
    let roster = iq_result(
        alice,
        iq_request(
            false,
            "roster-get",
            None,
            Element::builder("query", NS_ROSTER).build(),
        ),
    )
    .await
    .expect("roster payload");
    assert!(
        roster
            .children()
            .any(|item| item.attr("jid") == Some("bob@localhost")),
        "the roster must hold the stored contact: {roster:?}"
    );
}

/// vCard set and get, then private XML storage written twice and read back.
async fn exercise_profile_storage(alice: &mut WsXmppClient) {
    let vcard = Element::builder("vCard", NS_VCARD)
        .append(text_element("FN", NS_VCARD, "Alice Restricted"))
        .build();
    iq_result(alice, iq_request(true, "vcard-set", None, vcard)).await;
    let stored = iq_result(
        alice,
        iq_request(
            false,
            "vcard-get",
            None,
            Element::builder("vCard", NS_VCARD).build(),
        ),
    )
    .await
    .expect("vCard payload");
    assert!(contains_text(&stored, "Alice Restricted"), "{stored:?}");

    // The second write replaces the first through the store's upsert.
    for (id, theme) in [("private-set-1", "light"), ("private-set-2", "dark")] {
        let prefs = Element::builder("prefs", NS_PREFS)
            .append(text_element("theme", NS_PREFS, theme))
            .build();
        iq_result(
            alice,
            iq_request(
                true,
                id,
                None,
                Element::builder("query", NS_PRIVATE).append(prefs).build(),
            ),
        )
        .await;
    }
    let private = iq_result(
        alice,
        iq_request(
            false,
            "private-get",
            None,
            Element::builder("query", NS_PRIVATE)
                .append(Element::builder("prefs", NS_PREFS).build())
                .build(),
        ),
    )
    .await
    .expect("private XML payload");
    assert!(
        contains_text(&private, "dark") && !contains_text(&private, "light"),
        "private storage must return the replaced element: {private:?}"
    );
}

/// Block, list and unblock one JID.
async fn exercise_blocking(alice: &mut WsXmppClient) {
    let item = || {
        Element::builder("item", NS_BLOCKING)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "spammer@localhost",
            )
            .build()
    };
    let blocklist = |id: &str| {
        iq_request(
            false,
            id,
            None,
            Element::builder("blocklist", NS_BLOCKING).build(),
        )
    };
    iq_result(
        alice,
        iq_request(
            true,
            "block",
            None,
            Element::builder("block", NS_BLOCKING)
                .append(item())
                .build(),
        ),
    )
    .await;
    let blocked = iq_result(alice, blocklist("blocklist-1"))
        .await
        .expect("blocklist payload");
    assert!(
        blocked
            .children()
            .any(|item| item.attr("jid") == Some("spammer@localhost")),
        "{blocked:?}"
    );
    iq_result(
        alice,
        iq_request(
            true,
            "unblock",
            None,
            Element::builder("unblock", NS_BLOCKING)
                .append(item())
                .build(),
        ),
    )
    .await;
    let unblocked = iq_result(alice, blocklist("blocklist-2"))
        .await
        .expect("blocklist payload");
    assert_eq!(unblocked.children().count(), 0, "{unblocked:?}");
}

/// The server owner creates a room, a second account joins, a groupchat
/// message is reflected, the room archive returns it, and the member leaves.
async fn exercise_muc(admin: &mut WsXmppClient, alice: &mut WsXmppClient) {
    let room = format!("restricted-{}@muc.localhost", Uuid::new_v4().simple());
    let joined = |stanzas: &[Element]| {
        stanzas.iter().any(|stanza| {
            stanza.name() == "message"
                && has_descendant(stanza, "subject", xmpp_parsers::ns::JABBER_CLIENT)
        })
    };
    send_element(admin, muc_presence(&format!("{room}/owner"), false)).await;
    collect_until(admin, "room creation", joined).await;
    send_element(alice, muc_presence(&format!("{room}/alice"), false)).await;
    collect_until(alice, "room join", joined).await;

    let body = format!("restricted-room-{}", Uuid::new_v4());
    send_element(
        admin,
        chat_message(&room, MessageType::Groupchat, "room-message", &body),
    )
    .await;
    collect_until(alice, "room message", |stanzas| {
        stanzas.iter().any(|stanza| contains_text(stanza, &body))
    })
    .await;
    let archived = mam_query(admin, "room-mam", Some(&room)).await;
    assert!(
        archived.iter().any(|stanza| contains_text(stanza, &body)),
        "the room archive must return the message: {archived:?}"
    );

    send_element(alice, muc_presence(&format!("{room}/alice"), true)).await;
    collect_until(alice, "room leave", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| stanza.name() == "presence" && stanza.attr("type") == Some("unavailable"))
    })
    .await;
}

/// Request an upload slot, register a push device, and enable then disable
/// push for its node.
async fn exercise_upload_and_push(alice: &mut WsXmppClient) {
    let request = Element::builder("request", NS_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "restricted.txt",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "12")
        .attr(
            minidom::rxml::xml_ncname!("content-type").to_owned(),
            "text/plain",
        )
        .build();
    let slot = iq_result(
        alice,
        iq_request(false, "upload-slot", Some("upload.localhost"), request),
    )
    .await
    .expect("upload slot payload");
    assert!(slot.is("slot", NS_UPLOAD), "{slot:?}");

    let node = register_push_device(alice).await;
    for (id, name) in [("push-enable", "enable"), ("push-disable", "disable")] {
        let payload = Element::builder(name, NS_PUSH)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "push.localhost",
            )
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
            .build();
        iq_result(alice, iq_request(true, id, Some(DOMAIN), payload)).await;
    }
}

fn form_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(text_element("value", NS_DATA_FORMS, value))
        .build()
}

fn find_field_value(element: &Element, var: &str) -> Option<String> {
    if element.is("field", NS_DATA_FORMS) && element.attr("var") == Some(var) {
        return element.get_child("value", NS_DATA_FORMS).map(Element::text);
    }
    element
        .children()
        .find_map(|child| find_field_value(child, var))
}

/// Register a Web Push device with the Push Service through its ad-hoc
/// command and return the node the service assigned.
async fn register_push_device(client: &mut WsXmppClient) -> String {
    let command = |action: &str, session: Option<&str>| {
        let mut command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "register-device",
            )
            .attr(minidom::rxml::xml_ncname!("action").to_owned(), action);
        if let Some(session) = session {
            command = command.attr(minidom::rxml::xml_ncname!("sessionid").to_owned(), session);
        }
        command
    };
    let executing = iq_result(
        client,
        iq_request(
            true,
            "push-register-execute",
            Some("push.localhost"),
            command("execute", None).build(),
        ),
    )
    .await
    .expect("register-device session");
    let session = executing
        .attr("sessionid")
        .expect("register-device session ID")
        .to_owned();
    let mut form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    for (var, value) in [
        (
            "FORM_TYPE",
            "urn:waddle:push-service:commands:register-device:0",
        ),
        ("platform", "web"),
        ("environment", "prod"),
        ("app-id", "web"),
        (
            "web-push-endpoint",
            "https://push.example.test/restricted-runtime",
        ),
        ("web-push-p256dh", "restricted-p256-key"),
        ("web-push-auth", "restricted-auth-secret"),
    ] {
        form = form.append(form_field(var, value));
    }
    let completed = iq_result(
        client,
        iq_request(
            true,
            "push-register-complete",
            Some("push.localhost"),
            command("complete", Some(&session))
                .append(form.build())
                .build(),
        ),
    )
    .await
    .expect("register-device result");
    assert_eq!(completed.attr("status"), Some("completed"), "{completed:?}");
    find_field_value(&completed, "node").expect("registered push node")
}

/// The real server under the runtime login. A dangling roster row is seeded
/// first, so startup also runs its roster prune. The session then covers
/// login and bind, a direct message, roster set/get with its push, vCard,
/// private XML storage, the block list, a room created, joined, written to,
/// read back from its archive and left, a personal archive query, an upload
/// slot, push enable/disable, and XEP-0198 enable, acknowledgement and
/// same-node resumption.
///
/// Three witnesses show the runtime ran no DDL and was never refused a
/// privilege: the catalog list is unchanged, the role owns nothing, and the
/// relay in front of PostgreSQL saw neither a catalog-changing statement nor
/// an `insufficient_privilege` error on any runtime connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_runtime_role_serves_without_ddl() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    // An upgraded database can hold a roster item whose contact is not a
    // local account. Startup prunes it together with the owner's version.
    for statement in [
        format!(
            "INSERT INTO {SCHEMA}.roster_items (user_jid, contact_jid) \
             VALUES ('alice@localhost', 'ghost@elsewhere.example')"
        ),
        format!(
            "INSERT INTO {SCHEMA}.roster_versions (user_jid, version) \
             VALUES ('alice@localhost', 'stale')"
        ),
    ] {
        execute(&prepared.admin, &statement).await;
    }
    let before = catalog_entries(&prepared.admin).await;
    prepared.endpoint.reset_wire();
    let env = prepared.runtime_env();
    let server = tokio::task::spawn_blocking(move || {
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        TestServer::start_with_extra_envs(
            &[("alice", "alice-password"), ("bob", "bob-password")],
            &env,
        )
    })
    .await
    .expect("restricted runtime server starts");

    assert_eq!(
        probe(&format!("{}/healthz", server.http_base_url())).await,
        reqwest::StatusCode::OK
    );
    assert_eq!(
        probe(&format!("{}/readyz", server.http_base_url())).await,
        reqwest::StatusCode::OK,
        "the restricted runtime must report ready"
    );
    let dangling: i64 = sqlx::query_scalar(&format!(
        "SELECT (SELECT pg_catalog.count(*) FROM {SCHEMA}.roster_items \
                  WHERE contact_jid = 'ghost@elsewhere.example') \
              + (SELECT pg_catalog.count(*) FROM {SCHEMA}.roster_versions \
                  WHERE version = 'stale')"
    ))
    .fetch_one(&prepared.admin)
    .await
    .expect("count dangling roster rows");
    assert_eq!(dangling, 0, "startup must prune the dangling roster rows");

    let connect = |user: &'static str, password: String, resource: &'static str| {
        let url = server.ws_url();
        async move {
            WsXmppClient::connect_and_auth(&url, DOMAIN, user, &password, resource)
                .await
                .unwrap_or_else(|error| panic!("{user} authenticates and binds: {error}"))
        }
    };
    let mut alice = connect("alice", "alice-password".to_owned(), "laptop").await;
    let mut bob = connect("bob", "bob-password".to_owned(), "phone").await;
    let mut admin = connect(
        "admin",
        server.fixed_account_password().to_owned(),
        "console",
    )
    .await;

    send_element(
        &mut bob,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT).build(),
    )
    .await;
    let body = format!("restricted-runtime-{}", Uuid::new_v4());
    send_element(
        &mut alice,
        chat_message("bob@localhost", MessageType::Chat, "direct-message", &body),
    )
    .await;
    collect_until(&mut bob, "direct message", |stanzas| {
        stanzas.iter().any(|stanza| contains_text(stanza, &body))
    })
    .await;

    exercise_roster(&mut alice).await;
    exercise_profile_storage(&mut alice).await;
    exercise_blocking(&mut alice).await;
    exercise_muc(&mut admin, &mut alice).await;
    let personal = mam_query(&mut alice, "personal-mam", None).await;
    assert!(
        personal.iter().any(|stanza| contains_text(stanza, &body)),
        "the personal archive must return the direct message: {personal:?}"
    );
    exercise_upload_and_push(&mut alice).await;
    exercise_stream_management(&server, &server).await;

    alice.close().await.expect("alice closes");
    bob.close().await.expect("bob closes");
    admin.close().await.expect("admin closes");

    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "the restricted runtime changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;
    let wire = prepared.endpoint.take_wire();
    assert!(
        wire.catalog_statements.is_empty(),
        "the runtime login sent catalog-changing statements: {:#?}",
        wire.catalog_statements
    );
    let refused: Vec<_> = wire
        .errors
        .iter()
        .filter(|(sqlstate, _)| sqlstate == "42501")
        .collect();
    assert!(
        refused.is_empty(),
        "the runtime login was refused a privilege: {refused:#?}"
    );

    drop(server);
    prepared.cleanup().await;
}
