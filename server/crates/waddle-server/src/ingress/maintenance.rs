//! Periodic, bounded repair of terminal proofs followed by retention collection.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::telemetry::attributes::{IngressGcOutcome, IngressMaintenancePhase};

use crate::db::Database;
use crate::ingress_substrate::{
    receipt_complete_nonterminal_keys, set_local_transaction_timeouts,
    unreceipted_nonterminal_candidates, EffectReceiptKind, IngressSubstrateError,
    RecoveryCandidate, RecoveryEvidence,
};
use crate::ingress_uow::{IngressUnitOfWork, IngressUowError};

use super::gc::{run_retention_gc_with_budget, RetentionGcBudget};
use super::RecoveryEnvironment;

type MaintenancePosition = (DateTime<Utc>, MessageKey);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaintenanceOutcome {
    Complete,
    Partial,
    Failed,
    TimedOut,
}

#[derive(Clone, Copy)]
pub(crate) struct MaintenanceBudget {
    pub(crate) terminalization: Duration,
    pub(crate) recovery: Duration,
    pub(crate) recovery_row: Duration,
    pub(crate) recovery_page_size: u32,
    pub(crate) recovery_max_attempts: u32,
    pub(crate) retention: RetentionGcBudget,
    pub(crate) hard_deadline: Duration,
    pub(crate) page_size: u32,
    pub(crate) max_pages: u32,
    pub(crate) grace: chrono::Duration,
}

impl MaintenanceBudget {
    pub(crate) const DEFAULT: Self = Self {
        terminalization: Duration::from_secs(2),
        recovery: Duration::from_secs(4),
        recovery_row: Duration::from_secs(1),
        recovery_page_size: 64,
        recovery_max_attempts: 64,
        retention: RetentionGcBudget::DEFAULT,
        hard_deadline: Duration::from_secs(13),
        page_size: 256,
        max_pages: 4,
        grace: chrono::Duration::seconds(60),
    };
}

#[derive(Clone, Default)]
pub(super) struct MaintenanceCursor {
    after: Arc<Mutex<Option<MaintenancePosition>>>,
    recovery_after: Arc<Mutex<Option<MaintenancePosition>>>,
    recovery_unsupported: Arc<Mutex<UnsupportedRows>>,
    /// Rows attempted this pass, with the receipt count the scan observed.
    /// Drained by one detached worker after the phase so the credit survives
    /// the phase deadline without fanning out pooled reads.
    recovery_accounting: Arc<Mutex<Vec<(MessageKey, u32)>>>,
    /// Held by the accounting worker; continuations of a partial pass spawn
    /// their own worker, and this serializes them to one pooled read at a time.
    recovery_accounting_worker: Arc<tokio::sync::Mutex<()>>,
    /// Highest receipt total already credited per row, so a delayed worker
    /// and a later pass's worker never credit the same receipt twice.
    recovery_credited: Arc<Mutex<CreditedRows>>,
}

