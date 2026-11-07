//! Physical PostgreSQL witnesses for the Pending Delivery catalog edge.

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    Duration, Fixture, CATALOG_GRANT_LOCK,
};
use waddle_server::db::physical_attestation::CATALOG_MUTATION_ADVISORY_LOCK_KEY;
use waddle_server::pending_delivery::{
    bootstrap_pending_delivery_owner_schema, DatabasePendingDeliveryStorage,
};
use waddle_xmpp::pending_delivery::QuotaPolicy;

const RECEIPT_KEY: i64 = 7_305_784_319_670_198;
const RESET_MARKER: &str = "ingress_v1014_pending_claim_reset_v1";
const RECOVERY_KEY: &str = "pending_notification_recovery_ordinal";

fn owner_config(fixture: &Fixture, application: &str) -> DatabaseConfig {
    let mut url =
        url::Url::parse(&url_with_schema(&fixture.base_url, &fixture.target)).expect("owner URL");
    url.query_pairs_mut()
        .append_pair("application_name", application);
    DatabaseConfig {
        pool_size: 1,
        ..DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string())
    }
}

async fn retrying_backend(fixture: &Fixture, application: &str) -> i32 {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = $1 \
                 AND state = 'idle' AND xact_start IS NULL \
                 AND pg_catalog.starts_with(query, 'ROLLBACK') LIMIT 1",
            )
            .bind(application)
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe catalog retry");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("catalog retry acknowledged a rollback")
}

async fn assert_catalog_available_and_no_waiter(fixture: &Fixture, pid: i32) {
    let mut probe = fixture.admin.begin().await.expect("probe transaction");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let available: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *probe)
                    .await
                    .expect("probe catalog lock");
            if available {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("lower-gate retries released catalog-X");
    let waiting: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1 \
         AND locktype = 'advisory' AND NOT granted)",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect physical advisory waiters");
    assert!(!waiting, "retry leaves no queued advisory waiter");
    probe.rollback().await.expect("release probe");
}

