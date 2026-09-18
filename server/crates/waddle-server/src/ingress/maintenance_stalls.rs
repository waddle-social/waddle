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

struct StalledRow {
    evidence: RecoveryEvidence,
    consecutive: u32,
    classified: ClassificationEpisode,
    generation: u64,
}

#[derive(Default)]
pub(super) struct StalledRows {
    rows: HashMap<MessageKey, StalledRow>,
    order: VecDeque<MessageKey>,
}

impl StalledRows {
    pub(super) fn account(
        &mut self,
        attempt: &RecoveryAttempt,
        fresh: RecoveryEvidence,
        budget: MaintenanceBudget,
        suppressed: &mut UnsupportedRows,
    ) {
        if let Some(previous) = self.rows.get(&attempt.key) {
            if attempt.generation <= previous.generation {
                return;
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
        });
        row.generation = attempt.generation;
        if row.evidence != attempt.observed || fresh != attempt.observed {
            row.consecutive = 0;
            row.classified = ClassificationEpisode::Unclassified;
            suppressed.remove_stalled(attempt.key);
        }
        row.evidence = fresh;
        if attempt.classification == AttemptClassification::Inconclusive
            || fresh != attempt.observed
        {
            row.consecutive = 0;
            // A newer attempt can start before an older accounting worker parks
            // the row. Its uncertainty invalidates that older parking decision.
            suppressed.remove_stalled(attempt.key);
            return;
        }
        row.consecutive = row.consecutive.saturating_add(1);
        if row.consecutive < budget.recovery_stall_attempts {
            return;
        }
        suppressed.insert(
            attempt.key,
            fresh,
            Suppression::StalledUntil(Instant::now() + budget.recovery_stall_cooldown),
        );
        if row.classified == ClassificationEpisode::Unclassified {
            row.classified = ClassificationEpisode::Classified;
            for kind in &attempt.pending {
                increment_ingress_maintenance_unrecoverable_obligations(
                    1,
                    *kind,
                    IngressUnrecoverableReason::NoDurableProgress,
                );
            }
        }
    }
}
