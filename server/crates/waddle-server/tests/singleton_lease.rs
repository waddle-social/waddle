//! Real PostgreSQL races for the unselected singleton lease candidate.

#[path = "../src/test_support/postgres_proxy.rs"]
mod postgres_proxy;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use uuid::Uuid;
use waddle_server::clustering::singleton_lease::{
    prepare_owner_schema, GuardBackend, LeaseGrant, LeaseOutcome, LeaseReadback, LeaseRequest,
    ProcessIdentity, SingletonLeaseError, SingletonLeaseStore,
};
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};

const TTL: Duration = Duration::from_secs(30);

struct Fixture {
    admin: PgPool,
    store: SingletonLeaseStore,
    schema: String,
    url: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let url = waddle_ws_test_support::require_test_postgres_url()?;
        let admin = PgPool::connect(&url).await.expect("postgres admin");
        let version: String = sqlx::query_scalar("SHOW server_version_num")
            .fetch_one(&admin)
            .await
            .expect("server version");
        let version: u32 = version.parse().expect("numeric server version");
        assert!(
            (170_000..180_000).contains(&version),
            "PostgreSQL 17 required"
        );
        let schema = format!("singleton_candidate_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let db = Database::from_config(
            "singleton candidate",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url.clone()),
        )
        .await
        .expect("candidate database");
        prepare_owner_schema(&db, &schema)
            .await
            .expect("owner prepares candidate");
        let store = SingletonLeaseStore::new_unverified(db, &schema).expect("candidate store");
        Some(Self {
            admin,
            store,
            schema,
            url,
        })
    }

    async fn acquired(&self, identity: ProcessIdentity) -> LeaseGrant {
        match self
            .store
            .register(Uuid::new_v4(), identity, TTL)
            .await
            .expect("register")
        {
            LeaseOutcome::Acquired { grant, .. } => grant,
            other => panic!("expected acquired: {other:?}"),
        }
    }

    async fn expire(&self, grant: &LeaseGrant) {
        sqlx::query(&format!(
            "UPDATE {}.singleton_candidate_leases SET expires_at = pg_catalog.clock_timestamp() - interval '1 second' WHERE incarnation = $1",
            self.schema
        ))
        .bind(grant.identity().incarnation())
        .execute(&self.admin)
        .await
        .expect("expire fixture lease");
    }

    async fn wait_for_slot_waiter(&self) {
        self.wait_for_row_waiter("singleton_candidate_slot").await;
    }

    async fn guard_holds_slot(&self, backend: GuardBackend) -> bool {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity activity \
             JOIN pg_catalog.pg_locks held ON held.pid = activity.pid \
             WHERE activity.pid = $1 AND activity.backend_start = $2 \
               AND activity.xact_start = $3 AND activity.datid::bigint = $4 \
               AND activity.backend_xid::text = $5 \
               AND pg_catalog.pg_postmaster_start_time() = $6 \
               AND held.relation = pg_catalog.to_regclass($7) \
               AND held.mode = 'RowShareLock' AND held.granted)",
        )
        .bind(backend.pid)
        .bind(backend.backend_start)
        .bind(backend.transaction_start)
        .bind(i64::from(backend.database_oid))
        .bind((backend.transaction_id & u64::from(u32::MAX)).to_string())
        .bind(backend.postmaster_start)
        .bind(format!("{}.singleton_candidate_slot", self.schema))
        .fetch_one(&self.admin)
        .await
        .expect("inspect exact guard transaction")
    }

    async fn wait_for_guard_release(&self, backend: GuardBackend) {
        tokio::time::timeout(Duration::from_secs(8), async {
            while self.guard_holds_slot(backend).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("guard transaction releases its slot lock");
    }

    async fn wait_for_row_waiter(&self, relation: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pg_catalog.pg_stat_activity \
                     WHERE wait_event_type = 'Lock' AND query LIKE $1",
                )
                .bind(format!("%{}%{relation}%FOR UPDATE%", self.schema))
                .fetch_one(&self.admin)
                .await
                .expect("inspect slot waiter");
                if waiting > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("slot waiter appears");
    }

    async fn cleanup(self) {
        drop(self.store);
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop isolated schema");
        self.admin.close().await;
    }
}

