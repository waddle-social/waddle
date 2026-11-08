//! Disposable PostgreSQL witnesses for the registration owner's catalog fence.
#![cfg(feature = "test-support")]

#[path = "../src/test_support/postgres_proxy.rs"]
mod postgres_proxy;

use std::io;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, Notify};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{self, DeploymentUuid, PgSchemaName};
use waddle_server::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::push_registrations::{
    DatabasePushRegistrationStore, PushRegistrationOwnerError,
};
use waddle_xmpp::push::{PushSubscription, PushSubscriptionStore};

fn with_options(base: &str, database: &str, options: &str, app: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "dbname" | "options" | "application_name"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("options", options)
        .append_pair("application_name", app);
    url.to_string()
}

struct Fixture {
    cluster: PgPool,
    admin: PgPool,
    base: String,
    database: String,
    owner: String,
    definer: String,
    installer: String,
    runtime: String,
    schema: String,
    trusted: String,
    login: String,
    cleanup_guard: FixtureCleanup,
}

/// Tracks only successfully created objects, including during fixture setup.
/// A separate bounded runtime can clean up even when the test task panics and
/// its own Tokio runtime is shutting down.
struct FixtureCleanup {
    base: String,
    database: String,
    database_created: bool,
    roles_created: Vec<String>,
    armed: bool,
}

impl Drop for FixtureCleanup {
    fn drop(&mut self) {
        if !self.armed || (!self.database_created && self.roles_created.is_empty()) {
            return;
        }
        let base = self.base.clone();
        let database = self.database_created.then(|| self.database.clone());
        let roles = std::mem::take(&mut self.roles_created);
        let cleanup = std::thread::Builder::new()
            .name("registration-fixture-cleanup".to_owned())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                    eprintln!("could not create registration fixture cleanup runtime");
                    return;
                };
                runtime.block_on(async move {
                    let cleanup = async {
                        let pool = sqlx::postgres::PgPoolOptions::new()
                            .max_connections(1)
                            .acquire_timeout(Duration::from_secs(3))
                            .connect(&base)
                            .await?;
                        if let Some(database) = database {
                            if let Err(error) = sqlx::query(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
                                .execute(&pool).await {
                                eprintln!("could not drop registration fixture database {database}: {error}");
                            }
                        }
                        // DROP ROLE removes memberships after the database has
                        // been dropped; do not touch any role we did not create.
                        for role in roles.into_iter().rev() {
                            if let Err(error) = sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                                .execute(&pool).await {
                                eprintln!("could not drop registration fixture role {role}: {error}");
                            }
                        }
                        pool.close().await;
                        Ok::<(), sqlx::Error>(())
                    };
                    match tokio::time::timeout(Duration::from_secs(15), cleanup).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => eprintln!("registration fixture cleanup failed: {error}"),
                        Err(_) => eprintln!("registration fixture cleanup timed out"),
                    }
                });
            });
        match cleanup {
            Ok(thread) => {
                if thread.join().is_err() {
                    eprintln!("registration fixture cleanup thread panicked");
                }
            }
            Err(error) => eprintln!("could not start registration fixture cleanup: {error}"),
        }
    }
}

