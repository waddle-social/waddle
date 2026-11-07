//! PostgreSQL 17 witness for an Inbox owner whose COMMIT reply is withheld.
//!
//! The plaintext wire proxy connects only to a local, disposable PostgreSQL
//! cluster. The owner initializer must report an indeterminate commit, while a
//! direct connection to that same primary reconciles the committed catalog
//! before a later owner retry. This covers a server-processed COMMIT whose
//! CommandComplete reply is withheld; it does not witness a COMMIT that the
//! server never processes.

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
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::inbox::DatabaseInboxStorage;
use waddle_xmpp::inbox::storage::InboxStorageError;

const FRAME_LIMIT: u32 = 8 * 1024 * 1024;
const OWNER_TABLES: [&str; 2] = ["inbox_entries", "groupchat_notification_recovery"];
const OWNER_INDEXES: [&str; 4] = [
    "idx_inbox_entries_user_updated",
    "idx_inbox_entries_user_room_threads",
    "idx_groupchat_notification_recovery_pending",
    "idx_groupchat_notification_recovery_completed_prune",
];
const RECOVERY_MESSAGE_KEY_GATE: &str =
    "pg_try_advisory_xact_lock(pg_catalog.hashtext($1)::pg_catalog.int8)";

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
        let in_recovery: bool = sqlx::query_scalar("SELECT pg_catalog.pg_is_in_recovery()")
            .fetch_one(&cluster)
            .await
            .expect("check PostgreSQL primary");
        assert!(!in_recovery, "the PostgreSQL test target must be a primary");

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database = format!("inbox_unknown_{suffix}");
        let owner = format!("inbox_o_{suffix}");
        let schema = format!("inbox_s_{suffix}");
        let login: String = sqlx::query_scalar("SELECT session_user::text")
            .fetch_one(&cluster)
            .await
            .expect("read login role");
        for statement in [
            format!("CREATE DATABASE {database}"),
            format!("REVOKE CREATE, TEMPORARY ON DATABASE {database} FROM PUBLIC"),
            format!(
                "CREATE ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"
            ),
            format!("GRANT {owner} TO \"{}\"", login.replace('"', "\"\"")),
        ] {
            sqlx::query(&statement)
                .execute(&cluster)
                .await
                .expect("prepare isolated database and owner role");
        }
        let admin_url = scoped_url(url, &database, "", "inbox_unknown_admin");
        let admin = PgPool::connect(&admin_url)
            .await
            .expect("connect isolated database");
        for statement in [
            "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("prepare isolated owner schema");
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

    fn owner_url(&self, application: &str) -> String {
        scoped_url(
            &self.base_url,
            &self.database,
            &format!("-c role={} -c search_path={}", self.owner, self.schema),
            application,
        )
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.cluster)
            .await
            .expect("drop isolated database");
        sqlx::query(&format!(
            "REVOKE {} FROM \"{}\"",
            self.owner,
            self.login.replace('"', "\"\"")
        ))
        .execute(&self.cluster)
        .await
        .expect("revoke owner-role membership");
        sqlx::query(&format!("DROP ROLE {}", self.owner))
            .execute(&self.cluster)
            .await
            .expect("drop owner role");
        self.cluster.close().await;
    }
}

async fn owner_database(url: String) -> Database {
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url);
    config.pool_size = 1;
    Database::from_config("inbox owner unknown commit witness", &config)
        .await
        .expect("open one-slot owner handle")
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
        .expect("identify original owner backend")
}

async fn backend_present(admin: &PgPool, pid: i32) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)")
        .bind(pid)
        .fetch_one(admin)
        .await
        .expect("inspect original owner backend")
}

async fn backend_locks(admin: &PgPool, pid: i32) -> (bool, bool) {
    sqlx::query_as(
        "SELECT \
         EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = $1 \
           AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
           AND l.classid::bigint = ($2::bigint >> 32) \
           AND l.objid::bigint = ($2::bigint & 4294967295) AND l.mode = 'ExclusiveLock'), \
         EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = $1 \
           AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
           AND l.classid::bigint = ((pg_catalog.hashtext($3)::bigint >> 32) & 4294967295) \
           AND l.objid::bigint = (pg_catalog.hashtext($3)::bigint & 4294967295) \
           AND l.mode = 'ExclusiveLock')",
    )
    .bind(pid)
    .bind(waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY)
    .bind("groupchat_notification_recovery.message_key")
    .fetch_one(admin)
    .await
    .expect("inspect original owner advisory locks")
}

async fn catalog_available(admin: &PgPool) -> bool {
    sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(admin)
        .await
        .expect("probe catalog fence")
}

async fn relation_oid(admin: &PgPool, schema: &str, name: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::oid::bigint")
        .bind(format!("{schema}.{name}"))
        .fetch_one(admin)
        .await
        .expect("inspect committed owner relation")
}