fn identity(node_id: &str) -> ProcessIdentity {
    ProcessIdentity::new(node_id, Uuid::new_v4()).expect("valid process identity")
}

#[tokio::test]
async fn owner_provisioning_waits_for_catalog_readers_but_lease_operations_do_not() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let admin = PgPool::connect(&url).await.expect("postgres admin");
    let schema = format!("singleton_fence_{}", Uuid::new_v4().simple());
    let application = format!("waddle_singleton_fence_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create isolated schema");

    let mut scoped_url = url::Url::parse(&url).expect("parse postgres URL");
    let retained: Vec<_> = scoped_url
        .query_pairs()
        .filter(|(key, _)| key != "application_name")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    scoped_url
        .query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("application_name", &application);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, scoped_url.to_string());
    config.pool_size = 1;
    let db = Database::from_config("singleton owner fence", &config)
        .await
        .expect("one-slot owner pool");

    let mut reader = admin.begin().await.expect("catalog reader");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock_shared($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *reader)
        .await
        .expect("hold shared catalog fence");
    let mut worker = Box::pin(prepare_owner_schema(&db, &schema));

    // Observe a completed rollback from a real failed try, rather than
    // assuming the spawned task has reached the fence after a fixed delay.
    let retry_observed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let retried: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1 AND state = 'idle' AND query = 'ROLLBACK')",
            )
            .bind(&application)
            .fetch_one(&admin)
            .await
            .expect("inspect owner transaction retry");
            if retried {
                break;
            }
            tokio::task::yield_now().await;
        }
    });
    tokio::select! {
        result = &mut worker => panic!("owner completed while catalog was held: {result:?}"),
        observed = retry_observed => observed.expect("owner retries after catalog conflict"),
    }
    let relation: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{schema}.singleton_candidate_leases"))
        .fetch_one(&admin)
        .await
        .expect("inspect provisioning visibility");
    assert!(relation.is_none(), "no owner DDL before catalog-X");
    reader.commit().await.expect("release catalog reader");
    tokio::time::timeout(Duration::from_secs(5), &mut worker)
        .await
        .expect("owner provisioning completes")
        .expect("owner provisioning succeeds");
    drop(worker);
    let store = SingletonLeaseStore::new_unverified(db, &schema).expect("candidate store");

    let mut catalog_writer = admin.begin().await.expect("catalog writer");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *catalog_writer)
        .await
        .expect("hold exclusive catalog fence");
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        store.register(Uuid::new_v4(), identity("catalog-independent"), TTL),
    )
    .await
    .expect("ordinary lease operation does not wait on catalog-X")
    .expect("lease registration succeeds");
    assert!(matches!(outcome, LeaseOutcome::Acquired { .. }));
    catalog_writer
        .commit()
        .await
        .expect("release catalog writer");
    drop(store);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}