impl Fixture {
    async fn new(base: String) -> Self {
        let cluster = PgPool::connect(&base).await.expect("disposable cluster");
        let version: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&cluster)
                .await
                .expect("PostgreSQL version");
        assert!(
            (170_000..180_000).contains(&version),
            "requires PostgreSQL 17"
        );
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("reg_catalog_{suffix}");
        let owner = format!("reg_o_{suffix}");
        let definer = format!("reg_d_{suffix}");
        let installer = format!("reg_i_{suffix}");
        let runtime = format!("reg_r_{suffix}");
        let schema = format!("reg_s_{suffix}");
        let trusted = format!("reg_f_{suffix}");
        let login: String = sqlx::query_scalar("SELECT session_user::text")
            .fetch_one(&cluster)
            .await
            .expect("cluster login");
        let mut cleanup_guard = FixtureCleanup {
            base: base.clone(),
            database: database.clone(),
            database_created: false,
            roles_created: Vec::new(),
            armed: true,
        };
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&cluster)
            .await
            .expect("create fixture database");
        cleanup_guard.database_created = true;
        sqlx::query(&format!(
            "REVOKE CREATE, TEMPORARY ON DATABASE {database} FROM PUBLIC"
        ))
        .execute(&cluster)
        .await
        .expect("restrict fixture database");
        for (role, attributes) in [
            (
                &owner,
                "NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
            ),
            (
                &definer,
                "NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
            ),
            (&installer, "LOGIN"),
            (&runtime, "LOGIN"),
        ] {
            sqlx::query(&format!("CREATE ROLE {role} {attributes}"))
                .execute(&cluster)
                .await
                .expect("create fixture role");
            cleanup_guard.roles_created.push(role.clone());
        }
        for statement in [
            format!("GRANT {owner} TO \"{}\"", login.replace('"', "\"\"")),
            format!("GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"),
        ] {
            sqlx::query(&statement)
                .execute(&cluster)
                .await
                .expect("prepare cluster fixture");
        }
        let admin_url = with_options(&base, &database, "", "reg_catalog_admin");
        let admin = PgPool::connect(&admin_url).await.expect("fixture database");
        for statement in [
            "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
            format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"),
            format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}, {definer}"),
            format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}, {definer}"
            ),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("prepare schema fixture");
        }
        Self {
            cluster,
            admin,
            base,
            database,
            owner,
            definer,
            installer,
            runtime,
            schema,
            trusted,
            login,
            cleanup_guard,
        }
    }

    fn owner_url(&self, app: &str) -> String {
        with_options(
            &self.base,
            &self.database,
            &format!("-c role={} -c search_path={}", self.owner, self.schema),
            app,
        )
    }

    async fn database(&self, app: &str) -> Database {
        let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, self.owner_url(app));
        config.pool_size = 1;
        Database::from_config("registration catalog owner", &config)
            .await
            .expect("owner pool")
    }

    async fn cleanup(mut self) {
        self.admin.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster)
            .await
            .expect("drop fixture database");
        sqlx::query(&format!(
            "REVOKE {} FROM \"{}\"",
            self.owner,
            self.login.replace('"', "\"\"")
        ))
        .execute(&self.cluster)
        .await
        .expect("revoke owner membership");
        for role in [&self.installer, &self.runtime, &self.definer, &self.owner] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.cluster)
                .await
                .expect("drop fixture role");
        }
        self.cluster.close().await;
        self.cleanup_guard.armed = false;
    }
}

async fn wait_for_locks(admin: &PgPool, app: &str, mode: &str, relation_wait: bool, schema: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 JOIN pg_catalog.pg_locks l ON l.pid = a.pid \
                 WHERE a.application_name = $1 AND a.xact_start IS NOT NULL \
                   AND l.locktype = 'advisory' AND l.objsubid = 1 AND l.granted \
                   AND l.classid::bigint = (($2::bigint >> 32) & 4294967295::bigint) \
                   AND l.objid::bigint = ($2::bigint & 4294967295::bigint) \
                   AND l.mode = $3 AND ($4 = false OR EXISTS ( \
                     SELECT 1 FROM pg_catalog.pg_locks waiting \
                     JOIN pg_catalog.pg_class relation ON relation.oid = waiting.relation \
                     JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                     WHERE waiting.pid = a.pid AND waiting.locktype = 'relation' \
                       AND NOT waiting.granted AND namespace.nspname = $5 \
                       AND relation.relname = 'push_registrations')))",
            )
            .bind(app)
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .bind(mode)
            .bind(relation_wait)
            .bind(schema)
            .fetch_one(admin)
            .await
            .expect("observe operation backend and lock");
            if found {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("operation reached catalog fence");
}

