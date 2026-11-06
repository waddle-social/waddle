//! Real PG17 coverage of pinned fresh/reused physical pool hooks.

use std::sync::Arc;
use std::time::Duration;

use sqlx::{Connection, PgConnection};
use waddle_server::db::physical_attestation::{
    attested_pool, pinned_attested_pool, AttestedPgPool, DarkPhysicalAttestationPins,
    PhysicalAttestationConfig, PhysicalAttestationError, PinnedRuntimeAuthority,
};
use waddle_server::db::{DarkMigrationLedgerPins, DatabaseError};

use super::{
    attestor_probe_was_called, create_attestor_probe, dark_attestor_fixture, test_url, Fixture,
    CATALOG_GRANT_LOCK,
};

// The owner and runtime use the production lineage barrier key.
const LINEAGE_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_993;

// These synthetic nonphysical pins exercise bundle identity only: the hook
// intentionally does not claim to prove the ledger, epoch or guard manifest.
fn authority(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    attestor: DarkPhysicalAttestationPins,
    variant: u32,
) -> PinnedRuntimeAuthority {
    let schema_oid = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PG identity")
        .schema
        .oid;
    let base = 3_000_000_000 + variant * 10;
    let ledger = DarkMigrationLedgerPins {
        schema_oid,
        schema_owner_role_oid: attestor.schema_owner_role_oid,
        runtime_role_oid: attestor.runtime_role_oid,
        ledger_relation_oid: base,
        ledger_primary_index_oid: base + 1,
    };
    PinnedRuntimeAuthority::new(config, attestor, ledger, base + 2, base + 3, base + 4)
        .expect("valid independent test bundle")
}

fn pool(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    pins: PinnedRuntimeAuthority,
    timeout: Duration,
) -> AttestedPgPool {
    pinned_attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        pins,
        1,
        timeout,
    )
    .expect("pinned pool")
}

async fn backend_pid(connection: &mut PgConnection) -> i32 {
    sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(connection)
        .await
        .expect("backend PID")
}

async fn wait_for_lock(fixture: &Fixture, pid: i32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE pid = $1 AND wait_event_type = 'Lock')",
            )
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe lock wait");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("lock wait reached")
}

