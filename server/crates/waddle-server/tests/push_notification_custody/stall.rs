//! Loopback proxy witnesses for a lost post-gate PostgreSQL reply.

use std::io;
use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use waddle_server::db::physical_attestation::{
    attested_pool, PhysicalAttestationConfig, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::DatabaseError;
use waddle_server::server::push_notification_custody::{
    verify_attested_push_notification_custody, PushNotificationCustodyError,
};
use waddle_server::test_support::postgres_proxy;

use super::super::Fixture;

pub(super) async fn exercise(f: &Fixture, config: &PhysicalAttestationConfig) {
    if postgres_proxy::ensure_local_target(&f.admin_options).is_err() {
        return;
    }
    // A tighter session policy must remain effective. The second case begins
    // with the timeout disabled and witnesses the reader's 30s cap as well as
    // its client deadline and bounded rollback after the reply disappears.
    witness(f, config, "3s", Duration::from_secs(10), false).await;
    witness(f, config, "0", Duration::from_secs(40), true).await;
}

async fn witness(
    f: &Fixture,
    config: &PhysicalAttestationConfig,
    prior_idle_timeout: &str,
    server_release_bound: Duration,
    wait_for_client_deadline: bool,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local custody proxy");
    let port = listener.local_addr().expect("local proxy address").port();
    let (arm, armed) = oneshot::channel();
    let (blacked, blackout_started) = oneshot::channel();
    let proxy = tokio::spawn(blackhole_after_foundation_reply(
        listener,
        f.admin_options.clone(),
        armed,
        blacked,
    ));
    let application = format!("custody_stall_{}", uuid::Uuid::new_v4().simple());
    let mut proxy_url =
        postgres_proxy::local_proxy_url(&f.base_url, port).expect("local-only proxy URL");
    proxy_url
        .query_pairs_mut()
        .append_pair("user", &f.runtime)
        .append_pair(
            "options",
            &format!(
                "-c search_path={} -c idle_in_transaction_session_timeout={prior_idle_timeout}",
                f.target
            ),
        );
    let options = PgConnectOptions::from_str(proxy_url.as_str())
        .expect("proxied runtime options")
        .application_name(&application);
    let runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "custody lost reply",
        attested_pool(options, config.clone(), 1, Duration::from_secs(3))
            .expect("proxied attested pool"),
    );
    let mut foundation = f.admin.begin().await.expect("begin Foundation blocker");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_effect_descendants IN ACCESS EXCLUSIVE MODE",
        f.target
    ))
    .execute(&mut *foundation)
    .await
    .expect("hold Foundation relation");
    let db = runtime.clone();
    let reader =
        tokio::spawn(
            async move { verify_attested_push_notification_custody(db, &[7_u8; 32]).await },
        );
    let pid: i32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'active' \
                   AND a.wait_event_type = 'Lock' \
                   AND a.query LIKE '%ingress_effect_descendants%'",
            )
            .bind(&application)
            .fetch_optional(&f.admin)
            .await
            .expect("observe proxied operation backend");
            if let Some(pid) = pid {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("proxied reader reaches Foundation");
    arm.send(()).expect("arm reply blackout");
    foundation
        .rollback()
        .await
        .expect("release Foundation blocker");
    tokio::time::timeout(Duration::from_secs(5), blackout_started)
        .await
        .expect("proxy did not forward Foundation response")
        .expect("proxy entered blackout");

    // Observe the actual operation backend idle with catalog-S after it has
    // processed a later verification query whose reply the proxy withheld.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let idle_with_catalog: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 JOIN pg_catalog.pg_locks l ON l.pid = a.pid \
                 WHERE a.pid = $1 AND a.state = 'idle in transaction' \
                   AND a.xact_start IS NOT NULL AND l.locktype = 'advisory' \
                   AND l.granted AND l.mode = 'ShareLock' AND l.objsubid = 1 \
                   AND l.classid::bigint = ($2::bigint >> 32) \
                   AND l.objid::bigint = ($2::bigint & 4294967295))",
            )
            .bind(pid)
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&f.admin)
            .await
            .expect("observe idle catalog holder");
            if idle_with_catalog {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("lost reply leaves the operation idle holding catalog-S");
    tokio::time::timeout(server_release_bound, async {
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&f.admin)
            .await
            .expect("check server-side idle timeout");
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("server did not terminate the idle custody transaction");
    let mut catalog = f.admin.begin().await.expect("begin catalog probe");
    let acquired: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *catalog)
        .await
        .expect("probe catalog-X after server exit");
    assert!(acquired, "server exit did not release catalog-S");
    catalog.rollback().await.expect("release catalog probe");
    assert!(!reader.is_finished(), "proxy must still hide the reply");

    if wait_for_client_deadline {
        let result = tokio::time::timeout(Duration::from_secs(70), reader)
            .await
            .expect("client inspection or rollback exceeded its bound")
            .expect("join reader");
        assert!(matches!(
            result,
            Err(PushNotificationCustodyError::Database(
                DatabaseError::CatalogMutationRollbackUnconfirmed
            ))
        ));
    } else {
        reader.abort();
        assert!(reader
            .await
            .expect_err("join cancelled reader")
            .is_cancelled());
    }
    proxy.abort();
    let _ = proxy.await;
    drop(runtime);
}

async fn blackhole_after_foundation_reply(
    listener: TcpListener,
    upstream: PgConnectOptions,
    mut arm: oneshot::Receiver<()>,
    blacked: oneshot::Sender<()>,
) -> io::Result<()> {
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = async move {
        // Keep the client socket open even if PostgreSQL closes the upstream.
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
        std::future::pending::<io::Result<()>>().await
    };
    let backward = async move {
        let mut armed = false;
        let mut saw_lock = false;
        let mut blackholed = false;
        let mut blacked = Some(blacked);
        loop {
            let tag = tokio::select! {
                result = server_read.read_u8() => match result {
                    Ok(tag) => tag,
                    Err(_) if blackholed => return std::future::pending::<io::Result<()>>().await,
                    Err(error) => return Err(error),
                },
                result = &mut arm, if !armed => {
                    result.map_err(|_| io::Error::other("blackout arm dropped"))?;
                    armed = true;
                    continue;
                }
            };
            let length = server_read.read_u32().await?;
            if !(4..=1_048_576).contains(&length) {
                return Err(io::Error::other("invalid backend frame"));
            }
            let mut body = vec![0; (length - 4) as usize];
            server_read.read_exact(&mut body).await?;
            if blackholed {
                continue;
            }
            if armed && tag == b'C' && body == b"LOCK TABLE\0" {
                saw_lock = true;
            }
            client_write.write_u8(tag).await?;
            client_write.write_u32(length).await?;
            client_write.write_all(&body).await?;
            if saw_lock && tag == b'Z' {
                blackholed = true;
                blacked
                    .take()
                    .expect("first blackout")
                    .send(())
                    .map_err(|_| io::Error::other("blackout observer dropped"))?;
            }
        }
    };
    tokio::select! { result = forward => result, result = backward => result }
}
