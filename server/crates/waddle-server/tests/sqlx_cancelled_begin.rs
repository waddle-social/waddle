//! Guard the local SQLx PostgreSQL patch against returning a cancelled BEGIN
//! to the pool with a server-side transaction still open.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::Notify;

/// Forward one PostgreSQL connection. Once the client sends an ordinary
/// `BEGIN`, hold the server reply until the test cancels that exact begin.
/// The backend has entered its transaction before cancellation, verified by
/// an independent `pg_stat_activity` query.
enum BackendTarget {
    Tcp(String, u16),
    Unix(PathBuf),
}

trait ProxyStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProxyStream for T {}

async fn begin_reply_proxy(
    listener: TcpListener,
    target: BackendTarget,
    begin_sent: Arc<Notify>,
    reply_held: Arc<Notify>,
    release_reply: Arc<Notify>,
) -> std::io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server: Box<dyn ProxyStream> = match target {
        BackendTarget::Tcp(host, port) => {
            Box::new(TcpStream::connect((host.as_str(), port)).await?)
        }
        BackendTarget::Unix(path) => Box::new(UnixStream::connect(path).await?),
    };
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let hold = Arc::new(AtomicBool::new(false));
    let hold_on_begin = hold.clone();

    let upstream = async move {
        let mut buffer = [0_u8; 8192];
        let mut tail = Vec::new();
        loop {
            let count = client_read.read(&mut buffer).await?;
            if count == 0 {
                return Ok::<(), std::io::Error>(());
            }
            let mut scan = tail;
            scan.extend_from_slice(&buffer[..count]);
            if !hold_on_begin.load(Ordering::SeqCst)
                && scan
                    .windows(b"BEGIN\0".len())
                    .any(|part| part == b"BEGIN\0")
            {
                // Set this before forwarding the final query bytes, so the
                // other direction cannot leak an early reply to the client.
                hold_on_begin.store(true, Ordering::SeqCst);
                server_write.write_all(&buffer[..count]).await?;
                begin_sent.notify_one();
            } else {
                server_write.write_all(&buffer[..count]).await?;
            }
            tail = scan[scan.len().saturating_sub(b"BEGIN\0".len() - 1)..].to_vec();
        }
    };
    let downstream = async move {
        let mut buffer = [0_u8; 8192];
        loop {
            let count = server_read.read(&mut buffer).await?;
            if count == 0 {
                return Ok::<(), std::io::Error>(());
            }
            if hold.load(Ordering::SeqCst) {
                reply_held.notify_one();
                release_reply.notified().await;
                hold.store(false, Ordering::SeqCst);
            }
            client_write.write_all(&buffer[..count]).await?;
        }
    };
    tokio::select! {
        result = upstream => result,
        result = downstream => result,
    }
}

#[tokio::test]
async fn cancelled_ordinary_begin_never_reuses_an_open_postgres_transaction() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let target_url = url::Url::parse(&url).expect("PostgreSQL test URL");
    let query_host = target_url
        .query_pairs()
        .find(|(key, _)| key == "host")
        .map(|(_, value)| value.into_owned());
    let host = query_host
        .or_else(|| target_url.host_str().map(str::to_owned))
        .expect("PostgreSQL test host");
    let port = target_url
        .query_pairs()
        .find(|(key, _)| key == "port")
        .map(|(_, value)| value.parse::<u16>().expect("PostgreSQL test port"))
        .or_else(|| target_url.port())
        .unwrap_or(5432);
    let backend = if host.starts_with('/') {
        BackendTarget::Unix(PathBuf::from(host).join(format!(".s.PGSQL.{port}")))
    } else {
        BackendTarget::Tcp(host, port)
    };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local PostgreSQL proxy");
    let proxy_port = listener.local_addr().expect("proxy address").port();
    let begin_sent = Arc::new(Notify::new());
    let reply_held = Arc::new(Notify::new());
    let release_reply = Arc::new(Notify::new());
    let proxy = tokio::spawn(begin_reply_proxy(
        listener,
        backend,
        begin_sent.clone(),
        reply_held.clone(),
        release_reply.clone(),
    ));

    // The CI database listens on a Unix socket. Rewrite its URL rather than
    // calling PgConnectOptions::host(): that setter does not clear a socket
    // selected by the original URL's `host=/path` parameter. The proxy's
    // client leg uses plaintext TCP so it can see the ordinary BEGIN frame.
    let mut proxy_url = target_url.clone();
    proxy_url
        .set_host(Some("127.0.0.1"))
        .expect("set proxy host");
    proxy_url
        .set_port(Some(proxy_port))
        .expect("set proxy port");
    let parameters: Vec<_> = target_url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "host" | "hostaddr" | "port" | "sslmode"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    proxy_url.set_query(None);
    {
        let mut query = proxy_url.query_pairs_mut();
        for (key, value) in parameters {
            query.append_pair(&key, &value);
        }
        query.append_pair("sslmode", "disable");
    }
    let options = PgConnectOptions::from_str(proxy_url.as_str())
        .expect("parse proxied PostgreSQL test options");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .connect_with(options)
        .await
        .expect("connect through proxy");
    let observer = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("independent observer");
    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .expect("proxied backend pid");

    let begin = tokio::spawn({
        let pool = pool.clone();
        async move { pool.begin().await }
    });
    tokio::time::timeout(Duration::from_secs(5), begin_sent.notified())
        .await
        .expect("ordinary BEGIN sent upstream");
    tokio::time::timeout(Duration::from_secs(5), reply_held.notified())
        .await
        .expect("hold ordinary BEGIN reply");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state: String =
                sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
                    .bind(backend_pid)
                    .fetch_one(&observer)
                    .await
                    .expect("observe backend state");
            if state == "idle in transaction" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("PostgreSQL must enter the transaction before cancellation");

    begin.abort();
    assert!(begin.await.expect_err("cancel BEGIN task").is_cancelled());
    release_reply.notify_one();
    let mut reused = tokio::time::timeout(Duration::from_secs(5), pool.acquire())
        .await
        .expect("pool returns a connection")
        .expect("acquire after cancelled BEGIN");
    let reused_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *reused)
        .await
        .expect("query after cancelled BEGIN");
    let state: String = sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
        .bind(reused_pid)
        .fetch_one(&observer)
        .await
        .expect("reused backend state");
    drop(reused);
    pool.close().await;
    proxy.abort();
    assert_eq!(
        state, "idle",
        "backend {reused_pid} was reused in an unexpected state"
    );
}

#[tokio::test]
async fn postgres_begin_patch_preserves_nested_and_rejected_begin() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("disposable PostgreSQL pool");
    let mut outer = pool.begin().await.expect("outer BEGIN");
    let nested = sqlx::Acquire::begin(&mut *outer)
        .await
        .expect("nested savepoint");
    nested.rollback().await.expect("rollback nested savepoint");
    outer.commit().await.expect("commit outer transaction");

    assert!(pool.begin_with("SELECT 1").await.is_err());
    let fresh = pool.begin().await.expect("BEGIN after rejected statement");
    fresh.rollback().await.expect("rollback fresh transaction");
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("pool remains usable");
    assert_eq!(one, 1);
}