#[tokio::test]
async fn pinned_checkout_proves_fresh_and_reused_sessions_and_preserves_limits() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pool = pool(
        &fixture,
        &config,
        authority(&fixture, &config, attestor, 1),
        Duration::from_secs(5),
    );
    let mut first = pool
        .acquire_for_test()
        .await
        .expect("fresh pinned checkout");
    let pid = backend_pid(&mut first).await;
    for sql in [
        "SET search_path = pg_catalog",
        "SET lock_timeout = '4s'",
        "SET statement_timeout = '8s'",
        "SET idle_in_transaction_session_timeout = '9s'",
        "SET transaction_timeout = '18s'",
    ] {
        sqlx::query(sql).execute(&mut *first).await.expect(sql);
    }
    drop(first);
    let mut warm = pool.acquire_for_test().await.expect("warm pinned checkout");
    assert_eq!(backend_pid(&mut warm).await, pid);
    let (schema, lock, statement, idle, transaction): (String, String, String, String, String) =
        sqlx::query_as(
            "SELECT pg_catalog.current_schema()::text, pg_catalog.current_setting('lock_timeout'), \
             pg_catalog.current_setting('statement_timeout'), \
             pg_catalog.current_setting('idle_in_transaction_session_timeout'), \
             pg_catalog.current_setting('transaction_timeout')",
        )
        .fetch_one(&mut *warm)
        .await
        .expect("read normalized and preserved settings");
    assert_eq!(schema, fixture.target);
    assert_eq!(
        (
            lock.as_str(),
            statement.as_str(),
            idle.as_str(),
            transaction.as_str()
        ),
        ("4s", "8s", "9s", "18s")
    );
    drop(warm);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_bundle_rejects_inconsistent_or_different_pairs() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let schema_oid = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("PG identity")
        .schema
        .oid;
    let ledger = DarkMigrationLedgerPins {
        schema_oid,
        schema_owner_role_oid: attestor.schema_owner_role_oid,
        runtime_role_oid: attestor.runtime_role_oid,
        ledger_relation_oid: 3_000_000_000,
        ledger_primary_index_oid: 3_000_000_001,
    };
    let mut bad = attestor;
    bad.installation_operation_id = uuid::Uuid::nil();
    assert!(matches!(
        PinnedRuntimeAuthority::new(&config, bad, ledger, 2, 3, 4),
        Err(PhysicalAttestationError::InvalidRuntimePins)
    ));
    let mut bad = ledger;
    bad.runtime_role_oid = attestor.installer_role_oid;
    assert!(matches!(
        PinnedRuntimeAuthority::new(&config, attestor, bad, 2, 3, 4),
        Err(PhysicalAttestationError::InvalidRuntimePins)
    ));
    assert!(matches!(
        PinnedRuntimeAuthority::new(&config, attestor, ledger, ledger.ledger_relation_oid, 3, 4),
        Err(PhysicalAttestationError::InvalidRuntimePins)
    ));
    for different in [false, true] {
        let main = pinned_attested_pool(
            fixture.runtime_options.clone(),
            config.clone(),
            authority(&fixture, &config, attestor, 1),
            2,
            Duration::from_secs(3),
        )
        .expect("main pool");
        let control = if different {
            pool(
                &fixture,
                &config,
                authority(&fixture, &config, attestor, 2),
                Duration::from_secs(3),
            )
        } else {
            attested_pool(
                fixture.runtime_options.clone(),
                config.clone(),
                1,
                Duration::from_secs(3),
            )
            .expect("legacy pool")
        };
        assert!(matches!(
            waddle_server::test_support::database_pool_from_attested_postgres_pools(
                "mismatched bundles",
                main,
                Some((control, 1)),
                1,
                1,
            )
            .await,
            Err(DatabaseError::PhysicalAttestation(
                PhysicalAttestationError::Mismatch {
                    field: "control-plane pool attestation configuration"
                }
            ))
        ));
    }
    let dark = pinned_attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        authority(&fixture, &config, attestor, 3),
        2,
        Duration::from_secs(3),
    )
    .expect("dark pinned pool");
    assert!(matches!(
        waddle_server::db::DatabasePool::from_attested_postgres_pool(
            "unconverted pinned pool",
            dark,
            1,
            1,
        )
        .await,
        Err(DatabaseError::PhysicalAttestation(
            PhysicalAttestationError::UnconvertedPinnedTransaction
        ))
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_checkout_rolls_back_raw_snapshot_and_rejects_tracked_transaction() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pool = pool(
        &fixture,
        &config,
        authority(&fixture, &config, attestor, 1),
        Duration::from_millis(700),
    );
    let mut member = pool.acquire_for_test().await.expect("first checkout");
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *member)
        .await
        .expect("raw RR");
    sqlx::query(&format!("SELECT * FROM {}._lineage", fixture.target))
        .fetch_one(&mut *member)
        .await
        .expect("establish snapshot");
    sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1",
        fixture.target
    ))
    .bind(uuid::Uuid::new_v4().to_string())
    .execute(&fixture.admin)
    .await
    .expect("change lineage");
    drop(member);
    assert!(
        pool.acquire_for_test().await.is_err(),
        "stale RR snapshot cannot attest"
    );
    sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1",
        fixture.target
    ))
    .bind(fixture.lineage.lineage_uuid.to_string())
    .execute(&fixture.admin)
    .await
    .expect("restore lineage");
    let mut member = pool.acquire_for_test().await.expect("new valid checkout");
    let old_pid = backend_pid(&mut member).await;
    let tracked = member.begin().await.expect("tracked transaction");
    std::mem::forget(tracked);
    drop(member);
    let mut replacement = pool
        .acquire_for_test()
        .await
        .expect("tracked transaction discarded");
    assert_ne!(old_pid, backend_pid(&mut replacement).await);
    drop(replacement);
    pool.close().await;
    fixture.cleanup().await;
}
#[tokio::test]
async fn pinned_checkout_clears_temp_and_refuses_role_or_temp_grants() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pool = pool(
        &fixture,
        &config,
        authority(&fixture, &config, attestor, 1),
        Duration::from_millis(700),
    );
    let mut member = pool.acquire_for_test().await.expect("first checkout");
    let pid = backend_pid(&mut member).await;
    sqlx::query(&format!(
        "GRANT TEMP ON DATABASE {} TO {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("temporarily grant TEMP");
    sqlx::query("CREATE TEMP TABLE old_shadow (id integer)")
        .execute(&mut *member)
        .await
        .expect("create retained temp");
    sqlx::query(&format!(
        "REVOKE TEMP ON DATABASE {} FROM {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke TEMP");
    drop(member);
    let mut member = pool
        .acquire_for_test()
        .await
        .expect("checkout clears retained TEMP");
    assert_eq!(backend_pid(&mut member).await, pid);
    let exists: bool =
        sqlx::query_scalar("SELECT pg_catalog.to_regclass('pg_temp.old_shadow') IS NOT NULL")
            .fetch_one(&mut *member)
            .await
            .expect("check TEMP was discarded");
    assert!(!exists);
    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("allow role poisoning");
    sqlx::query(&format!("SET ROLE {}", fixture.outsider))
        .execute(&mut *member)
        .await
        .expect("poison role");
    drop(member);
    assert!(
        pool.acquire_for_test().await.is_err(),
        "assumed role and membership drift are refused"
    );
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.outsider, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke role poisoning");
    let member = pool
        .acquire_for_test()
        .await
        .expect("reconnect with the runtime role");
    drop(member);
    sqlx::query(&format!(
        "GRANT TEMP ON DATABASE {} TO {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant forbidden TEMP");
    assert!(
        pool.acquire_for_test().await.is_err(),
        "effective TEMP must deny a checkout"
    );
    sqlx::query(&format!(
        "REVOKE TEMP ON DATABASE {} FROM {}",
        fixture.database_name, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore TEMP boundary");
    drop(
        pool.acquire_for_test()
            .await
            .expect("checkout after TEMP revocation"),
    );
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_checkout_releases_leaked_session_advisory_locks_for_waiting_owner() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pool = Arc::new(pool(
        &fixture,
        &config,
        authority(&fixture, &config, attestor, 1),
        Duration::from_secs(5),
    ));
    let mut member = pool.acquire_for_test().await.expect("first checkout");
    let original_pid = backend_pid(&mut member).await;
    sqlx::query("SELECT pg_catalog.pg_advisory_lock_shared($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut *member)
        .await
        .expect("leak shared session lock");
    let mut owner = fixture.admin.acquire().await.expect("owner session");
    let owner_pid = backend_pid(&mut owner).await;
    let (got_lock_tx, got_lock_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let owner_task = tokio::spawn(async move {
        let mut tx = owner.begin().await.expect("owner transaction");
        sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
            .bind(LINEAGE_ADVISORY_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .expect("owner obtains exclusive lock");
        got_lock_tx.send(()).expect("signal exclusive owner");
        release_rx.await.expect("owner release signal");
        tx.rollback().await.expect("release owner lock");
    });
    wait_for_lock(&fixture, owner_pid).await;
    drop(member);
    let checkout = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move {
            let mut member = pool
                .acquire_for_test()
                .await
                .expect("warm checkout after owner");
            backend_pid(&mut member).await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), got_lock_rx)
        .await
        .expect("owner did not deadlock")
        .expect("owner signal");
    assert!(
        !checkout.is_finished(),
        "checkout waits for the owner's exclusive barrier"
    );
    release_tx.send(()).expect("release owner");
    owner_task.await.expect("owner task");
    assert_eq!(checkout.await.expect("checkout task"), original_pid);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_hooks_refuse_replaced_function_before_execution() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pins = authority(&fixture, &config, attestor, 1);
    let warm = pool(&fixture, &config, pins.clone(), Duration::from_millis(700));
    drop(
        warm.acquire_for_test()
            .await
            .expect("establish warm session"),
    );
    create_attestor_probe(&fixture).await;
    sqlx::query(&format!(
        r#"CREATE OR REPLACE FUNCTION {}.read_physical_identity()
RETURNS TABLE (
    lineage_uuid text, deployment_uuid text,
    stored_system_identifier text, stored_database_oid text, stored_database_name text,
    stored_schema_oid text, stored_schema_name text,
    live_system_identifier text, live_database_oid text, live_database_name text,
    live_schema_oid text, live_schema_name text, stored_lineage_format bigint
)
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $body$ SELECT pg_catalog.nextval('{}.attestor_probe'::pg_catalog.regclass)::text,
    'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text,
    'x'::text, 'x'::text, 'x'::text, 'x'::text, 'x'::text, 1::bigint $body$"#,
        fixture.trusted, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("replace function without changing OID");
    assert!(
        warm.acquire_for_test().await.is_err(),
        "warm hook must deny function drift"
    );
    let fresh = pool(&fixture, &config, pins, Duration::from_millis(700));
    assert!(
        fresh.acquire_for_test().await.is_err(),
        "fresh hook must deny function drift"
    );
    assert!(
        !attestor_probe_was_called(&fixture).await,
        "hostile body was never invoked"
    );
    warm.close().await;
    fresh.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_hooks_refuse_replacement_view_before_execution() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pins = authority(&fixture, &config, attestor, 1);
    let warm = pool(&fixture, &config, pins.clone(), Duration::from_millis(700));
    drop(
        warm.acquire_for_test()
            .await
            .expect("establish warm session"),
    );
    create_attestor_probe(&fixture).await;
    sqlx::query(&format!(
        "CREATE FUNCTION {}.attestor_probe_fn() RETURNS integer LANGUAGE sql VOLATILE \
         SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS \
         'SELECT pg_catalog.nextval(''{}.attestor_probe''::pg_catalog.regclass)::integer'",
        fixture.target, fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create side-effect witness");
    sqlx::query(&format!(
        "ALTER TABLE {}._lineage RENAME TO original_lineage",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("rename lineage");
    sqlx::query(&format!(
        "CREATE VIEW {}._lineage AS SELECT l.id + {}.attestor_probe_fn() AS id, \
         l.format,l.lineage_uuid,l.deployment_uuid,l.pg_system_identifier,l.pg_database_oid, \
         l.pg_database_name,l.pg_schema_oid,l.pg_schema_name,l.stamped_at FROM {}.original_lineage l",
        fixture.target, fixture.target, fixture.target
    )).execute(&fixture.admin).await.expect("replace with hostile view");
    for role in [&fixture.runtime, &fixture.definer] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}._lineage TO {role}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant view access");
    }
    assert!(
        warm.acquire_for_test().await.is_err(),
        "warm hook refuses view"
    );
    let fresh = pool(&fixture, &config, pins, Duration::from_millis(700));
    assert!(
        fresh.acquire_for_test().await.is_err(),
        "fresh hook refuses view"
    );
    assert!(
        !attestor_probe_was_called(&fixture).await,
        "view was never executed"
    );
    warm.close().await;
    fresh.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_hook_cancellation_and_timeout_do_not_lend_blocked_sessions() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let pool = Arc::new(pool(
        &fixture,
        &config,
        authority(&fixture, &config, attestor, 1),
        Duration::from_millis(800),
    ));
    let mut member = pool.acquire_for_test().await.expect("first checkout");
    let first_pid = backend_pid(&mut member).await;
    let mut blocker = fixture.admin.begin().await.expect("owner transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._lineage IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("block physical proof");
    drop(member);
    let acquiring = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.acquire_for_test().await.map(|_| ()) })
    };
    wait_for_lock(&fixture, first_pid).await;
    assert!(
        acquiring.await.expect("checkout task").is_err(),
        "deadline must not lend blocked session"
    );
    blocker.rollback().await.expect("release lock");
    let mut member = pool
        .acquire_for_test()
        .await
        .expect("replacement after timeout");
    let second_pid = backend_pid(&mut member).await;
    assert_ne!(first_pid, second_pid, "timed-out session was discarded");
    let mut blocker = fixture
        .admin
        .begin()
        .await
        .expect("second owner transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._lineage IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("block second proof");
    drop(member);
    let acquiring = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.acquire_for_test().await.map(|_| ()) })
    };
    wait_for_lock(&fixture, second_pid).await;
    acquiring.abort();
    assert!(acquiring
        .await
        .expect_err("cancelled checkout")
        .is_cancelled());
    blocker.rollback().await.expect("release second lock");
    let mut replacement = pool
        .acquire_for_test()
        .await
        .expect("replacement after cancellation");
    assert_ne!(
        second_pid,
        backend_pid(&mut replacement).await,
        "cancelled session was discarded"
    );
    drop(replacement);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_hook_never_lends_when_proof_rollback_response_is_lost() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, attestor) = dark_attestor_fixture(&fixture).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(lose_proof_rollback(
        listener,
        fixture.admin_options.clone(),
        lost_tx,
    ));
    let options = super::proxy_runtime_options(&fixture, port);
    let pool = pinned_attested_pool(
        options,
        config.clone(),
        authority(&fixture, &config, attestor, 1),
        1,
        Duration::from_secs(3),
    )
    .expect("proxied pinned pool");
    let task = tokio::spawn(async move {
        let result = pool.acquire_for_test().await.map(|_| ());
        pool.close().await;
        result
    });
    tokio::time::timeout(Duration::from_secs(5), lost_rx)
        .await
        .expect("proof rollback reached")
        .expect("lost rollback response");
    assert!(
        task.await.expect("checkout task").is_err(),
        "unconfirmed rollback must not lend a session"
    );
    proxy.await.expect("proxy task").expect("proxy fault");
    fixture.cleanup().await;
}

// Allow the initial inherited-transaction ROLLBACK through. Discard the
// CommandComplete for the proof ROLLBACK, which happened after attestation.
async fn lose_proof_rollback(
    listener: tokio::net::TcpListener,
    upstream: sqlx::postgres::PgConnectOptions,
    lost: tokio::sync::oneshot::Sender<()>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (client, _) = listener.accept().await?;
    let server = crate::postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
    });
    let mut rollbacks = 0;
    loop {
        let tag = server_read.read_u8().await?;
        let length = server_read.read_u32().await?;
        if !(4..=1_048_576).contains(&length) {
            return Err(std::io::Error::other("invalid PostgreSQL frame"));
        }
        let mut body = vec![0; (length - 4) as usize];
        server_read.read_exact(&mut body).await?;
        if tag == b'C' && body == b"ROLLBACK\0" {
            rollbacks += 1;
            if rollbacks == 2 {
                let _ = lost.send(());
                forward.abort();
                let _ = forward.await;
                return Ok(());
            }
        }
        client_write.write_u8(tag).await?;
        client_write.write_u32(length).await?;
        client_write.write_all(&body).await?;
        client_write.flush().await?;
    }
}
