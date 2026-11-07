//! PostgreSQL 17 witnesses for an Activity owner whose reply is lost.
//!
//! The plaintext wire proxy connects only to a local, disposable PostgreSQL
//! cluster. Each fixture has its own database because advisory locks are
//! database-scoped. These tests use the public owner initializer.

#[path = "../src/test_support/postgres_proxy.rs"]
pub mod postgres_proxy;

use std::future::Future;
use std::io;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::PgPool;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, Notify};
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::notification_activity::{NotificationActivityError, NotificationActivityStore};

const ACTIVITY_GATE: (i32, i32) = (0x5744444c, 0x4e414354);
const FRAME_LIMIT: u32 = 8 * 1024 * 1024;

async fn require_local_target(url: &str) {
    let options = PgConnectOptions::from_str(url).expect("PostgreSQL connection options");
    postgres_proxy::ensure_local_target(&options)
        .expect("this plaintext proxy witness requires a local PostgreSQL target");
    if options.get_socket().is_none() && !Path::new(options.get_host()).is_absolute() {
        let addresses: Vec<_> = tokio::net::lookup_host((options.get_host(), options.get_port()))
            .await
            .expect("resolve local PostgreSQL target")
            .collect();
        postgres_proxy::ensure_loopback_addresses(&addresses)
            .expect("PostgreSQL target must resolve only to loopback addresses");
    }
}

fn scoped_url(base: &str, database: &str, options: &str, application: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "dbname" | "options" | "application_name"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_path(&format!("/{database}"));
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("options", options)
        .append_pair("application_name", application);
    url.to_string()
}

struct Fixture {
    cluster: PgPool,
    admin: PgPool,
    database: String,
    owner: String,
    schema: String,
    login: String,
    base_url: String,
}

impl Fixture {
    async fn new(url: &str) -> Self {
        let cluster = PgPool::connect(url)
            .await
            .expect("connect disposable cluster");
        let version: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&cluster)
                .await
                .expect("read PostgreSQL version");
        assert!(
            (170_000..180_000).contains(&version),
            "requires PostgreSQL 17"
        );
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("activity_unknown_{suffix}");
        let owner = format!("activity_o_{suffix}");
        let schema = format!("activity_s_{suffix}");
        let login: String = sqlx::query_scalar("SELECT session_user::text")
            .fetch_one(&cluster)
            .await
            .expect("read login role");
        for statement in [
            format!("CREATE DATABASE {database}"),
            format!("REVOKE CREATE, TEMPORARY ON DATABASE {database} FROM PUBLIC"),
            format!("CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"),
            format!("GRANT {owner} TO \"{}\"", login.replace('"', "\"\"")),
        ] {
            sqlx::query(&statement).execute(&cluster).await.expect("prepare fixture");
        }
        let admin_url = scoped_url(url, &database, "", "activity_unknown_admin");
        let admin = PgPool::connect(&admin_url)
            .await
            .expect("connect fixture database");
        for statement in [
            "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {owner}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("prepare owner schema");
        }
        Self {
            cluster,
            admin,
            database,
            owner,
            schema,
            login,
            base_url: url.to_owned(),
        }
    }

    fn owner_url(&self, application: &str, extra_options: &str) -> String {
        scoped_url(
            &self.base_url,
            &self.database,
            &format!(
                "-c role={} -c search_path={} {extra_options}",
                self.owner, self.schema
            ),
            application,
        )
    }

    async fn cleanup(self) {
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
        .expect("revoke fixture role membership");
        sqlx::query(&format!("DROP ROLE {}", self.owner))
            .execute(&self.cluster)
            .await
            .expect("drop fixture role");
        self.cluster.close().await;
    }
}

async fn owner_database(url: String) -> Database {
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url);
    config.pool_size = 1;
    Database::from_config("activity uncertain commit witness", &config)
        .await
        .expect("open owner handle")
}

async fn wait_for<F, Fut>(message: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {message}"));
}

async fn backend_pid(admin: &PgPool, application: &str) -> i32 {
    sqlx::query_scalar("SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1")
        .bind(application)
        .fetch_one(admin)
        .await
        .expect("identify owner backend")
}

async fn backend_present(admin: &PgPool, pid: i32) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)")
        .bind(pid)
        .fetch_one(admin)
        .await
        .expect("inspect owner backend")
}

