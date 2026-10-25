//! Periodic, bounded repair of terminal proofs followed by retention collection.

use crate::ingress_substrate::TerminalizeOutcome;
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::telemetry::attributes::{
    IngressGcOutcome, IngressMaintenancePhase, ObserverHistoryTable,
};

use crate::db::Database;
use crate::ingress_substrate::{
    receipt_complete_nonterminal_keys, set_local_transaction_timeouts,
    unreceipted_nonterminal_candidates, AliasGcProgress, EffectReceiptKind, IngressSubstrateError,
    RecoveryCandidate, RecoveryEvidence,
};
use crate::ingress_uow::{
    IngressUnitOfWork, IngressUowError, ObservationError, ObserverRetentionBatch,
    RoomObservationRepository,
};

use super::gc::{run_retention_gc_with_budget, RetentionGcBudget};
use super::recovery_executor::AttemptClassification;
use super::RecoveryEnvironment;

#[path = "maintenance_stalls.rs"]
mod stalls;
use stalls::{RecoveryAttempt, StalledRows, Suppression, UnsupportedRows};

#[path = "maintenance_waits.rs"]
mod waits;

/// Attest before maintenance; scan and candidate transactions never overlap,
/// so even a pool of one admits foreground work between operations. Each phase
/// has an independent timeout inside the whole-pass hard deadline.
#[cfg(test)]
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

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;

mod implementation;
pub(crate) use implementation::collect_observer_history;
#[cfg(test)]
use implementation::evidence_now;
#[cfg(test)]
use implementation::recover_candidates;
#[cfg(test)]
use implementation::requeue_failed_accounting;
pub(super) use implementation::run_maintenance_pass_with_cursor;
#[cfg(test)]
use implementation::spawn_recovery_accounting;
pub(crate) use implementation::MaintenanceBudget;
pub(super) use implementation::MaintenanceCursor;
pub(crate) use implementation::MaintenanceOutcome;
#[cfg(test)]
use implementation::RECOVERY_ACCOUNTING_QUEUE_BOUND;