async fn assert_owner_catalog(admin: &PgPool, schema: &str) -> Vec<(String, i64)> {
    let in_recovery: bool = sqlx::query_scalar("SELECT pg_catalog.pg_is_in_recovery()")
        .fetch_one(admin)
        .await
        .expect("check PostgreSQL primary during reconciliation");
    assert!(!in_recovery, "catalog reconciliation must use the primary");

    let mut relations = Vec::new();
    for name in OWNER_TABLES.into_iter().chain(OWNER_INDEXES) {
        let oid = relation_oid(admin, schema, name)
            .await
            .unwrap_or_else(|| panic!("committed owner relation {schema}.{name} is visible"));
        relations.push((name.to_owned(), oid));
    }
    relations
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
    signals: Arc<ProxySignals>,
) -> io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let owner_commit_forwarded = Arc::new(AtomicBool::new(false));
    let upstream_signals = Arc::clone(&signals);
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
        let mut recovery_gate_seen = false;
        loop {
            let frame = read_frame(&mut client_read).await;
            if upstream_commit.load(Ordering::SeqCst) || frame.is_err() {
                // Keep the original backend socket alive until the test has
                // observed the indeterminate result and reconciled its commit.
                let keepalive = server_write;
                retain_until_release(upstream_signals.release.subscribe()).await;
                drop(keepalive);
                return Ok(());
            }
            let (tag, body) = frame?;
            if body
                .windows(RECOVERY_MESSAGE_KEY_GATE.len())
                .any(|part| part == RECOVERY_MESSAGE_KEY_GATE.as_bytes())
            {
                recovery_gate_seen = true;
            }
            // The hashtext gate belongs to the Inbox owner transaction. Wait
            // for its COMMIT, then forward COMMIT while withholding its reply.
            if recovery_gate_seen && tag == b'Q' && body == b"COMMIT\0" {
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
    let downstream_signals = Arc::clone(&signals);
    let downstream_commit = Arc::clone(&owner_commit_forwarded);
    let downstream = async move {
        loop {
            let (tag, body) = read_frame(&mut server_read).await?;
            if downstream_commit.load(Ordering::SeqCst) && tag == b'C' && body == b"COMMIT\0" {
                downstream_signals.held_reply.notify_one();
                let keepalive = (client_write, server_read);
                retain_until_release(downstream_signals.release.subscribe()).await;
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
    let task = tokio::spawn(proxy(listener, target, Arc::clone(&signals)));
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
async fn pg17_inbox_owner_lost_commit_reply_reconciles_before_retry() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    require_local_target(&url).await;
    let fixture = Fixture::new(&url).await;
    let application = format!("inbox_commit_{}", uuid::Uuid::new_v4().simple());
    let direct_url = fixture.owner_url(&application);
    let (proxy_url, signals, proxy_task) = start_proxy(&direct_url).await;
    let db = owner_database(proxy_url).await;
    let owner = tokio::spawn(DatabaseInboxStorage::from_database(db.clone()));

    tokio::time::timeout(Duration::from_secs(20), signals.before_commit.notified())
        .await
        .expect("intercept Inbox owner COMMIT request");
    let pid = backend_pid(&fixture.admin, &application).await;
    assert_eq!(backend_locks(&fixture.admin, pid).await, (true, true));
    assert!(
        !catalog_available(&fixture.admin).await,
        "Inbox owner holds the catalog fence until COMMIT"
    );
    for name in OWNER_TABLES {
        assert!(
            relation_oid(&fixture.admin, &fixture.schema, name)
                .await
                .is_none(),
            "{name} must not be visible before COMMIT"
        );
    }

    signals.send_commit.notify_one();
    tokio::time::timeout(Duration::from_secs(5), signals.held_reply.notified())
        .await
        .expect("COMMIT was processed and its reply withheld");

    // These catalog reads use the configured primary directly while the proxy
    // still retains the original backend whose COMMIT just completed.
    let committed_relations = assert_owner_catalog(&fixture.admin, &fixture.schema).await;
    wait_for("transaction locks released by COMMIT", || async {
        backend_locks(&fixture.admin, pid).await == (false, false)
    })
    .await;
    assert!(
        backend_present(&fixture.admin, pid).await,
        "the proxy still retains the original PostgreSQL backend"
    );
    assert!(catalog_available(&fixture.admin).await);

    let result = tokio::time::timeout(Duration::from_secs(8), owner)
        .await
        .expect("bounded owner commit timeout")
        .expect("join Inbox owner initialization");
    match result {
        Err(InboxStorageError::CommitIndeterminate) => {}
        Ok(storage) => {
            drop(storage);
            panic!("lost COMMIT reply must not return a successful storage handle");
        }
        Err(error) => panic!("expected typed CommitIndeterminate, got {error:?}"),
    }
    assert!(
        backend_present(&fixture.admin, pid).await,
        "client completion does not prove original backend exit"
    );

    // Resolve the uncertain result on the same writable primary before issuing
    // another owner attempt. Removing a committed index makes the retry perform
    // real catalog DDL instead of only accepting the existing tables.
    assert_eq!(
        assert_owner_catalog(&fixture.admin, &fixture.schema).await,
        committed_relations,
        "same-primary reconciliation observes the original committed objects"
    );
    release_proxy(&signals, proxy_task).await;
    wait_for("original owner backend exit", || async {
        !backend_present(&fixture.admin, pid).await
    })
    .await;
    drop(db);

    let missing_index = OWNER_INDEXES[0];
    sqlx::query(&format!("DROP INDEX {}.{}", fixture.schema, missing_index))
        .execute(&fixture.admin)
        .await
        .expect("remove index before retry");
    let retry_application = format!("inbox_retry_{}", uuid::Uuid::new_v4().simple());
    let retry_db = owner_database(fixture.owner_url(&retry_application)).await;
    let storage = tokio::time::timeout(
        Duration::from_secs(10),
        DatabaseInboxStorage::from_database(retry_db.clone()),
    )
    .await
    .expect("same-primary retry deadline")
    .expect("same-primary retry succeeds");
    let retried_relations = assert_owner_catalog(&fixture.admin, &fixture.schema).await;
    assert!(
        retried_relations
            .iter()
            .any(|(name, _)| name == missing_index),
        "retry recreated the missing owner index"
    );
    drop(storage);
    drop(retry_db);
    fixture.cleanup().await;
}
