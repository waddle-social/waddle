//! Physical witnesses for the public joint-custody reader's fence ordering.
//!
//! These deliberately minimal relations let the reader reach the gates; they
//! are not an owner-prepared schema and cannot yield custody evidence.

use super::{run_sql, Fixture};
use sqlx::{Postgres, Transaction};
use std::time::Duration;
use waddle_server::db::physical_attestation::{
    attested_pool, install, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::{Database, DatabaseError};
use waddle_server::notification_outbox::NotificationOutboxRuntimeAttachError;
use waddle_server::server::push_notification_custody::{
    verify_attested_push_notification_custody, PushNotificationCustodyError,
};

const OUTBOX_GATE: (i64, i64) = (0x5744444c, 0x4e4f5449);
const PUSH_GATE: i64 = 6_841_445_497_037_937_998;
const ACTIVITY_GATE: (i64, i64) = (0x5744444c, 0x4e414354);
const PUBSUB_GATE: i64 = 7_534_519_881_230_429_064;

#[derive(Clone, Copy)]
enum Gate {
    Push,
    Activity,
    PubSub,
}

impl Gate {
    fn prior_relation(self) -> &'static str {
        match self {
            Self::Push => "notification_candidates",
            Self::Activity => "push_registrations",
            Self::PubSub => "notification_activity",
        }
    }
}

#[tokio::test]
async fn pg17_custody_reader_releases_catalog_before_lower_gate_retry() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let f = Fixture::new(url).await;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
        .fetch_one(&f.admin)
        .await
        .expect("PostgreSQL version");
    assert!(
        (170_000..180_000).contains(&version),
        "requires PostgreSQL 17"
    );

    for table in [
        "notification_candidates",
        "notification_outbox_lineage",
        "notification_outbox",
        "notification_outbox_owner_state",
        "push_owner_locks",
        "push_nodes",
        "push_node_locks",
        "push_devices",
        "push_delivery_attempts",
        "push_publish_jobs",
        "push_publication_orders",
        "push_service_schema_marker",
        "push_service_vapid_keys",
        "push_service_vapid_schema_marker",
        "push_registrations",
        "notification_activity",
        "notification_activity_owner_state",
    ] {
        run_sql(
            &f,
            &format!("CREATE TABLE {}.{table} (placeholder integer)", f.target),
        )
        .await;
        run_sql(
            &f,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {}",
                f.target, f.runtime
            ),
        )
        .await;
    }
    let config = install(&f.installer_options, f.config(), f.operation_id)
        .await
        .expect("install the physical attestor");
    let application = format!("custody_reader_{}", uuid::Uuid::new_v4().simple());
    let runtime = waddle_server::test_support::database_from_attested_postgres_pool(
        "custody gate witness",
        attested_pool(
            f.runtime_options.clone().application_name(&application),
            config,
            1,
            Duration::from_secs(3),
        )
        .expect("one-slot attested reader"),
    );

    witness_outbox_gate(&f, &runtime, &application).await;
    for gate in [Gate::Push, Gate::Activity, Gate::PubSub] {
        witness_retry_and_relation_drift(&f, &runtime, &application, gate).await;
    }
    witness_role_drift(&f, &runtime, &application).await;
    witness_cancelled_operation(&f, &runtime, &application).await;
    drop(runtime);
    f.cleanup().await;
}

async fn hold_gate<'a>(f: &'a Fixture, gate: Gate) -> Transaction<'a, Postgres> {
    let mut holder = f.admin.begin().await.expect("begin lower gate holder");
    match gate {
        Gate::Push | Gate::PubSub => {
            let key = match gate {
                Gate::Push => PUSH_GATE,
                Gate::PubSub => PUBSUB_GATE,
                Gate::Activity => unreachable!(),
            };
            sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
                .bind(key)
                .execute(&mut *holder)
                .await
                .expect("hold exclusive lower gate");
        }
        Gate::Activity => {
            sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1::integer, $2::integer)")
                .bind(ACTIVITY_GATE.0 as i32)
                .bind(ACTIVITY_GATE.1 as i32)
                .execute(&mut *holder)
                .await
                .expect("hold activity gate");
        }
    }
    holder
}

async fn hold_relation<'a>(f: &'a Fixture, relation: &str) -> Transaction<'a, Postgres> {
    let mut holder = f.admin.begin().await.expect("begin relation holder");
    sqlx::query(&format!(
        "LOCK TABLE {}.{relation} IN ACCESS EXCLUSIVE MODE",
        f.target
    ))
    .execute(&mut *holder)
    .await
    .expect("hold preceding relation");
    holder
}

