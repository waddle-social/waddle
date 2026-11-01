#![cfg(feature = "test-support")]

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use sqlx::Connection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use waddle_server::db::ConnectionGuard;

const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

struct Gate {
    armed: AtomicBool,
    dropped: AtomicBool,
    entered: Semaphore,
    resume: Semaphore,
}

impl Gate {
    fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
            entered: Semaphore::new(0),
            resume: Semaphore::new(0),
        }
    }

    fn arm(&self) {
        self.dropped.store(false, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }

    async fn pause_if_armed(self: &Arc<Self>) {
        if self.armed.swap(false, Ordering::SeqCst) {
            let _guard = GateDrop(Arc::clone(self));
            self.entered.add_permits(1);
            self.resume.acquire().await.expect("gate open").forget();
        }
    }

    async fn wait_until_entered(&self) {
        tokio::time::timeout(TEST_TIMEOUT, self.entered.acquire())
            .await
            .expect("hook reached its gate")
            .expect("gate open")
            .forget();
    }

    fn release(&self) {
        self.resume.add_permits(1);
    }
}

struct GateDrop(Arc<Gate>);

impl Drop for GateDrop {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

fn pool_with_gates(url: &str, before: Arc<Gate>, after: Arc<Gate>) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .min_connections(0)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .before_acquire(move |_, _| {
            let before = Arc::clone(&before);
            Box::pin(async move {
                before.pause_if_armed().await;
                Ok(true)
            })
        })
        .after_release(move |_, _| {
            let after = Arc::clone(&after);
            Box::pin(async move {
                after.pause_if_armed().await;
                Ok(true)
            })
        })
        .connect_lazy(url)
        .expect("valid PostgreSQL test URL")
}

fn lane(pool: &PgPool) -> ConnectionGuard {
    ConnectionGuard::postgres_admission_for_test(pool.clone(), 1, ACQUIRE_TIMEOUT)
}

async fn wait_for_idle(pool: &PgPool) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while pool.size() == 0 || pool.num_idle() != pool.size() as usize {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection returned to the pool");
}

async fn assert_lane_waits(lane: &ConnectionGuard) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), lane.query("SELECT 1", ()))
            .await
            .is_err(),
        "the lane permit must remain held while the physical checkout is returning"
    );
}

async fn wait_for_advisory_lock(connection: &mut PgConnection, pid: i64) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
                 WHERE pid = $1 AND locktype = 'advisory' AND NOT granted)",
            )
            .bind(i32::try_from(pid).expect("backend PID fits i32"))
            .fetch_one(&mut *connection)
            .await
            .expect("inspect advisory lock wait");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("transaction reached the advisory lock");
}

async fn backend_pid(transaction: &mut waddle_server::db::Transaction<'_>) -> i64 {
    transaction
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("query backend PID")
        .next()
        .await
        .expect("read backend PID")
        .expect("PID row")
        .get(0)
        .expect("integer PID")
}

async fn forward_postgres_client(
    mut client: tokio::net::tcp::OwnedReadHalf,
    mut server: tokio::net::tcp::OwnedWriteHalf,
    begin_request_gate: Arc<Gate>,
    omit_rollback: bool,
    rollback_rewritten: Arc<AtomicBool>,
) -> std::io::Result<()> {
    // PostgreSQL's startup packet has no type byte. Subsequent packets have
    // a type byte followed by their length.
    let length = client.read_u32().await?;
    let mut startup = vec![0; usize::try_from(length - 4).expect("startup size")];
    client.read_exact(&mut startup).await?;
    server.write_u32(length).await?;
    server.write_all(&startup).await?;
    loop {
        let tag = client.read_u8().await?;
        let mut length = client.read_u32().await?;
        let mut body = vec![0; usize::try_from(length - 4).expect("packet size")];
        client.read_exact(&mut body).await?;
        if tag == b'Q' && body == b"BEGIN\0" {
            begin_request_gate.pause_if_armed().await;
        }
        if omit_rollback && tag == b'Q' && body == b"ROLLBACK\0" {
            // Preserve PostgreSQL's response count while simulating a missing
            // rollback. The connection remains inside the accepted BEGIN.
            body = b"SELECT 1\0".to_vec();
            length = u32::try_from(body.len() + 4).expect("replacement packet size");
            rollback_rewritten.store(true, Ordering::SeqCst);
        }
        server.write_u8(tag).await?;
        server.write_u32(length).await?;
        server.write_all(&body).await?;
    }
}