impl MaintenanceCursor {
    fn get(&self) -> Option<MaintenancePosition> {
        *self
            .after
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set(&self, after: Option<MaintenancePosition>) {
        *self
            .after
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = after;
    }
}

fn record(phase: IngressMaintenancePhase, outcome: MaintenanceOutcome) -> MaintenanceOutcome {
    use waddle_xmpp::telemetry::attributes::IngressMaintenanceOutcome;

    let attribute = match outcome {
        MaintenanceOutcome::Complete => IngressMaintenanceOutcome::Complete,
        MaintenanceOutcome::Partial => IngressMaintenanceOutcome::Partial,
        MaintenanceOutcome::Failed => IngressMaintenanceOutcome::Failed,
        MaintenanceOutcome::TimedOut => IngressMaintenanceOutcome::TimedOut,
    };
    waddle_xmpp::telemetry::reliability::increment_ingress_maintenance_run(phase, attribute);
    outcome
}

/// Attest before maintenance; scan and candidate transactions never overlap,
/// so even a pool of one admits foreground work between operations. Each phase
/// has an independent timeout inside the whole-pass hard deadline.
pub(crate) async fn run_maintenance_pass(
    database: &Database,
    uow: &IngressUnitOfWork,
    budget: MaintenanceBudget,
    environment: Option<Arc<dyn RecoveryEnvironment>>,
) -> MaintenanceOutcome {
    run_maintenance_pass_with_cursor(
        database,
        uow,
        budget,
        &MaintenanceCursor::default(),
        environment,
    )
    .await
}

pub(super) async fn run_maintenance_pass_with_cursor(
    database: &Database,
    uow: &IngressUnitOfWork,
    budget: MaintenanceBudget,
    cursor: &MaintenanceCursor,
    environment: Option<Arc<dyn RecoveryEnvironment>>,
) -> MaintenanceOutcome {
    let result = tokio::time::timeout(budget.hard_deadline, async {
        let attestation = async {
            uow.begin_with_timeouts(
                budget.retention.lock_timeout,
                budget.retention.statement_timeout,
            )
            .await?
            .commit()
            .await
        }
        .await;
        if let Err(error) = attestation {
            tracing::warn!(%error, "ingress maintenance attestation gate failed");
            let outcome = failure_outcome(&error);
            let gc_outcome = if matches!(error, IngressUowError::Lineage(_)) {
                IngressGcOutcome::Unattested
            } else if outcome == MaintenanceOutcome::TimedOut {
                IngressGcOutcome::TimedOut
            } else {
                IngressGcOutcome::Failed
            };
            waddle_xmpp::telemetry::reliability::increment_ingress_gc_run(gc_outcome);
            return outcome;
        }
        let terminalization = match tokio::time::timeout(
            budget.terminalization,
            terminalize_candidates(database, uow, budget, cursor),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => MaintenanceOutcome::TimedOut,
        };
        record(IngressMaintenancePhase::Terminalization, terminalization);
        let recovery = if let Some(environment) = environment {
            let outcome = tokio::time::timeout(
                budget.recovery,
                recover_candidates(database, uow, budget, cursor, environment.as_ref()),
            )
            .await
            .unwrap_or(MaintenanceOutcome::TimedOut);
            spawn_recovery_accounting(database, cursor);
            record(IngressMaintenancePhase::Recovery, outcome)
        } else {
            MaintenanceOutcome::Complete
        };
        // The GC helper owns the retention phase timeout and preserves its
        // committed-progress counter when that timeout fires.
        let retention = match run_retention_gc_with_budget(database, budget.retention).await {
            IngressGcOutcome::Completed => MaintenanceOutcome::Complete,
            IngressGcOutcome::Partial => MaintenanceOutcome::Partial,
            IngressGcOutcome::TimedOut => MaintenanceOutcome::TimedOut,
            IngressGcOutcome::Failed | IngressGcOutcome::Unattested => MaintenanceOutcome::Failed,
        };
        record(IngressMaintenancePhase::RetentionGc, retention);
        combine(combine(terminalization, recovery), retention)
    })
    .await
    .unwrap_or(MaintenanceOutcome::TimedOut);
    record(IngressMaintenancePhase::Pass, result)
}

fn failure_outcome(error: &IngressUowError) -> MaintenanceOutcome {
    match error {
        IngressUowError::Timeout | IngressUowError::Substrate(IngressSubstrateError::Timeout) => {
            MaintenanceOutcome::TimedOut
        }
        _ => MaintenanceOutcome::Failed,
    }
}

fn combine(left: MaintenanceOutcome, right: MaintenanceOutcome) -> MaintenanceOutcome {
    for outcome in [
        MaintenanceOutcome::TimedOut,
        MaintenanceOutcome::Failed,
        MaintenanceOutcome::Partial,
    ] {
        if left == outcome || right == outcome {
            return outcome;
        }
    }
    MaintenanceOutcome::Complete
}

async fn candidate_page(
    database: &Database,
    budget: MaintenanceBudget,
    after: Option<(DateTime<Utc>, MessageKey)>,
    older_than: DateTime<Utc>,
) -> Result<Vec<(DateTime<Utc>, MessageKey)>, IngressUowError> {
    let mut transaction = tokio::time::timeout(budget.retention.lock_timeout, database.begin())
        .await
        .map_err(|_| IngressUowError::Timeout)??;
    if !set_local_transaction_timeouts(
        &mut transaction,
        budget.retention.lock_timeout,
        budget.retention.scan_timeout,
    )
    .await?
    {
        return Err(IngressUowError::TransactionBoundsUnproven);
    }
    let candidates =
        receipt_complete_nonterminal_keys(&mut transaction, after, older_than, budget.page_size)
            .await?;
    transaction.commit().await?;
    Ok(candidates)
}

async fn terminalize_candidates(
    database: &Database,
    uow: &IngressUnitOfWork,
    budget: MaintenanceBudget,
    cursor: &MaintenanceCursor,
) -> MaintenanceOutcome {
    let older_than = Utc::now() - budget.grace;
    let mut after = cursor.get();
    let resumed = after.is_some();
    let mut outcome = MaintenanceOutcome::Complete;
    for _ in 0..budget.max_pages {
        let candidates = match candidate_page(database, budget, after, older_than).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "ingress maintenance candidate scan failed");
                return failure_outcome(&error);
            }
        };
        let exhausted = candidates.len() < budget.page_size as usize;
        for (created_at, key) in candidates {
            // Persist progress before attempting the row. If the phase timeout
            // cancels this future while the row is contended, the continuation
            // starts after it and cannot starve later candidates.
            after = Some((created_at, key));
            cursor.set(after);
            match super::execute::terminalize_if_complete_outcome(uow, key).await {
                Ok(Some(crate::ingress_substrate::TerminalizeOutcome::Terminalized)) => {
                    waddle_xmpp::telemetry::reliability::add_ingress_maintenance_terminalized_messages(1);
                }
                Ok(
                    Some(
                        crate::ingress_substrate::TerminalizeOutcome::AlreadyTerminal
                        | crate::ingress_substrate::TerminalizeOutcome::MessageVanished,
                    )
                    | None,
                ) => {}
                Err(error) => {
                    tracing::warn!(%error, "ingress maintenance terminalization deferred");
                    outcome = MaintenanceOutcome::Partial;
                }
            }
            tokio::task::yield_now().await;
        }
        if exhausted {
            cursor.set(None);
            // A resumed scan reached the tail. Schedule one wraparound pass so
            // skipped/contended rows before the saved cursor are retried.
            return if resumed {
                MaintenanceOutcome::Partial
            } else {
                outcome
            };
        }
    }
    MaintenanceOutcome::Partial
}

