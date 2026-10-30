//! Liveness/readiness split (#2046 W1.2) against a real server process.
//!
//! The server reaches PostgreSQL through a TCP proxy that can sever every
//! proxied connection and black-hole new ones (accepted, never answered),
//! the worst case for an unbounded pool acquisition. During that outage
//! `/healthz` must keep answering 200 (no restart) while `/readyz` answers
//! 503 inside the kubelet's 2 s probe timeout (no endpoints); when the
//! database returns, readiness recovers without a restart.
//!
//! The server runs without clustering. A clustered node whose outage outlasts
//! the node-lease TTL self-fences before re-serving, and one that takes a
//! fatal fence fails liveness (`terminally_fenced`); neither is exercised
//! here.
//!
//! PostgreSQL is optional in local development, so the test skips unless
//! `WADDLE_TEST_POSTGRES_URL` is configured.

use std::time::Duration;

use tokio::sync::watch;
use waddle_ws_test_support::TestServer;

/// Kubelet probe timeout in the chart (`probes.*.timeoutSeconds`).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const OUTAGE_DETECTION_TIMEOUT: Duration = Duration::from_secs(45);
/// When Postgres returns, the server's in-flight connection attempts and the
/// serial DB actor's queued readiness checks (each bounded by the main-pool
/// acquire timeout, 30 s by default) must drain first. Generous so a slow CI
/// runner is not a false failure.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(120);
/// Spacing of readiness probes during the outage, close to the kubelet's
/// 10 s period so the test does not build an artificial DB-actor backlog.
const OUTAGE_PROBE_SPACING: Duration = Duration::from_secs(5);

/// Where the proxy forwards to: the test URL's TCP address, or the
/// PostgreSQL Unix socket when the URL names a socket directory in its
/// `host` query parameter (the Nix CI lanes run PostgreSQL socket-only).
enum Upstream {
    Tcp(String),
    Unix(std::path::PathBuf),
}

/// Point `url` at `127.0.0.1:<proxy_port>` and return the upstream it named.
fn redirect_to_proxy(url: &mut url::Url, proxy_port: u16) -> Upstream {
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let query_value = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let port = query_value("port")
        .and_then(|port| port.parse::<u16>().ok())
        .or(url.port())
        .unwrap_or(5432);
    let upstream = match query_value("host") {
        Some(host) if host.starts_with('/') => {
            Upstream::Unix(std::path::Path::new(&host).join(format!(".s.PGSQL.{port}")))
        }
        Some(host) => Upstream::Tcp(format!("{host}:{port}")),
        None => Upstream::Tcp(format!(
            "{}:{port}",
            url.host_str()
                .filter(|host| !host.is_empty())
                .unwrap_or("localhost")
        )),
    };
    url.query_pairs_mut().clear().extend_pairs(
        pairs
            .iter()
            .filter(|(key, _)| key != "host" && key != "port")
            .map(|(key, value)| (key, value)),
    );
    url.set_host(Some("127.0.0.1"))
        .expect("proxy host is valid");
    url.set_port(Some(proxy_port)).expect("proxy port is valid");
    upstream
}

async fn forward(
    mut inbound: tokio::net::TcpStream,
    upstream: &Upstream,
    mut down_rx: watch::Receiver<bool>,
) {
    match upstream {
        Upstream::Tcp(address) => {
            let Ok(mut outbound) = tokio::net::TcpStream::connect(address).await else {
                return;
            };
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                _ = down_rx.wait_for(|down| *down) => {}
            }
        }
        Upstream::Unix(path) => {
            let Ok(mut outbound) = tokio::net::UnixStream::connect(path).await else {
                return;
            };
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                _ = down_rx.wait_for(|down| *down) => {}
            }
        }
    }
}

/// TCP proxy in front of PostgreSQL. `down = true` severs every proxied
/// connection and black-holes new ones; `down = false` forwards again.
struct PostgresProxy {
    down: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl PostgresProxy {
    async fn bind() -> (tokio::net::TcpListener, u16) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind PostgreSQL proxy");
        let port = listener.local_addr().expect("proxy address").port();
        (listener, port)
    }

