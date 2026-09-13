//! Periodic, bounded repair of terminal proofs followed by retention collection.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::telemetry::attributes::{IngressGcOutcome, IngressMaintenancePhase};

use crate::db::Database;
use crate::ingress_substrate::{
    receipt_complete_nonterminal_keys, set_local_transaction_timeouts, IngressSubstrateError,
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
    pub(crate) retention: RetentionGcBudget,
    pub(crate) hard_deadline: Duration,
    pub(crate) page_size: u32,
    pub(crate) max_pages: u32,
    pub(crate) grace: chrono::Duration,
}

impl MaintenanceBudget {
    pub(crate) const DEFAULT: Self = Self {
        terminalization: Duration::from_secs(2),
        retention: RetentionGcBudget::DEFAULT,
        hard_deadline: Duration::from_secs(9),
        page_size: 256,
        max_pages: 4,
        grace: chrono::Duration::seconds(60),
    };
}

#[derive(Clone, Default)]
pub(super) struct MaintenanceCursor {
    after: Arc<Mutex<Option<MaintenancePosition>>>,
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

/// Attest before either phase; scan and candidate transactions never overlap,
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
    _environment: Option<Arc<dyn RecoveryEnvironment>>,
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
        // The GC helper owns the retention phase timeout and preserves its
        // committed-progress counter when that timeout fires.
        let retention = match run_retention_gc_with_budget(database, budget.retention).await {
            IngressGcOutcome::Completed => MaintenanceOutcome::Complete,
            IngressGcOutcome::Partial => MaintenanceOutcome::Partial,
            IngressGcOutcome::TimedOut => MaintenanceOutcome::TimedOut,
            IngressGcOutcome::Failed | IngressGcOutcome::Unattested => MaintenanceOutcome::Failed,
        };
        record(IngressMaintenancePhase::RetentionGc, retention);
        combine(terminalization, retention)
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

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
