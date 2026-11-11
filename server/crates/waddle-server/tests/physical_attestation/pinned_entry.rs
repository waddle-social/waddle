//! PG17 integration coverage for the dark pinned read-only entry observation.

use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection, Row};
use waddle_server::db::physical_attestation::{
    pinned_attested_pool, AttestedPgPool, DarkPinnedEntryError, PinnedCatalogError,
    PinnedRuntimeAuthority,
};
use waddle_server::db::{
    DarkMigrationLedgerPins, Database, DatabaseConfig, DatabaseDriver, MigrationRunner,
};

use super::{dark_attestor_fixture, test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK};

pub(super) async fn pinned_pool(fixture: &Fixture, lock_timeout: Option<&str>) -> AttestedPgPool {
    let (config, attestor) = dark_attestor_fixture(fixture).await;
    let migration_url = url_with_schema(&fixture.base_url, &fixture.target);
    let database = Database::from_config(
        "pinned entry migration fixture",
        &DatabaseConfig::new(DatabaseDriver::Postgres, migration_url),
    )
    .await
    .expect("migration database");
    MigrationRunner::single()
        .run(&database)
        .await
        .expect("apply migrations through V1030");
    drop(database);
    for relation in [
        "_migrations",
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{} TO {}",
            fixture.target, relation, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("provision exact runtime read privilege");
    }

    let pins = sqlx::query(
        "SELECT n.oid::bigint, n.nspowner::bigint, runtime.oid::bigint, \
                ledger.oid::bigint, ledger_idx.indexrelid::bigint, \
                epoch.oid::bigint, epoch_idx.indexrelid::bigint, manifest.oid::bigint \
         FROM pg_catalog.pg_namespace n \
         JOIN pg_catalog.pg_roles runtime ON runtime.rolname = $2 \
         JOIN pg_catalog.pg_class ledger ON ledger.relnamespace = n.oid AND ledger.relname = '_migrations' \
         JOIN pg_catalog.pg_index ledger_idx ON ledger_idx.indrelid = ledger.oid AND ledger_idx.indisprimary \
         JOIN pg_catalog.pg_class epoch ON epoch.relnamespace = n.oid AND epoch.relname = 'ingress_protocol_epoch' \
         JOIN pg_catalog.pg_index epoch_idx ON epoch_idx.indrelid = epoch.oid AND epoch_idx.indisprimary \
         JOIN pg_catalog.pg_class manifest ON manifest.relnamespace = n.oid AND manifest.relname = 'ingress_epoch_guard_manifest' \
         WHERE n.nspname = $1",
    )
    .bind(&fixture.target)
    .bind(&fixture.runtime)
    .fetch_one(&fixture.admin)
    .await
    .expect("record independent ledger and epoch pins");
    let oid = |index| u32::try_from(pins.get::<i64, _>(index)).expect("OID fits u32");
    let ledger = DarkMigrationLedgerPins {
        schema_oid: oid(0),
        schema_owner_role_oid: oid(1),
        runtime_role_oid: oid(2),
        ledger_relation_oid: oid(3),
        ledger_primary_index_oid: oid(4),
    };
    let authority = PinnedRuntimeAuthority::new(&config, attestor, ledger, oid(5), oid(6), oid(7))
        .expect("authority from independent fixture pins");
    let mut runtime_options = fixture.runtime_options.clone();
    if let Some(lock_timeout) = lock_timeout {
        runtime_options = runtime_options.options([("lock_timeout", lock_timeout)]);
    }
    pinned_attested_pool(
        runtime_options,
        config,
        authority,
        1,
        Duration::from_secs(3),
    )
    .expect("pinned runtime pool")
}

async fn assert_relation_lock(fixture: &Fixture, pid: i32, relation_oid: u32) {
    let locked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND locktype = 'relation' AND relation = $2 \
         AND mode = 'AccessShareLock' AND granted)",
    )
    .bind(pid)
    .bind(Oid(relation_oid))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe relation lock");
    assert!(
        locked,
        "backend {pid} should hold AccessShareLock on {relation_oid}"
    );
}

async fn relation_oid(fixture: &Fixture, relation: &str) -> u32 {
    let oid: i64 = sqlx::query_scalar(
        "SELECT c.oid::bigint FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2",
    )
    .bind(&fixture.target)
    .bind(relation)
    .fetch_one(&fixture.admin)
    .await
    .expect("relation exists");
    u32::try_from(oid).expect("OID fits u32")
}

