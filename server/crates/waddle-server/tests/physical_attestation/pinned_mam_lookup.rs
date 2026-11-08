//! PostgreSQL 17 witnesses for the closed, pinned MAM lookup.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection, Row};
use waddle_server::db::physical_attestation::{
    pinned_attested_pool, AttestedPgPool, PinnedMamLookupAuthority, PinnedMamLookupError,
    PinnedMamLookupPins, PinnedRuntimeAuthority,
};
use waddle_server::db::{
    DarkMigrationLedgerPins, Database, DatabaseConfig, DatabaseDriver, MigrationRunner,
};
use waddle_xmpp::mam::SqlxMamStorage;
use waddle_xmpp_core::xep0359::StanzaId;

use super::{dark_attestor_fixture, test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK};

const INDEXES: [&str; 9] = [
    "mam_messages_pkey",
    "idx_mam_room_timestamp",
    "idx_mam_room_sender",
    "idx_mam_room_id",
    "idx_mam_room_thread",
    "idx_mam_room_reply_to",
    "idx_mam_room_origin",
    "idx_mam_room_stanza",
    "idx_mam_room_archive_seq",
];

fn stanza(value: &str) -> StanzaId {
    StanzaId::new(value, "room@example.test".parse().expect("archive JID"))
}

async fn oid(fixture: &Fixture, name: &str) -> u32 {
    let value: i64 = sqlx::query_scalar(
        "SELECT c.oid::bigint FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         WHERE n.nspname=$1 AND c.relname=$2",
    )
    .bind(&fixture.target)
    .bind(name)
    .fetch_one(&fixture.admin)
    .await
    .expect("fixture relation exists");
    u32::try_from(value).expect("OID fits u32")
}

async fn authority(fixture: &Fixture, lock_timeout: Option<&str>) -> PinnedMamLookupAuthority {
    let (config, attestor) = dark_attestor_fixture(fixture).await;
    let url = url_with_schema(&fixture.base_url, &fixture.target);
    let database = Database::from_config(
        "pinned MAM fixture migration",
        &DatabaseConfig::new(DatabaseDriver::Postgres, &url),
    )
    .await
    .expect("migration database");
    MigrationRunner::single()
        .run(&database)
        .await
        .expect("migrate fixture");
    drop(database);
    let mam = SqlxMamStorage::open(&url)
        .await
        .expect("bootstrap exact MAM schema");
    sqlx::query(&format!(
        "INSERT INTO {}.mam_messages \
         (id,room_jid,timestamp,from_jid,to_jid,stanza_id,archive_seq) \
         VALUES ('archive-1','room@example.test',now(),'alice@example.test', \
                 'room@example.test','wire-1',1)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed archive");
    drop(mam);
    for table in [
        "_migrations",
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
        "mam_messages",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{} TO {}",
            fixture.target, table, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant fixture SELECT");
    }
    let schema = sqlx::query(
        "SELECT n.oid::bigint, n.nspowner::bigint, r.oid::bigint \
         FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_roles r ON r.rolname=$2 \
         WHERE n.nspname=$1",
    )
    .bind(&fixture.target)
    .bind(&fixture.runtime)
    .fetch_one(&fixture.admin)
    .await
    .expect("independently record schema pins");
    let schema_oid = u32::try_from(schema.get::<i64, _>(0)).expect("schema OID");
    let owner_oid = u32::try_from(schema.get::<i64, _>(1)).expect("owner OID");
    let runtime_oid = u32::try_from(schema.get::<i64, _>(2)).expect("runtime OID");
    let ledger = DarkMigrationLedgerPins {
        schema_oid,
        schema_owner_role_oid: owner_oid,
        runtime_role_oid: runtime_oid,
        ledger_relation_oid: oid(fixture, "_migrations").await,
        ledger_primary_index_oid: oid(fixture, "_migrations_pkey").await,
    };
    let runtime = PinnedRuntimeAuthority::new(
        &config,
        attestor,
        ledger,
        oid(fixture, "ingress_protocol_epoch").await,
        oid(fixture, "ingress_protocol_epoch_pkey").await,
        oid(fixture, "ingress_epoch_guard_manifest").await,
    )
    .expect("runtime pins");
    let mut index_oids = [0; 9];
    for (index, name) in INDEXES.into_iter().enumerate() {
        index_oids[index] = oid(fixture, name).await;
    }
    let pins = PinnedMamLookupPins {
        messages_relation_oid: oid(fixture, "mam_messages").await,
        index_oids,
    };
    let mut options = fixture.runtime_options.clone();
    if let Some(timeout) = lock_timeout {
        options = options.options([("lock_timeout", timeout)]);
    }
    let pool: AttestedPgPool =
        pinned_attested_pool(options, config, runtime, 1, Duration::from_secs(3))
            .expect("pinned pool");
    PinnedMamLookupAuthority::new(pool, pins).expect("bound MAM lookup")
}

async fn waiting_pid(fixture: &Fixture, relation: u32) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_locks WHERE locktype='relation' \
                 AND relation=$1 AND mode='AccessShareLock' AND NOT granted LIMIT 1",
            )
            .bind(Oid(relation))
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe waiter");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("lookup reaches relation lock")
}