#[tokio::test]
async fn slot_rejects_other_processes_and_replays_exact_operation_result() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let first_identity = identity("node-a");
    let operation_id = Uuid::new_v4();
    let request = LeaseRequest::Register {
        operation_id,
        identity: first_identity.clone(),
        ttl: TTL,
    };
    let first = fixture
        .store
        .register(operation_id, first_identity.clone(), TTL)
        .await
        .expect("register first");
    let grant = match &first {
        LeaseOutcome::Acquired { grant, .. } => grant.clone(),
        other => panic!("expected acquisition: {other:?}"),
    };
    assert_eq!(grant.generation(), 1);
    assert_eq!(
        fixture.store.readback(&request).await.expect("readback"),
        LeaseReadback::Recorded(first.clone())
    );
    assert_eq!(
        fixture
            .store
            .register(operation_id, first_identity, TTL)
            .await
            .expect("idempotent retry"),
        first
    );
    for other in [identity("node-a"), identity("node-b")] {
        assert_eq!(
            fixture
                .store
                .register(Uuid::new_v4(), other, TTL)
                .await
                .expect("refuse live slot"),
            LeaseOutcome::Busy
        );
    }
    assert!(matches!(
        fixture
            .store
            .register(operation_id, identity("other"), TTL)
            .await,
        Err(SingletonLeaseError::OperationMismatch)
    ));
    assert_eq!(
        fixture
            .store
            .revoke(Uuid::new_v4(), grant.clone())
            .await
            .expect("revoke"),
        LeaseOutcome::Revoked
    );
    let successor = fixture.acquired(identity("node-a")).await;
    assert_eq!(successor.generation(), 2);
    assert!(matches!(
        fixture.store.guard(&grant).await,
        Err(SingletonLeaseError::NotCurrent)
    ));
    assert_eq!(
        fixture
            .store
            .renew(Uuid::new_v4(), grant, TTL)
            .await
            .expect("old heartbeat"),
        LeaseOutcome::Lost
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_different_node_registrations_have_one_winner() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let left = fixture.store.clone();
    let right = fixture.store.clone();
    let (left, right) = tokio::join!(
        left.register(Uuid::new_v4(), identity("node-a"), TTL),
        right.register(Uuid::new_v4(), identity("node-b"), TTL),
    );
    let left = left.expect("left result");
    let right = right.expect("right result");
    assert!(matches!(
        (&left, &right),
        (LeaseOutcome::Acquired { .. }, LeaseOutcome::Busy)
            | (LeaseOutcome::Busy, LeaseOutcome::Acquired { .. })
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn heartbeat_progresses_while_guard_holds_slot_and_takeover_is_queued() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let grant = fixture.acquired(identity("node-a")).await;
    let outer = fixture.store.guard(&grant).await.expect("outer guard");
    let contender = fixture.store.clone();
    let takeover = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await
    });
    fixture.wait_for_slot_waiter().await;
    let renewed = tokio::time::timeout(
        Duration::from_secs(1),
        fixture.store.renew(Uuid::new_v4(), grant.clone(), TTL),
    )
    .await
    .expect("heartbeat is independent of slot lock")
    .expect("heartbeat result");
    assert!(matches!(renewed, LeaseOutcome::Renewed { .. }));
    {
        let nested = tokio::time::timeout(Duration::from_secs(1), fixture.store.guard(&grant))
            .await
            .expect("nested guard cannot hang behind queued takeover");
        match nested {
            // PostgreSQL may grant another compatible share lock while an
            // exclusive waiter is queued. Release it before the outer guard.
            Ok(guard) => guard.release().await.expect("release nested guard"),
            Err(SingletonLeaseError::Sql(sqlx::Error::Database(error)))
                if error.code().as_deref() == Some("55P03") => {}
            Err(error) => panic!("unexpected nested guard error: {error}"),
        }
    }
    outer.release().await.expect("release outer");
    assert_eq!(
        takeover
            .await
            .expect("join takeover")
            .expect("takeover result"),
        LeaseOutcome::Busy
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn takeover_rechecks_fresh_lease_after_waiting_for_heartbeat_row() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old = fixture.acquired(identity("node-a")).await;
    fixture.expire(&old).await;
    let mut heartbeat = fixture.admin.begin().await.expect("begin held heartbeat");
    sqlx::query(&format!(
        "UPDATE {}.singleton_candidate_leases SET expires_at = pg_catalog.clock_timestamp() + interval '30 seconds' WHERE incarnation = $1",
        fixture.schema
    ))
    .bind(old.identity().incarnation())
    .execute(&mut *heartbeat)
    .await
    .expect("hold renewed lease");
    let contender = fixture.store.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await
    });
    fixture
        .wait_for_row_waiter("singleton_candidate_leases")
        .await;
    heartbeat.commit().await.expect("commit renewal");
    assert_eq!(
        pending.await.expect("join contender").expect("result"),
        LeaseOutcome::Busy
    );
    fixture
        .store
        .guard(&old)
        .await
        .expect("old lease remains current")
        .release()
        .await
        .expect("release guard");
    fixture.cleanup().await;
}