async fn wait_for_retries(admin: &PgPool, app: &str) {
    let mut rollbacks = std::collections::HashSet::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let observed: Option<(i32, String)> = sqlx::query_as(
                "SELECT pid, query_start::pg_catalog.text FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND state = 'idle' AND xact_start IS NULL \
                   AND pg_catalog.starts_with(query, 'ROLLBACK') LIMIT 1",
            )
            .bind(app)
            .fetch_optional(admin)
            .await
            .expect("observe catalog retries");
            if let Some(rollback) = observed {
                rollbacks.insert(rollback);
                if rollbacks.len() >= 2 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("contender rolled back and retried");
}

#[tokio::test]
async fn pg17_fixture_cleanup_runs_when_the_test_panics() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url.clone()).await;
    let database = f.database.clone();
    let roles = vec![
        f.owner.clone(),
        f.definer.clone(),
        f.installer.clone(),
        f.runtime.clone(),
    ];
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _fixture = f;
        panic!("exercise registration fixture cleanup");
    }));
    assert!(panic.is_err());
    let cluster = PgPool::connect(&url).await.expect("reconnect cluster");
    let database_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = $1)",
    )
    .bind(database)
    .fetch_one(&cluster)
    .await
    .expect("inspect fixture database cleanup");
    assert!(!database_exists);
    for role in roles {
        let role_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = $1)",
        )
        .bind(&role)
        .fetch_one(&cluster)
        .await
        .expect("inspect fixture role cleanup");
        assert!(!role_exists, "fixture role {role} leaked");
    }
    cluster.close().await;
}