async fn wait_for_operation(f: &Fixture, application: &str, gate: Gate) -> i32 {
    // This is an operation transaction, not the pool's checkout-hook
    // ROLLBACK: its backend is actively waiting for a named application
    // relation while it holds catalog-S and the already ordered gates.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row: Option<(i32, bool, bool)> = sqlx::query_as(
                "SELECT a.pid, \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                   AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
                   AND l.classid::bigint = ($3::bigint >> 32) \
                   AND l.objid::bigint = ($3::bigint & 4294967295) \
                   AND l.mode = 'ExclusiveLock'), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                   AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 2 \
                   AND l.classid::bigint = $6 AND l.objid::bigint = $7 \
                   AND l.mode = 'ShareLock') \
                 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name = $1 AND a.state = 'active' \
                   AND a.xact_start IS NOT NULL AND a.wait_event_type = 'Lock' \
                   AND a.query LIKE '%' || $2 || '%' \
                   AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                     AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
                     AND l.classid::bigint = ($4::bigint >> 32) \
                     AND l.objid::bigint = ($4::bigint & 4294967295) \
                     AND l.mode = 'ShareLock') \
                   AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid \
                     AND l.locktype = 'advisory' AND l.granted AND l.objsubid = 2 \
                     AND l.classid::bigint = $5 AND l.objid::bigint = $8 \
                     AND l.mode = 'ShareLock')",
            )
            .bind(application)
            .bind(gate.prior_relation())
            .bind(PUSH_GATE)
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .bind(OUTBOX_GATE.0)
            .bind(ACTIVITY_GATE.0)
            .bind(ACTIVITY_GATE.1)
            .bind(OUTBOX_GATE.1)
            .fetch_optional(&f.admin)
            .await
            .expect("observe operation backend and advisory locks");
            if let Some((pid, holds_push, holds_activity)) = row {
                assert_eq!(holds_push, !matches!(gate, Gate::Push));
                assert_eq!(holds_activity, matches!(gate, Gate::PubSub));
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("observe the fenced operation waiting on its preceding relation")
}

async fn take_catalog_exclusive(f: &Fixture) -> Transaction<'_, Postgres> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut probe = f.admin.begin().await.expect("begin catalog probe");
            let acquired: bool =
                sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
                    .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
                    .fetch_one(&mut *probe)
                    .await
                    .expect("probe catalog-X");
            if acquired {
                return probe;
            }
            probe.rollback().await.expect("release failed probe");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the observed operation must release catalog-S")
}

async fn witness_retry_and_relation_drift(
    f: &Fixture,
    runtime: &Database,
    application: &str,
    gate: Gate,
) {
    let lower = hold_gate(f, gate).await;
    let relation = hold_relation(f, gate.prior_relation()).await;
    let db = runtime.clone();
    let reader =
        tokio::spawn(
            async move { verify_attested_push_notification_custody(db, &[7_u8; 32]).await },
        );
    let pid = wait_for_operation(f, application, gate).await;
    relation
        .rollback()
        .await
        .expect("release preceding relation");

    // The same operation was observed holding catalog-S above. The still
    // contended lower gate prevents progress; acquiring catalog-X while the
    // task remains pending therefore witnesses an operation rollback, even
    // though later pool checkouts may themselves issue ROLLBACK.
    let mut catalog = take_catalog_exclusive(f).await;
    assert!(
        !reader.is_finished(),
        "reader exited before retry (backend {pid})"
    );
    let outbox_available: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1::integer, $2::integer)")
            .bind(OUTBOX_GATE.0 as i32)
            .bind(OUTBOX_GATE.1 as i32)
            .fetch_one(&mut *catalog)
            .await
            .expect("probe released outbox gate");
    assert!(outbox_available, "retry retained the outbox gate");
    sqlx::query(&format!(
        "ALTER TABLE {}.notification_candidates RENAME TO custody_hidden_candidates",
        f.target
    ))
    .execute(&mut *catalog)
    .await
    .expect("stage relation drift under catalog-X");
    lower.rollback().await.expect("release lower gate");
    catalog.commit().await.expect("publish relation drift");
    let result = tokio::time::timeout(Duration::from_secs(20), reader)
        .await
        .expect("reader did not retry")
        .expect("join reader");
    assert!(matches!(
        result,
        Err(PushNotificationCustodyError::Outbox(
            NotificationOutboxRuntimeAttachError::RelationLock(_)
        ))
    ));
    run_sql(
        f,
        &format!(
            "ALTER TABLE {}.custody_hidden_candidates RENAME TO notification_candidates",
            f.target
        ),
    )
    .await;
}

