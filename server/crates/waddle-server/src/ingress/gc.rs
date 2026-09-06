//! Bounded retention collection shared by ingress authority and the shadow observer.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use chrono::Utc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::db::Database;
use crate::ingress_substrate::{
    gc_expired_aliases, AliasGcBudget, AliasGcError, AliasGcFailure, AliasGcOutcome,
    AliasGcProgress,
};

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
                () = coordinator.trigger.notified() => {}
                () = tokio::time::sleep(coordinator.partial_retry_delay) => {}
            }
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
    pub(crate) fn new(database: Database) -> Self {
        Self {
            trigger: Arc::new(Notify::new()),
            run: Arc::new(move || {
                let database = database.clone();
                Box::pin(async move {
                    run_retention_gc_with_budget(&database, RetentionGcBudget::DEFAULT).await
                })
            }),
            partial_retry_delay: RETENTION_GC_PARTIAL_RETRY_DELAY,
        }
    }

    pub(crate) fn trigger(&self) {
        self.trigger.notify_one();
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
}