#[tokio::test]
async fn pg17_public_reader_and_owner_fence_in_both_orders() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url).await;
    let owner_app = format!("reg_owner_{}", uuid::Uuid::new_v4().simple());
    let reader_app = format!("reg_reader_{}", uuid::Uuid::new_v4().simple());
    let owner = f.database(&owner_app).await;
    let lineage = lineage::enroll(
        &owner,
        &LineageConfig {
            deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
            action: None,
        },
    )
    .await
    .expect("enroll fixture lineage");
    DatabasePushRegistrationStore::initialize_owner(owner.clone())
        .await
        .expect("prepare table");
    let config = PhysicalAttestationConfig::new(
        lineage,
        PgRoleName::new(f.installer.clone()).expect("installer"),
        PgRoleName::new(f.runtime.clone()).expect("runtime"),
        PgRoleName::new(f.definer.clone()).expect("definer"),
        PgSchemaName(f.trusted.clone()),
    )
    .expect("attestor config");
    for statement in [
        "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
        format!("GRANT SELECT ON {}._lineage TO {}", f.schema, f.definer),
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.push_registrations TO {}",
            f.schema, f.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&f.admin)
            .await
            .expect("prepare attestor grants");
    }
    let boundary_url = with_options(
        &f.base,
        &f.database,
        &format!("-c search_path={}", f.schema),
        "reg_install",
    );
    let options = PgConnectOptions::from_str(&boundary_url).expect("attestor options");
    install(
        &options.clone().username(&f.installer),
        config.clone(),
        uuid::Uuid::new_v4(),
    )
    .await
    .expect("install physical attestor");
    let pool = attested_pool(
        options.username(&f.runtime).application_name(&reader_app),
        config,
        1,
        Duration::from_secs(3),
    )
    .expect("one-slot reader pool");
    let runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "registration reader",
        pool,
    );
    DatabasePushRegistrationStore::attach_existing(runtime.clone())
        .await
        .expect("warm public reader");

    // Removing the index makes the owner perform real DDL after taking catalog-X.
    sqlx::query(&format!(
        "INSERT INTO {}.push_registrations \
        (owner_bare_jid, push_service_jid, status, created_at_ms, updated_at_ms) \
        VALUES ('user@example.test', 'push@example.test', 'enabled', 1, 1)",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("seed preserved registration");
    sqlx::query(&format!(
        "DROP INDEX {}.idx_push_registrations_owner_status",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("remove index for repair");
    let mut block = f.admin.begin().await.expect("relation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.push_registrations IN ACCESS EXCLUSIVE MODE",
        f.schema
    ))
    .execute(&mut *block)
    .await
    .expect("block relation");
    let task = tokio::spawn(DatabasePushRegistrationStore::try_new(owner.clone()));
    wait_for_locks(&f.admin, &owner_app, "ExclusiveLock", true, &f.schema).await;
    let reader = tokio::spawn(DatabasePushRegistrationStore::attach_existing(
        runtime.clone(),
    ));
    wait_for_retries(&f.admin, &reader_app).await;
    assert!(!task.is_finished() && !reader.is_finished());
    block.rollback().await.expect("release relation");
    task.await
        .expect("owner task")
        .expect("owner repairs index");
    reader
        .await
        .expect("reader task")
        .expect("reader sees completed repair");
    let rows: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.push_registrations",
        f.schema
    ))
    .fetch_one(&f.admin)
    .await
    .expect("check preserved registration");
    assert_eq!(rows, 1, "partial index repair must preserve existing data");

    // Reader first: the actual public reader holds catalog-S while blocked on
    // the relation; the public owner must retry without holding catalog-X.
    let mut block = f.admin.begin().await.expect("relation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.push_registrations IN ACCESS EXCLUSIVE MODE",
        f.schema
    ))
    .execute(&mut *block)
    .await
    .expect("block reader relation");
    let reader = tokio::spawn(DatabasePushRegistrationStore::attach_existing(
        runtime.clone(),
    ));
    wait_for_locks(&f.admin, &reader_app, "ShareLock", true, &f.schema).await;
    let task = tokio::spawn(DatabasePushRegistrationStore::initialize_owner(
        owner.clone(),
    ));
    wait_for_retries(&f.admin, &owner_app).await;
    assert!(!task.is_finished() && !reader.is_finished());
    block.rollback().await.expect("release reader relation");
    reader
        .await
        .expect("reader task")
        .expect("reader validates");
    task.await
        .expect("owner task")
        .expect("owner progresses on one-slot pool");

    // Cancel while index DDL waits on a relation lock. Dropping the local
    // future discards the lease; it does not itself prove backend exit.
    sqlx::query(&format!(
        "DROP INDEX {}.idx_push_registrations_owner_status",
        f.schema
    ))
    .execute(&f.admin)
    .await
    .expect("remove index before cancellation");
    let mut block = f.admin.begin().await.expect("cancellation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.push_registrations IN ACCESS EXCLUSIVE MODE",
        f.schema
    ))
    .execute(&mut *block)
    .await
    .expect("block owner DDL");
    let task = tokio::spawn(DatabasePushRegistrationStore::try_new(owner.clone()));
    wait_for_locks(&f.admin, &owner_app, "ExclusiveLock", true, &f.schema).await;
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity \
        WHERE application_name = $1 AND xact_start IS NOT NULL",
    )
    .bind(&owner_app)
    .fetch_one(&f.admin)
    .await
    .expect("capture owner backend");
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    block
        .rollback()
        .await
        .expect("release blocked server command");
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let alive: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 \
                FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&f.admin)
            .await
            .expect("observe original backend");
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("discarded backend exits");
    let index: Option<i64> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
        .bind(format!("{}.idx_push_registrations_owner_status", f.schema))
        .fetch_one(&f.admin)
        .await
        .expect("inspect canceled index");
    assert!(
        index.is_none(),
        "canceled transaction must not commit its index"
    );
    DatabasePushRegistrationStore::initialize_owner(owner.clone())
        .await
        .expect("repair after confirmed backend exit");
    DatabasePushRegistrationStore::attach_existing(runtime.clone())
        .await
        .expect("reader after cancellation");

    // Use a privileged, separate one-slot session only to model a legacy
    // search path with both another schema and a temporary table shadow.
    let shadow_schema = format!("{}_shadow", f.schema);
    sqlx::query(&format!("CREATE SCHEMA {shadow_schema}"))
        .execute(&f.admin)
        .await
        .expect("create shadow schema");
    sqlx::query(&format!(
        "CREATE TABLE {shadow_schema}.push_registrations (rogue TEXT)"
    ))
    .execute(&f.admin)
    .await
    .expect("create shadow table");
    let url = with_options(
        &f.base,
        &f.database,
        &format!("-c search_path={},{}", f.schema, shadow_schema),
        "reg_shadow_owner",
    );
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url);
    config.pool_size = 1;
    let shadow_db = Database::from_config("registration shadow witness", &config)
        .await
        .expect("shadow pool");
    let guard = shadow_db.guard().await.expect("shadow checkout");
    guard
        .execute("CREATE TEMP TABLE push_registrations (rogue TEXT)", ())
        .await
        .expect("create temporary shadow");
    let error = match DatabasePushRegistrationStore::initialize_owner(shadow_db.clone()).await {
        Ok(_) => panic!("temporary shadow must be refused"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("temporary table shadow"),
        "unexpected shadow refusal: {error}"
    );
    guard
        .execute("DROP TABLE pg_temp.push_registrations", ())
        .await
        .expect("remove temporary shadow");
    DatabasePushRegistrationStore::try_new(shadow_db.clone())
        .await
        .expect("initialize against the first persistent schema");
    let columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_attribute a \
        JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
        WHERE n.nspname = $1 AND c.relname = 'push_registrations' \
          AND a.attnum > 0 AND NOT a.attisdropped",
    )
    .bind(&shadow_schema)
    .fetch_one(&f.admin)
    .await
    .expect("inspect other-schema shadow");
    assert_eq!(columns, 1, "initializer did not touch the later schema");
    drop(guard);
    drop(shadow_db);

    drop(runtime);
    drop(owner);
    f.cleanup().await;
}