async fn forward_postgres_server(
    mut server: tokio::net::tcp::OwnedReadHalf,
    mut client: tokio::net::tcp::OwnedWriteHalf,
    begin_reply_gate: Arc<Gate>,
) -> std::io::Result<()> {
    loop {
        let tag = server.read_u8().await?;
        let length = server.read_u32().await?;
        let mut body = vec![0; usize::try_from(length - 4).expect("packet size")];
        server.read_exact(&mut body).await?;
        if tag == b'Z' && body == b"T" {
            // The server accepted BEGIN and sent ReadyForQuery(in transaction),
            // but SQLx has not yet received the transaction state.
            begin_reply_gate.pause_if_armed().await;
        }
        client.write_u8(tag).await?;
        client.write_u32(length).await?;
        client.write_all(&body).await?;
    }
}

fn begin_proxy_target(value: &str) -> Result<(url::Url, String, u16), &'static str> {
    let url = url::Url::parse(value).map_err(|_| "invalid PostgreSQL test URL")?;
    if url
        .query_pairs()
        .any(|(key, _)| matches!(key.as_ref(), "host" | "hostaddr" | "port"))
    {
        return Err("BEGIN wire gate does not support host, hostaddr, or port query overrides");
    }
    let options: PgConnectOptions = value.parse().map_err(|_| "invalid PostgreSQL test URL")?;
    if options.get_socket().is_some() {
        return Err("BEGIN wire gate requires a TCP loopback PostgreSQL fixture");
    }
    let host = match url.host() {
        Some(url::Host::Domain(host)) if host.eq_ignore_ascii_case("localhost") => host.to_owned(),
        Some(url::Host::Domain(host)) => match host.parse::<IpAddr>() {
            Ok(address) if address.is_loopback() => address.to_string(),
            _ => return Err("BEGIN wire gate requires localhost or a loopback IP literal"),
        },
        Some(url::Host::Ipv4(host)) if host.is_loopback() => host.to_string(),
        Some(url::Host::Ipv6(host)) if host.is_loopback() => host.to_string(),
        _ => return Err("BEGIN wire gate requires localhost or a loopback IP literal"),
    };
    Ok((url, host, options.get_port()))
}

fn require_loopback_addresses(addresses: &[SocketAddr]) -> Result<(), &'static str> {
    if addresses.is_empty() || addresses.iter().any(|address| !address.ip().is_loopback()) {
        return Err("BEGIN wire gate requires exclusively loopback PostgreSQL addresses");
    }
    Ok(())
}

async fn postgres_begin_proxy(
    database_url: &str,
    begin_request_gate: Arc<Gate>,
    begin_reply_gate: Arc<Gate>,
    omit_rollback: bool,
) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    // The proxy reads plaintext PostgreSQL protocol. Fail the required test
    // rather than downgrade a remote server's configured TLS connection.
    let (mut url, host, port) = begin_proxy_target(database_url)
        .expect("BEGIN wire gate requires a supported local PostgreSQL test fixture");
    let target: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .expect("resolve loopback PostgreSQL test fixture")
        .collect();
    require_loopback_addresses(&target).expect("PostgreSQL fixture resolved outside loopback");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind PostgreSQL proxy");
    let proxy_port = listener.local_addr().expect("proxy address").port();
    url.set_host(Some("127.0.0.1")).expect("set proxy host");
    url.set_port(Some(proxy_port)).expect("set proxy port");
    url.query_pairs_mut().append_pair("sslmode", "disable");

    let rollback_rewritten = Arc::new(AtomicBool::new(false));
    let rewrites = Arc::clone(&rollback_rewritten);
    let task = tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                return;
            };
            let target = target.clone();
            let begin_request_gate = Arc::clone(&begin_request_gate);
            let begin_reply_gate = Arc::clone(&begin_reply_gate);
            let rollback_rewritten = Arc::clone(&rewrites);
            tokio::spawn(async move {
                let Ok(server) = TcpStream::connect(target.as_slice()).await else {
                    return;
                };
                let Ok(peer) = server.peer_addr() else {
                    eprintln!("BEGIN wire gate could not inspect the PostgreSQL peer");
                    return;
                };
                if require_loopback_addresses(&[peer]).is_err() {
                    eprintln!("BEGIN wire gate refused a non-loopback PostgreSQL peer");
                    return;
                }
                let (client_read, client_write) = client.into_split();
                let (server_read, server_write) = server.into_split();
                tokio::select! {
                    _ = forward_postgres_client(client_read, server_write, begin_request_gate, omit_rollback, rollback_rewritten) => {}
                    _ = forward_postgres_server(server_read, client_write, begin_reply_gate) => {}
                }
            });
        }
    });
    (url.to_string(), task, rollback_rewritten)
}