async fn backend_locks(admin: &PgPool, pid: i32) -> (bool, bool) {
    sqlx::query_as(
        "SELECT \
         EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = $1 \
           AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
           AND l.classid::bigint = ($2::bigint >> 32) \
           AND l.objid::bigint = ($2::bigint & 4294967295) AND l.mode = 'ExclusiveLock'), \
         EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = $1 \
           AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 2 \
           AND l.classid::bigint = $3 AND l.objid::bigint = $4 AND l.mode = 'ExclusiveLock')",
    )
    .bind(pid)
    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
    .bind(i64::from(ACTIVITY_GATE.0))
    .bind(i64::from(ACTIVITY_GATE.1))
    .fetch_one(admin)
    .await
    .expect("inspect backend advisory locks")
}

async fn catalog_available(admin: &PgPool) -> bool {
    sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(admin)
        .await
        .expect("probe catalog fence")
}

async fn relation_oid(admin: &PgPool, schema: &str, name: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
        .bind(format!("{schema}.{name}"))
        .fetch_one(admin)
        .await
        .expect("inspect committed relation")
}

async fn marker(admin: &PgPool, schema: &str) -> String {
    sqlx::query_scalar(&format!(
        "SELECT prepared_revision FROM {schema}.notification_activity_owner_state"
    ))
    .fetch_one(admin)
    .await
    .expect("read committed owner marker")
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let tag = reader.read_u8().await?;
    let length = reader.read_u32().await?;
    if !(4..=FRAME_LIMIT).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid PostgreSQL frame length",
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

async fn retain_until_release(mut release: watch::Receiver<bool>) {
    if !*release.borrow() {
        let _ = release.changed().await;
    }
}

#[derive(Clone, Copy)]
enum Intercept {
    Commit,
    CatalogReply,
}

struct ProxySignals {
    before_commit: Notify,
    send_commit: Notify,
    held_reply: Notify,
    release: watch::Sender<bool>,
}

impl ProxySignals {
    fn new() -> Arc<Self> {
        let (release, _) = watch::channel(false);
        Arc::new(Self {
            before_commit: Notify::new(),
            send_commit: Notify::new(),
            held_reply: Notify::new(),
            release,
        })
    }
}

async fn proxy(
    listener: TcpListener,
    target: PgConnectOptions,
    intercept: Intercept,
    signals: Arc<ProxySignals>,
) -> io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let catalog_query_seen = Arc::new(AtomicBool::new(false));
    let response_held = Arc::new(AtomicBool::new(false));
    let owner_commit_forwarded = Arc::new(AtomicBool::new(false));
    let upstream_signals = Arc::clone(&signals);
    let upstream_seen = Arc::clone(&catalog_query_seen);
    let upstream_held = Arc::clone(&response_held);
    let upstream_commit = Arc::clone(&owner_commit_forwarded);
    let upstream = async move {
        // sslmode=disable makes the initial untagged packet a StartupMessage.
        let length = client_read.read_u32().await?;
        if !(4..=FRAME_LIMIT).contains(&length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid PostgreSQL startup length",
            ));
        }
        let mut startup = vec![0; (length - 4) as usize];
        client_read.read_exact(&mut startup).await?;
        server_write.write_u32(length).await?;
        server_write.write_all(&startup).await?;
        let mut activity_gate_seen = false;
        loop {
            let frame = read_frame(&mut client_read).await;
            if upstream_held.load(Ordering::SeqCst) || frame.is_err() {
                // Do not let cancellation or a queued rollback close the server socket.
                let keepalive = server_write;
                retain_until_release(upstream_signals.release.subscribe()).await;
                drop(keepalive);
                return Ok(());
            }
            let (tag, body) = frame?;
            if matches!(intercept, Intercept::CatalogReply)
                && body
                    .windows(b"pg_try_advisory_xact_lock(".len())
                    .any(|part| part == b"pg_try_advisory_xact_lock(")
            {
                upstream_seen.store(true, Ordering::SeqCst);
            }
            if body
                .windows(b"pg_try_advisory_xact_lock($1, $2)".len())
                .any(|part| part == b"pg_try_advisory_xact_lock($1, $2)")
            {
                activity_gate_seen = true;
            }
            // Identity discovery commits its own earlier transaction. Only
            // intercept the owner COMMIT after its subordinate gate query.
            if matches!(intercept, Intercept::Commit)
                && activity_gate_seen
                && tag == b'Q'
                && body == b"COMMIT\0"
            {
                upstream_signals.before_commit.notify_one();
                upstream_signals.send_commit.notified().await;
                upstream_commit.store(true, Ordering::SeqCst);
                write_frame(&mut server_write, tag, &body).await?;
                let keepalive = server_write;
                retain_until_release(upstream_signals.release.subscribe()).await;
                drop(keepalive);
                return Ok(());
            }
            write_frame(&mut server_write, tag, &body).await?;
        }
    };
    let downstream = async move {
        loop {
            let (tag, body) = read_frame(&mut server_read).await?;
            let acquired =
                body.len() == 7 && body[..6] == [0, 1, 0, 0, 0, 1] && matches!(body[6], 1 | b't');
            let hold = match intercept {
                Intercept::Commit => {
                    owner_commit_forwarded.load(Ordering::SeqCst)
                        && tag == b'C'
                        && body == b"COMMIT\0"
                }
                Intercept::CatalogReply => {
                    catalog_query_seen.load(Ordering::SeqCst) && tag == b'D' && acquired
                }
            };
            if hold {
                response_held.store(true, Ordering::SeqCst);
                signals.held_reply.notify_one();
                let keepalive = (client_write, server_read);
                retain_until_release(signals.release.subscribe()).await;
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

async fn start_proxy(
    url: &str,
    intercept: Intercept,
) -> (
    String,
    Arc<ProxySignals>,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let target = PgConnectOptions::from_str(url).expect("PostgreSQL connection options");
    postgres_proxy::ensure_local_target(&target)
        .expect("plaintext PostgreSQL witness requires a local target");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind PostgreSQL proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let proxy_url = postgres_proxy::local_proxy_url(url, port).expect("proxy URL");
    let signals = ProxySignals::new();
    let task = tokio::spawn(proxy(listener, target, intercept, Arc::clone(&signals)));
    (proxy_url.to_string(), signals, task)
}

async fn release_proxy(signals: &ProxySignals, task: tokio::task::JoinHandle<io::Result<()>>) {
    signals
        .release
        .send(true)
        .expect("release held proxy sockets");
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("proxy shutdown deadline")
        .expect("join proxy")
        .expect("proxy I/O");
}

#[tokio::test]
async fn pg17_activity_lost_commit_reply_is_indeterminate_and_retry_is_fenced() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    require_local_target(&url).await;
    let fixture = Fixture::new(&url).await;
    let app = format!("activity_commit_{}", uuid::Uuid::new_v4().simple());
    let direct_url = fixture.owner_url(&app, "");
    let (proxy_url, signals, proxy_task) = start_proxy(&direct_url, Intercept::Commit).await;
    let db = owner_database(proxy_url).await;
    let task = tokio::spawn(NotificationActivityStore::initialize_owner(db.clone()));
    tokio::time::timeout(Duration::from_secs(20), signals.before_commit.notified())
        .await
        .expect("intercept COMMIT request");
    let pid = backend_pid(&fixture.admin, &app).await;
    assert_eq!(backend_locks(&fixture.admin, pid).await, (true, true));
    assert!(!catalog_available(&fixture.admin).await);
    assert!(
        relation_oid(&fixture.admin, &fixture.schema, "notification_activity")
            .await
            .is_none(),
        "schema changes are not visible before COMMIT"
    );
    signals.send_commit.notify_one();
    tokio::time::timeout(Duration::from_secs(5), signals.held_reply.notified())
        .await
        .expect("intercept COMMIT reply");
    let activity_oid = relation_oid(&fixture.admin, &fixture.schema, "notification_activity")
        .await
        .expect("activity table committed before held reply");
    assert!(relation_oid(
        &fixture.admin,
        &fixture.schema,
        "idx_notification_activity_conversation_active"
    )
    .await
    .is_some());
    assert_eq!(marker(&fixture.admin, &fixture.schema).await, "1");
    wait_for("transaction locks released by COMMIT", || async {
        backend_locks(&fixture.admin, pid).await == (false, false)
    })
    .await;
    assert!(
        backend_present(&fixture.admin, pid).await,
        "proxy retains original backend socket"
    );
    assert!(catalog_available(&fixture.admin).await);
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .expect("bounded commit timeout")
        .expect("join owner initialization");
    assert!(
        matches!(result, Err(NotificationActivityError::CommitIndeterminate)),
        "lost reply must return the typed indeterminate error and no store"
    );
    assert!(
        backend_present(&fixture.admin, pid).await,
        "client completion does not prove backend exit"
    );
    release_proxy(&signals, proxy_task).await;
    wait_for("original backend exit", || async {
        !backend_present(&fixture.admin, pid).await
    })
    .await;
    drop(db);

    // Force real index DDL on the retry, then hold the relation on the same
    // primary so the retry pauses after taking both fences.
    sqlx::query(&format!(
        "DROP INDEX {}.idx_notification_activity_conversation_active",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove index before retry");
    let mut blocker = fixture.admin.begin().await.expect("begin relation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.notification_activity IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *blocker)
    .await
    .expect("block retry schema work");
    let retry_app = format!("activity_retry_{}", uuid::Uuid::new_v4().simple());
    let retry_db = owner_database(fixture.owner_url(&retry_app, "")).await;
    let retry = tokio::spawn(NotificationActivityStore::initialize_owner(
        retry_db.clone(),
    ));
    wait_for("retry holding both fences while blocked", || async {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1 \
             AND xact_start IS NOT NULL AND wait_event_type = 'Lock'",
        )
        .bind(&retry_app)
        .fetch_optional(&fixture.admin)
        .await
        .expect("inspect retry backend");
        match row {
            Some((retry_pid,)) => {
                retry_pid != pid && backend_locks(&fixture.admin, retry_pid).await == (true, true)
            }
            None => false,
        }
    })
    .await;
    blocker
        .rollback()
        .await
        .expect("release retry relation blocker");
    let store = tokio::time::timeout(Duration::from_secs(10), retry)
        .await
        .expect("retry deadline")
        .expect("join retry")
        .expect("same-primary fenced retry succeeds");
    assert_eq!(
        relation_oid(&fixture.admin, &fixture.schema, "notification_activity").await,
        Some(activity_oid)
    );
    assert!(relation_oid(
        &fixture.admin,
        &fixture.schema,
        "idx_notification_activity_conversation_active"
    )
    .await
    .is_some());
    assert_eq!(marker(&fixture.admin, &fixture.schema).await, "1");
    drop(store);
    drop(retry_db);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pg17_activity_cancelled_before_fence_reply_waits_for_backend_exit() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    require_local_target(&url).await;
    let fixture = Fixture::new(&url).await;
    let app = format!("activity_cancel_{}", uuid::Uuid::new_v4().simple());
    let direct_url = fixture.owner_url(&app, "-c idle_in_transaction_session_timeout=8s");
    let (proxy_url, signals, proxy_task) = start_proxy(&direct_url, Intercept::CatalogReply).await;
    let db = owner_database(proxy_url).await;
    let task = tokio::spawn(NotificationActivityStore::initialize_owner(db.clone()));
    tokio::time::timeout(Duration::from_secs(20), signals.held_reply.notified())
        .await
        .expect("hold acquired catalog reply");
    let pid = backend_pid(&fixture.admin, &app).await;
    assert_eq!(backend_locks(&fixture.admin, pid).await, (true, false));
    assert!(!catalog_available(&fixture.admin).await);
    task.abort();
    match task.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("owner initializer completed before cancellation"),
    }
    assert!(
        backend_present(&fixture.admin, pid).await,
        "cancelled client is not backend exit"
    );
    assert_eq!(backend_locks(&fixture.admin, pid).await, (true, false));
    assert!(
        !catalog_available(&fixture.admin).await,
        "catalog remains held after client cancellation"
    );
    wait_for(
        "server idle transaction timeout and backend exit",
        || async { !backend_present(&fixture.admin, pid).await },
    )
    .await;
    assert!(
        catalog_available(&fixture.admin).await,
        "catalog is released after backend exit"
    );
    assert!(
        relation_oid(&fixture.admin, &fixture.schema, "notification_activity")
            .await
            .is_none()
    );
    assert!(relation_oid(
        &fixture.admin,
        &fixture.schema,
        "notification_activity_owner_state"
    )
    .await
    .is_none());
    release_proxy(&signals, proxy_task).await;
    drop(db);
    fixture.cleanup().await;
}
