//! Bounded retention collection for terminal ingress authority records.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use chrono::Utc;
use futures::FutureExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::Database;
use crate::ingress_substrate::{
    gc_expired_aliases, AliasGcBudget, AliasGcError, AliasGcFailure, AliasGcOutcome,
    AliasGcProgress,
};
use crate::ingress_uow::{IngressUnitOfWork, IngressUowError};

const RETENTION_GC_BUDGET: Duration = Duration::from_secs(2);
/// Last-resort envelope around one GC run, sized from the longest path the
/// per-operation bounds allow after the final cooperative check: one scan
/// (`RETENTION_GC_SCAN_TIMEOUT`), then one candidate transaction — the epoch
/// lock wait plus the nine single-row statements it issues — and margin, so
/// slowness inside the bounds is classified by the run itself rather than
/// cancelled from outside.  2 s + 1 s + 0.1 s + 9 × 0.25 s ≈ 5.4 s.
const RETENTION_GC_HARD_DEADLINE: Duration = Duration::from_secs(6);
/// Strictly below the statement bound: PostgreSQL's statement timer covers
/// the lock wait, so an equal or larger lock bound would surface every lock
/// wait as a statement timeout.
const RETENTION_GC_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
/// Every candidate-transaction statement touches one row by primary key.
const RETENTION_GC_STATEMENT_TIMEOUT: Duration = Duration::from_millis(250);
const RETENTION_GC_SCAN_TIMEOUT: Duration = Duration::from_secs(1);
/// Pause between a `partial` run and its self-scheduled continuation, so a
/// backlog drains without an external trigger while leaving the dedicated
/// pool connection free between passes.
pub(crate) const RETENTION_GC_PARTIAL_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(crate) struct RetentionGcBudget {
    pub(crate) cooperative: Duration,
    pub(crate) hard_deadline: Duration,
    pub(crate) lock_timeout: Duration,
    pub(crate) statement_timeout: Duration,
    pub(crate) scan_timeout: Duration,
}

impl RetentionGcBudget {
    pub(crate) const DEFAULT: Self = Self {
        cooperative: RETENTION_GC_BUDGET,
        hard_deadline: RETENTION_GC_HARD_DEADLINE,
        lock_timeout: RETENTION_GC_LOCK_TIMEOUT,
        statement_timeout: RETENTION_GC_STATEMENT_TIMEOUT,
        scan_timeout: RETENTION_GC_SCAN_TIMEOUT,
    };
}

/// Retention GC runs on its own background task, off the per-stream worker
/// slots.  Triggers arrive through a `Notify`: `notify_one` stores exactly
/// one permit while a run is in flight, so a trigger that lands mid-run is
/// never lost and a burst of triggers coalesces into one follow-up run.
/// The task makes one pass at startup (reclaiming whatever an earlier
/// process left behind) and a `partial` run continues on its own after
/// `partial_retry_delay`, so a backlog converges without external triggers.
#[derive(Clone)]
pub(crate) struct RetentionGcCoordinator {
    pub(crate) trigger: Arc<Notify>,
    pub(crate) run: Arc<dyn Fn() -> RetentionGcRunFuture + Send + Sync>,
    pub(crate) partial_retry_delay: Duration,
}

type RetentionGcRunFuture = Pin<
    Box<dyn Future<Output = waddle_xmpp::telemetry::attributes::IngressGcOutcome> + Send + 'static>,
>;

pub(crate) async fn run_retention_gc_coordinator(
    coordinator: RetentionGcCoordinator,
    cancellation: CancellationToken,
    force_stop: CancellationToken,
) {
    use waddle_xmpp::telemetry::attributes::IngressGcOutcome;

    let mut pending = true;
    loop {
        if !pending {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                () = coordinator.trigger.notified() => {}
            }
        }
        // Graceful shutdown never starts a run: a fresh run could extend the
        // drain by a whole envelope, and the next process's startup pass
        // reclaims whatever is left.  A run already in flight finishes
        // unless the forced stop abandons it.
        if cancellation.is_cancelled() {
            return;
        }
        let outcome = tokio::select! {
            biased;
            () = force_stop.cancelled() => return,
            outcome = (coordinator.run)() => outcome,
        };
        pending = outcome == IngressGcOutcome::Partial;
        if pending {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                () = force_stop.cancelled() => return,
                () = tokio::time::sleep(coordinator.partial_retry_delay) => {}
            }
            // Commits during the pass or pause are covered by the pending
            // continuation. Consume their coalesced permit only after the
            // pause, so active traffic cannot bypass the pool's rest period.
            let _ = coordinator.trigger.notified().now_or_never();
        }
    }
}

