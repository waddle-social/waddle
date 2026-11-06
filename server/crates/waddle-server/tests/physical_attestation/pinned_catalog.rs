//! PG17 integration coverage for the pinned epoch and guard-manifest catalog.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, Row};
use waddle_server::db::physical_attestation::{
    pinned_attested_pool, AttestedPgPool, DarkPhysicalAttestationError, PinnedRuntimeAuthority,
};
use waddle_server::db::{
    DarkLedgerPreflightError, DarkMigrationLedgerPins, Database, DatabaseConfig, DatabaseDriver,
    MigrationRunner,
};

use super::{dark_attestor_fixture, test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK};

async fn pinned_pool(fixture: &Fixture, manifest_pin_override: Option<u32>) -> AttestedPgPool {
    let (config, attestor) = dark_attestor_fixture(fixture).await;
    let migration_url = url_with_schema(&fixture.base_url, &fixture.target);
    let database = Database::from_config(
        "pinned catalog migration fixture",
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
    .expect("record independent ledger, epoch, and manifest pins");
    let oid = |index| u32::try_from(pins.get::<i64, _>(index)).expect("OID fits u32");
    let ledger = DarkMigrationLedgerPins {
        schema_oid: oid(0),
        schema_owner_role_oid: oid(1),
        runtime_role_oid: oid(2),
        ledger_relation_oid: oid(3),
        ledger_primary_index_oid: oid(4),
    };
    let authority = PinnedRuntimeAuthority::new(
        &config,
        attestor,
        ledger,
        oid(5),
        oid(6),
        manifest_pin_override.unwrap_or_else(|| oid(7)),
    )
    .expect("authority from independent fixture pins");
    pinned_attested_pool(
        fixture.runtime_options.clone(),
        config,
        authority,
        1,
        Duration::from_secs(3),
    )
    .expect("pinned runtime pool")
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

async fn wait_for_entry_lock(fixture: &Fixture, pid: i32, relation_oid: u32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
                 WHERE pid = $1 AND locktype = 'relation' AND relation = $2 \
                 AND mode = 'AccessShareLock' AND NOT granted)",
            )
            .bind(pid)
            .bind(Oid(relation_oid))
            .fetch_one(&fixture.admin)
            .await
            .expect("observe relation lock wait");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("entry reaches relation lock wait");
}

async fn assert_waiting_relation_lock(fixture: &Fixture, pid: i32, relation_oid: u32) {
    let waiting: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND locktype = 'relation' AND relation = $2 \
         AND mode = 'AccessShareLock' AND NOT granted)",
    )
    .bind(pid)
    .bind(Oid(relation_oid))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe queued relation lock");
    assert!(
        waiting,
        "backend {pid} should be waiting for AccessShareLock on {relation_oid}"
    );
}