#[tokio::test]
async fn sqlite_registration_still_survives_reinitialization() {
    let db = Database::in_memory("registration catalog SQLite")
        .await
        .expect("SQLite database");
    let store = DatabasePushRegistrationStore::try_new(db.clone())
        .await
        .expect("initialize SQLite store");
    store
        .register(PushSubscription {
            user_jid: "user@example.test".to_owned(),
            service_jid: "push@example.test".to_owned(),
            node: Some("device".to_owned()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("register SQLite subscription");
    let reopened = DatabasePushRegistrationStore::initialize_owner(db)
        .await
        .expect("reinitialize SQLite store");
    let registrations = reopened
        .get_for_user("user@example.test")
        .await
        .expect("read SQLite subscriptions");
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].node.as_deref(), Some("device"));
}

const FRAME_LIMIT: u32 = 8 * 1024 * 1024;

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let tag = reader.read_u8().await?;
    let length = reader.read_u32().await?;
    if !(4..=FRAME_LIMIT).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid PostgreSQL frame",
        ));
    }
    let mut body = vec![0; (length - 4) as usize];
    reader.read_exact(&mut body).await?;
    Ok((tag, body))
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    tag: u8,
    body: &[u8],
) -> io::Result<()> {
    writer.write_u8(tag).await?;
    writer.write_u32(body.len() as u32 + 4).await?;
    writer.write_all(body).await
}

async fn hold_until_release(mut release: watch::Receiver<bool>) {
    if !*release.borrow() {
        let _ = release.changed().await;
    }
}

struct CommitSignals {
    before: Notify,
    send: Notify,
    held: Notify,
    release: watch::Sender<bool>,
}

async fn commit_proxy(
    listener: TcpListener,
    target: PgConnectOptions,
    signals: Arc<CommitSignals>,
) -> io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forwarded = Arc::new(AtomicBool::new(false));
    let upstream_forwarded = Arc::clone(&forwarded);
    let upstream_signals = Arc::clone(&signals);
    let upstream = async move {
        // sslmode=disable makes the first packet an untagged StartupMessage.
        let length = client_read.read_u32().await?;
        if !(4..=FRAME_LIMIT).contains(&length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid startup frame",
            ));
        }
        let mut startup = vec![0; (length - 4) as usize];
        client_read.read_exact(&mut startup).await?;
        server_write.write_u32(length).await?;
        server_write.write_all(&startup).await?;
        let mut catalog_seen = false;
        loop {
            let frame = read_frame(&mut client_read).await;
            if frame.is_err() {
                let keepalive = server_write;
                hold_until_release(upstream_signals.release.subscribe()).await;
                drop(keepalive);
                return Ok(());
            }
            let (tag, body) = frame?;
            if body
                .windows(b"pg_try_advisory_xact_lock(".len())
                .any(|window| window == b"pg_try_advisory_xact_lock(")
            {
                catalog_seen = true;
            }
            if catalog_seen && tag == b'Q' && body == b"COMMIT\0" {
                upstream_signals.before.notify_one();
                upstream_signals.send.notified().await;
                upstream_forwarded.store(true, Ordering::SeqCst);
                write_frame(&mut server_write, tag, &body).await?;
                let keepalive = server_write;
                hold_until_release(upstream_signals.release.subscribe()).await;
                drop(keepalive);
                return Ok(());
            }
            write_frame(&mut server_write, tag, &body).await?;
        }
    };
    let downstream = async move {
        loop {
            let (tag, body) = read_frame(&mut server_read).await?;
            if forwarded.load(Ordering::SeqCst) && tag == b'C' && body == b"COMMIT\0" {
                signals.held.notify_one();
                let keepalive = (client_write, server_read);
                hold_until_release(signals.release.subscribe()).await;
                drop(keepalive);
                return Ok(());
            }
            write_frame(&mut client_write, tag, &body).await?;
        }
    };
    let (upstream, downstream) = tokio::join!(upstream, downstream);
    upstream?;
    downstream
}