fn classify_retention_gc_result(
    result: Result<AliasGcOutcome, AliasGcFailure>,
) -> (waddle_xmpp::telemetry::attributes::IngressGcOutcome, usize) {
    use waddle_xmpp::telemetry::attributes::IngressGcOutcome;

    match result {
        Ok(AliasGcOutcome {
            deleted_messages,
            completed: true,
        }) => (IngressGcOutcome::Completed, deleted_messages),
        Ok(AliasGcOutcome {
            deleted_messages,
            completed: false,
        }) => (IngressGcOutcome::Partial, deleted_messages),
        Err(AliasGcFailure {
            deleted_messages,
            error: AliasGcError::DatabaseTimeout { .. },
        }) => (IngressGcOutcome::TimedOut, deleted_messages),
        Err(AliasGcFailure {
            deleted_messages,
            error: AliasGcError::Substrate(_),
        }) => (IngressGcOutcome::Failed, deleted_messages),
    }
}

fn record_retention_gc_result(
    outcome: waddle_xmpp::telemetry::attributes::IngressGcOutcome,
    deleted_messages: usize,
) -> waddle_xmpp::telemetry::attributes::IngressGcOutcome {
    waddle_xmpp::telemetry::reliability::increment_ingress_gc_run(outcome);
    if deleted_messages > 0 {
        waddle_xmpp::telemetry::reliability::add_ingress_gc_reclaimed_messages(
            u64::try_from(deleted_messages).unwrap_or(u64::MAX),
        );
    }
    outcome
}

impl RetentionGcCoordinator {
    pub(crate) fn new(database: Database, uow: IngressUnitOfWork) -> Self {
        Self {
            trigger: Arc::new(Notify::new()),
            run: Arc::new(move || {
                let database = database.clone();
                let uow = uow.clone();
                Box::pin(async move {
                    run_attested_retention_gc(&database, &uow, RetentionGcBudget::DEFAULT).await
                })
            }),
            partial_retry_delay: RETENTION_GC_PARTIAL_RETRY_DELAY,
        }
    }

    pub(crate) fn trigger(&self) {
        self.trigger.notify_one();
    }
}

/// Re-probe the same lineage and epoch policy used by admission before every
/// pass, including the immediate startup pass. An authority may boot unattested,
/// but its collector never reaches the bare database until this gate succeeds.
async fn run_attested_retention_gc(
    database: &Database,
    uow: &IngressUnitOfWork,
    budget: RetentionGcBudget,
) -> waddle_xmpp::telemetry::attributes::IngressGcOutcome {
    use waddle_xmpp::telemetry::attributes::IngressGcOutcome;
    let probe = tokio::time::timeout(budget.hard_deadline, async {
        uow.begin_with_timeouts(budget.lock_timeout, budget.statement_timeout)
            .await?
            .commit()
            .await
    })
    .await;
    match probe {
        Ok(Ok(())) => run_retention_gc_with_budget(database, budget).await,
        Ok(Err(error)) => {
            let outcome = match error {
                IngressUowError::Lineage(_) => IngressGcOutcome::Unattested,
                IngressUowError::Timeout => IngressGcOutcome::TimedOut,
                _ => IngressGcOutcome::Failed,
            };
            tracing::warn!(%error, "ingress retention GC attestation gate failed");
            record_retention_gc_result(outcome, 0)
        }
        Err(_) => record_retention_gc_result(IngressGcOutcome::TimedOut, 0),
    }
}

