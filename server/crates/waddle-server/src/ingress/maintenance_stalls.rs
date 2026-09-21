//! Bounded, process-local evidence and suppression for maintenance recovery.

use std::collections::{HashMap, VecDeque};

use tokio::time::Instant;
use waddle_xmpp::{
    ingress::{IngressEffectKind, MessageKey},
    telemetry::{
        attributes::IngressUnrecoverableReason,
        reliability::increment_ingress_maintenance_unrecoverable_obligations,
    },
};

use super::{AttemptClassification, MaintenanceBudget};
use crate::ingress_substrate::RecoveryEvidence;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecoveryAttempt {
    pub(super) key: MessageKey,
    /// When the pass attempted the row. The detached accounting worker
    /// serializes its reads, so it can observe an attempt much later than it
    /// happened; the streak must measure attempt times, not accounting times.
    pub(super) attempted_at: Instant,
    pub(super) observed: RecoveryEvidence,
    pub(super) classification: AttemptClassification,
    pub(super) pending: Vec<IngressEffectKind>,
    pub(super) generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Suppression {
    Unsupported,
    StalledUntil(Instant),
}

/// FIFO bounds memory; evidence changes always invalidate a suppression.
#[derive(Default)]
pub(super) struct UnsupportedRows {
    evidence: HashMap<MessageKey, (RecoveryEvidence, Suppression)>,
    order: VecDeque<MessageKey>,
}

impl UnsupportedRows {
    pub(super) fn get(
        &mut self,
        key: MessageKey,
        evidence: RecoveryEvidence,
    ) -> Option<Suppression> {
        let (stored, suppression) = *self.evidence.get(&key)?;
        if stored != evidence
            || matches!(suppression, Suppression::StalledUntil(until) if until <= Instant::now())
        {
            self.remove(key);
            return None;
        }
        Some(suppression)
    }

    pub(super) fn remove(&mut self, key: MessageKey) {
        if self.evidence.remove(&key).is_some() {
            self.order.retain(|stored| *stored != key);
        }
    }

    fn remove_stalled(&mut self, key: MessageKey) {
        if matches!(
            self.evidence.get(&key),
            Some((_, Suppression::StalledUntil(_)))
        ) {
            self.remove(key);
        }
    }

    pub(super) fn insert(
        &mut self,
        key: MessageKey,
        evidence: RecoveryEvidence,
        suppression: Suppression,
    ) {
        self.remove(key);
        if self.order.len() == 4096 {
            if let Some(oldest) = self.order.pop_front() {
                self.evidence.remove(&oldest);
            }
        }
        self.order.push_back(key);
        self.evidence.insert(key, (evidence, suppression));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassificationEpisode {
    Unclassified,
    Classified,
}

/// What one accounted attempt asks the maintenance worker to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StallVerdict {
    /// Nothing to decide: the row made progress, its attempt was
    /// inconclusive, or its streak has not reached the threshold.
    Open,
    /// The streak just reached the threshold. The row is neither suppressed
    /// nor classified yet: the caller gets one bounded repair attempt, then
    /// calls [`StalledRows::repaired`] or [`StalledRows::park`].
    Stalled,
}

struct StalledRow {
    evidence: RecoveryEvidence,
    consecutive: u32,
    classified: ClassificationEpisode,
    generation: u64,
    /// When this row last contributed a sample to the streak, so a burst of
    /// commit-triggered passes counts once rather than once per pass.
    last_counted: Option<Instant>,
}

#[derive(Default)]
pub(super) struct StalledRows {
    rows: HashMap<MessageKey, StalledRow>,
    order: VecDeque<MessageKey>,
}

impl StalledRows {
    /// Account for one attempt, without deciding the row's fate: parking and
    /// classification are the caller's, after its repair attempt.
    pub(super) fn account(
        &mut self,
        attempt: &RecoveryAttempt,
        fresh: RecoveryEvidence,
        budget: MaintenanceBudget,
        suppressed: &mut UnsupportedRows,
    ) -> StallVerdict {
        if let Some(previous) = self.rows.get(&attempt.key) {
            if attempt.generation <= previous.generation {
                return StallVerdict::Open;
            }
        } else {
            if self.order.len() == 4096 {
                if let Some(oldest) = self.order.pop_front() {
                    self.rows.remove(&oldest);
                }
            }
            self.order.push_back(attempt.key);
        }
        let row = self.rows.entry(attempt.key).or_insert(StalledRow {
            evidence: attempt.observed,
            consecutive: 0,
            classified: ClassificationEpisode::Unclassified,
            generation: 0,
            last_counted: None,
        });
        row.generation = attempt.generation;
        if row.evidence != attempt.observed || fresh != attempt.observed {
            row.consecutive = 0;
            row.classified = ClassificationEpisode::Unclassified;
            row.last_counted = None;
            suppressed.remove_stalled(attempt.key);
        }
        row.evidence = fresh;
        if attempt.classification == AttemptClassification::Inconclusive
            || fresh != attempt.observed
        {
            row.consecutive = 0;
            row.last_counted = None;
            // A newer attempt can start before an older accounting worker parks
            // the row. Its uncertainty invalidates that older parking decision.
            suppressed.remove_stalled(attempt.key);
            return StallVerdict::Open;
        }
        // Maintenance also runs at startup and after every committed decision.
        // Only one attempt per sample interval counts, so the streak measures
        // elapsed time without progress rather than commit volume.
        if let Some(last) = row.last_counted {
            if attempt.attempted_at.saturating_duration_since(last)
                < budget.recovery_stall_sample_interval
            {
                return StallVerdict::Open;
            }
        }
        row.last_counted = Some(attempt.attempted_at);
        row.consecutive = row.consecutive.saturating_add(1);
        if row.consecutive < budget.recovery_stall_attempts {
            return StallVerdict::Open;
        }
        StallVerdict::Stalled
    }

    /// Suppress the row for a cooldown and classify its pending obligations:
    /// nothing this node can do moves it until its evidence changes.
    pub(super) fn park(
        &mut self,
        attempt: &RecoveryAttempt,
        fresh: RecoveryEvidence,
        budget: MaintenanceBudget,
        suppressed: &mut UnsupportedRows,
    ) {
        suppressed.insert(
            attempt.key,
            fresh,
            Suppression::StalledUntil(Instant::now() + budget.recovery_stall_cooldown),
        );
        let Some(row) = self.rows.get_mut(&attempt.key) else {
            return;
        };
        if row.classified == ClassificationEpisode::Classified {
            return;
        }
        row.classified = ClassificationEpisode::Classified;
        for kind in &attempt.pending {
            increment_ingress_maintenance_unrecoverable_obligations(
                1,
                *kind,
                IngressUnrecoverableReason::NoDurableProgress,
            );
        }
    }

    /// A repair made durable progress on a stalled row. The streak restarts
    /// exactly as a delivered copy restarts it, so the row is neither
    /// suppressed nor classified `no_durable_progress` for this episode.
    pub(super) fn repaired(&mut self, key: MessageKey, suppressed: &mut UnsupportedRows) {
        suppressed.remove_stalled(key);
        let Some(row) = self.rows.get_mut(&key) else {
            return;
        };
        row.consecutive = 0;
        row.last_counted = None;
        row.classified = ClassificationEpisode::Unclassified;
    }

    /// Account for one attempt and park the row when that completes a stall
    /// streak. The maintenance worker splits the two so it can attempt a
    /// ghost-occupant repair in between.
    #[cfg(test)]
    pub(super) fn account_and_park(
        &mut self,
        attempt: &RecoveryAttempt,
        fresh: RecoveryEvidence,
        budget: MaintenanceBudget,
        suppressed: &mut UnsupportedRows,
    ) {
        if self.account(attempt, fresh, budget, suppressed) == StallVerdict::Stalled {
            self.park(attempt, fresh, budget, suppressed);
        }
    }
}
