//! PG17 races for the closed selected transaction entry and hook fence.

use std::time::Duration;

use waddle_server::db::physical_attestation::{
    CatalogFenceExclusive, CatalogFenceIndex, CatalogMutationFenceError, PinnedSelectedEntry,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};

use super::{pinned_entry_tests::pinned_pool, test_url, Fixture, CATALOG_GRANT_LOCK};

const WAIT: Duration = Duration::from_secs(15);

async fn wait_for_runtime_hook_try(fixture: &Fixture) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_stat_activity \
                 WHERE usename = $1 AND query LIKE 'SELECT pg_catalog.pg_try_advisory_lock_shared%' \
                 ORDER BY query_start DESC LIMIT 1",
            )
            .bind(&fixture.runtime)
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe runtime hook try-lock");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("runtime hook reaches catalog fence before attestor reads")
}

#[tokio::test]
async fn selected_entry_and_pool_hook_coordinate_with_catalog_mutator() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;

    let selected_pool = pinned_pool(&fixture, None).await;
    let selected = PinnedSelectedEntry::new(selected_pool).expect("closed pinned entry");

    // A hook arriving while the exclusive mutator is active may only poll the
    // session try-lock. It must not begin the attestor transaction or acquire
    // catalog relation locks until the mutator releases.
    let mutator = CatalogFenceExclusive::acquire(
        &fixture.admin_options,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        WAIT,
    )
    .await
    .expect("exclusive catalog mutator");
    let hook_pool = std::sync::Arc::new(pinned_pool(&fixture, None).await);
    let checkout_pool = hook_pool.clone();
    let checkout = tokio::spawn(async move { checkout_pool.acquire_for_test().await });
    let hook_pid = wait_for_runtime_hook_try(&fixture).await;
    let preproof_catalog_locks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks \
         WHERE pid = $1 AND locktype = 'relation' AND granted",
    )
    .bind(hook_pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect hook relation locks before exclusive release");
    assert_eq!(
        preproof_catalog_locks, 0,
        "hook reads wait behind the fence"
    );
    mutator
        .release()
        .await
        .expect("acknowledged mutator release");
    let connection = tokio::time::timeout(Duration::from_secs(5), checkout)
        .await
        .expect("pool hook completes after mutator release")
        .expect("checkout task joins")
        .expect("hook attestation succeeds");
    drop(connection);

    // The hook must release its temporary shared session lock before lending
    // the now-idle connection, so an exclusive mutator can proceed.
    let idle_mutator = CatalogFenceExclusive::acquire(
        &fixture.admin_options,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        WAIT,
    )
    .await
    .expect("no idle-held pool fence");
    idle_mutator
        .release()
        .await
        .expect("acknowledge idle fence release");

    // Pause a real selected READ COMMITTED, READ ONLY transaction after pinned
    // checks. The xact shared key remains held through the epoch observation
    // and acknowledged rollback, excluding concurrent catalog maintenance.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let selected = std::sync::Arc::new(selected);
    let selected_task = {
        let selected = selected.clone();
        tokio::spawn(async move {
            selected
                .observe_protocol_epoch_paused_for_test(ready_tx, release_rx)
                .await
        })
    };
    let backend_pid = match tokio::time::timeout(Duration::from_secs(8), ready_rx).await {
        Ok(Ok(pid)) => pid,
        Ok(Err(_)) => panic!(
            "selected operation ended before its protected pause: {:?}",
            selected_task.await.expect("selected task joins")
        ),
        Err(_) => panic!("selected operation did not reach its protected pause"),
    };
    let blocked = CatalogFenceExclusive::acquire(
        &fixture.admin_options,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        Duration::from_millis(300),
    )
    .await;
    assert!(
        matches!(blocked, Err(CatalogMutationFenceError::Contended)),
        "exclusive catalog mutation cannot pass the active selected transaction"
    );
    let transaction_live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
         WHERE pid = $1 AND xact_start IS NOT NULL)",
    )
    .bind(backend_pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("observe selected transaction at the mutation race");
    assert!(transaction_live);

    release_tx.send(()).expect("release selected operation");
    let epoch = tokio::time::timeout(Duration::from_secs(8), selected_task)
        .await
        .expect("selected read-only transaction resolves")
        .expect("selected task joins")
        .expect("selected read-only proof succeeds");
    let _observed_epoch = epoch;

    // The mutator now performs actual PostgreSQL concurrent index maintenance
    // under the exact protocol key used by both production candidates.
    let mut mutator = CatalogFenceExclusive::acquire(
        &fixture.admin_options,
        CATALOG_MUTATION_ADVISORY_LOCK_KEY,
        WAIT,
    )
    .await
    .expect("exclusive mutator proceeds after selected rollback");
    let index = CatalogFenceIndex::new(&fixture.target, "ingress_effect_receipts_pkey")
        .expect("fixed test index name");
    mutator
        .reindex(&index)
        .await
        .expect("concurrent index maintenance completes under exclusive fence");
    mutator
        .release()
        .await
        .expect("mutator release acknowledged");

    hook_pool.close().await;
    drop(selected);
    fixture.cleanup().await;
}