    fn start(listener: tokio::net::TcpListener, upstream: Upstream) -> Self {
        let upstream = std::sync::Arc::new(upstream);
        let (down, mut down_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut black_holed = Vec::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((inbound, _)) = accepted else {
                            return;
                        };
                        if *down_rx.borrow() {
                            black_holed.push(inbound);
                            continue;
                        }
                        let forward_down = down_rx.clone();
                        let upstream = std::sync::Arc::clone(&upstream);
                        tokio::spawn(async move { forward(inbound, &upstream, forward_down).await });
                    }
                    changed = down_rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        if !*down_rx.borrow_and_update() {
                            // Restored: close the black-holed sockets so the
                            // server's hung connection attempts fail now
                            // instead of at their acquire deadline.
                            black_holed.clear();
                        }
                    }
                }
            }
        });
        Self { down, task }
    }

    fn set_down(&self, down: bool) {
        self.down.send_replace(down);
    }
}

fn postgres_url_with_search_path(database_url: &str, schema: &str) -> url::Url {
    let mut url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url
}

/// One probe the way the kubelet runs it: a hard client timeout of
/// [`PROBE_TIMEOUT`]. `None` means the probe itself timed out.
async fn probe(client: &reqwest::Client, url: &str) -> Option<(reqwest::StatusCode, String)> {
    let response = client.get(url).timeout(PROBE_TIMEOUT).send().await.ok()?;
    let status = response.status();
    let body = response.text().await.ok()?;
    Some((status, body))
}

async fn wait_for_readiness(
    client: &reqwest::Client,
    server: &TestServer,
    expected: reqwest::StatusCode,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("no response");
    while tokio::time::Instant::now() < deadline {
        if let Some((status, body)) =
            probe(client, &format!("{}/readyz", server.http_base_url())).await
        {
            if status == expected {
                return;
            }
            last = format!("status={status}, body={body}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("readiness did not reach {expected}: {last}");
}

async fn assert_live(client: &reqwest::Client, server: &TestServer) {
    let (status, body) = probe(client, &format!("{}/healthz", server.http_base_url()))
        .await
        .expect("liveness must answer inside the probe timeout");
    assert_eq!(status, reqwest::StatusCode::OK, "liveness body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_outage_leaves_server_live_and_unready_then_recovers() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
        return;
    };
    let admin = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect PostgreSQL admin pool");
    let schema = format!(
        "waddle_test_liveness_split_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated PostgreSQL schema");

    let mut proxied = postgres_url_with_search_path(&database_url, &schema);
    let (listener, proxy_port) = PostgresProxy::bind().await;
    let upstream = redirect_to_proxy(&mut proxied, proxy_port);
    let proxy = PostgresProxy::start(listener, upstream);
    let proxied = proxied.to_string();
    let deployment_uuid = uuid::Uuid::new_v4().to_string();

    let server = tokio::task::spawn_blocking(move || {
        TestServer::start_with_extra_envs(
            &[],
            &[
                ("WADDLE_DB_DRIVER", "postgres"),
                ("WADDLE_DATABASE_URL", proxied.as_str()),
                ("WADDLE_DEPLOYMENT_UUID", deployment_uuid.as_str()),
                ("WADDLE_DB_LINEAGE_ACTION", "enroll"),
            ],
        )
    })
    .await
    .expect("server spawn task completes");
    let client = reqwest::Client::new();
    wait_for_readiness(&client, &server, reqwest::StatusCode::OK, STARTUP_TIMEOUT).await;
    assert_live(&client, &server).await;

    // Outage: every pooled connection is severed and new ones hang.
    proxy.set_down(true);
    let deadline = tokio::time::Instant::now() + OUTAGE_DETECTION_TIMEOUT;
    let mut unready_rounds = 0;
    while unready_rounds < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "readiness never reported the outage (unready rounds: {unready_rounds})"
        );
        let (status, body) = probe(&client, &format!("{}/readyz", server.http_base_url()))
            .await
            .expect("readiness must answer inside the probe timeout during an outage");
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            unready_rounds += 1;
        } else {
            assert_eq!(status, reqwest::StatusCode::OK, "readiness body: {body}");
        }
        assert_live(&client, &server).await;
        tokio::time::sleep(OUTAGE_PROBE_SPACING).await;
    }

    // Postgres returns: the same process becomes ready again, no restart.
    proxy.set_down(false);
    wait_for_readiness(&client, &server, reqwest::StatusCode::OK, RECOVERY_TIMEOUT).await;
    assert_live(&client, &server).await;

    drop(server);
    proxy.task.abort();
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated PostgreSQL schema");
}