#[tokio::test]
async fn successor_waits_for_a_writer_admitted_before_lease_expiry() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old = fixture.acquired(identity("node-a")).await;
    let guard = fixture
        .store
        .guard(&old)
        .await
        .expect("hold admitted writer");
    fixture.expire(&old).await;
    let contender = fixture.store.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await
    });
    fixture.wait_for_slot_waiter().await;
    assert!(
        !pending.is_finished(),
        "takeover cannot pass the old writer"
    );
    guard.release().await.expect("finish old writer");
    assert!(matches!(
        pending.await.expect("join takeover").expect("result"),
        LeaseOutcome::Acquired { .. }
    ));
    assert!(matches!(
        fixture.store.guard(&old).await,
        Err(SingletonLeaseError::NotCurrent)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn idle_guard_is_terminated_and_expired_slot_can_be_replaced() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old = fixture.acquired(identity("node-a")).await;
    let guard = fixture.store.guard(&old).await.expect("admit guard");
    let backend = guard.backend();
    assert!(fixture.guard_holds_slot(backend).await);
    fixture.expire(&old).await;

    let contender = fixture.store.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await
    });
    fixture.wait_for_slot_waiter().await;
    assert!(fixture.guard_holds_slot(backend).await);
    assert!(!pending.is_finished(), "takeover must wait for the guard");
    fixture.wait_for_guard_release(backend).await;
    assert!(matches!(
        pending
            .await
            .expect("join takeover")
            .expect("takeover result"),
        LeaseOutcome::Acquired { .. }
    ));
    assert!(matches!(
        guard.release().await,
        Err(SingletonLeaseError::GuardRollbackUnconfirmed { backend: observed })
            if observed == backend
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_guard_eventually_releases_its_exact_transaction() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old = fixture.acquired(identity("node-a")).await;
    let worker = fixture.store.clone();
    let worker_grant = old.clone();
    let (ready, received) = tokio::sync::oneshot::channel();
    let holder = tokio::spawn(async move {
        let guard = worker.guard(&worker_grant).await.expect("admit guard");
        ready.send(guard.backend()).expect("report backend");
        std::future::pending::<()>().await;
        drop(guard);
    });
    let backend = received.await.expect("guard backend");
    assert!(fixture.guard_holds_slot(backend).await);
    fixture.expire(&old).await;

    let contender = fixture.store.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await
    });
    fixture.wait_for_slot_waiter().await;
    assert!(fixture.guard_holds_slot(backend).await);
    assert!(!pending.is_finished(), "takeover must wait for the guard");
    holder.abort();
    assert!(holder.await.expect_err("holder cancelled").is_cancelled());
    fixture.wait_for_guard_release(backend).await;
    assert!(matches!(
        pending
            .await
            .expect("join takeover")
            .expect("takeover result"),
        LeaseOutcome::Acquired { .. }
    ));
    fixture.cleanup().await;
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn total_deadline_ends_a_guard_that_is_not_idle() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old = fixture.acquired(identity("node-a")).await;
    let mut guard = fixture.store.guard(&old).await.expect("admit guard");
    let backend = guard.backend();
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        guard.pulse_for_test().await.expect("guard remains active");
    }
    assert!(fixture.guard_holds_slot(backend).await);
    fixture.expire(&old).await;
    let contender = fixture.store.clone();
    let pending = tokio::spawn(async move {
        contender
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await
    });
    fixture.wait_for_slot_waiter().await;
    assert!(!pending.is_finished(), "takeover must wait for the guard");
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if guard.pulse_for_test().await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("total deadline stops the active guard");
    fixture.wait_for_guard_release(backend).await;
    assert!(matches!(
        pending
            .await
            .expect("join takeover")
            .expect("takeover result"),
        LeaseOutcome::Acquired { .. }
    ));
    assert!(matches!(
        guard.release().await,
        Err(SingletonLeaseError::GuardRollbackUnconfirmed { backend: observed })
            if observed == backend
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn missing_slot_or_referenced_lease_is_never_reconstructed() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let grant = fixture.acquired(identity("node-a")).await;
    let deleted = sqlx::query(&format!(
        "DELETE FROM {}.singleton_candidate_leases WHERE incarnation = $1",
        fixture.schema
    ))
    .bind(grant.identity().incarnation())
    .execute(&fixture.admin)
    .await;
    assert!(deleted.is_err(), "the occupied slot must retain its lease");
    sqlx::query(&format!(
        "DELETE FROM {}.singleton_candidate_slot WHERE slot = 1",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("simulate privileged slot loss");
    assert!(matches!(
        fixture.store.guard(&grant).await,
        Err(SingletonLeaseError::InvalidAuthority)
    ));
    assert_eq!(
        fixture
            .store
            .renew(Uuid::new_v4(), grant, TTL)
            .await
            .expect("missing-slot heartbeat"),
        LeaseOutcome::Lost
    );
    assert!(matches!(
        fixture
            .store
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await,
        Err(SingletonLeaseError::InvalidAuthority)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn expired_lease_cannot_revive_and_takeover_fences_old_grant() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old_identity = identity("same-node");
    let old = fixture.acquired(old_identity.clone()).await;
    fixture.expire(&old).await;
    assert_eq!(
        fixture
            .store
            .renew(Uuid::new_v4(), old.clone(), TTL)
            .await
            .expect("late renewal"),
        LeaseOutcome::Lost
    );
    assert!(matches!(
        fixture.store.guard(&old).await,
        Err(SingletonLeaseError::NotCurrent)
    ));
    assert_eq!(
        fixture
            .store
            .register(Uuid::new_v4(), old_identity, TTL)
            .await
            .expect("old incarnation"),
        LeaseOutcome::IncarnationReused
    );
    let successor = fixture.acquired(identity("same-node")).await;
    assert_eq!(successor.generation(), old.generation() + 1);
    assert_eq!(
        fixture
            .store
            .revoke(Uuid::new_v4(), old.clone())
            .await
            .expect("old revocation"),
        LeaseOutcome::NotCurrent
    );
    assert_eq!(
        fixture
            .store
            .renew(Uuid::new_v4(), old, TTL)
            .await
            .expect("old renewal"),
        LeaseOutcome::Lost
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn post_wait_heartbeat_rechecks_expiry_and_absent_authority_fails_closed() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let old = fixture.acquired(identity("node-a")).await;
    let mut blocker = fixture.admin.begin().await.expect("begin lease blocker");
    sqlx::query(&format!("UPDATE {}.singleton_candidate_leases SET expires_at = pg_catalog.clock_timestamp() - interval '1 second' WHERE incarnation = $1", fixture.schema))
        .bind(old.identity().incarnation()).execute(&mut *blocker).await.expect("hold expired lease");
    let worker = fixture.store.clone();
    let token = old.clone();
    let pending = tokio::spawn(async move { worker.renew(Uuid::new_v4(), token, TTL).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    blocker.commit().await.expect("commit expiry");
    assert_eq!(
        pending.await.expect("join heartbeat").expect("heartbeat"),
        LeaseOutcome::Lost
    );
    sqlx::query(&format!(
        "DELETE FROM {}.singleton_candidate_slot WHERE slot = 1",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("simulate missing slot");
    assert!(matches!(
        fixture
            .store
            .register(Uuid::new_v4(), identity("node-b"), TTL)
            .await,
        Err(SingletonLeaseError::InvalidAuthority)
    ));
    assert!(matches!(
        fixture.store.guard(&old).await,
        Err(SingletonLeaseError::InvalidAuthority)
    ));
    assert_eq!(
        fixture
            .store
            .renew(Uuid::new_v4(), old, TTL)
            .await
            .expect("missing-slot heartbeat"),
        LeaseOutcome::Lost
    );
    fixture.cleanup().await;
}

async fn hold_command_reply_proxy(
    listener: tokio::net::TcpListener,
    target: PgConnectOptions,
    command: &'static [u8],
    reply_held: Arc<tokio::sync::Notify>,
    disconnect: Arc<tokio::sync::Notify>,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (client, _) = listener.accept().await?;
    let server = postgres_proxy::connect_local_postgres(&target).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let hold = Arc::new(AtomicBool::new(false));
    let on_command = hold.clone();
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
            if scan.windows(command.len()).any(|part| part == command) {
                on_command.store(true, Ordering::SeqCst);
            }
            server_write.write_all(&buffer[..count]).await?;
            tail = scan[scan.len().saturating_sub(command.len() - 1)..].to_vec();
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
                disconnect.notified().await;
                return Ok(());
            }
            client_write.write_all(&buffer[..count]).await?;
        }
    };
    tokio::select! { result = upstream => result, result = downstream => result }
}

#[tokio::test]
async fn lost_rollback_reply_is_bounded_and_reports_exact_backend() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let grant = fixture.acquired(identity("node-a")).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let port = listener.local_addr().expect("proxy address").port();
    let held = Arc::new(tokio::sync::Notify::new());
    let disconnect = Arc::new(tokio::sync::Notify::new());
    let target = PgConnectOptions::from_str(&fixture.url).expect("target options");
    let proxy = tokio::spawn(hold_command_reply_proxy(
        listener,
        target,
        b"ROLLBACK\0",
        held.clone(),
        disconnect.clone(),
    ));
    let url = postgres_proxy::local_proxy_url(&fixture.url, port).expect("proxy URL");
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    config.pool_size = 1;
    let db = Database::from_config("rollback proxy", &config)
        .await
        .expect("proxied database");
    let proxied = SingletonLeaseStore::new_unverified(db, &fixture.schema).expect("proxied store");
    let guard = proxied.guard(&grant).await.expect("proxied guard");
    let backend = guard.backend();
    assert!(fixture.guard_holds_slot(backend).await);

    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(guard.release(), async {
            held.notified().await;
        })
    })
    .await
    .expect("bounded release and held response");
    assert!(matches!(
        result,
        Err(SingletonLeaseError::GuardRollbackUnconfirmed { backend: observed })
            if observed == backend
    ));
    // The proxy has seen the server's reply, but the caller has not. Observe
    // the actual transaction lock on the original primary before proceeding.
    fixture.wait_for_guard_release(backend).await;
    disconnect.notify_one();
    proxy.await.expect("join proxy").expect("proxy I/O");
    drop(proxied);
    fixture.cleanup().await;
}

#[tokio::test]
async fn lost_commit_reply_is_reconciled_by_exact_operation_readback() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let port = listener.local_addr().expect("proxy address").port();
    let held = Arc::new(tokio::sync::Notify::new());
    let disconnect = Arc::new(tokio::sync::Notify::new());
    let target = PgConnectOptions::from_str(&fixture.url).expect("target options");
    let proxy = tokio::spawn(hold_command_reply_proxy(
        listener,
        target,
        b"COMMIT\0",
        held.clone(),
        disconnect.clone(),
    ));
    let url = postgres_proxy::local_proxy_url(&fixture.url, port).expect("proxy URL");
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    config.pool_size = 1;
    let proxied_db = Database::from_config("commit proxy", &config)
        .await
        .expect("proxied database");
    let proxied =
        SingletonLeaseStore::new_unverified(proxied_db, &fixture.schema).expect("proxied store");
    let operation_id = Uuid::new_v4();
    let identity = identity("node-a");
    let request = LeaseRequest::Register {
        operation_id,
        identity: identity.clone(),
        ttl: TTL,
    };
    assert_eq!(
        fixture
            .store
            .readback(&request)
            .await
            .expect("initial readback"),
        LeaseReadback::Unconfirmed
    );
    let task = tokio::spawn(async move { proxied.register(operation_id, identity, TTL).await });
    tokio::time::timeout(Duration::from_secs(5), held.notified())
        .await
        .expect("hold commit reply");
    let committed = fixture
        .store
        .readback(&request)
        .await
        .expect("fresh readback");
    assert!(matches!(
        committed,
        LeaseReadback::Recorded(LeaseOutcome::Acquired { .. })
    ));
    disconnect.notify_one();
    proxy.await.expect("join proxy").expect("proxy I/O");
    assert!(
        matches!(task.await.expect("join request"), Err(SingletonLeaseError::CommitUnknown { operation_id: id }) if id == operation_id)
    );
    let LeaseReadback::Recorded(outcome) = committed else {
        panic!("receipt missing")
    };
    let LeaseRequest::Register { identity, .. } = request else {
        unreachable!()
    };
    assert_eq!(
        fixture
            .store
            .register(operation_id, identity, TTL)
            .await
            .expect("same operation retry"),
        outcome
    );
    fixture.cleanup().await;
}
