//! The existing receipt caller on a real, test-selected pinned Main lane.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection};
use waddle_server::db::physical_attestation::{
    pinned_attested_pool, PgRoleName, ReceiptAuthorityError, ReceiptCommit, ReceiptOperation,
    ReceiptOperationAuthority, ReceiptOperationPins, ReceiptReadback,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::ingress_substrate::EPOCH_GUARDED_TABLES;

use super::{pinned_receipt_tests, proxy_runtime_options, test_url, Fixture, CATALOG_GRANT_LOCK};

// The ledger lock is crate-private; inspect its stable protocol key externally.
const LEDGER_LOCK: i64 = 6_841_445_497_037_937_992;
const LINEAGE_LOCK: i64 = 6_841_445_497_037_937_993;

async fn execute(fixture: &Fixture, statement: &str) {
    sqlx::raw_sql(statement)
        .execute(&fixture.admin)
        .await
        .unwrap_or_else(|error| panic!("fixture SQL failed: {statement}: {error}"));
}

async fn count(fixture: &Fixture, operation: &ReceiptOperation) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.p21_receipt_operation_results WHERE operation_id=$1",
        fixture.target
    ))
    .bind(operation.operation_id)
    .fetch_one(&fixture.admin)
    .await
    .expect("journal count")
}

async fn mutate_epoch(
    fixture: &Fixture,
    epoch: i64,
    activated: Option<&str>,
    lineage: Option<&str>,
) {
    let table = format!("{}.ingress_protocol_epoch", fixture.target);
    let mut tx = fixture
        .admin
        .begin()
        .await
        .expect("epoch fixture transaction");
    sqlx::query(&format!("ALTER TABLE {table} DISABLE TRIGGER USER"))
        .execute(&mut *tx)
        .await
        .expect("disable epoch triggers");
    sqlx::query(&format!("UPDATE {table} SET epoch=$1, activated_at=$2::timestamptz, lineage_uuid=$3::uuid WHERE id=1"))
        .bind(epoch).bind(activated).bind(lineage).execute(&mut *tx).await.expect("set epoch fixture");
    for trigger in [
        "ingress_protocol_epoch_guard_row",
        "ingress_protocol_epoch_guard_truncate",
    ] {
        sqlx::query(&format!(
            "ALTER TABLE {table} ENABLE ALWAYS TRIGGER {trigger}"
        ))
        .execute(&mut *tx)
        .await
        .expect("restore exact trigger mode");
    }
    tx.commit().await.expect("publish epoch fixture");
}

async fn wait_discard(fixture: &Fixture, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
            )
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("backend status");
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("discarded backend terminated");
}