#[test]
fn postgres_wire_proxy_rejects_remote_and_host_overrides() {
    for url in [
        "postgresql://203.0.113.1/postgres?sslmode=require",
        "postgresql://[2001:db8::1]/postgres",
        "postgresql://db.example/postgres",
        "postgresql://127.0.0.1/postgres?hostaddr=203.0.113.1",
        "postgresql://127.0.0.1/postgres?host=db.example",
        "postgresql://127.0.0.1/postgres?port=5432",
    ] {
        assert!(
            begin_proxy_target(url).is_err(),
            "unsafe target was accepted: {url}"
        );
    }
    for url in [
        "postgresql://127.0.0.1/postgres?sslmode=require",
        "postgresql://[::1]/postgres",
        "postgresql://localhost/postgres",
    ] {
        assert!(
            begin_proxy_target(url).is_ok(),
            "loopback target was rejected: {url}"
        );
    }
    let loopback: SocketAddr = "127.0.0.1:5432".parse().expect("loopback address");
    let remote: SocketAddr = "203.0.113.1:5432".parse().expect("remote address");
    assert!(require_loopback_addresses(&[loopback]).is_ok());
    assert!(require_loopback_addresses(&[]).is_err());
    assert!(require_loopback_addresses(&[loopback, remote]).is_err());
    assert!(require_loopback_addresses(&[remote]).is_err());
}

#[tokio::test]
async fn checkout_return_retains_lane_after_commit_rollback_drop_and_guard_query() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let before = Arc::new(Gate::new());
    let after = Arc::new(Gate::new());
    let pool = pool_with_gates(&url, before, Arc::clone(&after));
    let main = lane(&pool);
    let ingress = lane(&pool);

    for outcome in ["commit", "rollback", "drop", "query", "execute", "batch"] {
        if matches!(outcome, "query" | "execute" | "batch") {
            after.arm();
            match outcome {
                "query" => {
                    main.query("SELECT 1", ()).await.expect("guard query");
                }
                "execute" => {
                    main.execute("SELECT 1", ()).await.expect("guard execute");
                }
                "batch" => main
                    .execute_batch("SELECT 1; SELECT 2")
                    .await
                    .expect("guard batch"),
                _ => unreachable!(),
            }
        } else {
            let transaction = main
                .begin_for_admission_test()
                .await
                .expect("begin transaction");
            after.arm();
            match outcome {
                "commit" => transaction.commit().await.expect("commit"),
                "rollback" => transaction.rollback().await.expect("rollback"),
                "drop" => drop(transaction),
                _ => unreachable!(),
            }
        }

        after.wait_until_entered().await;
        assert_lane_waits(&main).await;
        ingress
            .query("SELECT 1", ())
            .await
            .expect("reserved ingress physical capacity remains available");
        after.release();
        main.query("SELECT 1", ())
            .await
            .expect("main lane becomes available after physical return");
        wait_for_idle(&pool).await;
    }
    pool.close().await;
}

#[tokio::test]
async fn cancelled_begin_drops_floating_acquisition_before_releasing_admission() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let before = Arc::new(Gate::new());
    let pool = pool_with_gates(&url, Arc::clone(&before), Arc::new(Gate::new()));
    drop(
        pool.acquire()
            .await
            .expect("create idle physical connection"),
    );
    wait_for_idle(&pool).await;
    let main = lane(&pool);

    before.arm();
    let mut begin = Box::pin(main.begin_for_admission_test());
    tokio::select! {
        _ = before.wait_until_entered() => {}
        _ = &mut begin => panic!("acquisition finished before the hook"),
    }
    assert_eq!(pool.size(), 1, "SQLx has floated an existing connection");
    assert_lane_waits(&main).await;
    drop(begin);

    assert!(before.dropped.load(Ordering::SeqCst));
    assert_eq!(pool.size(), 0, "cancellation disposed SQLx's physical slot");
    main.query("SELECT 1", ())
        .await
        .expect("admission is reusable after the physical slot is released");
    pool.close().await;
}