#[tokio::test]
async fn dark_pinned_catalog_locks_exact_epoch_and_manifest_on_one_backend_then_rolls_back() {
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
    .expect("entry reaches catalog barriers");
    assert_relation_lock(
        &fixture,
        pid,
        relation_oid(&fixture, "ingress_protocol_epoch").await,
    )
    .await;
    assert_relation_lock(
        &fixture,
        pid,
        relation_oid(&fixture, "ingress_epoch_guard_manifest").await,
    )
    .await;
    release_tx.send(()).expect("release entry observation");
    entry.await.expect("exact pinned catalog entry succeeds");

    let idle: bool = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid = $1",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("observe rollback completion before reacquisition");
    assert!(idle, "the exact observation transaction was rolled back");
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_cancellation_releases_epoch_and_manifest_barriers() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = std::sync::Arc::new(pinned_pool(&fixture, None).await);
    let epoch_oid = relation_oid(&fixture, "ingress_protocol_epoch").await;
    let manifest_oid = relation_oid(&fixture, "ingress_epoch_guard_manifest").await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut entry =
        Box::pin(pool.inspect_dark_pinned_read_only_entry_paused_for_test(ready_tx, release_rx));
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            ready = ready_rx => ready.expect("entry reports backend PID"),
            result = &mut entry => panic!("entry returned before pause: {result:?}"),
        }
    })
    .await
    .expect("entry reaches catalog barriers");
    assert_relation_lock(&fixture, pid, epoch_oid).await;
    assert_relation_lock(&fixture, pid, manifest_oid).await;
    drop(entry);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let cleared: Option<bool> = sqlx::query_scalar(
                "SELECT a.xact_start IS NULL AND NOT EXISTS ( \
                 SELECT 1 FROM pg_catalog.pg_locks l \
                 WHERE l.pid = $1 AND l.locktype = 'relation' \
                 AND l.relation IN ($2, $3) AND l.granted) \
                 FROM pg_catalog.pg_stat_activity a WHERE a.pid = $1",
            )
            .bind(pid)
            .bind(Oid(epoch_oid))
            .bind(Oid(manifest_oid))
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe cancelled backend cleanup");
            if cleared.unwrap_or(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled entry clears both catalog barriers");
    let mut connection = pool
        .acquire_for_test()
        .await
        .expect("reacquire session after catalog cancellation");
    let usable: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&mut *connection)
        .await
        .expect("reacquired session remains usable");
    assert_eq!(usable, 1);
    drop(connection);
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_independently_wrong_manifest_oid() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    // pg_catalog.pg_class has a distinct, nonzero pg_class OID. The lineage
    // relation is already independently pinned by the physical attestor and
    // therefore cannot serve as a syntactically valid wrong manifest pin.
    let wrong_manifest_oid: i64 = sqlx::query_scalar(
        "SELECT 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid::bigint",
    )
    .fetch_one(&fixture.admin)
    .await
    .expect("read stable distinct catalog relation OID");
    let wrong_manifest_oid = u32::try_from(wrong_manifest_oid).expect("OID fits u32");
    let pool = pinned_pool(&fixture, Some(wrong_manifest_oid)).await;
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_epoch_expression_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch ALTER COLUMN epoch SET DEFAULT 1",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("introduce epoch default-expression drift");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_epoch_row_security_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch ENABLE ROW LEVEL SECURITY",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("enable epoch row security");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_manifest_replacement_before_entry() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_epoch_guard_manifest RENAME TO pinned_manifest_original",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("rename independently pinned manifest");
    sqlx::query(&format!(
        "CREATE TABLE {}.ingress_epoch_guard_manifest (table_name text)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create same-name manifest replacement");
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_epoch_guard_manifest TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant runtime access to replacement");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rechecks_manifest_pin_after_lock_wait() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = std::sync::Arc::new(pinned_pool(&fixture, None).await);
    let mut runtime = pool.acquire_for_test().await.expect("runtime session");
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *runtime)
        .await
        .expect("record runtime backend");
    drop(runtime);

    let manifest_oid = relation_oid(&fixture, "ingress_epoch_guard_manifest").await;
    let mut owner = sqlx::PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("manifest replacement lock owner");
    let mut owner_tx = owner.begin().await.expect("begin replacement transaction");
    sqlx::query(&format!(
        "LOCK TABLE {}.ingress_epoch_guard_manifest IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("hold manifest replacement lock");

    let task_pool = std::sync::Arc::clone(&pool);
    let entry = tokio::spawn(async move { task_pool.inspect_dark_pinned_read_only_entry().await });
    wait_for_entry_lock(&fixture, pid, manifest_oid).await;
    for relation in ["_lineage", "_migrations", "ingress_protocol_epoch"] {
        assert_relation_lock(&fixture, pid, relation_oid(&fixture, relation).await).await;
    }
    assert_waiting_relation_lock(&fixture, pid, manifest_oid).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_epoch_guard_manifest RENAME TO pinned_manifest_original",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("rename pinned manifest while entry waits");
    sqlx::query(&format!(
        "CREATE TABLE {}.ingress_epoch_guard_manifest (table_name text)",
        fixture.target
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("install manifest replacement while entry waits");
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_epoch_guard_manifest TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&mut *owner_tx)
    .await
    .expect("grant runtime access to replacement");
    owner_tx.commit().await.expect("release manifest lock");
    assert!(matches!(
        entry.await.expect("join catalog entry"),
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_epoch_primary_index_shape_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch DROP CONSTRAINT ingress_protocol_epoch_pkey",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove independently pinned epoch primary index");
    sqlx::query(&format!(
        "CREATE UNIQUE INDEX ingress_protocol_epoch_wrong_idx ON {}.ingress_protocol_epoch (epoch)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("create a wrong-shape unique index");
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch ADD CONSTRAINT ingress_protocol_epoch_pkey PRIMARY KEY USING INDEX ingress_protocol_epoch_wrong_idx",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("install the wrong-shape index as the replacement primary key");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_same_shape_epoch_primary_index_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch DROP CONSTRAINT ingress_protocol_epoch_pkey",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("drop independently pinned epoch primary key");
    sqlx::query(&format!(
        "ALTER TABLE {}.ingress_protocol_epoch ADD CONSTRAINT ingress_protocol_epoch_pkey PRIMARY KEY (id)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("recreate the same primary-key shape with a new index OID");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_runtime_acl_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "REVOKE SELECT ON {}.ingress_epoch_guard_manifest FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke runtime manifest privilege");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_runtime_write_privilege_from_public() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "GRANT UPDATE ON {}.ingress_protocol_epoch TO PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant public epoch write privilege");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_runtime_select_grant_option() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_epoch_guard_manifest TO {} WITH GRANT OPTION",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant runtime manifest SELECT with grant option");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_public_select_substituting_runtime_grant() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "REVOKE SELECT ON {}.ingress_epoch_guard_manifest FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke direct runtime manifest SELECT");
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_epoch_guard_manifest TO PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant manifest SELECT to PUBLIC");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_pinned_catalog_rejects_public_select_with_direct_runtime_grant() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    // pinned_pool provisions direct SELECT to runtime; retain it while adding
    // the PUBLIC grant to isolate the PUBLIC ACL rejection rule.
    sqlx::query(&format!(
        "GRANT SELECT ON {}.ingress_epoch_guard_manifest TO PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant manifest SELECT to PUBLIC alongside runtime SELECT");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(waddle_server::db::physical_attestation::DarkPinnedEntryError::Catalog(_))
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn dark_attestor_rejects_excess_lineage_constraints_before_attestor() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, pins) = dark_attestor_fixture(&fixture).await;

    for index in 1..=3 {
        sqlx::query(&format!(
            "ALTER TABLE {}._lineage ADD CONSTRAINT lineage_inventory_extra_{index} CHECK (id = 1)",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("add a valid extra lineage check constraint");
    }

    assert!(matches!(
        super::dark_attestor_attempt(&fixture, &config, pins).await,
        Err(DarkPhysicalAttestationError::EarlyCatalogDrift)
    ));
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_checkout_rejects_public_migration_ledger_system_column_select() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let pool = pinned_pool(&fixture, None).await;
    sqlx::query(&format!(
        "GRANT SELECT (tableoid) ON {}._migrations TO PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant PUBLIC SELECT on the migration ledger system column");
    assert!(matches!(
        pool.inspect_dark_pinned_read_only_entry().await,
        Err(
            waddle_server::db::physical_attestation::DarkPinnedEntryError::Ledger(
                DarkLedgerPreflightError::CatalogDrift
            )
        )
    ));
    pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn pinned_checkout_rejects_public_lineage_system_column_select() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, pins) = dark_attestor_fixture(&fixture).await;
    sqlx::query(&format!(
        "GRANT SELECT (tableoid) ON {}._lineage TO PUBLIC",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant PUBLIC SELECT on the lineage system column");
    assert!(matches!(
        super::dark_attestor_attempt(&fixture, &config, pins).await,
        Err(DarkPhysicalAttestationError::CatalogDrift)
    ));
    fixture.cleanup().await;
}