pub(crate) async fn run_retention_gc_with_budget(
    database: &Database,
    budget: RetentionGcBudget,
) -> waddle_xmpp::telemetry::attributes::IngressGcOutcome {
    let progress = AliasGcProgress::default();
    let result = tokio::time::timeout(
        budget.hard_deadline,
        gc_expired_aliases(
            database,
            Utc::now(),
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + budget.cooperative,
                lock_timeout: budget.lock_timeout,
                statement_timeout: budget.statement_timeout,
                scan_timeout: budget.scan_timeout,
                progress: progress.clone(),
            },
        ),
    )
    .await;
    match result {
        Ok(result) => {
            if let Err(failure) = &result {
                tracing::warn!(%failure, "ingress retention GC failed");
            }
            let (outcome, deleted_messages) = classify_retention_gc_result(result);
            record_retention_gc_result(outcome, deleted_messages)
        }
        Err(error) => {
            tracing::warn!(%error, "ingress retention GC exceeded hard deadline");
            record_retention_gc_result(
                waddle_xmpp::telemetry::attributes::IngressGcOutcome::TimedOut,
                progress.committed(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRunner;
    use crate::ingress_substrate::{record_message, terminalize_message};
    use waddle_xmpp::ingress::{MessageKey, SemanticDigest};
    use waddle_xmpp::telemetry::attributes::IngressGcOutcome;

    #[tokio::test]
    async fn sqlite_retention_gc_collects_expired_terminal_rows() {
        let database = Database::in_memory("ingress-gc")
            .await
            .expect("open SQLite");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrate SQLite");
        let key = MessageKey::new();
        let digest = SemanticDigest::from_storage(1, [7; 32]).expect("digest");
        let mut transaction = database.begin_immediate().await.expect("begin");
        record_message(&mut transaction, key, &digest, None)
            .await
            .expect("record canonical row");
        terminalize_message(
            &mut transaction,
            key,
            Utc::now() - chrono::Duration::days(9),
        )
        .await
        .expect("terminalize expired row");
        transaction.commit().await.expect("commit canonical row");

        assert_eq!(
            run_retention_gc_with_budget(&database, RetentionGcBudget::DEFAULT).await,
            IngressGcOutcome::Completed
        );
        let connection = database.guard().await.expect("read GC result");
        let mut rows = connection
            .query("SELECT COUNT(*) FROM ingress_messages", ())
            .await
            .expect("count messages");
        let count: i64 = rows
            .next()
            .await
            .expect("read count")
            .expect("count row")
            .get(0)
            .expect("decode count");
        assert_eq!(count, 0);
    }

    async fn unattested_boot_preserves_retention_rows(database: Database) {
        let enrolled = crate::ingress::test_lineage_config();
        crate::db::lineage::enroll(&database, &enrolled)
            .await
            .expect("enroll");
        let key = MessageKey::new();
        let digest = SemanticDigest::from_storage(1, [8; 32]).expect("digest");
        let mut transaction = database.begin_immediate().await.expect("begin");
        record_message(&mut transaction, key, &digest, None)
            .await
            .expect("message");
        terminalize_message(
            &mut transaction,
            key,
            Utc::now() - chrono::Duration::days(9),
        )
        .await
        .expect("terminalize");
        transaction.commit().await.expect("commit");
        let wrong_lineage = crate::config::LineageConfig {
            deployment_uuid: Some(
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a90002"
                    .parse()
                    .expect("different deployment UUID"),
            ),
            action: None,
        };
        let authority = crate::ingress::IngressAuthority::new(
            crate::ingress::IngressConfig::default(),
            database.clone(),
            wrong_lineage,
            #[cfg(feature = "clustering")]
            None,
        )
        .await
        .expect("unattested authority boots");
        assert_eq!((authority.gc.run)().await, IngressGcOutcome::Unattested);
        let connection = database.guard().await.expect("read");
        let mut rows = connection
            .query("SELECT COUNT(*) FROM ingress_messages", ())
            .await
            .expect("count");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("count row")
            .get(0)
            .expect("count value");
        assert_eq!(count, 1);
        drop(rows);
        drop(connection);
        authority.cancellation.cancel();
        if let Some(task) = authority.gc_task.lock().await.take() {
            task.await.expect("GC exits");
        }
        // The exact same retained row is eligible once the policy attests.
        let uow = IngressUnitOfWork::open(database.clone(), enrolled).expect("uow");
        assert_eq!(
            run_attested_retention_gc(&database, &uow, RetentionGcBudget::DEFAULT).await,
            IngressGcOutcome::Completed
        );
        let connection = database.guard().await.expect("read");
        let mut rows = connection
            .query("SELECT COUNT(*) FROM ingress_messages", ())
            .await
            .expect("count");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("count row")
            .get(0)
            .expect("count value");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn sqlite_ingress_unattested_boot_gc_reclaims_nothing() {
        let directory = tempfile::tempdir().expect("SQLite directory");
        let config = crate::db::DatabaseConfig::new(
            crate::db::DatabaseDriver::Sqlite,
            directory
                .path()
                .join("gc.db")
                .to_string_lossy()
                .into_owned(),
        );
        let database = Database::from_config("unattested-gc", &config)
            .await
            .expect("database");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrate");
        unattested_boot_preserves_retention_rows(database).await;
    }

    #[tokio::test]
    async fn postgres_ingress_unattested_boot_gc_reclaims_nothing() {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping postgres_ingress_unattested_boot_gc_reclaims_nothing: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("postgres admin");
        let schema = format!("ingress_gc_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("schema");
        let mut url = url::Url::parse(&database_url).expect("URL");
        url.query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
        let config =
            crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, url.to_string());
        let database = Database::from_config("unattested-gc", &config)
            .await
            .expect("database");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrate");
        unattested_boot_preserves_retention_rows(database).await;
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop schema");
    }

    #[tokio::test]
    async fn retention_coordinator_cancelled_before_start_does_not_run() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let coordinator = RetentionGcCoordinator {
            trigger: Arc::new(Notify::new()),
            run: Arc::new(|| panic!("cancelled coordinator must not start a pass")),
            partial_retry_delay: RETENTION_GC_PARTIAL_RETRY_DELAY,
        };
        run_retention_gc_coordinator(coordinator, cancellation, CancellationToken::new()).await;
    }

    fn partial_coordinator_with_pending_trigger() -> (
        RetentionGcCoordinator,
        tokio::sync::mpsc::UnboundedReceiver<usize>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let trigger = Arc::new(Notify::new());
        let run_trigger = trigger.clone();
        let runs = AtomicUsize::new(0);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let coordinator = RetentionGcCoordinator {
            trigger,
            run: Arc::new(move || {
                let run = runs.fetch_add(1, Ordering::SeqCst);
                sender.send(run).expect("test observes every GC pass");
                let outcome = if run == 0 {
                    // A commit while the first pass is running leaves a
                    // stored permit that must not short-circuit the pause.
                    run_trigger.notify_one();
                    IngressGcOutcome::Partial
                } else {
                    IngressGcOutcome::Completed
                };
                Box::pin(async move { outcome })
            }),
            partial_retry_delay: Duration::from_secs(1),
        };
        (coordinator, receiver)
    }

    #[tokio::test(start_paused = true)]
    async fn retention_coordinator_partial_waits_with_pending_trigger_and_coalesces_it() {
        let (coordinator, mut runs) = partial_coordinator_with_pending_trigger();
        let trigger = coordinator.trigger.clone();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(run_retention_gc_coordinator(
            coordinator,
            cancellation.clone(),
            CancellationToken::new(),
        ));
        assert_eq!(runs.recv().await, Some(0));
        tokio::time::advance(Duration::from_millis(999)).await;
        tokio::task::yield_now().await;
        assert!(runs.try_recv().is_err(), "pending trigger bypassed pause");
        // A later trigger in the pause must coalesce with the continuation.
        trigger.notify_one();
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(runs.recv().await, Some(1));
        tokio::task::yield_now().await;
        assert!(runs.try_recv().is_err(), "trigger caused an extra pass");
        cancellation.cancel();
        task.await.expect("coordinator exits");
    }

    #[tokio::test(start_paused = true)]
    async fn retention_coordinator_partial_pause_cancellation_exits_promptly() {
        for force in [false, true] {
            let (coordinator, mut runs) = partial_coordinator_with_pending_trigger();
            let cancellation = CancellationToken::new();
            let force_stop = CancellationToken::new();
            let task = tokio::spawn(run_retention_gc_coordinator(
                coordinator,
                cancellation.clone(),
                force_stop.clone(),
            ));
            assert_eq!(runs.recv().await, Some(0));
            let started = tokio::time::Instant::now();
            if force {
                force_stop.cancel();
            } else {
                cancellation.cancel();
            }
            task.await.expect("cancellation interrupts the pause");
            assert_eq!(started.elapsed(), Duration::ZERO);
            assert!(runs.try_recv().is_err(), "cancellation started a new pass");
        }
    }
}