#[tokio::test]
async fn cancelled_begin_before_send_retains_lane_until_cleanup() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let begin_gate = Arc::new(Gate::new());
    let (proxy_url, proxy, _) =
        postgres_begin_proxy(&url, Arc::clone(&begin_gate), Arc::new(Gate::new()), false).await;
    let pool = pool_with_gates(&proxy_url, Arc::new(Gate::new()), Arc::new(Gate::new()));
    let main = lane(&pool);
    let ingress = lane(&pool);

    begin_gate.arm();
    let mut begin = Box::pin(main.begin_for_admission_test());
    tokio::select! {
        _ = begin_gate.wait_until_entered() => {}
        _ = &mut begin => panic!("BEGIN completed before reaching the wire gate"),
    }
    drop(begin);
    assert_lane_waits(&main).await;
    ingress
        .query("SELECT 1", ())
        .await
        .expect("ingress retains its reserved physical slot");
    begin_gate.release();
    main.query("SELECT 1", ())
        .await
        .expect("main lane becomes available after cleanup");
    pool.close().await;
    proxy.abort();
}

async fn backend_state(connection: &mut PgConnection, pid: i32) -> (String, bool) {
    sqlx::query_as(
        "SELECT state, xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid = $1",
    )
    .bind(pid)
    .fetch_optional(connection)
    .await
    .expect("inspect backend state")
    .expect("the same backend remains connected")
}

async fn assert_accepted_begin_cleanup(url: &str, omit_rollback: bool) {
    let begin_reply_gate = Arc::new(Gate::new());
    let (proxy_url, proxy, rollback_rewritten) = postgres_begin_proxy(
        url,
        Arc::new(Gate::new()),
        Arc::clone(&begin_reply_gate),
        omit_rollback,
    )
    .await;
    let pool = pool_with_gates(&proxy_url, Arc::new(Gate::new()), Arc::new(Gate::new()));
    let main = lane(&pool);
    let mut observer = PgConnection::connect(url)
        .await
        .expect("observer connection");

    let mut checkout = pool.acquire().await.expect("warm checkout");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *checkout)
        .await
        .expect("capture backend PID");
    drop(checkout);
    wait_for_idle(&pool).await;

    begin_reply_gate.arm();
    let mut begin = Box::pin(main.begin_for_admission_test());
    tokio::select! {
        _ = begin_reply_gate.wait_until_entered() => {}
        _ = &mut begin => panic!("BEGIN completed before its reply was released"),
    }
    assert_eq!(
        backend_state(&mut observer, pid).await,
        ("idle in transaction".to_owned(), false),
        "PostgreSQL accepted BEGIN before the caller was cancelled"
    );
    drop(begin);
    assert_lane_waits(&main).await;
    begin_reply_gate.release();

    let mut rows = main
        .query("SELECT pg_backend_pid()", ())
        .await
        .expect("reuse the lane after cleanup");
    let reused_pid: i64 = rows
        .next()
        .await
        .expect("read reused PID")
        .expect("reused PID row")
        .get(0)
        .expect("integer PID");
    assert_eq!(reused_pid, i64::from(pid), "the same checkout was reused");
    wait_for_idle(&pool).await;

    let expected = if omit_rollback {
        assert!(
            rollback_rewritten.load(Ordering::SeqCst),
            "negative control intercepted the queued ROLLBACK"
        );
        ("idle in transaction".to_owned(), false)
    } else {
        ("idle".to_owned(), true)
    };
    assert_eq!(backend_state(&mut observer, pid).await, expected);
    pool.close().await;
    observer.close().await.expect("close observer");
    proxy.abort();
}

#[tokio::test]
async fn cancelled_accepted_begin_rolls_back_before_pool_return() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    assert_accepted_begin_cleanup(&url, false).await;
}

#[tokio::test]
async fn accepted_begin_negative_control_detects_missing_rollback() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    assert_accepted_begin_cleanup(&url, true).await;
}