#[tokio::test]
async fn pending_owner_legacy_gates_retry_without_holding_catalog() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let db = Database::from_config(
        "pending catalog owner",
        &owner_config(&fixture, "pending_catalog_owner"),
    )
    .await
    .expect("owner pool");
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("prepare schema");

    for gate in ["receipts", "v1014", "recovery"] {
        let mut blocker = fixture
            .admin
            .begin()
            .await
            .expect("hold old initializer gate");
        match gate {
            "receipts" => {
                sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
                    .bind(RECEIPT_KEY)
                    .execute(&mut *blocker)
                    .await
                    .expect("receipt gate");
            }
            "v1014" => {
                sqlx::query(
                    "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext($1)::bigint)",
                )
                .bind(RESET_MARKER)
                .execute(&mut *blocker)
                .await
                .expect("reset gate");
            }
            _ => {
                sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext($1), pg_catalog.hashtext($2))")
                    .bind(&fixture.target).bind(RECOVERY_KEY).execute(&mut *blocker).await.expect("recovery gate");
            }
        }
        let running_db = db.clone();
        let bootstrap =
            tokio::spawn(async move { bootstrap_pending_delivery_owner_schema(&running_db).await });
        let pid = retrying_backend(&fixture, "pending_catalog_owner").await;
        assert!(
            !bootstrap.is_finished(),
            "{gate} gate must stop its actual stage"
        );
        assert_catalog_available_and_no_waiter(&fixture, pid).await;
        blocker.rollback().await.expect("release legacy gate");
        tokio::time::timeout(Duration::from_secs(10), bootstrap)
            .await
            .expect("initializer progresses")
            .expect("task")
            .expect("bootstrap succeeds");
    }
    drop(db);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_owner_pins_persistent_schema_and_reset_is_atomic() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let db = Database::from_config(
        "pending shadow owner",
        &owner_config(&fixture, "pending_shadow_owner"),
    )
    .await
    .expect("owner pool");
    {
        let conn = db.guard().await.expect("same backend");
        conn.execute("CREATE TEMP TABLE pending_delivery (trap TEXT)", ())
            .await
            .expect("temporary shadow");
    }
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("target schema is used despite temporary shadow");
    let target_exists: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1, 'pending_delivery')) IS NOT NULL")
        .bind(&fixture.target).fetch_one(&fixture.admin).await.expect("target table");
    assert!(target_exists);
    {
        let conn = db.guard().await.expect("same pooled backend");
        let mut rows = conn
            .query("SELECT count(*) FROM pg_temp.pending_delivery", ())
            .await
            .expect("temporary shadow remains");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("count")
                .get::<i64>(0)
                .expect("count"),
            0
        );
    }
    for sql in [
        format!("DELETE FROM {}.pending_delivery_startup_migrations WHERE name = '{RESET_MARKER}'", fixture.target),
        format!("INSERT INTO {}.pending_delivery (row_id, recipient_jid, original_receipt_at, payload_kind, flushed_in_session, outbound_sequence, claimed_at_ms) VALUES ('claimed', 'a@example.test', 1, 'transient', 'session', 4, 5)", fixture.target),
        format!("CREATE FUNCTION {}.reject_reset_marker() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.name = '{RESET_MARKER}' THEN RAISE EXCEPTION 'injected reset failure'; END IF; RETURN NEW; END $$", fixture.target),
        format!("CREATE TRIGGER reject_reset_marker BEFORE INSERT ON {}.pending_delivery_startup_migrations FOR EACH ROW EXECUTE FUNCTION {}.reject_reset_marker()", fixture.target, fixture.target),
    ] {
        sqlx::query(&sql).execute(&fixture.admin).await.expect("prepare reset fault");
    }
    assert!(bootstrap_pending_delivery_owner_schema(&db).await.is_err());
    let state: (Option<String>, Option<i32>, Option<i64>, bool, bool) = sqlx::query_as(&format!(
        "SELECT flushed_in_session, outbound_sequence, claimed_at_ms, \
         EXISTS (SELECT 1 FROM {0}.pending_delivery_startup_migrations WHERE name = $1), \
         pg_catalog.to_regclass('{0}.pending_delivery_sm_promotions') IS NOT NULL \
         FROM {0}.pending_delivery WHERE row_id = 'claimed'",
        fixture.target
    ))
    .bind(RESET_MARKER)
    .fetch_one(&fixture.admin)
    .await
    .expect("failed reset state");
    assert_eq!(
        state,
        (Some("session".into()), Some(4), Some(5), false, true)
    );
    for sql in [
        format!(
            "DROP TRIGGER reject_reset_marker ON {}.pending_delivery_startup_migrations",
            fixture.target
        ),
        format!("DROP FUNCTION {}.reject_reset_marker()", fixture.target),
    ] {
        sqlx::query(&sql)
            .execute(&fixture.admin)
            .await
            .expect("remove reset fault");
    }
    bootstrap_pending_delivery_owner_schema(&db)
        .await
        .expect("retry reset");
    let state: (Option<String>, Option<i32>, Option<i64>, bool) = sqlx::query_as(&format!(
        "SELECT flushed_in_session, outbound_sequence, claimed_at_ms, \
         EXISTS (SELECT 1 FROM {0}.pending_delivery_startup_migrations WHERE name = $1) \
         FROM {0}.pending_delivery WHERE row_id = 'claimed'",
        fixture.target
    ))
    .bind(RESET_MARKER)
    .fetch_one(&fixture.admin)
    .await
    .expect("reset state");
    assert_eq!(state, (None, None, None, true));
    drop(db);
    fixture.cleanup().await;
}

#[tokio::test]
async fn pending_read_attachment_waits_at_catalog_before_relation_locks() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = Database::from_config(
        "pending reader owner",
        &owner_config(&fixture, "pending_reader_owner"),
    )
    .await
    .expect("owner pool");
    bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("owner preparation");
    for table in [
        "pending_delivery",
        "pending_delivery_sm_promotions",
        "pending_delivery_startup_migrations",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("read grant");
    }
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("attestor");
    let runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "pending catalog reader",
        attested_pool(
            fixture
                .runtime_options
                .clone()
                .application_name("pending_catalog_reader"),
            config,
            1,
            Duration::from_secs(8),
        )
        .expect("attested pool"),
    );
    {
        drop(runtime.guard().await.expect("prewarm checkout hook"));
    }
    let mut blocker = fixture.admin.begin().await.expect("catalog blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *blocker)
        .await
        .expect("catalog-X");
    let reader = tokio::spawn(async move {
        DatabasePendingDeliveryStorage::attach_existing(runtime, QuotaPolicy::Unlimited).await
    });
    let pid = retrying_backend(&fixture, "pending_catalog_reader").await;
    assert!(!reader.is_finished(), "reader must wait for catalog-S");
    let relations_locked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_class c ON c.oid = l.relation \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE l.pid = $1 AND n.nspname = $2 \
         AND c.relname IN ('pending_delivery', 'pending_delivery_sm_promotions', 'pending_delivery_startup_migrations'))",
    ).bind(pid).bind(&fixture.target).fetch_one(&fixture.admin).await.expect("inspect exact reader backend");
    assert!(
        !relations_locked,
        "reader has not reached named relation locks"
    );
    blocker.rollback().await.expect("release catalog-X");
    let storage = tokio::time::timeout(Duration::from_secs(10), reader)
        .await
        .expect("reader progresses")
        .expect("task")
        .expect("read-only grants are sufficient");
    drop(storage);
    drop(owner);
    fixture.cleanup().await;
}