#[tokio::test]
async fn pinned_caller_keeps_one_transaction_and_positive_only_reconciliation() {
    let _guard = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new_with_c_collation(url).await;
    let (guarded, leases, _, operations, api, config, pinned, guarded_pins) =
        pinned_receipt_tests::setup(&fixture).await;
    drop(guarded);
    drop(leases);
    let pins = guarded_pins.receipt;
    // Switch the disposable fixture from the separate guarded caller to the
    // existing receipt caller's exact profile. R receives no table grants.
    execute(
        &fixture,
        &format!(
            "DROP FUNCTION {s}.waddle_p21_guarded_record_effect_receipt(text,uuid,bigint,uuid,uuid,integer,bytea); \
             SET ROLE {api}; GRANT EXECUTE ON FUNCTION {s}.waddle_p21_record_effect_receipt(uuid,uuid,integer,bytea) TO {r}; RESET ROLE",
            s = fixture.target,
            r = fixture.runtime,
        ),
    )
    .await;
    let pool = pinned_attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        pinned.clone(),
        2,
        Duration::from_secs(3),
    )
    .expect("pinned pool");
    let authority = ReceiptOperationAuthority::postgres_pinned_for_test(
        pool,
        PgRoleName::new(&api).expect("API role"),
        pins,
    )
    .expect("closed pinned caller");

    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime");
    for relation in EPOCH_GUARDED_TABLES
        .iter()
        .copied()
        .chain(["p21_receipt_operation_results"])
    {
        let error = sqlx::query(&format!(
            "SELECT * FROM {}.{relation} LIMIT 1",
            fixture.target
        ))
        .execute(&mut runtime)
        .await
        .expect_err("restricted runtime cannot read guarded or journal rows");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("42501")
        );
    }
    runtime.close().await.expect("close direct runtime");

    let operation = operations[0];
    assert_eq!(
        authority.readback(&operation).await.expect("missing"),
        ReceiptReadback::Indeterminate
    );
    let expected = ReceiptCommit::Committed {
        operation_id: operation.operation_id,
        inserted: true,
    };
    assert_eq!(
        authority.record(&operation).await.expect("record"),
        expected
    );
    assert_eq!(
        authority.record(&operation).await.expect("replay"),
        expected
    );
    assert_eq!(
        authority.readback(&operation).await.expect("readback"),
        ReceiptReadback::Committed { inserted: true }
    );
    let conflict = ReceiptOperation {
        semantic_identity_hash: [0x99; 32],
        ..operation
    };
    assert!(matches!(
        authority.readback(&conflict).await,
        Err(ReceiptAuthorityError::OperationConflict)
    ));
    assert!(matches!(
        authority.record(&conflict).await,
        Err(ReceiptAuthorityError::OperationConflict)
    ));

    // Observe the same live PID/XID around the fixed operation and verify each
    // transaction barrier before permitting completion.
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (resume, release) = tokio::sync::oneshot::channel();
    let mut write = Box::pin(authority.record_pinned_paused_for_test(
        &operations[1],
        Duration::from_secs(15),
        ready,
        release,
    ));
    let (before, after) = tokio::select! {
        result = &mut write => panic!("ended before pause: {result:?}"),
        pair = waiting => pair.expect("pause observation"),
    };
    assert_eq!(before, after);
    let (pid, xid) = after;
    let live: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND backend_xid::text=$2)")
        .bind(pid).bind((xid as u32).to_string()).fetch_one(&fixture.admin).await.expect("live transaction");
    assert!(live);
    for key in [
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        LINEAGE_LOCK,
        LEDGER_LOCK,
    ] {
        let locked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
             AND classid=(($2::bigint >> 32) & 4294967295)::oid AND objid=($2::bigint & 4294967295)::oid \
             AND objsubid=1 AND mode='ShareLock' AND granted)",
        ).bind(pid).bind(key).fetch_one(&fixture.admin).await.expect("advisory lock");
        assert!(locked, "missing advisory barrier {key}");
    }
    for name in [
        "_lineage",
        "_migrations",
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
    ] {
        let relation: Oid = sqlx::query_scalar("SELECT $1::pg_catalog.regclass::pg_catalog.oid")
            .bind(format!("{}.{name}", fixture.target))
            .fetch_one(&fixture.admin)
            .await
            .expect("relation oid");
        let locked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND relation=$2 AND mode='AccessShareLock' AND granted)")
            .bind(pid).bind(relation).fetch_one(&fixture.admin).await.expect("relation lock");
        assert!(locked, "missing relation barrier {name}");
    }
    let mut mutator = fixture.admin.begin().await.expect("catalog mutator");
    let exclusive: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *mutator)
        .await
        .expect("exclusive try");
    assert!(!exclusive);
    mutator.rollback().await.expect("end mutator");
    assert_eq!(
        authority
            .readback(&operations[1])
            .await
            .expect("uncommitted"),
        ReceiptReadback::Indeterminate
    );
    resume.send(()).expect("release write");
    assert!(matches!(
        write.await,
        Ok(ReceiptCommit::Committed { inserted: true, .. })
    ));
    assert_eq!(
        authority.readback(&operations[1]).await.expect("committed"),
        ReceiptReadback::Committed { inserted: true }
    );

    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (_resume, release) = tokio::sync::oneshot::channel();
    let mut cancelled = Box::pin(authority.record_pinned_paused_for_test(
        &operations[2],
        Duration::from_secs(15),
        ready,
        release,
    ));
    let (_, (pid, _)) = tokio::select! {
        result = &mut cancelled => panic!("ended before cancellation: {result:?}"),
        pair = waiting => pair.expect("paused transaction"),
    };
    drop(cancelled);
    wait_discard(&fixture, pid).await;
    assert_eq!(
        authority.readback(&operations[2]).await.expect("cancelled"),
        ReceiptReadback::Indeterminate
    );
    assert_eq!(count(&fixture, &operations[2]).await, 0);

    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (_resume, release) = tokio::sync::oneshot::channel();
    let mut timed = Box::pin(authority.record_pinned_paused_for_test(
        &operations[3],
        Duration::from_secs(5),
        ready,
        release,
    ));
    let (_, (pid, _)) = tokio::select! {
        result = &mut timed => panic!("ended before timeout: {result:?}"),
        pair = waiting => pair.expect("paused transaction"),
    };
    assert_eq!(
        timed.await.expect("unknown timeout"),
        ReceiptCommit::Indeterminate {
            operation_id: operations[3].operation_id
        }
    );
    wait_discard(&fixture, pid).await;
    assert_eq!(
        authority
            .readback(&operations[3])
            .await
            .expect("timeout readback"),
        ReceiptReadback::Indeterminate
    );

    let old_checksum: String = sqlx::query_scalar(&format!(
        "SELECT checksum FROM {}._migrations WHERE version=1030",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("checksum");
    execute(
        &fixture,
        &format!(
            "UPDATE {}._migrations SET checksum='bad' WHERE version=1030",
            fixture.target
        ),
    )
    .await;
    assert!(
        authority.record(&operations[4]).await.is_err(),
        "complete ledger drift must refuse"
    );
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    sqlx::query(&format!(
        "UPDATE {}._migrations SET checksum=$1 WHERE version=1030",
        fixture.target
    ))
    .bind(old_checksum)
    .execute(&fixture.admin)
    .await
    .expect("restore ledger");
    let (old_epoch, old_activated, old_lineage): (i64, Option<String>, Option<String>) = sqlx::query_as(&format!(
        "SELECT epoch, activated_at::text, lineage_uuid::text FROM {}.ingress_protocol_epoch WHERE id=1", fixture.target
    )).fetch_one(&fixture.admin).await.expect("original epoch");
    mutate_epoch(
        &fixture,
        4_294_967_295,
        Some("2024-01-01T00:00:00Z"),
        Some("11111111-1111-4111-8111-111111111111"),
    )
    .await;
    assert!(
        authority.record(&operations[4]).await.is_err(),
        "unsupported epoch must refuse"
    );
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    mutate_epoch(
        &fixture,
        old_epoch,
        old_activated.as_deref(),
        old_lineage.as_deref(),
    )
    .await;
    assert_eq!(
        authority
            .readback(&operation)
            .await
            .expect("restored epoch"),
        ReceiptReadback::Committed { inserted: true }
    );
    execute(
        &fixture,
        &format!(
            "ALTER FUNCTION {}.read_physical_identity() RENAME TO read_physical_identity_drift",
            fixture.trusted
        ),
    )
    .await;
    assert!(
        matches!(
            authority.record(&operations[4]).await,
            Err(ReceiptAuthorityError::Database)
        ),
        "physical attestor drift must fail the pinned checkout"
    );
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    execute(
        &fixture,
        &format!(
            "ALTER FUNCTION {}.read_physical_identity_drift() RENAME TO read_physical_identity",
            fixture.trusted
        ),
    )
    .await;
    assert_eq!(
        authority
            .readback(&operation)
            .await
            .expect("restored physical attestor"),
        ReceiptReadback::Committed { inserted: true }
    );
    execute(
        &fixture,
        &format!(
            "ALTER TABLE {}.ingress_protocol_epoch RENAME TO ingress_protocol_epoch_temporarily_renamed",
            fixture.target
        ),
    )
    .await;
    assert!(
        authority.record(&operations[4]).await.is_err(),
        "epoch catalog drift must refuse"
    );
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    execute(
        &fixture,
        &format!(
            "ALTER TABLE {}.ingress_protocol_epoch_temporarily_renamed RENAME TO ingress_protocol_epoch",
            fixture.target
        ),
    )
    .await;
    assert_eq!(
        authority
            .readback(&operation)
            .await
            .expect("restored epoch catalog"),
        ReceiptReadback::Committed { inserted: true }
    );
    execute(
        &fixture,
        &format!(
            "GRANT SELECT ON {}.p21_receipt_operation_results TO {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    assert!(
        authority.record(&operations[4]).await.is_err(),
        "journal ACL drift must refuse"
    );
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    execute(
        &fixture,
        &format!(
            "REVOKE SELECT ON {}.p21_receipt_operation_results FROM {}",
            fixture.target, fixture.runtime
        ),
    )
    .await;
    let wrong = ReceiptOperationPins {
        journal_function_oid: u32::MAX,
        ..pins
    };
    let pool = pinned_attested_pool(
        fixture.runtime_options.clone(),
        config.clone(),
        pinned.clone(),
        1,
        Duration::from_secs(3),
    )
    .expect("wrong-pin pool");
    let wrong = ReceiptOperationAuthority::postgres_pinned_for_test(
        pool,
        PgRoleName::new(&api).expect("API"),
        wrong,
    )
    .expect("valid independent pin syntax");
    assert!(
        wrong.record(&operations[4]).await.is_err(),
        "wrong journal function pin must refuse"
    );
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    drop(wrong);

    let mut held = fixture.admin.begin().await.expect("exclusive fence");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *held)
        .await
        .expect("hold exclusive fence");
    // The pinned checkout hook sees X before a transaction is started.
    assert!(matches!(
        authority.record(&operations[4]).await,
        Err(ReceiptAuthorityError::Database)
    ));
    assert_eq!(count(&fixture, &operations[4]).await, 0);
    held.rollback().await.expect("release exclusive fence");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let port = listener.local_addr().expect("proxy port").port();
    let (held, waiting) = tokio::sync::oneshot::channel();
    let (disconnect, release) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(pinned_receipt_tests::lose_commit_reply(
        listener,
        fixture.admin_options.clone(),
        held,
        release,
    ));
    let pool = pinned_attested_pool(
        proxy_runtime_options(&fixture, port),
        config,
        pinned,
        1,
        Duration::from_secs(3),
    )
    .expect("proxied pool");
    let proxied = ReceiptOperationAuthority::postgres_pinned_for_test(
        pool,
        PgRoleName::new(&api).expect("API"),
        pins,
    )
    .expect("proxied authority");
    let lost = operations[5];
    let write = tokio::spawn(async move { proxied.record(&lost).await });
    tokio::time::timeout(Duration::from_secs(15), waiting)
        .await
        .expect("server committed")
        .expect("reply held");
    assert_eq!(
        authority
            .readback(&lost)
            .await
            .expect("positive historical evidence"),
        ReceiptReadback::Committed { inserted: true }
    );
    disconnect.send(()).expect("discard acknowledgement");
    assert_eq!(
        write.await.expect("task").expect("unknown commit"),
        ReceiptCommit::Indeterminate {
            operation_id: lost.operation_id
        }
    );
    proxy.await.expect("proxy task").expect("proxy result");

    drop(authority);
    execute(&fixture, &format!("DROP OWNED BY {api}; DROP ROLE {api}")).await;
    fixture.cleanup().await;
}