#[tokio::test]
async fn dark_pinned_entry_holds_exact_read_only_barriers_and_rolls_back() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let entry = pool.inspect_dark_pinned_read_only_entry_paused_for_test(ready_tx, release_rx);
    tokio::pin!(entry);
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            ready = ready_rx => ready.expect("entry reports backend PID"),
            result = &mut entry => panic!("entry returned before pause: {result:?}"),
        }
    })
    .await
    .expect("entry reaches all barriers");
    assert_relation_lock(&fixture, pid, relation_oid(&fixture, "_migrations").await).await;
    assert_relation_lock(&fixture, pid, relation_oid(&fixture, "_lineage").await).await;
    assert_relation_lock(
        &fixture,
        pid,
        relation_oid(&fixture, "ingress_protocol_epoch").await,
    )
    .await;
    release_tx.send(()).expect("release entry observation");
    entry.await.expect("exact pinned entry succeeds");

    // Inspect from the administrator before another checkout runs its own
    // unconditional ROLLBACK hook; this is the entry's rollback witness.
    let idle: bool = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid = $1",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("observe rollback completion before reacquisition");
    assert!(idle, "the exact observation transaction was rolled back");

    let mut connection = pool
        .acquire_for_test()
        .await
        .expect("reacquire pool session");
    let pid_after: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .expect("read reused backend PID");
    assert_eq!(
        pid_after, pid,
        "single-connection pool reuses the observed session"
    );
    drop(connection);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_entry_cancellation_releases_exact_transaction() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut entry =
        Box::pin(pool.inspect_dark_pinned_read_only_entry_paused_for_test(ready_tx, release_rx));
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            ready = ready_rx => ready.expect("entry reports backend PID"),
            result = &mut entry => panic!("entry returned before pause: {result:?}"),
        }
    })
    .await
    .expect("entry reaches all barriers");
    let lineage_oid = relation_oid(&fixture, "_lineage").await;
    let ledger_oid = relation_oid(&fixture, "_migrations").await;
    let epoch_oid = relation_oid(&fixture, "ingress_protocol_epoch").await;
    for oid in [lineage_oid, ledger_oid, epoch_oid] {
        assert_relation_lock(&fixture, pid, oid).await;
    }
    drop(entry);
    drop(release_tx);

    // No checkout hook has run yet. PgLease must eventually roll back or
    // discard the cancelled backend and release each transaction barrier.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cleared: Option<bool> = sqlx::query_scalar(
                "SELECT a.xact_start IS NULL AND NOT EXISTS ( \
                 SELECT 1 FROM pg_catalog.pg_locks l \
                 WHERE l.pid = $1 AND l.locktype = 'relation' \
                 AND l.relation IN ($2, $3, $4) AND l.granted) \
                 FROM pg_catalog.pg_stat_activity a WHERE a.pid = $1",
            )
            .bind(pid)
            .bind(Oid(lineage_oid))
            .bind(Oid(ledger_oid))
            .bind(Oid(epoch_oid))
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe cancelled backend");
            if cleared.unwrap_or(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled entry clears its server transaction");
    let mut connection = pool
        .acquire_for_test()
        .await
        .expect("reacquire pool after cancellation");
    let usable: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut *connection)
        .await
        .expect("reacquired checkout remains usable");
    assert_eq!(usable, 1);
    drop(connection);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_entry_rejects_epoch_relation_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch RENAME TO pinned_epoch_original",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("rename independently pinned epoch heap");
    sqlx::query(&format!(
        "CREATE TABLE {}.ingress_protocol_epoch (id integer PRIMARY KEY, epoch bigint NOT NULL)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create replacement epoch heap");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_entry_times_out_on_held_epoch_lock_and_rolls_back() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = Arc::new(pinned_pool(&fixture, Some("500ms")).await);
    let epoch_oid = relation_oid(&fixture, "ingress_protocol_epoch").await;

    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("epoch lock owner");
    let mut owner_tx = owner.begin().await.expect("begin lock transaction");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("hold epoch replacement lock");
    let task_pool = Arc::clone(&pool);
    let entry = tokio::spawn(async move { task_pool.inspect_dark_pinned_read_only_entry().await });
    let pid = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_locks WHERE locktype = 'relation' \
                 AND relation = $1 AND mode = 'AccessShareLock' AND NOT granted LIMIT 1",
            )
            .bind(Oid(epoch_oid))
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe actual entry lock wait");
            if let Some(pid) = waiting {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("entry waits on pinned epoch heap");
    assert!(matches!(
        entry.await.expect("join timed-out entry"),
        Err(DarkPinnedEntryError::Catalog(PinnedCatalogError::Postgres(
            _
        )))
    ));

    // Observe the exact waiting backend before a new checkout's hook can
    // sanitize it. A discarded backend is also a valid failure outcome.
    let cleared: Option<bool> = sqlx::query_scalar(
        "SELECT a.xact_start IS NULL AND NOT EXISTS ( \
         SELECT 1 FROM pg_catalog.pg_locks l \
         WHERE l.pid = $1 AND l.locktype = 'relation' AND l.relation = $2 AND l.granted) \
         FROM pg_catalog.pg_stat_activity a WHERE a.pid = $1",
    )
    .bind(pid)
    .bind(Oid(epoch_oid))
    .fetch_optional(&fixture.admin)
    .await
    .expect("observe transaction and relation-lock cleanup before reacquisition");
    assert!(
        cleared.unwrap_or(true),
        "failed entry retained a transaction or epoch lock"
    );

    let mut runtime = pool
        .acquire_for_test()
        .await
        .expect("reacquire session after failed entry");
    let usable: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut *runtime)
        .await
        .expect("reacquired checkout remains usable");
    assert_eq!(usable, 1);
    drop(runtime);
    owner_tx.rollback().await.expect("release held epoch lock");
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_entry_rechecks_epoch_pin_after_lock_wait() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = Arc::new(pinned_pool(&fixture, None).await);
    let mut runtime = pool.acquire_for_test().await.expect("runtime session");
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *runtime)
        .await
        .expect("record runtime backend");
    drop(runtime);

    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("epoch lock owner");
    let mut owner_tx = owner.begin().await.expect("begin lock transaction");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("hold epoch relation lock");

    let task_pool = Arc::clone(&pool);
    let entry = tokio::spawn(async move { task_pool.inspect_dark_pinned_read_only_entry().await });
    let epoch_oid = relation_oid(&fixture, "ingress_protocol_epoch").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
                 WHERE pid = $1 AND locktype = 'relation' AND relation = $2 \
                 AND mode = 'AccessShareLock' AND NOT granted)",
            )
            .bind(pid)
            .bind(Oid(epoch_oid))
            .fetch_one(&fixture.admin)
            .await
            .expect("observe entry lock wait");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("entry reaches relation lock wait");

    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch RENAME TO pinned_epoch_original",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("replace pinned name while entry waits");
    sqlx::query(&format!(
        "CREATE TABLE {}.ingress_protocol_epoch (id integer PRIMARY KEY, epoch bigint NOT NULL)",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("create replacement epoch heap");
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_protocol_epoch TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("provision replacement relation lock privilege");
    owner_tx.commit().await.expect("release relation lock");
    let observed = entry.await.expect("join entry task");
    assert!(
        matches!(observed, Err(DarkPinnedEntryError::Catalog(_))),
        "post-wait pin recheck should reject replacement: {observed:?}"
    );
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_entry_rejects_missing_registered_ledger_row() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    let deleted = sqlx::query(&format!(
        "DELETE FROM {}._migrations WHERE version = 1030",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("introduce V1030 ledger drift");
    assert_eq!(
        deleted.rows_affected(),
        1,
        "V1030 row is present before drift"
    );
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(DarkPinnedEntryError::Ledger(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

/// The two entry stages require different runtime ACLs on the epoch
/// relation, and each refuses the other's. The read-only stage admits
/// SELECT alone; the startup stage requires SELECT plus exactly the
/// column-level `UPDATE (id)` the registered guard's `FOR SHARE` read needs.
#[tokio::test]
async fn dark_pinned_entry_stages_require_their_exact_epoch_grant() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(base_url) = test_url() else { return };
    let fixture = Fixture::new(base_url).await;
    let pool = pinned_pool(&fixture, None).await;
    let epoch = format!("{}.ingress_protocol_epoch", fixture.target);
    let drift = |result: Result<(), DarkPinnedEntryError>| {
        matches!(
            result,
            Err(DarkPinnedEntryError::Catalog(
                PinnedCatalogError::CatalogDrift
            ))
        )
    };

    pool.inspect_dark_pinned_read_only_entry()
        .await
        .expect("the read-only stage accepts SELECT alone");
    assert!(
        drift(pool.inspect_dark_pinned_startup_entry().await),
        "the startup stage must refuse SELECT alone"
    );

    sqlx::query(&format!(
        "GRANT UPDATE (id) ON {epoch} TO {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant the serving epoch lock privilege");
    pool.inspect_dark_pinned_startup_entry()
        .await
        .expect("the startup stage accepts the serving grant");
    assert!(
        drift(pool.inspect_dark_pinned_read_only_entry().await),
        "the read-only stage must refuse a column update grant"
    );

    for widening in [
        format!("GRANT UPDATE (epoch) ON {epoch} TO {}", fixture.runtime),
        format!("GRANT UPDATE ON {epoch} TO {}", fixture.runtime),
    ] {
        sqlx::query(&widening)
            .execute(&fixture.admin)
            .await
            .expect("widen the epoch grant");
        assert!(
            drift(pool.inspect_dark_pinned_startup_entry().await),
            "the startup stage must refuse: {widening}"
        );
        sqlx::query(
            &widening
                .replacen("GRANT", "REVOKE", 1)
                .replace(" TO ", " FROM "),
        )
        .execute(&fixture.admin)
        .await
        .expect("undo the widened epoch grant");
    }

    pool.close().await;
    fixture.cleanup().await;
}