async fn holds(fixture: &Fixture, pid: i32, relation: u32) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
         AND locktype='relation' AND relation=$2 AND mode='AccessShareLock' AND granted)",
    )
    .bind(pid)
    .bind(Oid(relation))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe backend locks")
}

async fn assert_rolled_back(fixture: &Fixture, pid: i32) {
    let idle: Option<bool> = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_optional(&fixture.admin)
    .await
    .expect("observe backend transaction");
    assert_eq!(idle, Some(true), "same backend must acknowledge rollback");
}

#[tokio::test]
async fn lookup_holds_ordered_barriers_on_one_read_only_backend() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    let lineage = oid(&fixture, "_lineage").await;
    let ledger = oid(&fixture, "_migrations").await;
    let epoch = oid(&fixture, "ingress_protocol_epoch").await;
    let manifest = oid(&fixture, "ingress_epoch_guard_manifest").await;
    let mam = oid(&fixture, "mam_messages").await;
    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("owner");
    let mut guard = owner.begin().await.expect("begin owner transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._migrations IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *guard)
    .await
    .expect("hold ledger");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let job = lookup
        .start_lookup_paused_for_test(stanza("wire-1"), ready_tx, release_rx)
        .expect("start owned lookup");
    let pid = waiting_pid(&fixture, ledger).await;
    assert!(holds(&fixture, pid, lineage).await);
    for later in [epoch, manifest, mam] {
        assert!(
            !holds(&fixture, pid, later).await,
            "later relation locked before ledger"
        );
    }
    guard.rollback().await.expect("release ledger");
    let (observed_pid, read_only, read_committed) = ready_rx.await.expect("observation");
    assert_eq!(pid, observed_pid);
    assert!(read_only && read_committed);
    for relation in [lineage, ledger, epoch, manifest, mam] {
        assert!(holds(&fixture, pid, relation).await);
    }
    release_tx.send(()).expect("release lookup");
    let result = job
        .finish()
        .await
        .expect("lookup succeeds")
        .expect("message found");
    assert_eq!(result.id, "archive-1");
    assert_rolled_back(&fixture, pid).await;
    let by_archive = lookup
        .start_lookup(stanza("archive-1"))
        .expect("start archive lookup")
        .finish()
        .await
        .expect("archive lookup succeeds")
        .expect("archive found");
    assert_eq!(by_archive.id, "archive-1");
    let miss = lookup
        .start_lookup(stanza("absent"))
        .expect("start miss")
        .finish()
        .await
        .expect("acknowledged miss");
    assert!(miss.is_none());
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_receiver_keeps_backend_and_permit_until_rollback() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let job = lookup
        .start_lookup_paused_for_test(stanza("wire-1"), ready_tx, release_rx)
        .expect("start lookup");
    let (pid, _, _) = ready_rx.await.expect("observe backend");
    drop(job);
    assert!(matches!(
        lookup.start_lookup(stanza("wire-1")),
        Err(PinnedMamLookupError::Overloaded)
    ));
    assert!(holds(&fixture, pid, oid(&fixture, "mam_messages").await).await);
    release_tx.send(()).expect("release detached worker");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let idle: Option<bool> = sqlx::query_scalar(
                "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
            )
            .bind(pid)
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe backend");
            if idle == Some(true) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("detached worker rolled back its original backend");
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match lookup.start_lookup(stanza("wire-1")) {
                Ok(job) => break job,
                Err(PinnedMamLookupError::Overloaded) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("unexpected admission error: {error}"),
            }
        }
    })
    .await
    .expect("permit released only after cleanup");
    assert!(next.finish().await.expect("next lookup succeeds").is_some());
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn lookup_rechecks_index_pin_after_wait_and_rejects_replacement() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    let mam = oid(&fixture, "mam_messages").await;
    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("owner");
    let mut guard = owner.begin().await.expect("owner transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.mam_messages IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *guard)
    .await
    .expect("hold MAM heap");
    let job = lookup.start_lookup(stanza("wire-1")).expect("start lookup");
    let pid = waiting_pid(&fixture, mam).await;
    sqlx::query(&format!(
        "DROP INDEX {}.idx_mam_room_stanza",
        fixture.target
    ))
    .execute(&mut *guard)
    .await
    .expect("drop pinned index");
    sqlx::query(&format!(
        "CREATE INDEX idx_mam_room_stanza ON {}.mam_messages(room_jid, stanza_id)",
        fixture.target
    ))
    .execute(&mut *guard)
    .await
    .expect("replace with same-shape index");
    guard.commit().await.expect("publish replacement");
    assert!(matches!(
        job.finish().await,
        Err(PinnedMamLookupError::CatalogDrift)
    ));
    assert_rolled_back(&fixture, pid).await;
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn lookup_times_out_behind_mam_lock_and_rolls_back() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, Some("500ms")).await;
    let mam = oid(&fixture, "mam_messages").await;
    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("owner");
    let mut guard = owner.begin().await.expect("owner transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.mam_messages IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *guard)
    .await
    .expect("hold MAM heap");
    let job = lookup.start_lookup(stanza("wire-1")).expect("start lookup");
    let pid = waiting_pid(&fixture, mam).await;
    assert!(matches!(
        job.finish().await,
        Err(PinnedMamLookupError::Postgres(_))
    ));
    assert_rolled_back(&fixture, pid).await;
    guard.rollback().await.expect("release MAM heap");
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn lookup_refuses_oversized_compressed_selected_row_without_false_miss() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    // The later ordinal must win before the size decision: it must not fall
    // back to the smaller earlier row or turn oversize into a miss.
    sqlx::query(&format!(
        "INSERT INTO {}.mam_messages \
         (id,room_jid,timestamp,from_jid,to_jid,stanza_id,stanza_xml,archive_seq) \
         VALUES ('archive-2','room@example.test',now(),'alice@example.test', \
                 'room@example.test','wire-1',pg_catalog.repeat('x',17*1024*1024),2)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("insert compressible oversized row");
    let (logical, stored): (i32, i32) = sqlx::query_as(&format!(
        "SELECT pg_catalog.octet_length(stanza_xml), pg_catalog.pg_column_size(stanza_xml) \
         FROM {}.mam_messages WHERE id='archive-2'",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe compressed text");
    assert!(logical > 16 * 1024 * 1024 && stored < 1024 * 1024);
    assert!(matches!(
        lookup
            .start_lookup(stanza("wire-1"))
            .expect("start")
            .finish()
            .await,
        Err(PinnedMamLookupError::Mam(
            waddle_xmpp::mam::storage::BoundedMamLookupError::RowTooLarge
        ))
    ));
    let huge_key = "x".repeat(1024 * 1024 + 1);
    assert!(matches!(
        lookup
            .start_lookup(stanza(&huge_key))
            .expect("start")
            .finish()
            .await,
        Err(PinnedMamLookupError::Mam(
            waddle_xmpp::mam::storage::BoundedMamLookupError::KeyTooLarge
        ))
    ));
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn lookup_rejects_public_and_runtime_write_acl_or_unpinned_index() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    for (grant, revoke) in [
        (
            format!("GRANT SELECT ON {}.mam_messages TO PUBLIC", fixture.target),
            format!(
                "REVOKE SELECT ON {}.mam_messages FROM PUBLIC",
                fixture.target
            ),
        ),
        (
            format!(
                "GRANT INSERT ON {}.mam_messages TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE INSERT ON {}.mam_messages FROM {}",
                fixture.target, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT INSERT (id) ON {}.mam_messages TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE INSERT (id) ON {}.mam_messages FROM {}",
                fixture.target, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT UPDATE (id) ON {}.mam_messages TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE UPDATE (id) ON {}.mam_messages FROM {}",
                fixture.target, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT SELECT ON {}.mam_messages TO {} WITH GRANT OPTION",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE GRANT OPTION FOR SELECT ON {}.mam_messages FROM {}",
                fixture.target, fixture.runtime
            ),
        ),
        (
            format!(
                "CREATE INDEX extra_mam_lookup ON {}.mam_messages ((pg_catalog.lower(id)))",
                fixture.target
            ),
            format!("DROP INDEX {}.extra_mam_lookup", fixture.target),
        ),
    ] {
        sqlx::query(&grant)
            .execute(&fixture.admin)
            .await
            .expect("introduce drift");
        assert!(matches!(
            lookup
                .start_lookup(stanza("wire-1"))
                .expect("start")
                .finish()
                .await,
            Err(PinnedMamLookupError::CatalogDrift)
        ));
        sqlx::query(&revoke)
            .execute(&fixture.admin)
            .await
            .expect("restore catalog");
    }
    assert!(lookup
        .start_lookup(stanza("wire-1"))
        .expect("start")
        .finish()
        .await
        .expect("restored")
        .is_some());
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn lookup_rejects_replacement_view_before_volatile_expression_runs() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    sqlx::raw_sql(&format!(
        "ALTER TABLE {}.mam_messages RENAME TO mam_original; \
         CREATE VIEW {}.mam_messages AS SELECT \
         CASE WHEN pg_catalog.pg_advisory_lock(987654321) IS NULL \
              THEN id ELSE id || 'x' END AS id, \
         room_jid,timestamp,from_jid,to_jid,body,stanza_id,thread_id,reply_to_id, \
         reply_to_jid,origin_id,message_type,stanza_xml,rich_payload,nickname_generation, \
         parent_thread_id,archive_seq,created_at FROM {}.mam_original; \
         GRANT SELECT ON {}.mam_messages TO {}",
        fixture.target, fixture.target, fixture.target, fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("install hostile replacement view");
    assert!(matches!(
        lookup
            .start_lookup(stanza("wire-1"))
            .expect("start")
            .finish()
            .await,
        Err(PinnedMamLookupError::CatalogDrift)
    ));
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' \
         AND objsubid=1 AND ((classid::pg_catalog.int8 << 32) | objid::pg_catalog.int8)=987654321 AND granted)",
    ).fetch_one(&fixture.admin).await.expect("inspect volatile side effect");
    assert!(!held, "replacement view expression must never execute");
    drop(lookup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn bounded_lookup_uses_selective_indexes_at_archive_scale() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let lookup = authority(&fixture, None).await;
    sqlx::query(&format!(
        "INSERT INTO {}.mam_messages \
         (id,room_jid,timestamp,from_jid,to_jid,stanza_id,archive_seq) \
         SELECT 'archive-bulk-' || n,'room@example.test',now(),'alice@example.test', \
                'room@example.test','wire-bulk-' || n,n+1 \
         FROM pg_catalog.generate_series(1,100000) AS n",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed realistic archive cardinality");
    sqlx::query(&format!("ANALYZE {}.mam_messages", fixture.target))
        .execute(&fixture.admin)
        .await
        .expect("refresh archive statistics");

    let key = stanza("wire-bulk-50000");
    let found = lookup
        .start_lookup(key.clone())
        .expect("start pinned lookup")
        .finish()
        .await
        .expect("lookup succeeds")
        .expect("lookup finds row");
    let missing_key = stanza("wire-bulk-not-present");
    let missing_result = lookup
        .start_lookup(missing_key.clone())
        .expect("start missing lookup")
        .finish()
        .await
        .expect("missing lookup succeeds");

    // Inspect the exact production query, with its actual parameter values,
    // as the restricted runtime role and without disabling sequential scans.
    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime connection for EXPLAIN");
    let mut tx = runtime.begin().await.expect("explain transaction");
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY")
        .execute(&mut *tx)
        .await
        .expect("read-only explain");
    let mut plans = Vec::new();
    for (label, key) in [("existing", &key), ("missing", &missing_key)] {
        let plan = SqlxMamStorage::explain_bounded_lookup_for_test(&mut tx, &fixture.target, key)
            .await
            .expect("explain exact bounded query")
            .join("\n");
        plans.push((label, plan));
    }
    let generic = SqlxMamStorage::explain_generic_bounded_lookup_for_test(&mut tx, &fixture.target)
        .await
        .expect("explain generic bounded query")
        .join("\n");
    plans.push(("generic", generic));
    tx.rollback().await.expect("finish explain");
    drop(runtime);
    drop(lookup);
    fixture.cleanup().await;
    assert_eq!(found.id, "archive-bulk-50000");
    assert!(missing_result.is_none());
    for (label, plan) in plans {
        assert_selective_plan(label, &plan);
    }
}

fn assert_selective_plan(label: &str, plan: &str) {
    assert!(
        plan.contains("BitmapOr")
            && (plan.contains("mam_messages_pkey") || plan.contains("idx_mam_room_id"))
            && plan.contains("idx_mam_room_stanza"),
        "{label} query must use selective ID and stanza indexes:\n{plan}"
    );
    assert!(
        !plan.contains("Seq Scan on mam_messages"),
        "{label} query must not scan the entire archive:\n{plan}"
    );
}