/// FIFO bounds memory; evidence changes always invalidate an unsupported evaluation.
#[derive(Default)]
struct UnsupportedRows {
    evidence: HashMap<MessageKey, RecoveryEvidence>,
    order: VecDeque<MessageKey>,
}

impl UnsupportedRows {
    fn contains(&self, candidate: &RecoveryCandidate) -> bool {
        self.evidence.get(&candidate.key) == Some(&candidate.evidence)
    }

    fn remove(&mut self, key: MessageKey) {
        if self.evidence.remove(&key).is_some() {
            self.order.retain(|stored| *stored != key);
        }
    }

    fn insert(&mut self, candidate: RecoveryCandidate) {
        self.remove(candidate.key);
        if self.order.len() == 4096 {
            if let Some(oldest) = self.order.pop_front() {
                self.evidence.remove(&oldest);
            }
        }
        self.order.push_back(candidate.key);
        self.evidence.insert(candidate.key, candidate.evidence);
    }
}

fn recoverable_receipt_kinds() -> Vec<EffectReceiptKind> {
    super::recovery_rebuild::RECOVERABLE_KINDS
        .iter()
        .map(|kind| EffectReceiptKind::from_storage(kind.storage_tag()))
        .collect()
}

async fn recovery_page(
    database: &Database,
    budget: MaintenanceBudget,
    after: Option<MaintenancePosition>,
    older_than: DateTime<Utc>,
) -> Result<Vec<RecoveryCandidate>, IngressUowError> {
    let mut tx = tokio::time::timeout(budget.retention.lock_timeout, database.begin())
        .await
        .map_err(|_| IngressUowError::Timeout)??;
    if !set_local_transaction_timeouts(
        &mut tx,
        budget.retention.lock_timeout,
        budget.retention.scan_timeout,
    )
    .await?
    {
        return Err(IngressUowError::TransactionBoundsUnproven);
    }
    let candidates = unreceipted_nonterminal_candidates(
        &mut tx,
        after,
        older_than,
        &recoverable_receipt_kinds(),
        budget.recovery_page_size,
    )
    .await?;
    tx.commit().await?;
    Ok(candidates)
}