async fn witness_role_drift(f: &Fixture, runtime: &Database, application: &str) {
    let lower = hold_gate(f, Gate::PubSub).await;
    let relation = hold_relation(f, Gate::PubSub.prior_relation()).await;
    let db = runtime.clone();
    let reader =
        tokio::spawn(
            async move { verify_attested_push_notification_custody(db, &[7_u8; 32]).await },
        );
    wait_for_operation(f, application, Gate::PubSub).await;
    relation
        .rollback()
        .await
        .expect("release activity relation");
    let mut catalog = take_catalog_exclusive(f).await;
    assert!(!reader.is_finished());
    sqlx::query(&format!(
        "GRANT TEMPORARY ON DATABASE {} TO {}",
        f.database_name, f.runtime
    ))
    .execute(&mut *catalog)
    .await
    .expect("stage runtime TEMP drift under catalog-X");
    lower.rollback().await.expect("release PubSub gate");
    catalog.commit().await.expect("publish role drift");
    assert!(tokio::time::timeout(Duration::from_secs(20), reader)
        .await
        .expect("drifted reader did not finish")
        .expect("join drifted reader")
        .is_err());
    run_sql(
        f,
        &format!(
            "REVOKE TEMPORARY ON DATABASE {} FROM {}",
            f.database_name, f.runtime
        ),
    )
    .await;
    // This refusal may occur in the checkout hook; it is not evidence that
    // the later in-transaction role checks ran.
}

async fn witness_cancelled_operation(f: &Fixture, runtime: &Database, application: &str) {
    let lower = hold_gate(f, Gate::Push).await;
    let relation = hold_relation(f, Gate::Push.prior_relation()).await;
    let db = runtime.clone();
    let reader =
        tokio::spawn(
            async move { verify_attested_push_notification_custody(db, &[7_u8; 32]).await },
        );
    let pid = wait_for_operation(f, application, Gate::Push).await;
    reader.abort();
    assert!(reader
        .await
        .expect_err("join cancelled reader")
        .is_cancelled());
    relation
        .rollback()
        .await
        .expect("release preceding relation");
    lower.rollback().await.expect("release Push gate");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
            )
            .bind(pid)
            .fetch_one(&f.admin)
            .await
            .expect("check cancelled backend");
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("cancelled uncertain checkout must be discarded");
    let catalog = take_catalog_exclusive(f).await;
    catalog.rollback().await.expect("release catalog probe");
}

async fn witness_outbox_gate(f: &Fixture, runtime: &Database, application: &str) {
    let catalog = take_catalog_exclusive(f).await;
    let db = runtime.clone();
    let reader =
        tokio::spawn(
            async move { verify_attested_push_notification_custody(db, &[7_u8; 32]).await },
        );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE application_name = $1)",
            )
            .bind(application)
            .fetch_one(&f.admin)
            .await
            .expect("observe reader checkout");
            if present {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("reader backend appears");
    assert!(!reader.is_finished());
    let mut outbox = f.admin.begin().await.expect("begin outbox gate holder");
    let available: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1::integer, $2::integer)")
            .bind(OUTBOX_GATE.0 as i32)
            .bind(OUTBOX_GATE.1 as i32)
            .fetch_one(&mut *outbox)
            .await
            .expect("probe outbox gate while catalog-X is held");
    assert!(available, "reader took the outbox gate before catalog-S");
    catalog.rollback().await.expect("release catalog-X");

    // GateContended can only be returned after the operation itself has
    // failed the outbox try and acknowledged its rollback. The preceding
    // backend-presence check alone could instead have observed a checkout hook.
    let result = tokio::time::timeout(Duration::from_secs(25), reader)
        .await
        .expect("outbox gate retry exceeded its bound")
        .expect("join reader");
    assert!(matches!(
        result,
        Err(PushNotificationCustodyError::Database(
            DatabaseError::CatalogMutationGateContended
        ))
    ));
    let catalog = take_catalog_exclusive(f).await;
    catalog.rollback().await.expect("release catalog probe");
    outbox.rollback().await.expect("release outbox gate");
}