#[tokio::test]
async fn pg17_lost_owner_commit_reply_is_typed_and_does_not_return_a_store() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let base_target = PgConnectOptions::from_str(&url).expect("proxy target options");
    postgres_proxy::ensure_local_target(&base_target).expect("proxy target must be local");
    let f = Fixture::new(url).await;
    let app = format!("reg_commit_{}", uuid::Uuid::new_v4().simple());
    let direct_url = f.owner_url(&app);
    let target = PgConnectOptions::from_str(&direct_url).expect("target options");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let proxy_url = postgres_proxy::local_proxy_url(&direct_url, port).expect("proxy URL");
    let (release, _) = watch::channel(false);
    let signals = Arc::new(CommitSignals {
        before: Notify::new(),
        send: Notify::new(),
        held: Notify::new(),
        release,
    });
    let proxy = tokio::spawn(commit_proxy(listener, target, Arc::clone(&signals)));
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, proxy_url.to_string());
    config.pool_size = 1;
    let db = Database::from_config("registration unknown commit", &config)
        .await
        .expect("proxy owner pool");
    let task = tokio::spawn(DatabasePushRegistrationStore::initialize_owner(db.clone()));
    tokio::time::timeout(Duration::from_secs(20), signals.before.notified())
        .await
        .expect("owner COMMIT request");
    wait_for_locks(&f.admin, &app, "ExclusiveLock", false, &f.schema).await;
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1",
    )
    .bind(&app)
    .fetch_one(&f.admin)
    .await
    .expect("original backend");
    let absent: Option<i64> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
        .bind(format!("{}.push_registrations", f.schema))
        .fetch_one(&f.admin)
        .await
        .expect("uncommitted table");
    assert!(absent.is_none());
    signals.send.notify_one();
    tokio::time::timeout(Duration::from_secs(5), signals.held.notified())
        .await
        .expect("hold COMMIT reply");
    for name in ["push_registrations", "idx_push_registrations_owner_status"] {
        let oid: Option<i64> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
            .bind(format!("{}.{name}", f.schema))
            .fetch_one(&f.admin)
            .await
            .expect("inspect committed relation");
        assert!(oid.is_some(), "{name} committed while response is withheld");
    }
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("bounded COMMIT wait")
        .expect("owner task");
    assert!(matches!(
        result,
        Err(PushRegistrationOwnerError::CommitIndeterminate)
    ));
    let alive: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
    )
    .bind(pid)
    .fetch_one(&f.admin)
    .await
    .expect("original backend state");
    assert!(
        alive,
        "dropping the client lease does not prove backend exit"
    );
    signals.release.send(true).expect("release proxy sockets");
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy deadline")
        .expect("join proxy")
        .expect("proxy I/O");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let alive: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&f.admin)
            .await
            .expect("original backend state");
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("original backend exit");
    drop(db);
    let retry = f.database("reg_commit_reconciled_retry").await;
    DatabasePushRegistrationStore::try_new(retry.clone())
        .await
        .expect("retry after observed committed state and backend exit");
    drop(retry);
    f.cleanup().await;
}