#[tokio::test]
async fn cancelled_query_and_commit_hold_the_lane_until_cleanup() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let after = Arc::new(Gate::new());
    let pool = pool_with_gates(&url, Arc::new(Gate::new()), Arc::clone(&after));
    let main = lane(&pool);
    let ingress = lane(&pool);
    let mut observer = PgConnection::connect(&url)
        .await
        .expect("observer connection");

    for (case, lock_id) in [("query", 795_204_601_i64), ("commit", 795_204_602)] {
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(lock_id)
            .execute(&mut observer)
            .await
            .expect("hold advisory lock");
        let mut transaction = main
            .begin_for_admission_test()
            .await
            .expect("begin transaction");
        let pid = backend_pid(&mut transaction).await;

        if case == "commit" {
            transaction
                .execute_batch(
                    "CREATE TEMP TABLE admission_commit (value INTEGER);\
                     CREATE FUNCTION pg_temp.admission_wait() RETURNS trigger LANGUAGE plpgsql AS $$\
                       BEGIN PERFORM pg_advisory_xact_lock(795204602); RETURN NEW; END $$;\
                     CREATE CONSTRAINT TRIGGER admission_wait AFTER INSERT ON admission_commit \
                       DEFERRABLE INITIALLY DEFERRED FOR EACH ROW \
                       EXECUTE FUNCTION pg_temp.admission_wait();\
                     INSERT INTO admission_commit VALUES (1);",
                )
                .await
                .expect("install commit-time lock");
            let mut commit = Box::pin(transaction.commit());
            tokio::select! {
                _ = wait_for_advisory_lock(&mut observer, pid) => {}
                _ = &mut commit => panic!("commit completed before reaching the lock"),
            }
            after.arm();
            // PostgreSQL may commit after cancellation; the result is indeterminate.
            drop(commit);
        } else {
            let mut query =
                Box::pin(transaction.execute("SELECT pg_advisory_xact_lock(?)", [lock_id]));
            tokio::select! {
                _ = wait_for_advisory_lock(&mut observer, pid) => {}
                _ = &mut query => panic!("query completed before reaching the lock"),
            }
            drop(query);
            after.arm();
            drop(transaction);
        }

        after.wait_until_entered().await;
        assert_lane_waits(&main).await;
        ingress
            .query("SELECT 1", ())
            .await
            .expect("ingress retains reserved capacity");
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(lock_id)
            .execute(&mut observer)
            .await
            .expect("release advisory lock");
        after.release();
        main.query("SELECT 1", ())
            .await
            .expect("lane is reusable after flushing pending work");
        wait_for_idle(&pool).await;
    }
    pool.close().await;
    observer.close().await.expect("close observer");
}

#[tokio::test]
async fn stuck_return_is_bounded_and_closes_the_checkout_before_reusing_admission() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let after = Arc::new(Gate::new());
    let pool = pool_with_gates(&url, Arc::new(Gate::new()), Arc::clone(&after));
    let main = lane(&pool);
    let transaction = main
        .begin_for_admission_test()
        .await
        .expect("begin transaction");
    after.arm();
    let started = Instant::now();
    drop(transaction);
    after.wait_until_entered().await;
    assert_lane_waits(&main).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        while !after.dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded cleanup cancelled the stuck hook");
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert_eq!(pool.size(), 0, "timed-out return closed the checkout");
    main.query("SELECT 1", ())
        .await
        .expect("lane can open a replacement after the timeout");
    pool.close().await;
}

#[test]
fn runtime_shutdown_disposes_pending_return_and_drop_after_shutdown() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("first runtime");
    let after = Arc::new(Gate::new());
    let pool = runtime
        .block_on(async { pool_with_gates(&url, Arc::new(Gate::new()), Arc::clone(&after)) });
    let main = lane(&pool);

    runtime.block_on(async {
        let transaction = main
            .begin_for_admission_test()
            .await
            .expect("begin transaction");
        after.arm();
        drop(transaction);
        after.wait_until_entered().await;
    });
    drop(runtime);
    assert!(after.dropped.load(Ordering::SeqCst));
    assert_eq!(pool.size(), 0, "shutdown disposed the floating connection");

    let second = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("second runtime");
    let transaction = second
        .block_on(main.begin_for_admission_test())
        .expect("lane reopens after shutdown");
    drop(second);
    drop(transaction);
    assert_eq!(pool.size(), 0, "drop after shutdown disposed the checkout");

    let third = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("third runtime");
    third.block_on(async {
        main.query("SELECT 1", ())
            .await
            .expect("admission reused after a lease was dropped off-runtime");
        pool.close().await;
    });
}