async fn recover_candidates(
    database: &Database,
    uow: &IngressUnitOfWork,
    budget: MaintenanceBudget,
    cursor: &MaintenanceCursor,
    environment: &dyn RecoveryEnvironment,
) -> MaintenanceOutcome {
    let deps = environment.recovery_deps();
    let older_than = Utc::now() - budget.grace;
    let mut after = *cursor
        .recovery_after
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let resumed = after.is_some();
    let mut outcome = MaintenanceOutcome::Complete;
    let mut attempts = 0;
    if budget.recovery_page_size == 0 {
        return MaintenanceOutcome::Partial;
    }
    loop {
        let candidates = match recovery_page(database, budget, after, older_than).await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "ingress recovery candidate scan failed");
                return failure_outcome(&error);
            }
        };
        let exhausted = candidates.len() < budget.recovery_page_size as usize;
        for candidate in candidates {
            if !candidate.recoverable {
                // Paged so the cursor moves past it; nothing here to attempt.
                *cursor
                    .recovery_after
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((candidate.created_at, candidate.key));
                after = Some((candidate.created_at, candidate.key));
                continue;
            }
            let unsupported = cursor
                .recovery_unsupported
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&candidate);
            if !unsupported && attempts >= budget.recovery_max_attempts {
                return MaintenanceOutcome::Partial;
            }
            // Advance before awaiting a row, but never past an unattempted budget boundary.
            after = Some((candidate.created_at, candidate.key));
            *cursor
                .recovery_after
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = after;
            if unsupported {
                continue;
            }
            cursor
                .recovery_unsupported
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(candidate.key);
            attempts += 1;
            // Queue accounting before the attempt: the phase deadline can cancel
            // this await after an effect committed, and the credit must survive.
            cursor
                .recovery_accounting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((candidate.key, candidate.evidence.receipts));
            let deadline = tokio::time::Instant::now() + budget.recovery_row;
            let result = tokio::time::timeout_at(
                deadline,
                super::recovery_executor::recover_row(
                    database,
                    uow,
                    &deps,
                    candidate.key,
                    deadline,
                ),
            )
            .await;
            match record_recovery_result(candidate, result) {
                RowDisposition::Deferred => outcome = MaintenanceOutcome::Partial,
                RowDisposition::Done => {}
                RowDisposition::Unsupported => {
                    // Cache the evidence as it stands after this attempt: a
                    // sibling that settled during execution must not make the
                    // next scan re-invoke the warning-only observer.
                    match tokio::time::timeout(
                        RECOVERY_ACCOUNTING_BUDGET,
                        receipts_now(database, candidate.key),
                    )
                    .await
                    {
                        Ok(Ok(receipts)) => {
                            let mut cached = candidate;
                            cached.evidence.receipts = u32::try_from(receipts).unwrap_or(u32::MAX);
                            cursor
                                .recovery_unsupported
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(cached);
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(%error, key = ?candidate.key, "ingress recovery cache refresh failed");
                        }
                        Err(_) => {
                            tracing::debug!(key = ?candidate.key, "ingress recovery cache refresh timed out");
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
        if exhausted {
            *cursor
                .recovery_after
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            return if resumed {
                MaintenanceOutcome::Partial
            } else {
                outcome
            };
        }
    }
}

/// Credit recovered obligations for every row attempted this pass: the
/// receipts that appeared since the scan observed the row. One detached task
/// reads them sequentially (one pooled connection at a time) so the phase
/// deadline cannot cancel the credit and telemetry cannot crowd out the
/// dedicated ingress pool. A concurrent client retransmission settling the
/// same row in this window is attributed here too; the counter is progress
/// telemetry, not an audit log.
fn spawn_recovery_accounting(database: &Database, cursor: &MaintenanceCursor) {
    let attempted: Vec<(MessageKey, u32)> = std::mem::take(
        &mut *cursor
            .recovery_accounting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if attempted.is_empty() {
        return;
    }
    let database = database.clone();
    let worker = Arc::clone(&cursor.recovery_accounting_worker);
    let credited = Arc::clone(&cursor.recovery_credited);
    tokio::spawn(async move {
        use waddle_xmpp::telemetry::reliability::increment_ingress_maintenance_recovered_obligations;
        // One accounting read at a time across passes: a partial pass's
        // continuation (gc.rs backoff) must not stack a second worker onto the
        // dedicated ingress pool while an earlier one is still draining.
        let _serial = worker.lock().await;
        let mut recovered = 0;
        for (key, before) in attempted {
            match tokio::time::timeout(RECOVERY_ACCOUNTING_BUDGET, receipts_now(&database, key))
                .await
            {
                Ok(Ok(now)) => {
                    // Credit only above the higher of this pass's scan baseline
                    // and what any earlier worker already credited for the row.
                    recovered += credited
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .credit(key, u64::from(before), now);
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, ?key, "ingress recovery accounting failed");
                }
                Err(_) => {
                    tracing::warn!(?key, "ingress recovery accounting timed out");
                }
            }
        }
        if recovered > 0 {
            increment_ingress_maintenance_recovered_obligations(recovered);
        }
    });
}

/// Per-row receipt totals already credited to `recovered_obligations`.
/// Bounded FIFO like the unsupported cache; eviction only risks re-crediting
/// a row after 4096 other rows were accounted, never losing credit.
#[derive(Default)]
struct CreditedRows {
    totals: HashMap<MessageKey, u64>,
    order: VecDeque<MessageKey>,
}

impl CreditedRows {
    fn credit(&mut self, key: MessageKey, before: u64, now: u64) -> u64 {
        let base = before.max(self.totals.get(&key).copied().unwrap_or(0));
        let delta = now.saturating_sub(base);
        if self.totals.insert(key, now.max(base)).is_none() {
            if self.order.len() == 4096 {
                if let Some(oldest) = self.order.pop_front() {
                    self.totals.remove(&oldest);
                }
            }
            self.order.push_back(key);
        }
        delta
    }
}

/// Upper bound for the detached receipt-count read behind the recovered counter.
const RECOVERY_ACCOUNTING_BUDGET: Duration = Duration::from_secs(1);

/// Account for the row after its future finished, failed or was cancelled by
/// the row deadline: recovered obligations are the receipts that appeared since
/// the scan observed the row, so credit survives cancellation mid-execution.
/// A concurrent client retransmission settling the same row in this window is
/// attributed here too; the counter is progress telemetry, not an audit log.
/// What the maintenance loop does with a row after its attempt.
enum RowDisposition {
    /// Deferred by an error or the row deadline; the phase reports `Partial`.
    Deferred,
    Done,
    /// Nothing left on the row can progress; cache it with fresh evidence.
    Unsupported,
}

fn record_recovery_result(
    candidate: RecoveryCandidate,
    result: Result<
        Result<super::recovery_executor::RowRecovery, IngressUowError>,
        tokio::time::error::Elapsed,
    >,
) -> RowDisposition {
    use super::recovery_executor::RowRecovery;
    use waddle_xmpp::telemetry::reliability::increment_ingress_maintenance_unrecoverable_obligations;
    match result {
        Ok(Ok(RowRecovery::Executed {
            unrecoverable,
            unsupported,
        })) => {
            for kind in unrecoverable {
                increment_ingress_maintenance_unrecoverable_obligations(1, kind);
            }
            if unsupported {
                RowDisposition::Unsupported
            } else {
                RowDisposition::Done
            }
        }
        Ok(Ok(RowRecovery::Vanished | RowRecovery::NothingPending)) => RowDisposition::Done,
        Ok(Err(error)) => {
            tracing::warn!(%error, key = ?candidate.key, "ingress recovery row deferred");
            RowDisposition::Deferred
        }
        Err(_) => {
            tracing::debug!(key = ?candidate.key, "ingress recovery row deadline elapsed");
            RowDisposition::Deferred
        }
    }
}

async fn receipts_now(database: &Database, key: MessageKey) -> Result<u64, IngressUowError> {
    crate::ingress_uow::EffectReceiptRepository::count_pooled(database, key).await
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
